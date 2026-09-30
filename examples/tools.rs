//! Tools: the model calls them, and a few lines of the caller's code feed the results back.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example tools
//! ```
//!
//! The model has to support tool calls.

mod common;

use serde::Deserialize;
use serde_json::json;
use svir::prelude::*;

/// A model that keeps calling tools is stopped after this many answers.
const TURNS: usize = 8;

#[derive(Deserialize)]
struct City {
    city: String,
}

/// A stand-in for a weather service. Its error goes to the model as the tool's result.
fn weather(city: &str) -> Result<String, String> {
    match city.to_lowercase().as_str() {
        "oslo" => Ok("4 C, light snow".to_owned()),
        "lisbon" => Ok("19 C, clear".to_owned()),
        _ => Err(format!("no weather station in {city}")),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::openai(common::url()).build()?;

    let describe = Tool::new("weather", "The weather in a city right now.").schema(json!({
        "type": "object",
        "properties": {"city": {"type": "string", "description": "The name of the city"}},
        "required": ["city"]
    }));
    // Arguments that do not deserialize into `City` never reach the handler: the model is told
    // that they are invalid.
    let tools = Tools::new().add_tool(describe, |args: City| async move { weather(&args.city) });

    let mut request = Request::new(common::model())
        .tools(&tools)
        .user("Is it warmer in Oslo or in Lisbon right now?");

    for _ in 0..TURNS {
        let done = client.complete(&request).await?;
        if done.calls.is_empty() {
            println!("{}", done.text.trim());
            return Ok(());
        }

        for call in &done.calls {
            eprintln!("-- {}({})", call.name, call.arguments);
        }
        let results = tools.call_all(&done.calls).await;
        request = request.assistant(done).tool_results(results);
    }

    Err("the model kept calling tools".into())
}
