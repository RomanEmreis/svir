//! Runs the encoder cases of the conformance suite: `tests/conformance/encoder/*.json`, in the
//! format described in `tests/conformance/README.md`.

#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde_json::Value;
use svir::openai::chat::{Body, Encoder};
use svir::{Effort, ErrorKind, Image, Message, Reasoning, Request, Role, TextFile, Tool, ToolCall};

fn case_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance/encoder")
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn content(attachment: &Value) -> Vec<u8> {
    match attachment.get("hex") {
        Some(data) => hex(data.as_str().unwrap()),
        None => attachment["text"].as_str().unwrap().as_bytes().to_vec(),
    }
}

/// Where attachments are: files under `dir`, or in memory.
enum Attachments<'a> {
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

/// Builds a request from the suite's notation.
fn request(case: &Value, attachments: &Attachments<'_>) -> Request {
    let notation = &case["request"];
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
    if let Some(send) = case["options"]["send_reasoning"].as_bool() {
        request = request.send_reasoning(send);
    }
    for tool in notation["tools"].as_array().into_iter().flatten() {
        request = request.tool(
            Tool::new(
                tool["name"].as_str().unwrap(),
                tool["description"].as_str().unwrap(),
            )
            .schema(tool["input_schema"].clone()),
        );
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
            built = match kind.as_str() {
                "text" => built.with(value.as_str().unwrap()),
                "image" => {
                    let key = value.as_str().unwrap();
                    let attachment = &case["attachments"][key];
                    let media_type = attachment["media_type"].as_str().unwrap();
                    built.with(match attachments {
                        Attachments::Files(dir) => {
                            Image::path(dir.join(key)).media_type(media_type)
                        }
                        Attachments::Memory => Image::bytes(content(attachment), media_type),
                    })
                }
                "file" => {
                    let key = value.as_str().unwrap();
                    let attachment = &case["attachments"][key];
                    let name = attachment["name"].as_str().unwrap();
                    built.with(match attachments {
                        Attachments::Files(dir) => TextFile::path(dir.join(key)).name(name),
                        Attachments::Memory => {
                            TextFile::text(name, String::from_utf8(content(attachment)).unwrap())
                        }
                    })
                }
                "reasoning" => built.with(Reasoning::new(
                    serde_json::from_value(value["source"].clone()).unwrap(),
                    value["text"].as_str().unwrap(),
                )),
                "tool_call" => built.with(ToolCall::new(
                    value["id"].as_str().unwrap(),
                    value["name"].as_str().unwrap(),
                    value["arguments"].as_str().unwrap(),
                )),
                "tool_result" => built.with(svir::ToolResult::new(
                    value["call_id"].as_str().unwrap(),
                    value["content"].as_str().unwrap(),
                )),
                other => panic!("unknown part {other}"),
            };
        }
        request = request.message(built);
    }
    request
}

fn encoder(case: &Value, block: Option<usize>) -> Encoder {
    let options = &case["options"];
    let mut encoder = Encoder::new().lean(options["lean"].as_bool().unwrap_or(false));
    if let Some(bytes) = block {
        encoder = encoder.block_bytes(bytes);
    }
    if let Some(tokens) = options["context_tokens"].as_u64() {
        encoder = encoder.context_tokens(tokens);
    }
    encoder
}

/// Writes the attachments as files. One with a `declared_size` is written at that size, so the
/// body is built against it; `change` then writes what the case says the file holds.
fn write(case: &Value, dir: &Path, change: bool) {
    std::fs::create_dir_all(dir).unwrap();
    for (key, attachment) in case["attachments"].as_object().into_iter().flatten() {
        let real = content(attachment);
        let bytes = match attachment["declared_size"].as_u64() {
            Some(size) if !change => vec![b'x'; size as usize],
            _ => real,
        };
        std::fs::write(dir.join(key), bytes).unwrap();
    }
}

