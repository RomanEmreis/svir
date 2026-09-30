//! The loop that feeds tool results back: caller code, a few lines long.

#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use svir::Error;
use svir::http::{Backend, HttpRequest, HttpResponse};
use svir::prelude::*;

const CALL: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-a\",",
    "\"type\":\"function\",\"function\":{\"name\":\"double\",\"arguments\":\"{\\\"value\\\":21}\"}}]},",
    "\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
);

const FINAL: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"It is 42.\"},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// A model that answers each request with the next scripted stream.
#[derive(Clone)]
struct Model {
    answers: Arc<Mutex<VecDeque<&'static str>>>,
    bodies: Arc<Mutex<Vec<Value>>>,
}

type Once = stream::Iter<std::array::IntoIter<Result<Bytes, Error>, 1>>;

impl Backend for Model {
    type Body = Once;

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse<Once>, Error> {
        let mut sent = Vec::new();
        let mut body = request.body.unwrap();
        while let Some(block) = body.stream.next().await {
            sent.extend_from_slice(&block?);
        }
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&sent).unwrap());

        let answer = self.answers.lock().unwrap().pop_front().unwrap();
        let headers = vec![("content-type".to_owned(), "text/event-stream".to_owned())];
        let body = stream::iter([Ok(Bytes::from_static(answer.as_bytes()))]);
        Ok(HttpResponse::new(200, headers, body))
    }
}

#[derive(Deserialize)]
struct Double {
    value: i64,
}

#[tokio::test]
async fn tool_results_go_back_until_the_model_answers() {
    let model = Model {
        answers: Arc::new(Mutex::new([CALL, FINAL].into())),
        bodies: Arc::default(),
    };
    let client = Client::openai("http://127.0.0.1:1")
        .include_usage(false)
        .http(model.clone())
        .build()
        .unwrap();

    let double = Tool::new("double", "Double a value.").schema(json!({
        "type": "object",
        "properties": {"value": {"type": "integer"}},
        "required": ["value"]
    }));
    let tools = Tools::new().add_tool(double, |args: Double| async move {
        (args.value * 2).to_string()
    });

    let mut request = Request::new("m").tools(&tools).user("What is 21 doubled?");
    let answer = loop {
        let done = client.complete(&request).await.unwrap();
        if done.calls.is_empty() {
            break done.text;
        }

        let results = tools.call_all(&done.calls).await;
        request = request.assistant(done).tool_results(results);
    };
    assert_eq!(answer, "It is 42.");

    let bodies = model.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["tools"][0]["function"]["name"], "double");
    assert_eq!(
        bodies[1]["messages"],
        json!([
            {"role": "user", "content": "What is 21 doubled?"},
            {"role": "assistant", "content": "", "tool_calls": [{
                "id": "call-a",
                "type": "function",
                "function": {"name": "double", "arguments": "{\"value\":21}"}
            }]},
            {"role": "tool", "tool_call_id": "call-a", "content": "42"}
        ])
    );
}
