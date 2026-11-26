//! A request, encoded as a Chat Completions body.

use serde::Serialize;
use serde_json::Value;

use super::wire::FUNCTION_TYPE;
use crate::{
    Effort, Error, Image, Message, Part, ReasoningSource, Request, ResponseFormat, Role, TextFile,
    ToolCall,
    body::Body,
    encode::{
        self, Choice, Measure, Segments, Settings, in_memory, media_type, not_for_model,
        not_for_tool, not_for_user,
    },
};
#[cfg(feature = "client")]
use crate::{Source, body::Encoding};

/// Encodes a [`Request`] as a Chat Completions body.
///
/// The body carries `model`, `messages`, and `stream: true`, and otherwise only what the request
/// sets. Its exact length is known before the first byte, and doubles as the estimate for context
/// admission, with each image counted as no more than [`image_tokens`](Self::image_tokens).
///
/// ```
/// use svir::openai::chat::Encoder;
/// use svir::Request;
///
/// let body = Encoder::new().encode(&Request::new("m").user("hi"))?;
/// let length = body.len();
/// let bytes = body.into_bytes()?;
///
/// assert_eq!(bytes.len() as u64, length);
/// assert_eq!(
///     bytes,
///     r#"{"model":"m","stream":true,"messages":[{"role":"user","content":"hi"}]}"#
/// );
/// # Ok::<(), svir::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Encoder {
    lean: bool,
    include_usage: bool,
    settings: Settings,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// An encoder that sends what the request sets and nothing else.
    pub fn new() -> Self {
        Self {
            lean: false,
            include_usage: false,
            settings: Settings::default(),
        }
    }

    /// Leaves out the optional fields, `reasoning_effort` and `stream_options`, for a server
    /// known to reject them. What the answer must meet, `tool_choice` and `response_format`, is
    /// not optional and stays.
    pub fn lean(mut self, lean: bool) -> Self {
        self.lean = lean;
        self
    }

    /// Whether to ask for token usage when the request does not say. Off by default here; the
    /// client turns it on.
    pub fn include_usage(mut self, include: bool) -> Self {
        self.include_usage = include;
        self
    }

    /// Sets how many bytes of an attachment are read at a time, rounded down to a multiple of 3
    /// and at least 3. The default, 48 KiB, suits everything but tests.
    pub fn block_bytes(mut self, bytes: usize) -> Self {
        self.settings.block_bytes(bytes);
        self
    }

    /// Sets the model's context size for admission: a request whose estimate plus `max_tokens`
    /// does not fit, or whose `max_tokens` is 0, is
    /// [`ErrorKind::ContextOverflow`](crate::ErrorKind::ContextOverflow) before anything is sent.
    /// The estimate is the body length in bytes, which never underestimates text, with each
    /// image's base64 counted as no more than [`image_tokens`](Self::image_tokens).
    pub fn context_tokens(mut self, tokens: u64) -> Self {
        self.settings.context_tokens(tokens);
        self
    }

    /// Sets the most one image counts for in admission: its base64 length, but no more than
    /// this many tokens. 16384 by default, the most the common vision encoders make of one image
    /// at their default settings; a model that charges less, as hosted APIs do, can be given its
    /// own figure. Without [`context_tokens`](Self::context_tokens) it has no effect.
    pub fn image_tokens(mut self, tokens: u64) -> Self {
        self.settings.image_tokens(tokens);
        self
    }

    /// Encodes `request` without I/O. Every attachment must be in memory; one held as a file
    /// path is [`ErrorKind::Attachment`](crate::ErrorKind::Attachment), and needs
    /// `encode_files`.
    pub fn encode(&self, request: &Request) -> Result<Body, Error> {
        let choice = tool_choice(request)?;

        self.build(request, choice, &mut in_memory)
    }

    /// Encodes `request`, first measuring the attachments held as file paths: an image's size,
    /// and a text file's length once escaped, which takes one read of the file unless
    /// [`TextFile::escaped_len`] declares it. The files are read again while the body streams,
    /// and must not have changed by then.
    #[cfg(feature = "client")]
    pub async fn encode_files(&self, request: &Request) -> Result<Body, Error> {
        // A tool choice that cannot be met fails before any file is read.
        let choice = tool_choice(request)?;
        let mut measured =
            crate::encode::measure_files(attachments(request), self.settings.block()).await?;

        self.build(
            request,
            choice,
            &mut crate::encode::measured_before(&mut measured),
        )
    }

    fn build<'a>(
        &self,
        request: &'a Request,
        tool_choice: Option<WireToolChoice<'a>>,
        measure: &mut Measure<'_>,
    ) -> Result<Body, Error> {
        let mut out = Segments::default();

        // `{"model":...,"stream":true` -- the object, left open for `messages`.
        out.open_object(&Head {
            model: &request.model,
            stream: true,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            reasoning_effort: request.reasoning.filter(|_| !self.lean).map(effort),
            stream_options: (!self.lean && request.include_usage.unwrap_or(self.include_usage))
                .then_some(StreamOptions {
                    include_usage: true,
                }),
            tools: request
                .tools
                .iter()
                .map(|tool| WireTool {
                    kind: FUNCTION_TYPE,
                    function: WireFunction {
                        name: &tool.name,
                        description: &tool.description,
                        parameters: &tool.input_schema,
                    },
                })
                .collect(),
            tool_choice,
            response_format: response_format(&request.response_format),
        })?;
        out.raw(br#","messages":["#);

        let mut first = true;
        let mut separate = |out: &mut Segments| {
            if !std::mem::take(&mut first) {
                out.raw(b",");
            }
        };

        if let Some(system) = &request.system {
            separate(&mut out);
            out.raw(br#"{"role":"system","content":"#);
            out.string(system);
            out.raw(b"}");
        }

        for message in &request.messages {
            match message.role {
                Role::User => {
                    separate(&mut out);
                    user(message, &mut out, measure)?;
                }
                Role::Assistant => {
                    separate(&mut out);
                    assistant(message, request.send_reasoning, &mut out)?;
                }
                Role::Tool => {
                    for part in &message.parts {
                        let Part::ToolResult(result) = part else {
                            return Err(not_for_tool());
                        };
                        separate(&mut out);
                        out.raw(br#"{"role":"tool","tool_call_id":"#);
                        out.string(&result.call_id);
                        // Chat Completions has no field for a failure; it is told in the text.
                        out.raw(br#","content":""#);
                        if result.is_error {
                            out.escaped(b"error: ");
                        }
                        out.escaped(result.content.as_bytes());
                        out.raw(br#""}"#);
                    }
                }
            }
        }
        out.raw(b"]}");

        self.settings.body(out, request.max_tokens)
    }
}

/// The attachments of a request, in the order the body uses them, with the escaped length a
/// text file declares.
#[cfg(feature = "client")]
fn attachments(request: &Request) -> impl Iterator<Item = (&Source, Encoding, Option<u64>)> {
    fn of(
        message: &Message,
        images: bool,
    ) -> impl Iterator<Item = (&Source, Encoding, Option<u64>)> {
        message.parts.iter().filter_map(move |part| match part {
            Part::File(file) if !images => {
                Some((&file.source, Encoding::JsonString, file.escaped_len))
            }
            Part::Image(image) if images => Some((&image.source, Encoding::Base64, None)),
            _ => None,
        })
    }

    request
        .messages
        .iter()
        .flat_map(|message| of(message, false).chain(of(message, true)))
}

/// A user message: text and files as one text, then images.
fn user(message: &Message, out: &mut Segments, measure: &mut Measure<'_>) -> Result<(), Error> {
    let mut texts: Vec<Text<'_>> = Vec::new();
    let mut images: Vec<&Image> = Vec::new();
    for part in &message.parts {
        match part {
            Part::Text(text) => texts.push(Text::Plain(text)),
            Part::File(file) => texts.push(Text::File(file)),
            Part::Image(image) => images.push(image),
            _ => return Err(not_for_user()),
        }
    }

    out.raw(br#"{"role":"user","content":"#);

    if images.is_empty() {
        content_string(&texts, out, measure)?;
    } else {
        out.raw(b"[");
        if !texts.is_empty() {
            out.raw(br#"{"type":"text","text":"#);
            content_string(&texts, out, measure)?;
            out.raw(b"}");
        }
        for (i, image) in images.iter().enumerate() {
            if i > 0 || !texts.is_empty() {
                out.raw(b",");
            }
            let media_type = media_type(image)?;
            out.raw(br#"{"type":"image_url","image_url":{"url":"data:"#);
            out.escaped(media_type.as_bytes());
            out.raw(b";base64,");
            out.image(&image.source, measure)?;
            out.raw(br#""}}"#);
        }
        out.raw(b"]");
    }

    out.raw(b"}");

    Ok(())
}

enum Text<'a> {
    Plain(&'a str),
    File(&'a TextFile),
}

/// Text and files as one JSON string, in the order given, separated by a blank line.
fn content_string(
    texts: &[Text<'_>],
    out: &mut Segments,
    measure: &mut Measure<'_>,
) -> Result<(), Error> {
    out.raw(b"\"");
    for (i, text) in texts.iter().enumerate() {
        if i > 0 {
            out.escaped(b"\n\n");
        }

        match text {
            Text::Plain(text) => out.escaped(text.as_bytes()),
            Text::File(file) => out.file(file, measure)?,
        }
    }

    out.raw(b"\"");
    Ok(())
}

/// A model message: its text, its tool calls, and, when asked, its reasoning.
fn assistant(message: &Message, send_reasoning: bool, out: &mut Segments) -> Result<(), Error> {
    let mut text = String::new();
    let mut calls: Vec<WireCall<'_>> = Vec::new();
    let mut reasoning_content = String::new();
    let mut reasoning = String::new();

    for part in &message.parts {
        match part {
            Part::Text(piece) => {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(piece);
            }
            Part::ToolCall(call) => calls.push(WireCall::from(call)),
            // Reasoning goes back under the field it arrived in. What was split out of <think>
            // tags has no such field and never goes back.
            Part::Reasoning(part) => match part.source {
                ReasoningSource::ReasoningContent => reasoning_content.push_str(&part.text),
                ReasoningSource::Reasoning => reasoning.push_str(&part.text),
                _ => {}
            },
            _ => return Err(not_for_model()),
        }
    }

    out.raw(br#"{"role":"assistant","content":"#);
    out.string(&text);

    if !calls.is_empty() {
        out.raw(br#","tool_calls":"#);
        out.json(&calls)?;
    }

    if send_reasoning {
        for (key, text) in [
            (&br#","reasoning_content":"#[..], &reasoning_content),
            (&br#","reasoning":"#[..], &reasoning),
        ] {
            if !text.is_empty() {
                out.raw(key);
                out.string(text);
            }
        }
    }

    out.raw(b"}");
    Ok(())
}

/// The tool choice as Chat Completions writes it.
fn tool_choice(request: &Request) -> Result<Option<WireToolChoice<'_>>, Error> {
    Ok(encode::tool_choice(request)?.map(|choice| match choice {
        Choice::None => WireToolChoice::Mode("none"),
        Choice::Required => WireToolChoice::Mode("required"),
        Choice::Tool(name) => WireToolChoice::Function {
            kind: FUNCTION_TYPE,
            function: WireName { name },
        },
    }))
}

fn response_format(format: &ResponseFormat) -> Option<WireFormat<'_>> {
    match format {
        ResponseFormat::Text => None,
        ResponseFormat::Json => Some(WireFormat::JsonObject),
        ResponseFormat::Schema(schema) => Some(WireFormat::JsonSchema {
            json_schema: WireSchema {
                name: &schema.name,
                schema: &schema.schema,
                strict: schema.strict,
            },
        }),
    }
}

fn effort(effort: Effort) -> &'static str {
    match effort {
        Effort::Off => "none",
        Effort::Minimal => "minimal",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::XHigh => "xhigh",
        Effort::Max => "max",
    }
}

#[derive(Serialize)]
struct Head<'a> {
    model: &'a str,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<WireToolChoice<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<WireFormat<'a>>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireToolChoice<'a> {
    /// `none` or `required`.
    Mode(&'static str),
    /// One tool, by name.
    Function {
        #[serde(rename = "type")]
        kind: &'static str,
        function: WireName<'a>,
    },
}

#[derive(Serialize)]
struct WireName<'a> {
    name: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireFormat<'a> {
    JsonObject,
    JsonSchema { json_schema: WireSchema<'a> },
}

#[derive(Serialize)]
struct WireSchema<'a> {
    name: &'a str,
    schema: &'a Value,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    strict: bool,
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireFunction<'a>,
}

#[derive(Serialize)]
struct WireFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
}

#[derive(Serialize)]
struct WireCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireCallFunction<'a>,
}

#[derive(Serialize)]
struct WireCallFunction<'a> {
    name: &'a str,
    arguments: &'a str,
}

impl<'a> From<&'a ToolCall> for WireCall<'a> {
    fn from(call: &'a ToolCall) -> Self {
        Self {
            id: &call.id,
            kind: FUNCTION_TYPE,
            function: WireCallFunction {
                name: &call.name,
                arguments: &call.arguments,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Completion, ErrorKind, FinishReason, Reasoning, Tool};

    fn body(encoder: &Encoder, request: &Request) -> Value {
        let body = encoder.encode(request).unwrap();
        let length = body.len();
        let bytes = body.into_bytes().unwrap();
        assert_eq!(bytes.len() as u64, length, "the promised length");
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn a_short_temperature_stays_short() {
        let request = Request::new("m").temperature(0.1).user("hi");
        assert_eq!(body(&Encoder::new(), &request)["temperature"], json!(0.1));
    }

    #[test]
    fn the_client_default_for_usage_yields_to_the_request() {
        let on = Encoder::new().include_usage(true);
        let asked = json!({"include_usage": true});
        assert_eq!(
            body(&on, &Request::new("m").user("hi"))["stream_options"],
            asked
        );
        assert_eq!(
            body(&on, &Request::new("m").include_usage(false).user("hi")).get("stream_options"),
            None
        );
        assert_eq!(
            body(&on.lean(true), &Request::new("m").user("hi")).get("stream_options"),
            None
        );
    }

    #[test]
    fn effort_off_is_sent_as_none() {
        let request = Request::new("m").reasoning(Effort::Off).user("hi");
        assert_eq!(body(&Encoder::new(), &request)["reasoning_effort"], "none");
    }

    #[test]
    fn think_reasoning_is_never_sent_back() {
        let mut done = Completion::new(FinishReason::Stop);
        done.text = "42".into();
        done.reasoning = vec![
            Reasoning::new(ReasoningSource::Think, "inline"),
            Reasoning::new(ReasoningSource::Reasoning, "field"),
        ];
        let request = Request::new("m")
            .user("hi")
            .assistant(done)
            .send_reasoning(true);
        assert_eq!(
            body(&Encoder::new(), &request)["messages"][1],
            json!({"role": "assistant", "content": "42", "reasoning": "field"})
        );
    }

    #[test]
    fn parts_a_role_cannot_carry_are_an_error_not_a_loss() {
        let misplaced =
            Request::new("m").message(Message::assistant("x").with(Image::path("a.png")));
        assert_eq!(
            Encoder::new().encode(&misplaced).unwrap_err().kind(),
            ErrorKind::Unsupported
        );

        let no_type = Request::new("m").message(Message::user("").with(Image::path("a.bin")));
        assert_eq!(
            Encoder::new().encode(&no_type).unwrap_err().kind(),
            ErrorKind::Attachment
        );
    }

    #[test]
    fn a_file_path_needs_the_measuring_encoder() {
        let request = Request::new("m").message(Message::user("").with(Image::path("a.png")));
        assert_eq!(
            Encoder::new().encode(&request).unwrap_err().kind(),
            ErrorKind::Attachment
        );
    }

    #[cfg(feature = "client")]
    #[tokio::test]
    async fn a_tool_choice_that_cannot_be_met_fails_before_any_file_is_read() {
        let request = Request::new("m")
            .tool_choice(crate::ToolChoice::Required)
            .message(Message::user("").with(TextFile::path("no-such-file.txt")));

        let error = Encoder::new().encode_files(&request).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn tools_are_function_tools_with_no_tool_choice() {
        let request = Request::new("m")
            .tool(Tool::new("now", "The time."))
            .user("hi");
        let body = body(&Encoder::new(), &request);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "now");
        assert_eq!(body.get("tool_choice"), None);
    }

    #[test]
    fn an_image_counts_16384_tokens_at_most_by_default() {
        let photo = Image::bytes(vec![0u8; 30_000], "image/jpeg");
        let request = Request::new("m")
            .max_tokens(16)
            .message(Message::user("").with(photo));

        let length = Encoder::new().encode(&request).unwrap().len();
        let estimate = length - crate::encode::base64_len(30_000) + 16_384;
        let admits = |context: u64| Encoder::new().context_tokens(context).encode(&request);

        assert!(admits(estimate + 16).is_ok());
        assert_eq!(
            admits(estimate + 15).unwrap_err().kind(),
            ErrorKind::ContextOverflow
        );
    }
}
