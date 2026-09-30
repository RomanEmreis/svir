//! Layers around a call, against a scripted backend.

#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use svir::http::{Backend, BoxBody, HttpRequest, HttpResponse};
use svir::layer::{Retry, Timeout};
use svir::prelude::*;
use svir::{Error, ErrorKind};

const MS: Duration = Duration::from_millis(1);

fn text(content: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{content}\"}},\"finish_reason\":null}}]}}\n\n"
    )
}

const STOP: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

/// What the backend does with one request.
enum Reply {
    /// The connection is refused: the request never reaches the server.
    Refused,
    /// A response with this status and these headers, and no body.
    Status(u16, &'static [(&'static str, &'static str)]),
    /// An event stream: each piece after a wait.
    Answer(Vec<(Duration, String)>),
    /// No response, ever.
    Hang,
}

fn answer(pieces: &[(u64, &str)]) -> Reply {
    Reply::Answer(
        pieces
            .iter()
            .map(|(wait, piece)| (Duration::from_millis(*wait), (*piece).to_owned()))
            .collect(),
    )
}

fn pong() -> Reply {
    Reply::Answer(vec![
        (Duration::ZERO, text("pong")),
        (Duration::ZERO, STOP.to_owned()),
    ])
}

/// A backend that plays a script and remembers the bodies it was sent.
#[derive(Clone, Default)]
struct Script {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    calls: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Script {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: Arc::new(Mutex::new(replies.into_iter().collect())),
            ..Self::default()
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn client(&self) -> svir::ClientBuilder<Script> {
        Client::openai("http://127.0.0.1:1")
            .include_usage(false)
            .http(self.clone())
    }
}

impl Backend for Script {
    type Body = BoxBody;

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse<BoxBody>, Error> {
        use futures_util::StreamExt;

        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut sent = Vec::new();
        if let Some(mut body) = request.body {
            while let Some(block) = body.stream.next().await {
                sent.extend_from_slice(&block?);
            }
        }
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&sent).unwrap());

        let reply = self.replies.lock().unwrap().pop_front().expect("a reply");
        let sse = vec![("content-type".to_owned(), "text/event-stream".to_owned())];
        match reply {
            Reply::Refused => Err(Error::new(ErrorKind::Transport)
                .with_detail("refused")
                .with_unsent()),
            Reply::Status(status, headers) => {
                let headers = headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect();
                Ok(HttpResponse::new(
                    status,
                    headers,
                    Box::pin(stream::empty()),
                ))
            }
            Reply::Hang => std::future::pending().await,
            Reply::Answer(pieces) => {
                let body = stream::unfold(pieces.into_iter(), |mut pieces| async move {
                    let (wait, piece) = pieces.next()?;
                    tokio::time::sleep(wait).await;
                    Some((Ok(Bytes::from(piece)), pieces))
                });
                Ok(HttpResponse::new(200, sse, Box::pin(body)))
            }
        }
    }
}

fn request() -> Request {
    Request::new("m").user("ping")
}

#[tokio::test]
async fn retry_connect_retries_what_never_reached_the_server() {
    let script = Script::new([Reply::Refused, Reply::Refused, pong()]);
    let client = script
        .client()
        .layer(Retry::connect(3).backoff(MS))
        .build()
        .unwrap();

    assert_eq!(client.complete(request()).await.unwrap().text, "pong");
    assert_eq!(script.calls(), 3);
}

#[tokio::test]
async fn retry_connect_leaves_a_server_error_alone() {
    let script = Script::new([Reply::Status(503, &[]), pong()]);
    let client = script
        .client()
        .layer(Retry::connect(3).backoff(MS))
        .build()
        .unwrap();

    let error = client.complete(request()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Transport);
    assert_eq!(script.calls(), 1);
}

#[tokio::test]
async fn retry_transient_retries_what_may_pass_and_nothing_else() {
    let script = Script::new([
        Reply::Status(503, &[]),
        Reply::Status(429, &[("retry-after", "0")]),
        Reply::Refused,
        pong(),
    ]);
    let client = script
        .client()
        .layer(Retry::transient(5).backoff(MS))
        .build()
        .unwrap();
    assert_eq!(client.complete(request()).await.unwrap().text, "pong");
    assert_eq!(script.calls(), 4);

    let script = Script::new([Reply::Status(401, &[]), pong()]);
    let client = script
        .client()
        .layer(Retry::transient(5).backoff(MS))
        .build()
        .unwrap();
    let error = client.complete(request()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Authentication);
    assert_eq!(script.calls(), 1);
}

#[tokio::test]
async fn retry_gives_up_after_its_retries() {
    let script = Script::new([Reply::Refused, Reply::Refused, Reply::Refused, pong()]);
    let client = script
        .client()
        .layer(Retry::connect(2).backoff(MS))
        .build()
        .unwrap();

    let error = client.complete(request()).await.unwrap_err();
    assert!(error.is_unsent());
    assert_eq!(script.calls(), 3);
}

