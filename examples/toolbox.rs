//! A tool set of one's own: anything that describes tools and answers calls is a `Toolbox`.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo run --example toolbox
//! ```
//!
//! `Tools` is a registry of independent handlers. Tools that share state, or that come from
//! somewhere else (another process, a plugin system), implement the trait themselves.

mod common;

use std::sync::Mutex;

use serde::Deserialize;
use serde_json::json;
use svir::prelude::*;

/// A model that keeps calling tools is stopped after this many answers.
const TURNS: usize = 8;

/// The tools, by the names the model calls them by.
const NOTE: &str = "note";
const NOTES: &str = "notes";

/// Notes the model keeps during a conversation.
#[derive(Default)]
struct Notes {
    kept: Mutex<Vec<String>>,
}

#[derive(Deserialize)]
struct Note {
    text: String,
}

impl Toolbox for Notes {
    fn tools(&self) -> Vec<Tool> {
        let note = Tool::new(NOTE, "Keep a note for later.").schema(json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        }));
        let notes = Tool::new(NOTES, "Read back every note kept so far.");

        vec![note, notes]
    }

    async fn call(&self, call: &ToolCall) -> ToolResult {
        let mut kept = self.kept.lock().expect("no holder of the lock panics");

        let answer = match call.name.as_str() {
            NOTE => match call.parse::<Note>() {
                Ok(note) => {
                    kept.push(note.text);
                    Ok(format!("kept as note {}", kept.len()))
                }
                Err(error) => Err(format!("invalid arguments: {error}")),
            },
            NOTES => Ok(kept.join("\n")),
            other => Err(format!("no tool named {other}")),
        };

        // A failure is a result too: the model reads it and can try again.
        match answer {
            Ok(content) => ToolResult::new(&call.id, content),
            Err(message) => ToolResult::error(&call.id, message),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::openai(common::url()).build()?;
    let notes = Notes::default();

    let mut request = Request::new(common::model()).tools(&notes).user(
        "Note that the review is on Thursday, and that Ada brings the slides. \
         Then read the notes back and tell me what you kept.",
    );

    for _ in 0..TURNS {
        let done = client.complete(&request).await?;
        if done.calls.is_empty() {
            println!("{}", done.text.trim());
            return Ok(());
        }

        for call in &done.calls {
            eprintln!("-- {}({})", call.name, call.arguments);
        }
        let results = notes.call_all(&done.calls).await;
        request = request.assistant(done).tool_results(results);
    }

    Err("the model kept calling tools".into())
}
