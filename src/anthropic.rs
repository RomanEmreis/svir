//! The Anthropic Messages API.
//!
//! Most callers never need this module: the client uses it, through
//! `Client::anthropic()`. It is here for a custom transport or a proxy that reads a stream it
//! forwards elsewhere.

pub mod messages;
