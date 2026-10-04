//! Chat Completions: `POST /v1/chat/completions` with `stream: true`.
//!
//! [`Encoder`] turns a [`Request`](crate::Request) into a [`Body`] whose length is known before
//! its first byte. [`Decoder`] reads the server's answer stream: bytes in,
//! [`Event`](crate::Event)s out, no I/O.

mod decoder;
mod encoder;
mod overflow;
mod sse;
#[cfg(feature = "client")]
pub(crate) mod status;
mod think;
mod wire;

pub use crate::body::Body;
pub use decoder::Decoder;
#[cfg(feature = "client")]
pub(crate) use decoder::truncated;
pub use encoder::Encoder;
