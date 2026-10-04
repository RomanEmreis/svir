//! The serde form of the public types is a contract (D22). These tests pin it, and check that the
//! conformance suite's outcome notation is that same form.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use serde_json::{Value, json};
use svir::prelude::*;
use svir::{ReasoningSource, Timing, ToolCallDelta};

fn outcomes(dir: &Path) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let case: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let mut push = |outcome: &Value| found.push((name.clone(), outcome.clone()));
        let expect = &case["expect"];
        if expect.get("strict").is_some() {
            push(&expect["strict"]);
            if expect["lenient"].is_object() {
                push(&expect["lenient"]);
            }
        } else {
            for call in case["calls"].as_array().into_iter().flatten() {
                push(&call["expect"]);
            }
        }
    }
    found
}

#[test]
fn conformance_completions_are_the_serde_form_of_completion() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let mut checked = 0;
    for dir in ["decoder", "transport"] {
        for (name, outcome) in outcomes(&root.join(dir)) {
            let Some(expected) = outcome.get("completion") else {
                continue;
            };
            let completion: Completion =
                serde_json::from_value(expected.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut expected = expected.clone();
            expected
                .as_object_mut()
                .unwrap()
                .entry("text")
                .or_insert(json!(""));
            assert_eq!(
                serde_json::to_value(&completion).unwrap(),
                expected,
                "{name}"
            );
            checked += 1;
        }
    }
    assert!(checked > 30, "only {checked} completions found");
}

#[test]
fn conformance_error_kinds_are_the_serde_form_of_error_kind() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let mut checked = 0;
    for dir in ["decoder", "encoder", "transport"] {
        for (name, outcome) in outcomes(&root.join(dir)) {
            let Some(kind) = outcome.get("error") else {
                continue;
            };
            let parsed: ErrorKind = serde_json::from_value(kind.clone())
                .unwrap_or_else(|e| panic!("{name}: {kind} {e}"));
            assert_eq!(&serde_json::to_value(parsed).unwrap(), kind);
            checked += 1;
        }
    }
    assert!(checked > 30, "only {checked} errors found");
}

#[test]
fn a_request_serializes_as_documented() {
    let request = Request::new("m")
        .system("Be brief.")
        .reasoning(Effort::XHigh)
        .max_tokens(16)
        .message(
            Message::user("Compare")
                .with(Image::path("a.png"))
                .with(Image::bytes(vec![1u8, 2, 3], "image/jpeg"))
                .with(TextFile::text("notes.txt", "hi"))
                .with(TextFile::path("diff.patch").escaped_len(120)),
        )
        .tool_result("call-a", "42")
        .tool_results([ToolResult::error("call-b", "no tool named lookup")]);

    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value,
        json!({
            "model": "m",
            "system": "Be brief.",
            "messages": [
                {"role": "user", "parts": [
                    {"text": "Compare"},
                    {"image": {"path": "a.png", "media_type": "image/png"}},
                    {"image": {"bytes": "AQID", "media_type": "image/jpeg"}},
                    {"file": {"name": "notes.txt", "bytes": "aGk="}},
                    {"file": {"name": "diff.patch", "path": "diff.patch", "escaped_len": 120}}
                ]},
                {"role": "tool", "parts": [{"tool_result": {"call_id": "call-a", "content": "42"}}]},
                {"role": "tool", "parts": [{"tool_result": {
                    "call_id": "call-b", "content": "no tool named lookup", "is_error": true
                }}]}
            ],
            "max_tokens": 16,
            "reasoning": "xhigh"
        })
    );
    let back: Request = serde_json::from_value(value).unwrap();
    assert_eq!(back, request);
}

#[test]
fn events_serialize_as_documented() {
    let mut delta = ToolCallDelta::new(1, r#"{"value":"#);
    delta.id = Some("call-b".into());

    let mut done = Completion::new(FinishReason::Stop);
    done.text = "42".into();
    done.usage = Some(Usage::new(24, 3));
    done.timing = Some(Timing::new(
        std::time::Duration::from_millis(120),
        std::time::Duration::from_millis(480),
    ));

    let events = [
        Event::Text("4".into()),
        Event::Reasoning(Reasoning::new(ReasoningSource::Think, "why")),
        Event::ToolCallDelta(delta),
        Event::Completed(done),
    ];
    assert_eq!(
        serde_json::to_value(&events).unwrap(),
        json!([
            {"text": "4"},
            {"reasoning": {"source": "think", "text": "why"}},
            {"tool_call_delta": {"index": 1, "id": "call-b", "arguments": "{\"value\":"}},
            {"completed": {
                "finish": "stop",
                "text": "42",
                "usage": {"input": 24, "output": 3},
                "timing": {"first_token_ms": 120, "last_token_ms": 480}
            }}
        ])
    );
}
