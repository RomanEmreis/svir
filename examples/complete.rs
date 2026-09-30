//! The whole answer to one question.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example complete
//! ```

mod common;

use svir::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Error> {
    // A hosted server also needs a key: `.api_key_env("PROVIDER_API_KEY")` before `build`.
    let client = Client::openai(common::url()).build()?;
    let request = Request::new(common::model())
        .system("Answer in one short paragraph.")
        .user("Why do rivers meander?");

    let answer = client.complete(&request).await?;

    println!("{}", answer.text.trim());
    if let Some(usage) = answer.usage {
        eprintln!("-- {} tokens in, {} out", usage.input, usage.output);
    }
    if let Some(rate) = answer.tokens_per_second() {
        eprintln!("-- {rate:.1} tokens per second");
    }

    Ok(())
}