#[tokio::test]
async fn nothing_is_retried_once_the_answer_has_started() {
    // The stream ends after some text, with no [DONE]: a retryable error, but too late to retry.
    let script = Script::new([answer(&[(0, &text("par"))]), pong()]);
    let client = script
        .client()
        .layer(Retry::transient(3).backoff(MS))
        .build()
        .unwrap();

    let mut stream = client.stream(request()).await.unwrap();
    assert!(matches!(stream.next().await, Some(Ok(Event::Text(t))) if t == "par"));
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TruncatedStream);
    assert_eq!(script.calls(), 1);
}

#[tokio::test]
async fn the_first_layer_added_is_the_outermost() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let note = |label: &'static str| {
        let order = order.clone();
        move |request: Request, next: svir::layer::Next<Script>| {
            let order = order.clone();
            async move {
                order.lock().unwrap().push(format!("{label} in"));
                let answer = next.run(request).await;
                order.lock().unwrap().push(format!("{label} out"));
                answer
            }
        }
    };

    let script = Script::new([pong()]);
    let client = script
        .client()
        .wrap(note("outer"))
        .wrap(note("inner"))
        .build()
        .unwrap();
    client.complete(request()).await.unwrap();

    assert_eq!(
        *order.lock().unwrap(),
        ["outer in", "inner in", "inner out", "outer out"]
    );
}

#[tokio::test]
async fn a_layer_can_change_the_request_and_watch_the_answer() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let watch = seen.clone();

    let script = Script::new([pong()]);
    let client = script
        .client()
        .wrap(move |request, next| {
            let watch = watch.clone();
            async move {
                let stream = next.run(request.system("Be brief.")).await?;
                Ok(stream.inspect(move |item| {
                    let label = match item {
                        Ok(Event::Text(text)) => format!("text {text}"),
                        Ok(Event::Completed(done)) => format!("completed {}", done.text),
                        Ok(_) => "other".to_owned(),
                        Err(error) => format!("error {:?}", error.kind()),
                    };
                    watch.lock().unwrap().push(label);
                }))
            }
        })
        .build()
        .unwrap();

    assert_eq!(client.complete(request()).await.unwrap().text, "pong");
    assert_eq!(*seen.lock().unwrap(), ["text pong", "completed pong"]);
    let bodies = script.bodies.lock().unwrap();
    assert_eq!(bodies[0]["messages"][0]["role"], "system");
    assert_eq!(bodies[0]["messages"][0]["content"], "Be brief.");
}

#[tokio::test]
async fn first_token_timeout_covers_the_wait_for_the_first_of_the_answer() {
    let limit = Timeout::first_token(Duration::from_millis(60));

    // The first token is late.
    let script = Script::new([answer(&[(300, &text("late")), (0, STOP)])]);
    let client = script.client().layer(limit).build().unwrap();
    let error = client.complete(request()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);

    // The first token is in time; what follows may take longer than the limit.
    let script = Script::new([answer(&[(0, &text("a")), (150, &text("b")), (0, STOP)])]);
    let client = script.client().layer(limit).build().unwrap();
    assert_eq!(client.complete(request()).await.unwrap().text, "ab");

    // No response at all.
    let script = Script::new([Reply::Hang]);
    let client = script.client().layer(limit).build().unwrap();
    let error = client.stream(request()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
}

#[tokio::test]
async fn total_timeout_covers_the_whole_answer() {
    let script = Script::new([answer(&[
        (0, &text("a")),
        (40, &text("b")),
        (300, &text("c")),
        (0, STOP),
    ])]);
    let client = script
        .client()
        .layer(Timeout::total(Duration::from_millis(120)))
        .build()
        .unwrap();

    let mut stream = client.stream(request()).await.unwrap();
    let mut text = String::new();
    let error = loop {
        match stream.next().await.unwrap() {
            Ok(Event::Text(delta)) => text.push_str(&delta),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert_eq!(text, "ab");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(stream.next().await.is_none(), "an error ends the stream");
}

#[tokio::test]
async fn idle_timeout_covers_silence_in_the_middle() {
    let script = Script::new([answer(&[(0, &text("a")), (300, &text("b")), (0, STOP)])]);
    let client = script
        .client()
        .layer(Timeout::idle(Duration::from_millis(60)))
        .build()
        .unwrap();

    let error = client.complete(request()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
}

#[tokio::test]
async fn the_backend_is_chosen_before_the_layers() {
    let script = Script::new([]);
    let error = Client::openai("http://127.0.0.1:1")
        .layer(Retry::connect(1))
        .http(script)
        .build()
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Config);
}

#[cfg(feature = "tracing")]
#[tokio::test]
async fn trace_passes_the_answer_through() {
    let script = Script::new([pong(), Reply::Status(503, &[])]);
    let client = script.client().layer(svir::layer::Trace).build().unwrap();

    assert_eq!(client.complete(request()).await.unwrap().text, "pong");
    assert!(client.complete(request()).await.is_err());
}
