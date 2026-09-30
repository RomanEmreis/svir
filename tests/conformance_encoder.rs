//! Runs the encoder cases of the conformance suite: `tests/conformance/encoder/*.json`, in the
//! format described in `tests/conformance/README.md`.

#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;

use common::Attachments;
use futures_util::StreamExt;
use serde_json::Value;
use svir::openai::chat::{Body, Encoder};
use svir::{ErrorKind, Request};

fn request(case: &Value, place: &Attachments<'_>) -> Request {
    let send_reasoning = case["options"]["send_reasoning"].as_bool();
    common::request(
        &case["request"],
        send_reasoning,
        &case["attachments"],
        place,
    )
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
        let real = common::content(attachment);
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
    let (_, cases) = common::cases("encoder");
    let scratch =
        std::env::temp_dir().join(format!("svir-conformance-encoder-{}", std::process::id()));
    let (mut failures, mut pending, mut runs) = (Vec::new(), Vec::new(), 0);

    for (name, case) in &cases {
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
        let dir = scratch.join(name);
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
            let encoder = encoder(case, block);
            let request = request(case, &Attachments::Files(&dir));

            write(case, &dir, false);
            let built = encoder.encode_files(&request).await;
            write(case, &dir, true);
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
            let request = request(case, &Attachments::Memory);
            let got = match encoder(case, None).encode(&request) {
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
