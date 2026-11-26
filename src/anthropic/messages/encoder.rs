//! A request, encoded as a Messages body.

use serde::Serialize;
use serde_json::Value;

use crate::{
    Effort, Error, Image, Message, Part, Reasoning, ReasoningSource, Request, ResponseFormat, Role,
    ToolCall, ToolResult,
    body::Body,
    encode::{
        self, Choice, Measure, Segments, Settings, in_memory, media_type, not_for_model,
        not_for_tool, not_for_user, unsupported,
    },
};
#[cfg(feature = "client")]
use crate::{Source, body::Encoding};

/// Encodes a [`Request`] as a Messages body.
///
/// The body carries `model`, `max_tokens`, and `stream: true`, and otherwise only what the request
/// sets. Every part is a block of its own, in the order given; a model message goes back as it
/// came, its thinking blocks included. Its exact length is known before the first byte, and
/// doubles as the estimate for context admission, with each image counted as no more than
/// [`image_tokens`](Self::image_tokens).
///
/// ```
/// use svir::anthropic::messages::Encoder;
/// use svir::Request;
///
/// let body = Encoder::new().encode(&Request::new("m").max_tokens(16).user("hi"))?;
/// let length = body.len();
/// let bytes = body.into_bytes()?;
///
/// assert_eq!(bytes.len() as u64, length);
/// assert_eq!(
///     bytes,
///     r#"{"model":"m","max_tokens":16,"stream":true,"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#
/// );
/// # Ok::<(), svir::Error>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct Encoder {
    settings: Settings,
}

impl Encoder {
    /// An encoder that sends what the request sets, and nothing else.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets how many bytes of an attachment are read at a time, rounded down to a multiple of 3
    /// and at least 3. The default, 48 KiB, suits everything but tests.
    pub fn block_bytes(mut self, bytes: usize) -> Self {
        self.settings.block_bytes(bytes);
        self
    }

    /// Sets the model's context size for admission: a request whose estimate plus `max_tokens`
    /// does not fit, or whose `max_tokens` is 0, is
    /// [`ErrorKind::ContextOverflow`](crate::ErrorKind::ContextOverflow) before anything is
    /// sent. The estimate is the body length in bytes, which never underestimates text, with
    /// each image's base64 counted as no more than [`image_tokens`](Self::image_tokens).
    pub fn context_tokens(mut self, tokens: u64) -> Self {
        self.settings.context_tokens(tokens);
        self
    }

    /// Sets the most one image counts for in admission: its base64 length, but no more than
    /// this many tokens. 16384 by default. Without [`context_tokens`](Self::context_tokens) it
    /// has no effect.
    pub fn image_tokens(mut self, tokens: u64) -> Self {
        self.settings.image_tokens(tokens);
        self
    }

    /// Encodes `request` without I/O. Every attachment must be in memory; one held as a file
    /// path is [`ErrorKind::Attachment`](crate::ErrorKind::Attachment), and needs
    /// `encode_files`.
    pub fn encode(&self, request: &Request) -> Result<Body, Error> {
        let head = head(request)?;

        self.build(request, &head, &mut in_memory)
    }

    /// Encodes `request`, first measuring the attachments held as file paths, as
    /// [`openai::chat::Encoder::encode_files`](crate::openai::chat::Encoder::encode_files)
    /// does.
    #[cfg(feature = "client")]
    pub async fn encode_files(&self, request: &Request) -> Result<Body, Error> {
        // A request the API cannot take fails before any file is read.
        let head = head(request)?;
        let mut measured =
            encode::measure_files(attachments(request), self.settings.block()).await?;

        self.build(request, &head, &mut encode::measured_before(&mut measured))
    }

    fn build(
        &self,
        request: &Request,
        head: &Head<'_>,
        measure: &mut Measure<'_>,
    ) -> Result<Body, Error> {
        let mut out = Segments::default();

        // `{"model":...,"stream":true` -- the object, left open for `messages`.
        out.open_object(head)?;
        out.raw(br#","messages":["#);

        let mut turns = Turns::default();
        for message in &request.messages {
            match message.role {
                Role::User => user(message, &mut turns, &mut out, measure)?,
                Role::Assistant => assistant(message, &mut turns, &mut out)?,
                Role::Tool => tool(message, &mut turns, &mut out)?,
            }
        }
        turns.close(&mut out);
        out.raw(b"]}");

        self.settings.body(out, request.max_tokens)
    }
}

/// Everything before `messages`, once the request is known to be one the API can take.
fn head(request: &Request) -> Result<Head<'_>, Error> {
    let max_tokens = request
        .max_tokens
        .ok_or_else(|| unsupported("the Messages API needs max_tokens"))?;

