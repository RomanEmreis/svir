//! Layers: middleware around a call.
//!
//! A layer sees the request on its way to the model and the answer on its way back. It wraps the
//! call itself, `Request -> Result<EventStream, Error>`, not HTTP, so the same layer works with
//! any backend and any wire API.
//!
//! ```no_run
//! use std::time::Duration;
//! use svir::layer::{Retry, Timeout};
//! use svir::prelude::*;
//!
//! # fn build() -> Result<(), svir::Error> {
//! let client = Client::openai("http://127.0.0.1:1234")
//!     .layer(Retry::connect(3))
//!     .layer(Timeout::first_token(Duration::from_secs(120)))
//!     .wrap(|request, next| async move {
//!         let model = request.model.clone();
//!         let answer = next.run(request).await;
//!         println!("{model} answered: {}", answer.is_ok());
//!         answer
//!     })
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! The first layer added is the outermost. A client without layers pays nothing for them; a
//! client with layers boxes one future per layer per request, which is what lets its type stay
//! the same whatever its layers are.

use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

use crate::{
    Client, Error, ErrorKind, EventStream, Request,
    http::{Backend, Hyper},
};

/// A retry waits at most this long, whatever the server asked for.
const BACKOFF_LIMIT: Duration = Duration::from_secs(30);

/// Middleware around a call.
///
/// An implementation usually passes the request, changed or not, to `next`, and then looks at
/// the result, or shapes the stream with [`EventStream::inspect`] and its deadlines.
pub trait Layer<B: Backend = Hyper>: Send + Sync + 'static {
    /// Handles `request`.
    fn call(
        &self,
        request: Request,
        next: Next<B>,
    ) -> impl Future<Output = Result<EventStream<B>, Error>> + Send;
}

/// The rest of the stack under a layer: the layers added after it, then the model server.
pub struct Next<B: Backend = Hyper> {
    client: Client<B>,
    index: usize,
}

impl<B: Backend> Next<B> {
    pub(crate) fn new(client: Client<B>, index: usize) -> Self {
        Self { client, index }
    }

    /// Passes `request` on and returns the answer.
    pub async fn run(self, request: Request) -> Result<EventStream<B>, Error> {
        self.client.run_from(self.index, request).await
    }
}

impl<B: Backend> Clone for Next<B> {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            index: self.index,
        }
    }
}

impl<B: Backend> std::fmt::Debug for Next<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Next").field("index", &self.index).finish()
    }
}

/// A layer behind a trait object. This is the one place a client with layers boxes a future.
pub(crate) trait Erased<B: Backend>: Send + Sync {
    fn call<'a>(
        &'a self,
        request: Request,
        next: Next<B>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream<B>, Error>> + Send + 'a>>;
}

impl<B: Backend, L: Layer<B>> Erased<B> for L {
    fn call<'a>(
        &'a self,
        request: Request,
        next: Next<B>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream<B>, Error>> + Send + 'a>> {
        Box::pin(Layer::call(self, request, next))
    }
}

/// A closure as a layer.
pub(crate) struct Wrap<F>(pub(crate) F);

