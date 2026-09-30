//! Runs the transport cases of the conformance suite: `tests/conformance/transport/*.json`, in
//! the format described in `tests/conformance/README.md`, against a scripted server on loopback.

#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Attachments, server::Server};
use serde_json::{Value, json};
use svir::{Client, Error, Event};

/// Builds the client a case describes, with the usage default off: the suite's notation sends a
/// field only when the request sets it.
fn client(config: &Value, base_url: &str) -> Result<Client, Error> {
    let mut builder = Client::openai(base_url).include_usage(false);
    if let Some(key) = config["api_key"].as_str() {
        builder = builder.api_key(key);
    }
    if let Some(ms) = config["timeout_ms"].as_u64() {
        builder = builder.idle_timeout(Duration::from_millis(ms));
    }
    if config["allow_http"].as_bool() == Some(true) {
        builder = builder.allow_http();
    }
    builder.build()
}

fn failed(error: &Error, text: &str, reasoning: &str) -> Value {
    json!({
        "error": serde_json::to_value(error.kind()).unwrap(),
        "retryable": error.is_retryable(),
        "retry_after_ms": error.retry_after().map(|delay| delay.as_millis() as u64),
        "status": error.status(),
        "message": error.server_message(),
        "partial": {"text": text, "reasoning": reasoning},
    })
}

/// Makes one call and returns what happened, in the suite's outcome notation, with every error
/// seen on the way.
async fn call(client: &Client, call: &Value, errors: &mut Vec<String>) -> Value {
    let mut note = |error: &Error| errors.push(format!("{error} {error:?}"));

    if call.get("list_models").is_some() {
        return match client.list_models().await {
            Ok(models) => json!({"models": models}),
            Err(error) => {
                note(&error);
                failed(&error, "", "")
            }
        };
    }

    let request = match call.get("request") {
        Some(notation) => common::request(notation, None, &Value::Null, &Attachments::Memory),
        None => common::default_request(),
    };

    let (mut text, mut reasoning) = (String::new(), String::new());
    let mut stream = match client.stream(&request).await {
        Ok(stream) => stream,
        Err(error) => {
            note(&error);
            return failed(&error, &text, &reasoning);
        }
    };

    let read_all = [json!("read_all")];
    let actions = call
        .get("client")
        .and_then(Value::as_array)
        .map_or(&read_all[..], Vec::as_slice);

    for action in actions {
        if action == "cancel" {
            drop(stream);
            return json!({"cancelled": true, "partial": {"text": text, "reasoning": reasoning}});
        }

        let events = action
            .get("read_events")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);

        let deadline = action
            .get("read_for_ms")
            .and_then(Value::as_u64)
            .map(|ms| Instant::now() + Duration::from_millis(ms));

        for _ in 0..events {
            let next = match deadline {
                None => stream.next().await,
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match tokio::time::timeout(left, stream.next()).await {
                        Ok(next) => next,
                        Err(_) => break,
                    }
                }
            };

            match next {
                Some(Ok(Event::Text(delta))) => text.push_str(&delta),
                Some(Ok(Event::Reasoning(delta))) => reasoning.push_str(&delta.text),
                Some(Ok(Event::Completed(done))) => {
                    let mut completion = serde_json::to_value(&done).unwrap();
                    completion.as_object_mut().unwrap().remove("timing");
                    return json!({"completion": completion});
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    note(&error);
                    return failed(&error, &text, &reasoning);
                }
                None => return json!({"ended": true}),
            }
        }
    }

    json!({"pending": true, "partial": {"text": text, "reasoning": reasoning}})
}

