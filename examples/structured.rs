//! An answer as JSON that matches the schema of a type, read back into that type.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example structured --features schemars
//! ```
//!
//! With the `schemars` feature the schema comes from the type, doc comments included. The answer
//! is not checked against it on the way in: parsing it into the type is the check.

mod common;

use schemars::JsonSchema;
use serde::Deserialize;
use svir::prelude::*;

/// A river, as an atlas lists it.
#[derive(Deserialize, JsonSchema)]
struct River {
    /// The river's name in English.
    name: String,
    /// Its length in kilometres.
    length_km: u32,
    /// The lake or sea it flows into.
    mouth: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::openai(common::url()).build()?;
    let request = Request::new(common::model())
        .response_format(Schema::of::<River>())
        .user("Describe the river that joins Lake Onega to Lake Ladoga.");

    let answer = client.complete(&request).await?;
    let River {
        name,
        length_km,
        mouth,
    } = answer.parse()?;

    println!("{name}: {length_km} km, into {mouth}");

    Ok(())
}