impl<B, F, Fut> Layer<B> for Wrap<F>
where
    B: Backend,
    F: Fn(Request, Next<B>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<EventStream<B>, Error>> + Send,
{
    fn call(
        &self,
        request: Request,
        next: Next<B>,
    ) -> impl Future<Output = Result<EventStream<B>, Error>> + Send {
        (self.0)(request, next)
    }
}

/// Retries a call that failed before the server began to answer.
///
/// Once the answer has started, nothing is retried: the caller would see the same text twice.
/// A retry waits as long as the server asked (`Retry-After`), or else 500 ms, doubling each
/// time, and never more than 30 seconds.
#[derive(Debug, Clone)]
pub struct Retry {
    retries: u32,
    transient: bool,
    backoff: Duration,
}

impl Retry {
    /// Retries, up to `retries` times, a call whose request never reached the server: a
    /// connection that was refused or timed out. Always safe.
    pub fn connect(retries: u32) -> Self {
        Self {
            retries,
            transient: false,
            backoff: Duration::from_millis(500),
        }
    }

    /// Retries, up to `retries` times, any failure that may pass: a connection that could not be
    /// made, a timeout, a rate limit, a server error.
    pub fn transient(retries: u32) -> Self {
        Self {
            transient: true,
            ..Self::connect(retries)
        }
    }

    /// Sets the wait before the first retry. It doubles after each one.
    pub fn backoff(mut self, initial: Duration) -> Self {
        self.backoff = initial;
        self
    }

    fn covers(&self, error: &Error) -> bool {
        error.is_unsent() || (self.transient && error.is_retryable())
    }
}

impl<B: Backend> Layer<B> for Retry {
    async fn call(&self, request: Request, next: Next<B>) -> Result<EventStream<B>, Error> {
        let mut backoff = self.backoff;
        for _ in 0..self.retries {
            let error = match next.clone().run(request.clone()).await {
                Ok(stream) => return Ok(stream),
                Err(error) if self.covers(&error) => error,
                Err(error) => return Err(error),
            };

            let wait = error.retry_after().unwrap_or(backoff).min(BACKOFF_LIMIT);
            tokio::time::sleep(wait).await;
            backoff = backoff.saturating_mul(2).min(BACKOFF_LIMIT);
        }

        // The last attempt: whatever it gives is the answer.
        next.run(request).await
    }
}

/// Fails a call that takes too long with [`ErrorKind::Timeout`].
#[derive(Debug, Clone, Copy)]
pub struct Timeout {
    limit: Duration,
    what: Limited,
}

#[derive(Debug, Clone, Copy)]
enum Limited {
    FirstToken,
    Idle,
    Total,
}

impl Timeout {
    /// At most `limit` from the call to the first of the answer: text, reasoning, or a piece of
    /// a tool call.
    pub fn first_token(limit: Duration) -> Self {
        Self {
            limit,
            what: Limited::FirstToken,
        }
    }

    /// At most `limit` of silence from the server, before the answer and during it.
    pub fn idle(limit: Duration) -> Self {
        Self {
            limit,
            what: Limited::Idle,
        }
    }

    /// At most `limit` from the call to the complete answer.
    pub fn total(limit: Duration) -> Self {
        Self {
            limit,
            what: Limited::Total,
        }
    }
}

impl<B: Backend> Layer<B> for Timeout {
    async fn call(&self, request: Request, next: Next<B>) -> Result<EventStream<B>, Error> {
        let deadline = Instant::now() + self.limit;
        let stream = tokio::time::timeout(self.limit, next.run(request))
            .await
            .map_err(|_| {
                Error::new(ErrorKind::Timeout).with_detail("the server did not answer in time")
            })??;

        Ok(match self.what {
            Limited::FirstToken => stream.first_token_by(deadline),
            Limited::Idle => stream.idle_timeout(self.limit),
            Limited::Total => stream.complete_by(deadline),
        })
    }
}

/// Reports each call through `tracing`: the model and the number of messages going in; how long
/// the response took to start, how the answer finished, its token counts, and any failure coming
/// back. It never records text, reasoning, tool arguments, or the server's messages.
#[cfg(feature = "tracing")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Trace;

#[cfg(feature = "tracing")]
impl<B: Backend> Layer<B> for Trace {
    async fn call(&self, request: Request, next: Next<B>) -> Result<EventStream<B>, Error> {
        use crate::Event;

        let started = Instant::now();
        let model = request.model.clone();
        tracing::debug!(model = %model, messages = request.messages.len(), "request");

        let stream = match next.run(request).await {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(
                    model = %model,
                    kind = ?error.kind(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "request failed"
                );
                return Err(error);
            }
        };
        tracing::debug!(
            model = %model,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "response started"
        );

        Ok(stream.inspect(move |item| match item {
            Ok(Event::Completed(done)) => tracing::info!(
                model = %model,
                finish = ?done.finish,
                input_tokens = done.usage.map(|usage| usage.input),
                output_tokens = done.usage.map(|usage| usage.output),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "answer complete"
            ),
            Err(error) => tracing::warn!(
                model = %model,
                kind = ?error.kind(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "answer failed"
            ),
            Ok(_) => {}
        }))
    }
}