    let (thinking, effort) = match request.reasoning {
        None => (None, None),
        Some(Effort::Off) => (Some(WireThinking::Disabled), None),
        Some(Effort::Minimal) => {
            return Err(unsupported("the Messages API has no minimal effort"));
        }
        Some(level) => (
            Some(WireThinking::Adaptive {
                display: "summarized",
            }),
            Some(effort(level)),
        ),
    };

    let format = match &request.response_format {
        ResponseFormat::Text => None,
        ResponseFormat::Json => {
            return Err(unsupported(
                "the Messages API takes a JSON answer only with a schema",
            ));
        }
        ResponseFormat::Schema(schema) => Some(WireFormat::JsonSchema {
            schema: &schema.schema,
        }),
    };

    let tool_choice = encode::tool_choice(request)?.map(|choice| match choice {
        Choice::None => WireToolChoice::None,
        Choice::Required => WireToolChoice::Any,
        Choice::Tool(name) => WireToolChoice::Tool { name },
    });

    Ok(Head {
        model: &request.model,
        max_tokens,
        stream: true,
        system: request.system.as_deref(),
        temperature: request.temperature,
        thinking,
        output_config: (effort.is_some() || format.is_some())
            .then_some(OutputConfig { effort, format }),
        tools: request
            .tools
            .iter()
            .map(|tool| WireTool {
                name: &tool.name,
                description: &tool.description,
                input_schema: &tool.input_schema,
            })
            .collect(),
        tool_choice,
    })
}

fn effort(level: Effort) -> &'static str {
    match level {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::XHigh => "xhigh",
        // `Off` and `Minimal` never get here; anything above `xhigh` is all the model has.
        _ => "max",
    }
}

/// The attachments of a request, in the order the body uses them, with the escaped length a
/// text file declares.
#[cfg(feature = "client")]
fn attachments(request: &Request) -> impl Iterator<Item = (&Source, Encoding, Option<u64>)> {
    request
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            Part::File(file) => Some((&file.source, Encoding::JsonString, file.escaped_len)),
            Part::Image(image) => Some((&image.source, Encoding::Base64, None)),
            _ => None,
        })
}

/// The wire messages as they are written: messages of one wire role in a row are one message,
/// opened at its first block, so a message that writes no block leaves nothing behind.
#[derive(Default)]
struct Turns {
    /// The role of the message open now, if one is.
    open: Option<&'static str>,
    /// Whether a message was written before the open one.
    written: bool,
    /// Whether the open message has a block yet.
    blocks: bool,
}