/// Differences between an expected outcome and what a call produced.
fn compare(expected: &Value, got: &Value, elapsed: Duration) -> Vec<String> {
    let mut diffs = Vec::new();
    if let Some(limit) = expected.get("within_ms").and_then(Value::as_u64) {
        if elapsed > Duration::from_millis(limit) {
            diffs.push(format!("took {} ms, limit {limit}", elapsed.as_millis()));
        }
    }

    if let Some(want) = expected.get("completion") {
        let mut want = want.clone();
        want.as_object_mut()
            .unwrap()
            .entry("text")
            .or_insert(json!(""));

        if got.get("completion") != Some(&want) {
            diffs.push(format!("{got} != completion {want}"));
        }
    } else if let Some(kind) = expected.get("error") {
        if got.get("error") != Some(kind) {
            diffs.push(format!("{got} != error {kind}"));
            return diffs;
        }

        for field in ["retryable", "retry_after_ms", "status", "message"] {
            if let Some(want) = expected.get(field) {
                if &got[field] != want {
                    diffs.push(format!("{field} {} != {want}", got[field]));
                }
            }
        }
    } else if expected.get("cancelled").is_some() {
        if got.get("cancelled").is_none() {
            diffs.push(format!("{got} != cancelled"));
        }
    } else if let Some(models) = expected.get("models") {
        if got.get("models") != Some(models) {
            diffs.push(format!("{got} != models {models}"));
        }
    } else {
        diffs.push(format!("unknown expectation {expected}"));
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

    diffs
}

async fn run(dir: &Path, case: &Value) -> Vec<String> {
    let config = &case["config"];
    let urls: Vec<&str> = match &config["base_url"] {
        Value::Array(list) => list.iter().map(|url| url.as_str().unwrap()).collect(),
        url => vec![url.as_str().unwrap()],
    };

    let key = config["api_key"].as_str();
    let mut diffs = Vec::new();

    if let Some(want) = case.get("expect_config") {
        for url in urls {
            // Without the `tls` feature an https URL is refused, whatever the case expects.
            if url.starts_with("https:") && !cfg!(any(feature = "tls", feature = "tls-aws-lc")) {
                continue;
            }
            let got = match client(config, url) {
                Ok(_) => "accepted",
                Err(_) => "rejected",
            };
            if want.as_str() != Some(got) {
                diffs.push(format!("{url}: {got} != {want}"));
            }
        }
        return diffs;
    }

    for url in urls {
        let server = Server::start(dir, case["server"].as_array().unwrap()).await;
        let client = match client(config, &url.replace("{server}", &server.base)) {
            Ok(client) => client,
            Err(error) => {
                diffs.push(format!("{url}: the client was not built: {error}"));
                continue;
            }
        };

        let mut shown = vec![format!("{client:?}")];
        for step in case["calls"].as_array().unwrap() {
            let expected = &step["expect"];
            let started = Instant::now();
            let got = call(&client, step, &mut shown).await;
            // An open expectation still makes its call; only the outcome is not compared.
            if expected.get("open").is_none() {
                diffs.extend(compare(expected, &got, started.elapsed()));
            }
        }

        if case.get("expect_closed").is_some()
            && !server.closed_within(Duration::from_secs(1)).await
        {
            diffs.push("the connection was not closed".into());
        }

        diffs.extend(common::server::check_requests(
            &case["expect_requests"],
            &server.requests(),
        ));

        if let Some(key) = key {
            if shown.iter().any(|text| text.contains(key)) {
                diffs.push("the API key shows in Debug or Display output".into());
            }
        }
    }
    diffs
}

#[tokio::test]
async fn transport_conformance() {
    let (dir, cases) = common::cases("transport");
    let (mut failures, mut pending) = (Vec::new(), Vec::new());

    for (name, case) in &cases {
        let open = case.get("open").is_some();
        let skipped = case["calls"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|step| step["expect"].get("open"));
        for decision in skipped {
            pending.push(format!("{name}: open {decision}"));
        }
        for diff in run(&dir, case).await {
            let line = format!("{name}: {diff}");
            if open {
                pending.push(line)
            } else {
                failures.push(line)
            }
        }
    }

    for line in &pending {
        eprintln!("pending: {line}");
    }
    assert!(cases.len() > 40, "only {} cases", cases.len());
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