async fn collect(body: Body) -> Result<Vec<u8>, ErrorKind> {
    let mut stream = std::pin::pin!(body.into_stream());
    let mut out = Vec::new();
    while let Some(block) = stream.next().await {
        out.extend_from_slice(&block.map_err(|error| error.kind())?);
    }
    Ok(out)
}

/// Differences between the expectation and what encoding produced, empty when they agree.
fn compare(expect: &Value, got: &Result<(u64, Vec<u8>), ErrorKind>) -> Vec<String> {
    let mut diffs = Vec::new();
    match (expect.get("body"), got) {
        (Some(want), Ok((length, bytes))) => {
            if *length != bytes.len() as u64 {
                diffs.push(format!("streamed {} bytes, declared {length}", bytes.len()));
            }
            match serde_json::from_slice::<Value>(bytes) {
                Ok(body) if body == *want => {}
                Ok(body) => diffs.push(format!("body {body} != {want}")),
                Err(error) => diffs.push(format!("not JSON: {error}")),
            }
        }
        (Some(_), Err(kind)) => diffs.push(format!("error {kind:?}, expected a body")),
        (None, Ok(_)) => diffs.push(format!("a body, expected error {}", expect["error"])),
        (None, Err(kind)) => {
            if serde_json::to_value(kind).unwrap() != expect["error"] {
                diffs.push(format!("error {kind:?} != {}", expect["error"]));
            }
        }
    }
    diffs
}

#[tokio::test]
async fn encoder_conformance() {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(case_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no encoder cases found");

    let scratch =
        std::env::temp_dir().join(format!("svir-conformance-encoder-{}", std::process::id()));
    let (mut failures, mut pending, mut runs) = (Vec::new(), Vec::new(), 0);

    for path in &paths {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let expect = &case["expect"];
        if let Some(open) = expect.get("open") {
            pending.push(format!("{name}: open {open}"));
            continue;
        }
        let open = case.get("open").is_some();
        let mut report = |label: &str, diffs: Vec<String>| {
            for diff in diffs {
                let line = format!("{name} {label}: {diff}");
                if open {
                    pending.push(line)
                } else {
                    failures.push(line)
                }
            }
        };

        // From files, once per block size and once with the default.
        let dir = scratch.join(&name);
        let mut blocks: Vec<Option<usize>> = case["options"]["block_bytes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| Some(v.as_u64().unwrap() as usize))
            .collect();
        blocks.push(None);
        for block in blocks {
            runs += 1;
            let label = format!("files/{block:?}");
            let encoder = encoder(&case, block);
            let request = request(&case, &Attachments::Files(&dir));

            write(&case, &dir, false);
            let built = encoder.encode_files(&request).await;
            write(&case, &dir, true);
            let got = match built {
                Ok(body) => {
                    let length = body.len();
                    let again = body.clone();
                    let first = collect(body).await;
                    if let Ok(bytes) = &first {
                        if collect(again).await.as_ref() != Ok(bytes) {
                            report(&label, vec!["a clone produced different bytes".into()]);
                        }
                    }
                    first.map(|bytes| (length, bytes))
                }
                Err(error) => Err(error.kind()),
            };
            report(&label, compare(expect, &got));
        }

        // From memory, without I/O, when no attachment is said to change on disk.
        let changes = case["attachments"]
            .as_object()
            .is_some_and(|all| all.values().any(|a| a.get("declared_size").is_some()));
        if !changes {
            runs += 1;
            let request = request(&case, &Attachments::Memory);
            let got = match encoder(&case, None).encode(&request) {
                Ok(body) => {
                    let length = body.len();
                    body.into_bytes()
                        .map(|bytes| (length, bytes.to_vec()))
                        .map_err(|e| e.kind())
                }
                Err(error) => Err(error.kind()),
            };
            report("memory", compare(expect, &got));
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
    for line in &pending {
        eprintln!("pending: {line}");
    }
    assert!(runs > 40, "only {runs} runs");
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
