//! Lists the models a server has.
//!
//! ```sh
//! cargo run --example models
//! ```

mod common;

use svir::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let client = Client::openai(common::url()).build()?;

    for model in client.list_models().await? {
        println!("{}", model.id);
    }

    Ok(())
}
