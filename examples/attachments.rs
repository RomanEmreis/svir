//! An image and a text file in one message.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example attachments [image]
//! ```
//!
//! The model has to see images. Without an argument the image is a red circle.

mod common;

use svir::prelude::*;

const IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/data/red-circle.png");

#[tokio::main]
async fn main() -> Result<(), Error> {
    let image = std::env::args().nth(1).unwrap_or_else(|| IMAGE.to_owned());
    let client = Client::openai(common::url()).build()?;

    // Nothing is read here. The file is read while the request is sent, a block at a time, so
    // a large image is never in memory whole.
    let message = Message::user("Does the note describe the image? Say what differs, if anything.")
        .with(Image::path(image))
        .with(TextFile::text(
            "note.txt",
            "A blue square on a white background.",
        ));
    let request = Request::new(common::model()).message(message);

    let answer = client.complete(&request).await?;
    println!("{}", answer.text.trim());

    Ok(())
}
