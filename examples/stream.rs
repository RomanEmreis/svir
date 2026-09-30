//! An answer as it arrives: reasoning first, then the text.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example stream
//! ```
//!
//! Reasoning goes to stderr and the answer to stdout, so `2>/dev/null` leaves the answer alone.

mod common;

use std::io::Write;

use svir::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let client = Client::openai(common::url()).build()?;
    let request = Request::new(common::model())
        .reasoning(Effort::Low)
        .user("A farmer has 17 sheep. All but 9 run away. How many are left?");

    let mut stream = client.stream(&request).await?;

    while let Some(event) = stream.next().await {
        match event? {
            Event::Reasoning(piece) => eprint!("{}", piece.text),
            Event::Text(piece) => {
                print!("{piece}");
                let _ = std::io::stdout().flush();
            }
            // The last event carries the whole answer: what a caller keeps.
            Event::Completed(done) => {
                println!();
                eprintln!("-- finished: {:?}, usage: {:?}", done.finish, done.usage);
            }
            _ => {}
        }
    }

    Ok(())
}
