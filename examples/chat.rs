//! A conversation: the history is the request, and the caller keeps it.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example chat
//! ```
//!
//! Type a line and press Enter; end the input (Ctrl-D) to leave.

mod common;

use std::io::Write;

use svir::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::openai(common::url()).build()?;
    let mut request = Request::new(common::model()).system("You are a concise assistant.");

    let mut lines = std::io::stdin().lines();
    loop {
        eprint!("> ");
        let Some(line) = lines.next() else { break };
        request = request.user(line?);

        let mut stream = client.stream(&request).await?;
        while let Some(event) = stream.next().await {
            match event? {
                Event::Text(piece) => {
                    print!("{piece}");
                    std::io::stdout().flush()?;
                }
                // The answer joins the history with everything the next request needs.
                Event::Completed(done) => {
                    println!();
                    request = request.assistant(done);
                }
                _ => {}
            }
        }
    }

    Ok(())
}
