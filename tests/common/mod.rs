//! Shared by the conformance runners: reading cases and the suite's request notation.

#![allow(dead_code, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use serde_json::Value;
use svir::{
    Effort, Image, Message, Reasoning, Request, Role, Source, TextFile, Tool, ToolCall, ToolResult,
};

pub mod server;

/// The cases of one directory of the suite, by name, in order.
pub fn cases(directory: &str) -> (PathBuf, Vec<(String, Value)>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/conformance")
        .join(directory);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no cases in {directory}");
    let cases = paths
        .iter()
        .map(|path| {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(path).unwrap();
            (name, serde_json::from_str(&text).unwrap())
        })
        .collect();
    (dir, cases)
}

pub fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// The bytes of an attachment of the notation.
pub fn content(attachment: &Value) -> Vec<u8> {
    match attachment.get("hex") {
        Some(data) => hex(data.as_str().unwrap()),
        None => attachment["text"].as_str().unwrap().as_bytes().to_vec(),
    }
}

/// Where a request's attachments are: files under a directory, or in memory.
pub enum Attachments<'a> {
    Files(&'a Path),
    Memory,
}

fn effort(name: &str) -> Effort {
    match name {
        "none" => Effort::Off,
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "xhigh" => Effort::XHigh,
        other => panic!("unknown effort {other}"),
    }
}

/// The request used when a transport call does not give one.
pub fn default_request() -> Request {
    Request::new("m").user("hi")
}

/// Builds a request from the suite's notation. `attachments` is the case's attachment table.
pub fn request(
    notation: &Value,
    send_reasoning: Option<bool>,
    attachments: &Value,
    place: &Attachments<'_>,
) -> Request {
    let mut request = Request::new(notation["model"].as_str().unwrap());
    if let Some(n) = notation["max_tokens"].as_u64() {
        request = request.max_tokens(n);
    }
    if let Some(t) = notation["temperature"].as_f64() {
        request = request.temperature(t as f32);
    }
    if let Some(e) = notation["reasoning_effort"].as_str() {
        request = request.reasoning(effort(e));
    }
    if let Some(include) = notation["include_usage"].as_bool() {
        request = request.include_usage(include);
    }
    if let Some(send) = send_reasoning {
        request = request.send_reasoning(send);
    }
    for tool in notation["tools"].as_array().into_iter().flatten() {
        let (name, description) = (
            tool["name"].as_str().unwrap(),
            tool["description"].as_str().unwrap(),
        );
        request = request.tool(Tool::new(name, description).schema(tool["input_schema"].clone()));
    }

    for message in notation["messages"].as_array().unwrap() {
        let parts = message["parts"].as_array().unwrap();
        let role = match message["role"].as_str().unwrap() {
            "system" => {
                request = request.system(parts[0]["text"].as_str().unwrap());
                continue;
            }
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            other => panic!("unknown role {other}"),
        };
        let mut built = Message::new(role);
        for part in parts {
            let (kind, value) = part.as_object().unwrap().iter().next().unwrap();
            let text = |key: &str| value[key].as_str().unwrap();
            built = match kind.as_str() {
                "text" => built.with(value.as_str().unwrap()),
                "image" => {
                    let key = value.as_str().unwrap();
                    let attachment = &attachments[key];
                    let media_type = attachment["media_type"].as_str().unwrap();
                    built.with(match place {
                        Attachments::Files(dir) => {
                            Image::path(dir.join(key)).media_type(media_type)
                        }
                        Attachments::Memory => Image::bytes(content(attachment), media_type),
                    })
                }
                "file" => {
                    let key = value.as_str().unwrap();
                    let attachment = &attachments[key];
                    let name = attachment["name"].as_str().unwrap();
                    let mut file = match place {
                        Attachments::Files(dir) => TextFile::path(dir.join(key)).name(name),
                        Attachments::Memory => {
                            // Bytes as given, so a file that is not UTF-8 reaches the encoder.
                            let mut file = TextFile::text(name, "");
                            file.source = Source::Bytes(content(attachment).into());
                            file
                        }
                    };
                    if let Some(escaped) = attachment["escaped_len"].as_u64() {
                        file = file.escaped_len(escaped);
                    }
                    built.with(file)
                }
                "reasoning" => {
                    let source = serde_json::from_value(value["source"].clone()).unwrap();
                    built.with(Reasoning::new(source, text("text")))
                }
                "tool_call" => {
                    built.with(ToolCall::new(text("id"), text("name"), text("arguments")))
                }
                "tool_result" => built.with(if value["is_error"].as_bool() == Some(true) {
                    ToolResult::error(text("call_id"), text("content"))
                } else {
                    ToolResult::new(text("call_id"), text("content"))
                }),
                other => panic!("unknown part {other}"),
            };
        }
        request = request.message(built);
    }
    request
}
