//! Chat Completions: `POST /v1/chat/completions` with `stream: true`.
//!
//! [`Decoder`] reads the server's answer stream: bytes in, [`Event`](crate::Event)s out, no I/O.

mod decoder;
mod sse;
mod think;

pub use decoder::Decoder;
