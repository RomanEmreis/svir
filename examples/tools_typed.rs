//! Tools described by their types, and their calls shown as they stream.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example tools_typed --features schemars
//! ```
//!
//! With the `schemars` feature the input schema comes from the type of the handler's arguments,
//! doc comments included.

mod common;

use std::io::Write;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use svir::prelude::*;

/// A model that keeps calling tools is stopped after this many answers.
const TURNS: usize = 8;

#[derive(Deserialize, JsonSchema)]
struct Convert {
    /// The amount to convert.
    amount: f64,
    /// The currency to convert from, as a three-letter code.
    from: String,
    /// The currency to convert to, as a three-letter code.
    to: String,
}

/// A stand-in for an exchange: how many euros one unit of a currency buys.
fn euros(currency: &str) -> Result<f64, String> {
    match currency.to_uppercase().as_str() {
        "EUR" => Ok(1.0),
        "USD" => Ok(0.8),
        "NOK" => Ok(0.1),
        _ => Err(format!("{currency} is not traded here")),
    }
}

async fn convert(args: Convert) -> Result<Value, String> {
    let amount = args.amount * euros(&args.from)? / euros(&args.to)?;

    Ok(json!({"amount": amount, "currency": args.to}))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::openai(common::url()).build()?;
    let tools = Tools::new().add("convert", "Convert an amount between currencies.", convert);

    let mut request = Request::new(common::model())
        .tools(&tools)
        .user("I have 250 US dollars and 1200 Norwegian kroner. How many euros is that in all?");

    for _ in 0..TURNS {
        let mut stream = client.stream(&request).await?;
        let mut answer = None;

        while let Some(event) = stream.next().await {
            match event? {
                Event::Text(piece) => {
                    print!("{piece}");
                    std::io::stdout().flush()?;
                }
                // A call is only shown while it streams. It can be run once it is complete, and
                // complete calls are in the completion.
                Event::ToolCallDelta(piece) => {
                    if let Some(name) = piece.name {
                        eprint!("\n-- {name} ");
                    }
                    eprint!("{}", piece.arguments);
                }
                Event::Completed(done) => answer = Some(done),
                _ => {}
            }
        }

        let Some(done) = answer else { break };
        if done.calls.is_empty() {
            println!();
            return Ok(());
        }

        eprintln!();
        let results = tools.call_all(&done.calls).await;
        request = request.assistant(done).tool_results(results);
    }

    Err("the model kept calling tools".into())
}
