//! Runs the decoder cases of the conformance suite: `tests/conformance/decoder/*.json`, in the
//! format described in `tests/conformance/README.md`.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use svir::openai::chat::Decoder;
use svir::{Event, Limits, Mode, Think};

const DEFAULT_STRIDES: [usize; 3] = [1, 3, 8192];

struct Variant {
    label: String,
    chunks: Vec<Vec<u8>>,
    as_given: bool,
}

/// What one run produced, in the suite's outcome notation.
struct Run {
    outcome: Value,
    visible: Vec<bool>,
}

fn case_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance/decoder")
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        if byte == b'\n' {
            out.push(b'\r');
        }
        out.push(byte);
    }
    out
}

fn variants(case: &Value) -> Vec<Variant> {
    let (pieces, from_file) = match case["file"].as_str() {
        Some(file) => {
            let mut text = std::fs::read_to_string(case_dir().join(file)).unwrap();
            for pair in case["replace"].as_array().into_iter().flatten() {
                let (from, to) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
                assert!(text.contains(from), "replace pattern not found: {from}");
                text = text.replace(from, to);
            }
            if let Some(cut) = case["cut_before"].as_str() {
                text.truncate(text.find(cut).expect("cut_before pattern not found"));
            }
            (vec![text.into_bytes()], true)
        }
        None => {
            let chunks = case["chunks"].as_array().expect("file or chunks");
            let pieces = chunks
                .iter()
                .map(|chunk| match chunk {
                    Value::String(text) => text.as_bytes().to_vec(),
                    other => hex(other["hex"].as_str().unwrap()),
                })
                .collect();
            (pieces, false)
        }
    };

    let endings: Vec<&str> = match case["line_endings"].as_array() {
        Some(list) => list.iter().map(|v| v.as_str().unwrap()).collect(),
        None if from_file => vec!["lf", "crlf"],
        None => vec!["lf"],
    };
    let strides: Option<Vec<usize>> = match case["strides"].as_array() {
        Some(list) => Some(list.iter().map(|v| v.as_u64().unwrap() as usize).collect()),
        None if from_file => Some(DEFAULT_STRIDES.to_vec()),
        None => None,
    };

    let mut out = Vec::new();
    for ending in endings {
        let converted: Vec<Vec<u8>> = pieces
            .iter()
            .map(|p| if ending == "crlf" { crlf(p) } else { p.clone() })
            .collect();
        match &strides {
            None => out.push(Variant {
                label: format!("{ending}/as-given"),
                chunks: converted,
                as_given: true,
            }),
            Some(strides) => {
                let joined = converted.concat();
                for &stride in strides {
                    out.push(Variant {
                        label: format!("{ending}/{stride}"),
                        chunks: joined.chunks(stride).map(<[u8]>::to_vec).collect(),
                        as_given: false,
                    });
                }
            }
        }
    }
    out
}

fn decoder(case: &Value, mode: Mode) -> Decoder {
    let options = &case["options"];
    let mut limits = Limits::default();
    if let Some(n) = options["limits"]["wire_bytes"].as_u64() {
        limits = limits.wire_bytes(n as usize);
    }
    if let Some(n) = options["limits"]["event_bytes"].as_u64() {
        limits = limits.event_bytes(n as usize);
    }
    if let Some(n) = options["limits"]["tool_calls"].as_u64() {
        limits = limits.tool_calls(n as usize);
    }
    let mut decoder = Decoder::new(mode).limits(limits);
    match options["think"].as_str() {
        Some("split") => decoder = decoder.think(Think::Split),
        Some("keep") => decoder = decoder.think(Think::Keep),
        Some(other) => panic!("unknown think option {other}"),
        None => {}
    }
    decoder
}

