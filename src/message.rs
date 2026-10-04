//! Messages and their parts: text, attachments, reasoning, tool calls, and tool results.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::{Completion, Reasoning, ToolCall};

/// Who a message is from.
///
/// There is no system role: the system prompt is [`Request::system`](crate::Request::system),
/// placed where each API expects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    /// The person or the application asking.
    User,
    /// The model.
    Assistant,
    /// Results of the tool calls the model made.
    Tool,
}

/// One turn of a conversation: a role, and its parts in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Message {
    /// Who it is from.
    pub role: Role,
    /// What it says, in order.
    pub parts: Vec<Part>,
}

impl Message {
    /// An empty message from `role`.
    pub fn new(role: Role) -> Self {
        Self {
            role,
            parts: Vec::new(),
        }
    }

    /// A user message with this text. Empty text adds no part, so `Message::user("")` followed
    /// by images carries only the images.
    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User).with_text(text.into())
    }

    /// A model message with this text, for example when replaying a stored conversation.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant).with_text(text.into())
    }

    /// A message carrying the result of one tool call.
    pub fn tool_result(result: ToolResult) -> Self {
        Self::new(Role::Tool).with(result)
    }

    /// Adds a part after the ones already there.
    pub fn with(mut self, part: impl Into<Part>) -> Self {
        self.parts.push(part.into());
        self
    }

    fn with_text(self, text: String) -> Self {
        if text.is_empty() {
            self
        } else {
            self.with(text)
        }
    }
}

impl From<Completion> for Message {
    /// The model's answer as the next request needs it: reasoning, text, then tool calls.
    fn from(done: Completion) -> Self {
        let mut parts = Vec::with_capacity(done.reasoning.len() + 1 + done.calls.len());
        parts.extend(done.reasoning.into_iter().map(Part::Reasoning));
        if !done.text.is_empty() {
            parts.push(Part::Text(done.text));
        }
        parts.extend(done.calls.into_iter().map(Part::ToolCall));
        Self {
            role: Role::Assistant,
            parts,
        }
    }
}

/// A piece of a message.
///
/// Which parts a role may carry is checked when the request is encoded; nothing is dropped
/// silently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Part {
    /// Text.
    Text(String),
    /// An image.
    Image(Image),
    /// A text file, sent as text inside the message.
    File(TextFile),
    /// Reasoning the model produced with this answer.
    Reasoning(Reasoning),
    /// A tool call the model made.
    ToolCall(ToolCall),
    /// The result of a tool call.
    ToolResult(ToolResult),
}

impl From<String> for Part {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for Part {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<Image> for Part {
    fn from(image: Image) -> Self {
        Self::Image(image)
    }
}

impl From<TextFile> for Part {
    fn from(file: TextFile) -> Self {
        Self::File(file)
    }
}

impl From<Reasoning> for Part {
    fn from(reasoning: Reasoning) -> Self {
        Self::Reasoning(reasoning)
    }
}

impl From<ToolCall> for Part {
    fn from(call: ToolCall) -> Self {
        Self::ToolCall(call)
    }
}

impl From<ToolResult> for Part {
    fn from(result: ToolResult) -> Self {
        Self::ToolResult(result)
    }
}

/// Where an attachment's bytes are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Source {
    /// A file, read while the request body streams. Its size is measured when the body is built
    /// and checked while it streams.
    Path(PathBuf),
    /// Bytes in memory. Serialized as base64.
    Bytes(#[serde(with = "base64_bytes")] Bytes),
}

/// An image attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Image {
    /// Where the bytes are.
    #[serde(flatten)]
    pub source: Source,
    /// The media type, such as `image/png`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

impl Image {
    /// An image file. Nothing is read yet; the media type comes from the extension (`png`, `jpg`,
    /// `jpeg`, `gif`, `webp`) unless [`media_type`](Self::media_type) sets it.
    pub fn path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let media_type = media_type_of(&path);
        Self {
            source: Source::Path(path),
            media_type,
        }
    }

    /// An image in memory.
    pub fn bytes(data: impl Into<Bytes>, media_type: impl Into<String>) -> Self {
        Self {
            source: Source::Bytes(data.into()),
            media_type: Some(media_type.into()),
        }
    }

    /// Sets the media type.
    pub fn media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }
}