impl Turns {
    /// Starts a block of a message of `role`: in the message open now if it has that role, or in
    /// a new one.
    fn block(&mut self, role: &'static str, out: &mut Segments) {
        if self.open == Some(role) {
            if self.blocks {
                out.raw(b",");
            }
        } else {
            self.close(out);
            if self.written {
                out.raw(b",");
            }
            out.raw(br#"{"role":"#);
            out.string(role);
            out.raw(br#","content":["#);
            self.open = Some(role);
        }
        self.blocks = true;
    }

    /// Ends the open message, if one is.
    fn close(&mut self, out: &mut Segments) {
        if self.open.take().is_some() {
            out.raw(b"]}");
            self.written = true;
            self.blocks = false;
        }
    }
}

const USER: &str = "user";
const ASSISTANT: &str = "assistant";

/// A user message: its parts as blocks, in order.
fn user(
    message: &Message,
    turns: &mut Turns,
    out: &mut Segments,
    measure: &mut Measure<'_>,
) -> Result<(), Error> {
    for part in &message.parts {
        match part {
            // The API takes no empty text block.
            Part::Text(text) if text.is_empty() => {}
            Part::Text(text) => {
                turns.block(USER, out);
                text_block(text, out);
            }
            Part::File(file) => {
                turns.block(USER, out);
                out.raw(br#"{"type":"text","text":""#);
                out.file(file, measure)?;
                out.raw(br#""}"#);
            }
            Part::Image(image) => {
                turns.block(USER, out);
                image_block(image, out, measure)?;
            }
            _ => return Err(not_for_user()),
        }
    }
    Ok(())
}

/// A model message, as it came: text, thinking blocks, and tool calls, in order.
fn assistant(message: &Message, turns: &mut Turns, out: &mut Segments) -> Result<(), Error> {
    for part in &message.parts {
        match part {
            Part::Text(text) if text.is_empty() => {}
            Part::Text(text) => {
                turns.block(ASSISTANT, out);
                text_block(text, out);
            }
            Part::Reasoning(reasoning) => {
                // Only the API's own blocks go back to it; other reasoning carries no signature
                // it would take (D54).
                if matches!(
                    reasoning.source,
                    ReasoningSource::Thinking | ReasoningSource::RedactedThinking
                ) {
                    turns.block(ASSISTANT, out);
                    reasoning_block(reasoning, out);
                }
            }
            Part::ToolCall(call) => {
                let input = input(call)?;
                turns.block(ASSISTANT, out);
                out.raw(br#"{"type":"tool_use","id":"#);
                out.string(&call.id);
                out.raw(br#","name":"#);
                out.string(&call.name);
                out.raw(br#","input":"#);
                out.raw(input.as_bytes());
                out.raw(b"}");
            }
            _ => return Err(not_for_model()),
        }
    }
    Ok(())
}

/// A tool message: a user message of tool results.
fn tool(message: &Message, turns: &mut Turns, out: &mut Segments) -> Result<(), Error> {
    for part in &message.parts {
        let Part::ToolResult(result) = part else {
            return Err(not_for_tool());
        };
        turns.block(USER, out);
        result_block(result, out);
    }
    Ok(())
}

fn text_block(text: &str, out: &mut Segments) {
    out.raw(br#"{"type":"text","text":"#);
    out.string(text);
    out.raw(b"}");
}

fn image_block(image: &Image, out: &mut Segments, measure: &mut Measure<'_>) -> Result<(), Error> {
    let media_type = media_type(image)?;

    out.raw(br#"{"type":"image","source":{"type":"base64","media_type":"#);
    out.string(media_type);
    out.raw(br#","data":""#);
    out.image(&image.source, measure)?;
    out.raw(br#""}}"#);

    Ok(())
}

/// A thinking block with its signature, or a redacted block with its data.
fn reasoning_block(reasoning: &Reasoning, out: &mut Segments) {
    let signature = reasoning.signature.as_deref().unwrap_or_default();

    if reasoning.source == ReasoningSource::RedactedThinking {
        out.raw(br#"{"type":"redacted_thinking","data":"#);
        out.string(signature);
    } else {
        out.raw(br#"{"type":"thinking","thinking":"#);
        out.string(&reasoning.text);
        out.raw(br#","signature":"#);
        out.string(signature);
    }
    out.raw(b"}");
}

fn result_block(result: &ToolResult, out: &mut Segments) {
    out.raw(br#"{"type":"tool_result","tool_use_id":"#);
    out.string(&result.call_id);
    out.raw(br#","content":"#);
    out.string(&result.content);
    if result.is_error {
        out.raw(br#","is_error":true"#);
    }
    out.raw(b"}");
}

/// A call's arguments as its input: a JSON object, written as it came, or `{}` for none.
fn input(call: &ToolCall) -> Result<&str, Error> {
    let arguments = call.arguments.trim();
    if arguments.is_empty() {
        return Ok("{}");
    }

    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(_)) => Ok(arguments),
        _ => Err(unsupported(
            "tool call arguments that are not a JSON object",
        )),
    }
}

#[derive(Serialize)]
struct Head<'a> {
    model: &'a str,
    max_tokens: u64,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<WireThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<WireToolChoice<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireThinking {
    Adaptive { display: &'static str },
    Disabled,
}

#[derive(Serialize)]
struct OutputConfig<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<WireFormat<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireFormat<'a> {
    JsonSchema { schema: &'a Value },
}

#[derive(Serialize)]
struct WireTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a Value,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireToolChoice<'a> {
    None,
    Any,
    Tool { name: &'a str },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    #[test]
    fn a_message_that_writes_no_block_leaves_nothing() {
        let request = Request::new("m")
            .max_tokens(16)
            .user("hi")
            .message(
                Message::new(Role::Assistant)
                    .with(Reasoning::new(ReasoningSource::ReasoningContent, "chat")),
            )
            .user("again");

        let bytes = Encoder::new()
            .encode(&request)
            .unwrap()
            .into_bytes()
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_file_path_needs_the_measuring_encoder() {
        let request = Request::new("m")
            .max_tokens(16)
            .message(Message::user("").with(Image::path("a.png")));
        assert_eq!(
            Encoder::new().encode(&request).unwrap_err().kind(),
            ErrorKind::Attachment
        );
    }
}
