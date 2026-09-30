//! The codec on its own: a request to bytes and bytes to events, with no client and no server.
//!
//! ```sh
//! cargo run --example codec --no-default-features
//! ```
//!
//! This is what an application with its own HTTP stack uses.

use svir::openai::chat::{Decoder, Encoder};
use svir::prelude::*;

/// What a server might send back, cut here into arbitrary pieces.
const RESPONSE: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"A greeting.\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" there.\"},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":5}}\n\n",
    "data: [DONE]\n\n",
);

fn main() -> Result<(), Error> {
    let request = Request::new("any-model").system("Be brief.").user("Hello!");

    // The length is known before the first byte: it is the `Content-Length` to send.
    let body = Encoder::new().include_usage(true).encode(&request)?;
    let length = body.len();
    let bytes = body.into_bytes()?;
    println!("{length} bytes: {}", String::from_utf8_lossy(&bytes));

    // The decoder takes the bytes however the transport cut them, and gives the same events.
    let mut decoder = Decoder::strict();
    for piece in RESPONSE.as_bytes().chunks(11) {
        for event in decoder.push(piece) {
            println!("{:?}", event?);
        }
    }

    decoder.finish()
}
