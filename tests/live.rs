//! Runs against a real model server. Never part of the default run: every test is ignored.
//!
//! ```sh
//! SVIR_MODEL=<model> cargo test --test live -- --ignored --test-threads=1
//! ```
//!
//! `SVIR_URL` is the server, `http://127.0.0.1:1234` unless set. The model has to call tools and
//! see images. One test at a time: a local server answers one request at a time anyway.
//!
//! A model's words are not deterministic, so each test asks for something only a working round
//! trip can produce, and checks for that.

#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use svir::layer::Timeout;
use svir::prelude::*;

const IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/data/red-circle.png");

fn builder() -> svir::ClientBuilder {
    let url = std::env::var("SVIR_URL").unwrap_or_else(|_| "http://127.0.0.1:1234".to_owned());

    Client::openai(url)
}

fn client() -> Client {
    builder().build().unwrap()
}

fn request() -> Request {
    let model = std::env::var("SVIR_MODEL").expect("set SVIR_MODEL to a model the server has");

    Request::new(model).reasoning(Effort::Low)
}

fn said(answer: &Completion, word: &str) -> bool {
    answer.text.to_lowercase().contains(word)
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn a_whole_answer_has_usage_and_timing() {
    let answer = client()
        .complete(request().user("Reply with the single word: heron"))
        .await
        .unwrap();

    assert_eq!(answer.finish, FinishReason::Stop);
    assert!(said(&answer, "heron"), "{:?}", answer.text);
    assert!(answer.calls.is_empty());

    let usage = answer.usage.expect("usage is asked for by default");
    assert!(usage.input > 0 && usage.output > 0, "{usage:?}");
    assert!(answer.timing.is_some());
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn a_stream_is_the_answer_in_pieces() {
    let request = request().user("Count from one to five, in words.");
    let mut stream = client().stream(&request).await.unwrap();

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut done = None;
    while let Some(event) = stream.next().await {
        assert!(done.is_none(), "the completion is the last event");

        match event.unwrap() {
            Event::Text(piece) => text.push_str(&piece),
            Event::Reasoning(piece) => reasoning.push_str(&piece.text),
            Event::Completed(answer) => done = Some(answer),
            _ => {}
        }
    }

    let done = done.expect("the stream ends with the completion");
    let kept: String = done.reasoning.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(done.text, text);
    assert_eq!(kept, reasoning);
    assert!(said(&done, "three"), "{:?}", done.text);
    assert!(!done.text.contains("<think>"));
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn an_output_limit_is_reported() {
    let request = request()
        .max_tokens(8)
        .user("Write a long essay about rivers.");
    let answer = client().complete(&request).await.unwrap();

    assert_eq!(answer.finish, FinishReason::Length);
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn a_conversation_keeps_its_history() {
    let client = client();
    let first = request().user("The code word is 'osprey'. Reply with: noted");
    let noted = client.complete(&first).await.unwrap();

    // Reasoning goes back under the field it came in: the server has to take it.
    let second = first
        .assistant(noted)
        .send_reasoning(true)
        .user("What is the code word? Reply with the word alone.");
    let answer = client.complete(&second).await.unwrap();

    assert!(said(&answer, "osprey"), "{:?}", answer.text);
}

#[derive(Deserialize)]
struct Locker {
    number: u32,
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn tool_results_reach_the_answer() {
    let client = client();
    let open = Tool::new("open_locker", "Open a locker and say what is in it.").schema(json!({
        "type": "object",
        "properties": {"number": {"type": "integer"}},
        "required": ["number"]
    }));
    let tools = Tools::new().add_tool(open, |args: Locker| async move {
        match args.number {
            7 => Ok("a brass key stamped 4417".to_owned()),
            other => Err(format!("locker {other} is empty")),
        }
    });

    let mut request = request()
        .tools(&tools)
        .user("Open locker 7 and tell me the number stamped on what is inside.");
    let mut called = 0;
    let mut tokens = 0;
    let answer = loop {
        let done = client.complete(&request).await.unwrap();
        tokens += done.usage.map_or(0, |usage| usage.output);
        if done.calls.is_empty() {
            break done;
        }

        assert_eq!(done.finish, FinishReason::ToolCalls);
        called += done.calls.len();
        assert!(called <= 4, "the model keeps calling tools");

        let results = tools.call_all(&done.calls).await;
        request = request.assistant(done).tool_results(results);
    };

    assert!(called >= 1, "the model answered without the tool");
    assert!(said(&answer, "4417"), "{:?}", answer.text);
    assert!(tokens > 0);
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn an_image_read_from_disk_is_seen() {
    let message = Message::user("What color is the shape, and what shape is it? Two words.")
        .with(Image::path(IMAGE));
    let answer = client().complete(request().message(message)).await.unwrap();

    assert!(said(&answer, "red"), "{:?}", answer.text);
    assert!(said(&answer, "circle"), "{:?}", answer.text);
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn a_text_file_is_read() {
    let message = Message::user("What is the berth number in the attached file?").with(
        TextFile::text("harbor.txt", "Berth 23 is reserved for the \"Onega\".\n"),
    );
    let answer = client().complete(request().message(message)).await.unwrap();

    assert!(said(&answer, "23"), "{:?}", answer.text);
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn the_model_is_listed() {
    let models = client().list_models().await.unwrap();
    let wanted = request().model;

    assert!(models.iter().any(|model| model.id == wanted), "{models:?}");
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn a_dropped_stream_leaves_the_client_usable() {
    let client = client();
    let long = request().user("Write a very long essay about rivers.");

    let mut stream = client.stream(&long).await.unwrap();
    stream.next().await.unwrap().unwrap();
    drop(stream);

    let answer = client
        .complete(request().user("Reply with the single word: heron"))
        .await
        .unwrap();
    assert!(said(&answer, "heron"), "{:?}", answer.text);
}

#[tokio::test]
#[ignore = "needs a model server"]
async fn a_deadline_that_cannot_be_met_is_a_timeout() {
    let client = builder()
        .layer(Timeout::total(Duration::from_millis(300)))
        .build()
        .unwrap();
    let error = client
        .complete(request().user("Write a very long essay about rivers."))
        .await
        .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::Timeout);
}
