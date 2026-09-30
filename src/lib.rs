//! A small, composable SDK for talking to large language models.
//!
//! The core is the wire protocol between an application and a model server: the types in this
//! crate, an encoder, a decoder, a transport, and compatibility handling. Layers and tool sets
//! sit on top, opt-in. There is no agent loop: feeding tool results back is a few lines of the
//! caller's code.
//!
//! So far this crate has the types, the errors, and the Chat Completions codec
//! ([`openai::chat::Encoder`] and [`openai::chat::Decoder`]); the client follows.
//!
//! ```
//! use svir::prelude::*;
//!
//! let request = Request::new("qwen3-27b")
//!     .system("Be precise.")
//!     .reasoning(Effort::Low)
//!     .message(
//!         Message::user("What changed between these two?")
//!             .with(Image::path("before.png"))
//!             .with(Image::path("after.png"))
//!             .with(TextFile::path("diff.patch")),
//!     );
//!
//! assert_eq!(request.messages[0].parts.len(), 4);
//! ```

mod decode;
mod error;
mod message;
mod request;
mod response;
mod tool;

pub mod openai;

pub use decode::{Limits, Mode, Think};
pub use error::{Error, ErrorKind, Result};
pub use message::{Image, Message, Part, Role, Source, TextFile, ToolResult};
pub use request::{Effort, Request};
pub use response::{
    Completion, Event, FinishReason, Reasoning, ReasoningSource, Timing, ToolCallDelta, Usage,
};
pub use tool::{Tool, ToolCall};

/// The everyday imports.
pub mod prelude {
    pub use crate::{
        Completion, Effort, Error, ErrorKind, Event, FinishReason, Image, Message, Part, Reasoning,
        Request, Role, TextFile, Tool, ToolCall, ToolResult, Usage,
    };
}
