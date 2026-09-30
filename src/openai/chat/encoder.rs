//! A request, encoded as a Chat Completions body.

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;

use crate::body::{
    Body, Encoding, Segment, attachment,
    escape::{escape_into, escaped_len},
};
use crate::{
    Effort, Error, ErrorKind, Image, Message, Part, ReasoningSource, Request, Role, Source,
    TextFile, ToolCall,
};

/// Attachments are read this many bytes at a time. A multiple of 3, so every base64 block but the
/// last encodes without padding and the blocks join into one valid string.
const BLOCK: usize = 48 * 1024;

/// Encodes a [`Request`] as a Chat Completions body.
///
/// The body carries `model`, `messages`, and `stream: true`, and otherwise only what the request
/// sets. Its exact length is known before the first byte, and doubles as the estimate for context
/// admission.
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
    block: usize,
    context: Option<u64>,
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
            block: BLOCK,
            context: None,
        }
    }

    /// Leaves out the optional fields, `reasoning_effort` and `stream_options`, for a server
    /// known to reject them.
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
        self.block = (bytes / 3).max(1) * 3;
        self
    }

    /// Sets the model's context size for admission: a request whose body length plus
    /// `max_tokens` does not fit, or whose `max_tokens` is 0, is
    /// [`ErrorKind::ContextOverflow`] before anything is sent. The body length in bytes stands in
    /// for its length in tokens, which it never underestimates.
    pub fn context_tokens(mut self, tokens: u64) -> Self {
        self.context = Some(tokens);
        self
    }

    /// Encodes `request` without I/O. Every attachment must be in memory; one held as a file
    /// path is [`ErrorKind::Attachment`], and needs `encode_files`.
    pub fn encode(&self, request: &Request) -> Result<Body, Error> {
        self.build(request, &mut |source, encoding| match source {
            Source::Bytes(data) => measure_bytes(data, encoding),
            _ => Err(attachment(
                "a file attachment needs Encoder::encode_files (feature `client`)",
            )),
        })
    }

    /// Encodes `request`, first measuring the attachments held as file paths: an image's size,
    /// and a text file's length once escaped, which takes one read of the file. The files are
    /// read again while the body streams, and must not have changed by then.
    #[cfg(feature = "client")]
    pub async fn encode_files(&self, request: &Request) -> Result<Body, Error> {
        let mut measured = std::collections::VecDeque::new();
        for (source, encoding) in attachments(request) {
            if let Source::Path(path) = source {
                measured.push_back(files::measure(path, encoding, self.block).await?);
            }
        }

        self.build(request, &mut |source, encoding| match source {
            Source::Bytes(data) => measure_bytes(data, encoding),
            _ => measured
                .pop_front()
                .ok_or_else(|| attachment("an attachment was not measured")),
        })
    }

    fn build(&self, request: &Request, measure: &mut Measure<'_>) -> Result<Body, Error> {
        let mut out = Segments::default();

        // `{"model":...,"stream":true` -- the object, left open for `messages`.
        let head = to_json(&Head {
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
                    kind: "function",
                    function: WireFunction {
                        name: &tool.name,
                        description: &tool.description,
                        parameters: &tool.input_schema,
                    },
                })
                .collect(),
        })?;

        out.raw(&head[..head.len() - 1]);
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
                            return Err(unsupported("a tool message carries only tool results"));
                        };
                        separate(&mut out);
                        out.raw(br#"{"role":"tool","tool_call_id":"#);
                        out.string(&result.call_id);
                        out.raw(br#","content":"#);
                        out.string(&result.content);
                        out.raw(b"}");
                    }
                }
            }
        }
        out.raw(b"]}");

        let (segments, length) = out.finish();
        if let Some(context) = self.context {
            let reserved = request.max_tokens.unwrap_or(0);
            if request.max_tokens == Some(0) || length.saturating_add(reserved) > context {
                return Err(Error::new(ErrorKind::ContextOverflow)
                    .with_detail("the body and max_tokens do not fit in the context size"));
            }
        }

        Ok(Body::new(segments, length, self.block))
    }
}

/// Measures one attachment: its size, and what it encodes to.
type Measure<'a> = dyn FnMut(&Source, Encoding) -> Result<Measured, Error> + 'a;

#[derive(Debug, Clone, Copy)]
struct Measured {
    size: u64,
    encoded: u64,
}

fn measure_bytes(data: &Bytes, encoding: Encoding) -> Result<Measured, Error> {
    let size = data.len() as u64;
    let encoded = match encoding {
        Encoding::Base64 => base64_len(size),
        Encoding::JsonString => {
            if std::str::from_utf8(data).is_err() {
                return Err(attachment("a text file is not UTF-8"));
            }
            escaped_len(data)
        }
    };

    Ok(Measured { size, encoded })
}

/// How long the base64 of `size` bytes is, padding included.
fn base64_len(size: u64) -> u64 {
    size.div_ceil(3) * 4
}

