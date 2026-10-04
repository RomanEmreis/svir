//! A caller-supplied HTTP backend in place of the built-in one.

#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::{StreamExt, stream};
use svir::http::{Backend, BoxBody, HttpRequest, HttpResponse, Method};
use svir::prelude::*;
use svir::{Error, ErrorKind};

const ANSWER: &str = concat!(
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"pong\"},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// What the backend was asked to send, with the body it read.
struct Sent {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// A backend that answers every request with the same stream and remembers what it was sent.
#[derive(Clone, Default)]
struct Canned {
    seen: Arc<Mutex<Vec<Sent>>>,
}

/// A body type with a name: a backend does not have to box its stream.
type Once = stream::Iter<std::array::IntoIter<Result<Bytes, Error>, 1>>;

impl Backend for Canned {
    type Body = Once;

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse<Once>, Error> {
        let mut sent = Vec::new();
        let mut declared = 0;
        if let Some(mut body) = request.body {
            declared = body.length;
            while let Some(block) = body.stream.next().await {
                sent.extend_from_slice(&block?);
            }
        }
        assert_eq!(
            sent.len() as u64,
            declared,
            "the body is as long as it says"
        );
        self.seen.lock().unwrap().push(Sent {
            method: request.method,
            url: request.url,
            headers: request.headers,
            body: sent,
        });

        let body = stream::iter([Ok(Bytes::from_static(ANSWER.as_bytes()))]);
        let headers = vec![("content-type".to_owned(), "text/event-stream".to_owned())];
        Ok(HttpResponse::new(200, headers, body))
    }
}

#[tokio::test]
async fn a_custom_backend_carries_the_whole_exchange() {
    let backend = Canned::default();
    // The backend decides how to reach the host, so plain HTTP is for it to refuse or allow.
    let client: Client<Canned> = Client::openai("http://models.internal:8080/v1")
        .allow_http()
        .api_key("secret-key")
        .header("X-Title", "svir tests")
        .http(backend.clone())
        .build()
        .unwrap();

    let answer = client
        .complete(Request::new("m").user("ping"))
        .await
        .unwrap();
    assert_eq!(answer.text, "pong");

    let seen = backend.seen.lock().unwrap();
    let Sent {
        method,
        url,
        headers,
        body,
    } = &seen[0];
    assert_eq!(*method, Method::Post);
    assert_eq!(url, "http://models.internal:8080/v1/chat/completions");
    assert!(headers.contains(&("authorization".to_owned(), "Bearer secret-key".to_owned())));
    assert!(headers.contains(&("content-type".to_owned(), "application/json".to_owned())));
    assert!(headers.contains(&("x-title".to_owned(), "svir tests".to_owned())));
    let body: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(body["messages"][0]["content"], "ping");
    // Usage is asked for by default at the client.
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn credentials_are_withheld_from_a_request_shown() {
    /// A backend whose client gives a stream it cannot name can box it.
    struct Shown(Arc<Mutex<String>>);

    impl Backend for Shown {
        type Body = BoxBody;

        async fn send(&self, request: HttpRequest) -> Result<HttpResponse<BoxBody>, Error> {
            *self.0.lock().unwrap() = format!("{request:?}");
            Err(Error::new(ErrorKind::Transport).with_detail("refused"))
        }
    }

    let shown = Arc::new(Mutex::new(String::new()));
    let client = Client::openai("http://127.0.0.1:1")
        .api_key("secret-key")
        .header("x-gateway-key", "gateway-secret")
        .http(Shown(shown.clone()))
        .build()
        .unwrap();
    let error = client
        .complete(Request::new("m").user("ping"))
        .await
        .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::Transport);
    let shown = shown.lock().unwrap();
    assert!(
        shown.contains("authorization") && !shown.contains("secret-key"),
        "{shown}"
    );
    assert!(
        shown.contains("x-gateway-key") && !shown.contains("gateway-secret"),
        "{shown}"
    );
    assert!(shown.contains("application/json"), "{shown}");
}
