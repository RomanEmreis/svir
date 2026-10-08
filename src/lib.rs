//! A small, composable SDK for talking to large language models.
//!
//! The core is the wire protocol between an application and a model server: the types in this
//! crate, an encoder, a decoder, a transport, and compatibility handling. Layers and tool sets
//! sit on top, opt-in. There is no agent loop: feeding tool results back is a few lines of the
//! caller's code.
//!
//! With the `client` feature, `Client` sends a [`Request`] to an OpenAI-compatible server and
//! returns the answer whole or as an `EventStream`. The codec under it,
//! [`openai::chat::Encoder`] and [`openai::chat::Decoder`], works on its own with any transport.
//! Layers (`svir::layer`) wrap every call; [`Toolbox`] and [`Tools`] describe tools to a model
//! and answer its calls.
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

pub mod body;
mod decode;
mod error;
mod message;
mod request;
mod response;
mod tool;
mod tools;

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
pub mod http;
#[cfg(feature = "client")]
pub mod layer;
pub mod openai;
#[cfg(feature = "client")]
mod stream;

#[cfg(feature = "client")]
pub use client::{Client, ClientBuilder, Model};
#[cfg(feature = "client")]
pub use stream::{EventStream, RawStream};

pub use decode::{Limits, Mode, Think};
pub use error::{Error, ErrorKind, Result};
pub use message::{Image, Message, Part, Role, Source, TextFile, ToolResult};
pub use request::{Effort, Request, ResponseFormat, Schema};
pub use response::{
    Completion, Event, FinishReason, Reasoning, ReasoningSource, Timing, ToolCallDelta, Usage,
};
pub use tool::{Tool, ToolCall, ToolChoice};
pub use tools::{ToolOutput, Toolbox, Tools};

/// The everyday imports.
pub mod prelude {
    #[cfg(feature = "client")]
    pub use crate::{Client, EventStream};
    pub use crate::{
        Completion, Effort, Error, ErrorKind, Event, FinishReason, Image, Message, Part, Reasoning,
        Request, ResponseFormat, Role, Schema, TextFile, Tool, ToolCall, ToolChoice, ToolResult,
        Toolbox, Tools, Usage,
    };
}