/// A text file attachment, sent as text inside the message with its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TextFile {
    /// The name the model is told.
    pub name: String,
    /// Where the bytes are. They must be UTF-8 text.
    #[serde(flatten)]
    pub source: Source,
    /// The file's length once escaped into a JSON string, when the caller measured it already,
    /// with [`body::escaped_len`](crate::body::escaped_len). See
    /// [`escaped_len`](Self::escaped_len).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escaped_len: Option<u64>,
}

impl TextFile {
    /// A text file. Nothing is read yet; the name is the file name unless
    /// [`name`](Self::name) sets it.
    pub fn path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            name,
            source: Source::Path(path),
            escaped_len: None,
        }
    }

    /// Text in memory, sent as a file with this name.
    pub fn text(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: Source::Bytes(Bytes::from(text.into())),
            escaped_len: None,
        }
    }

    /// Sets the name the model is told.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Declares the file's length once escaped into a JSON string, as
    /// [`body::escaped_len`](crate::body::escaped_len) measures it, for example when the file
    /// was stored. The body is then built without reading the file first: its size is all that
    /// is looked up.
    ///
    /// A length the file does not have fails with [`ErrorKind::Attachment`](crate::ErrorKind::Attachment),
    /// when the body is built or while it streams, and so does a file that is not UTF-8: no
    /// body goes out that disagrees with its length.
    pub fn escaped_len(mut self, escaped: u64) -> Self {
        self.escaped_len = Some(escaped);
        self
    }
}

/// The result of a tool call, as the caller's own string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolResult {
    /// The ID of the call this answers.
    pub call_id: String,
    /// The result. Structured results are serialized by the caller.
    pub content: String,
    /// The call failed, and `content` says what went wrong. Each wire API tells the model as it
    /// can: Chat Completions has no field for it, and sends `error: ` before the content.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

impl ToolResult {
    /// The result of call `call_id`.
    pub fn new(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// The failure of call `call_id`: `message` says what went wrong, so the model can try
    /// again or tell the user.
    pub fn error(call_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::new(call_id, message)
        }
    }
}

fn media_type_of(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    };
    Some(media_type.to_owned())
}

/// `Bytes` as a base64 string.
mod base64_bytes {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub(super) fn serialize<S: Serializer>(
        value: &Bytes,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(value))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Bytes, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD
            .decode(text)
            .map(Bytes::from)
            .map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinishReason, ReasoningSource};

    #[test]
    fn parts_keep_the_order_given() {
        let msg = Message::user("What changed?")
            .with(Image::path("before.png"))
            .with(TextFile::path("dir/diff.patch"))
            .with(Image::path("after.JPG"));
        let kinds: Vec<_> = msg
            .parts
            .iter()
            .map(|p| match p {
                Part::Text(_) => "text",
                Part::Image(_) => "image",
                Part::File(_) => "file",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["text", "image", "file", "image"]);
    }

    #[test]
    fn empty_text_adds_no_part() {
        let msg = Message::user("").with(Image::path("a.png"));
        assert_eq!(msg.parts.len(), 1);
        assert!(Message::assistant("").parts.is_empty());
    }

    #[test]
    fn attachments_know_their_media_type_and_name_without_io() {
        assert_eq!(
            Image::path("a.png").media_type.as_deref(),
            Some("image/png")
        );
        assert_eq!(
            Image::path("a.JPEG").media_type.as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(Image::path("a.heic").media_type, None);
        assert_eq!(
            Image::path("a.heic")
                .media_type("image/heic")
                .media_type
                .as_deref(),
            Some("image/heic")
        );
        assert_eq!(TextFile::path("src/diff.patch").name, "diff.patch");
        assert_eq!(
            TextFile::path("src/diff.patch").name("changes").name,
            "changes"
        );
    }

    #[test]
    fn a_completion_becomes_a_message_with_nothing_lost() {
        let mut done = Completion::new(FinishReason::ToolCalls);
        done.text = "Let me look.".into();
        done.reasoning = vec![Reasoning::new(ReasoningSource::ReasoningContent, "hmm")];
        done.calls = vec![ToolCall::new("call-a", "lookup", r#"{"value":1}"#)];

        let msg = Message::from(done);
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(
            msg.parts,
            [
                Part::Reasoning(Reasoning::new(ReasoningSource::ReasoningContent, "hmm")),
                Part::Text("Let me look.".into()),
                Part::ToolCall(ToolCall::new("call-a", "lookup", r#"{"value":1}"#)),
            ]
        );
    }
}
