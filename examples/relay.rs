//! A proxy: the server's bytes go on unchanged, and are read on the way past.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example relay
//! ```
//!
//! Standard output stands for the downstream connection: it gets the event stream exactly as the
//! server sent it. What the proxy learns from it goes to stderr.

mod common;

use std::io::Write;

use svir::openai::chat::Decoder;
use svir::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::openai(common::url()).build()?;
    let request = Request::new(common::model()).user("Count from one to five.");

    // `send` has done the status mapping and the compatibility handling; what is left is bytes.
    let mut raw = client.send(&request).await?;
    let mut decoder = Decoder::lenient();
    let mut answer = None;

    while let Some(bytes) = raw.next().await {
        let bytes = bytes?;
        std::io::stdout().write_all(&bytes)?;

        for event in decoder.push(&bytes) {
            if let Event::Completed(done) = event? {
                answer = Some(done);
            }
        }
    }
    // A stream that ended without its end marker is an error here, not a short answer.
    decoder.finish()?;

    if let Some(done) = answer {
        eprintln!("-- relayed {:?}, usage: {:?}", done.text, done.usage);
    }

    Ok(())
}