/// The attachments of a request, in the order the body uses them.
#[cfg(feature = "client")]
fn attachments(request: &Request) -> impl Iterator<Item = (&Source, Encoding)> {
    fn of(message: &Message, images: bool) -> impl Iterator<Item = (&Source, Encoding)> {
        message.parts.iter().filter_map(move |part| match part {
            Part::File(file) if !images => Some((&file.source, Encoding::JsonString)),
            Part::Image(image) if images => Some((&image.source, Encoding::Base64)),
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
            _ => {
                return Err(unsupported(
                    "a user message carries text, files, and images",
                ));
            }
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
            let media_type = image
                .media_type
                .as_deref()
                .ok_or_else(|| attachment("an image has no media type"))?;
            out.raw(br#"{"type":"image_url","image_url":{"url":"data:"#);
            out.escaped(media_type.as_bytes());
            out.raw(b";base64,");
            out.data(&image.source, Encoding::Base64, measure)?;
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
            Text::File(file) => {
                let name = file.name.replace('"', "&quot;");
                out.escaped(format!("<file name=\"{name}\">\n").as_bytes());
                out.data(&file.source, Encoding::JsonString, measure)?;
                out.escaped(b"\n</file>");
            }
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
            _ => {
                return Err(unsupported(
                    "a model message carries text, reasoning, and tool calls",
                ));
            }
        }
    }

    out.raw(br#"{"role":"assistant","content":"#);
    out.string(&text);

    if !calls.is_empty() {
        out.raw(br#","tool_calls":"#);
        out.raw(&to_json(&calls)?);
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

fn effort(effort: Effort) -> &'static str {
    match effort {
        Effort::Off => "none",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::XHigh => "xhigh",
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|source| {
        Error::new(ErrorKind::Unsupported)
            .with_detail("the request cannot be written as JSON")
            .with_source(source)
    })
}

fn unsupported(detail: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported).with_detail(detail)
}

/// The body as it is put together: literal bytes, run together until an attachment interrupts.
#[derive(Default)]
struct Segments {
    parts: Vec<Segment>,
    pending: Vec<u8>,
    length: u64,
}

impl Segments {
    /// Bytes that are JSON already.
    fn raw(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// Text, escaped for the JSON string being written.
    fn escaped(&mut self, text: &[u8]) {
        escape_into(text, &mut self.pending);
    }

    /// Text as a whole JSON string.
    fn string(&mut self, text: &str) {
        self.raw(b"\"");
        self.escaped(text.as_bytes());
        self.raw(b"\"");
    }

    /// An attachment, encoded when the body is read.
    fn data(
        &mut self,
        source: &Source,
        encoding: Encoding,
        measure: &mut Measure<'_>,
    ) -> Result<(), Error> {
        let measured = measure(source, encoding)?;
        self.flush();
        self.length += measured.encoded;
        self.parts.push(Segment::Data {
            source: source.clone(),
            size: measured.size,
            encoded: measured.encoded,
            encoding,
        });
        Ok(())
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            self.length += self.pending.len() as u64;
            self.parts.push(Segment::Bytes(Bytes::from(std::mem::take(
                &mut self.pending,
            ))));
        }
    }

    fn finish(mut self) -> (Vec<Segment>, u64) {
        self.flush();
        (self.parts, self.length)
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
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
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
            kind: "function",
            function: WireCallFunction {
                name: &call.name,
                arguments: &call.arguments,
            },
        }
    }
}

#[cfg(feature = "client")]
mod files {
    use std::path::Path;

    use tokio::{fs::File, io::AsyncReadExt};

    use super::{Encoding, Measured, attachment, base64_len};
    use crate::{
        Error, ErrorKind,
        body::escape::{Utf8Check, escaped_len},
    };

    /// Measures a file: an image by its size, a text file by reading it once.
    pub(super) async fn measure(
        path: &Path,
        encoding: Encoding,
        block: usize,
    ) -> Result<Measured, Error> {
        match encoding {
            Encoding::Base64 => {
                let size = tokio::fs::metadata(path).await.map_err(unreadable)?.len();
                Ok(Measured {
                    size,
                    encoded: base64_len(size),
                })
            }
            Encoding::JsonString => {
                let mut file = File::open(path).await.map_err(unreadable)?;
                let mut buffer = vec![0; block.max(8 * 1024)];
                let mut check = Utf8Check::default();
                let (mut size, mut encoded) = (0u64, 0u64);
                loop {
                    let read = file.read(&mut buffer).await.map_err(unreadable)?;
                    if read == 0 {
                        break;
                    }
                    if !check.push(&buffer[..read]) {
                        return Err(attachment("a text file is not UTF-8"));
                    }
                    size += read as u64;
                    encoded += escaped_len(&buffer[..read]);
                }
                if !check.finish() {
                    return Err(attachment("a text file is not UTF-8"));
                }
                Ok(Measured { size, encoded })
            }
        }
    }

    fn unreadable(source: std::io::Error) -> Error {
        Error::new(ErrorKind::Attachment)
            .with_detail("an attachment could not be read")
            .with_source(source)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Completion, FinishReason, Reasoning, Tool};

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
}
