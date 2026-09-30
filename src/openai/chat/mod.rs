//! Chat Completions: `POST /v1/chat/completions` with `stream: true`.
//!
//! [`Encoder`] turns a [`Request`](crate::Request) into a [`Body`] whose length is known before
//! its first byte. [`Decoder`] reads the server's answer stream: bytes in,
//! [`Event`](crate::Event)s out, no I/O.

mod body;
mod decoder;
mod encoder;
mod escape;
mod sse;
mod think;

pub use body::Body;
pub use decoder::Decoder;
pub use encoder::Encoder;
