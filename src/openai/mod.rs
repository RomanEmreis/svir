//! OpenAI-compatible wire APIs.
//!
//! Most callers never need this module: the client uses it. It is here for a custom transport or
//! a proxy that reads a stream it forwards elsewhere.

pub mod chat;