fn run(mut decoder: Decoder, chunks: &[Vec<u8>]) -> Run {
    let (mut text, mut reasoning) = (String::new(), String::new());
    let mut visible = Vec::new();
    for chunk in chunks {
        let mut seen = false;
        for item in decoder.push(chunk) {
            match item {
                Ok(Event::Text(delta)) => {
                    seen |= !delta.is_empty();
                    text.push_str(&delta);
                }
                Ok(Event::Reasoning(delta)) => {
                    seen |= !delta.text.is_empty();
                    reasoning.push_str(&delta.text);
                }
                Ok(Event::Completed(done)) => {
                    visible.push(seen);
                    let mut outcome = serde_json::to_value(&done).unwrap();
                    outcome.as_object_mut().unwrap().remove("timing");
                    return Run {
                        outcome: json!({"completion": outcome}),
                        visible,
                    };
                }
                Ok(_) => {}
                Err(error) => {
                    visible.push(seen);
                    let outcome = json!({
                        "error": serde_json::to_value(error.kind()).unwrap(),
                        "partial": {"text": text, "reasoning": reasoning},
                        "message": error.server_message(),
                    });
                    return Run { outcome, visible };
                }
            }
        }
        visible.push(seen);
    }
    let error = decoder
        .finish()
        .expect_err("the input ended without a completion or an error");
    let outcome = json!({
        "error": serde_json::to_value(error.kind()).unwrap(),
        "partial": {"text": text, "reasoning": reasoning},
        "message": error.server_message(),
    });
    Run { outcome, visible }
}

/// Differences between an expected outcome and a run, empty when they agree.
fn compare(expected: &Value, run: &Run, as_given: bool) -> Vec<String> {
    let mut diffs = Vec::new();
    let got = &run.outcome;
    if let Some(want) = expected.get("completion") {
        let mut want = want.clone();
        want.as_object_mut()
            .unwrap()
            .entry("text")
            .or_insert(json!(""));
        match got.get("completion") {
            Some(completion) if *completion == want => {}
            Some(completion) => diffs.push(format!("completion {completion} != {want}")),
            None => diffs.push(format!("{got} != completion {want}")),
        }
    } else if let Some(kind) = expected.get("error") {
        if got.get("error") != Some(kind) {
            diffs.push(format!("{got} != error {kind}"));
        }
        for field in ["text", "reasoning"] {
            if let Some(want) = expected.pointer(&format!("/partial/{field}")) {
                let have = got
                    .pointer(&format!("/partial/{field}"))
                    .unwrap_or(&Value::Null);
                if have != want {
                    diffs.push(format!("partial {field} {have} != {want}"));
                }
            }
        }
        if let Some(want) = expected.get("message") {
            if got.get("message") != Some(want) {
                diffs.push(format!("message {} != {want}", got["message"]));
            }
        }
    } else {
        diffs.push(format!("unknown expectation {expected}"));
    }
    if let (Some(want), true) = (expected.get("visible_by_chunk"), as_given) {
        if *want != json!(run.visible) {
            diffs.push(format!("visible_by_chunk {:?} != {want}", run.visible));
        }
    }
    diffs
}

#[test]
fn decoder_conformance() {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(case_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no decoder cases found");

    let (mut failures, mut pending, mut runs) = (Vec::new(), Vec::new(), 0);
    for path in &paths {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let open = case.get("open").is_some();
        let strict = case["expect"]["strict"].clone();
        let lenient = match &case["expect"]["lenient"] {
            Value::String(same) if same == "same" => strict.clone(),
            other => other.clone(),
        };

        for (mode, expected) in [(Mode::Strict, &strict), (Mode::Lenient, &lenient)] {
            if expected.get("open").is_some() {
                pending.push(format!("{name} {mode:?}: open {}", expected["open"]));
                continue;
            }
            for variant in variants(&case) {
                runs += 1;
                let run = run(decoder(&case, mode), &variant.chunks);
                for diff in compare(expected, &run, variant.as_given) {
                    let line = format!("{name} {mode:?} {}: {diff}", variant.label);
                    if open {
                        pending.push(line)
                    } else {
                        failures.push(line)
                    }
                }
            }
        }
    }

    for line in &pending {
        eprintln!("pending: {line}");
    }
    assert!(runs > 300, "only {runs} runs");
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
