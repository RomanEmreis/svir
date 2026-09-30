//! The answer as it arrives: its bytes, and its events.
//!
//! Both streams are state machines written by hand, so they have names and need no pinning.

use std::{
    collections::VecDeque,
    fmt,
    future::{Future, poll_fn},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use futures_core::Stream;
use tokio::time::{Instant, Sleep};

use crate::{
    Completion, Error, ErrorKind, Event,
    http::{Backend, Hyper},
    openai::chat::Decoder,
};

/// A timer. Boxed so that the streams stay `Unpin`, and `next` needs no pinning by the caller.
type Timer = Pin<Box<Sleep>>;

/// Something that watches the items of an [`EventStream`] go by.
type Inspector = Box<dyn FnMut(&Result<Event, Error>) + Send>;

/// The bytes of a response as the server sent them, failing if it goes silent for too long.
///
/// Returned by [`Client::send`](crate::Client::send). It implements [`Stream`], and has its own
/// [`next`](Self::next). Dropping it closes the connection.
pub struct RawStream<B: Backend = Hyper> {
    body: Option<B::Body>,
    idle: Option<Idle>,
}

/// The idle timeout of a stream: how long silence may last, and the timer counting it.
struct Idle {
    limit: Duration,
    timer: Timer,
}

impl<B: Backend> RawStream<B> {
    pub(crate) fn new(body: B::Body, idle: Option<Duration>) -> Self {
        let mut stream = Self {
            body: Some(body),
            idle: None,
        };
        if let Some(limit) = idle {
            stream.tighten_idle(limit);
        }

        stream
    }

    /// The next piece, or `None` when the response is over.
    pub async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }

    /// Lowers the idle timeout to `limit`, unless it is already lower.
    fn tighten_idle(&mut self, limit: Duration) {
        if self.idle.as_ref().is_some_and(|idle| idle.limit <= limit) {
            return;
        }

        self.idle = Some(Idle {
            limit,
            timer: Box::pin(tokio::time::sleep(limit)),
        });
    }
}

impl<B: Backend> Stream for RawStream<B> {
    type Item = Result<Bytes, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        let Some(body) = this.body.as_mut() else {
            return Poll::Ready(None);
        };

        match Pin::new(body).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                if let Some(idle) = &mut this.idle {
                    idle.timer.as_mut().reset(Instant::now() + idle.limit);
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.body = None;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.body = None;
                Poll::Ready(None)
            }
            Poll::Pending => {
                let stalled = this
                    .idle
                    .as_mut()
                    .is_some_and(|idle| idle.timer.as_mut().poll(cx).is_ready());
                if !stalled {
                    return Poll::Pending;
                }

                this.body = None;
                Poll::Ready(Some(Err(timeout("the response stalled"))))
            }
        }
    }
}

impl<B: Backend> fmt::Debug for RawStream<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawStream").finish_non_exhaustive()
    }
}

/// The answer to one request, as a stream of events.
///
/// It implements [`Stream`], and has its own [`next`](Self::next) so that no extension trait is
/// needed to read it. [`Event::Completed`] is the last item; an error ends the stream. Dropping
/// it cancels the request. It is `Send` and owns everything it needs, so it can be moved into a
/// task.
///
/// A layer shapes the stream it passes on with [`first_token_by`](Self::first_token_by),
/// [`complete_by`](Self::complete_by), [`idle_timeout`](Self::idle_timeout), and
/// [`inspect`](Self::inspect). They leave its type as it is, so a client has the same type
/// whatever its layers.
pub struct EventStream<B: Backend = Hyper> {
    bytes: Option<RawStream<B>>,
    decoder: Option<Decoder>,
    queue: VecDeque<Result<Event, Error>>,
    /// Fires if nothing of the answer has arrived by then.
    first_token: Option<Timer>,
    /// Fires if the answer is not complete by then.
    complete: Option<Timer>,
    inspectors: Vec<Inspector>,
}

impl<B: Backend> EventStream<B> {
    pub(crate) fn new(bytes: RawStream<B>, decoder: Decoder) -> Self {
        Self {
            bytes: Some(bytes),
            decoder: Some(decoder),
            queue: VecDeque::new(),
            first_token: None,
            complete: None,
            inspectors: Vec::new(),
        }
    }

    /// The next event, or `None` when the stream is over.
    pub async fn next(&mut self) -> Option<Result<Event, Error>> {
        poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }

    /// Reads the stream to its end and returns the whole answer.
    pub async fn completion(mut self) -> Result<Completion, Error> {
        while let Some(event) = self.next().await {
            if let Event::Completed(done) = event? {
                return Ok(done);
            }
        }

        Err(Error::new(ErrorKind::TruncatedStream).with_detail("the stream ended before [DONE]"))
    }

    /// Fails the stream with [`ErrorKind::Timeout`] if nothing of the answer (text, reasoning, or
    /// a piece of a tool call) has arrived by `deadline`. An earlier deadline already set stays.
    pub fn first_token_by(mut self, deadline: std::time::Instant) -> Self {
        earlier(&mut self.first_token, deadline);
        self
    }

    /// Fails the stream with [`ErrorKind::Timeout`] if the answer is not complete by `deadline`.
    /// An earlier deadline already set stays.
    pub fn complete_by(mut self, deadline: std::time::Instant) -> Self {
        earlier(&mut self.complete, deadline);
        self
    }

    /// Fails the stream with [`ErrorKind::Timeout`] if the server sends nothing for `limit`. A
    /// lower limit already set stays.
    pub fn idle_timeout(mut self, limit: Duration) -> Self {
        if let Some(bytes) = self.bytes.as_mut() {
            bytes.tighten_idle(limit);
        }

        self
    }

    /// Calls `watch` with every item just before it is returned, the last one included: for
    /// metrics and logging. It sees the items; it cannot change them.
    pub fn inspect(mut self, watch: impl FnMut(&Result<Event, Error>) + Send + 'static) -> Self {
        self.inspectors.push(Box::new(watch));
        self
    }

    /// Hands out one item, closing the stream if it is the last.
    fn deliver(&mut self, item: Result<Event, Error>) -> Poll<Option<Result<Event, Error>>> {
        if item.is_ok() {
            // Anything of the answer counts as its first token.
            self.first_token = None;
        }
        if matches!(item, Ok(Event::Completed(_)) | Err(_)) {
            // The answer is over: stop reading and close the connection.
            self.bytes = None;
            self.decoder = None;
            self.queue.clear();
            self.first_token = None;
            self.complete = None;
        }
        for watch in &mut self.inspectors {
            watch(&item);
        }

        Poll::Ready(Some(item))
    }
}

impl<B: Backend> Stream for EventStream<B> {
    type Item = Result<Event, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        loop {
            if let Some(item) = this.queue.pop_front() {
                return this.deliver(item);
            }
            let (Some(bytes), Some(decoder)) = (this.bytes.as_mut(), this.decoder.as_mut()) else {
                return Poll::Ready(None);
            };

            match Pin::new(bytes).poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => this.queue.extend(decoder.push(&chunk)),
                Poll::Ready(Some(Err(error))) => this.queue.push_back(Err(error)),
                Poll::Ready(None) => {
                    this.bytes = None;
                    if let Some(Err(error)) = this.decoder.take().map(Decoder::finish) {
                        this.queue.push_back(Err(error));
                    }
                }
                Poll::Pending => {
                    let late = if fired(&mut this.first_token, cx) {
                        "nothing of the answer arrived in time"
                    } else if fired(&mut this.complete, cx) {
                        "the answer was not complete in time"
                    } else {
                        return Poll::Pending;
                    };

                    return this.deliver(Err(timeout(late)));
                }
            }
        }
    }
}

impl<B: Backend> fmt::Debug for EventStream<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventStream").finish_non_exhaustive()
    }
}

/// Sets `timer` to `deadline` unless it already fires earlier.
fn earlier(timer: &mut Option<Timer>, deadline: std::time::Instant) {
    let deadline = Instant::from_std(deadline);
    if timer.as_ref().is_some_and(|set| set.deadline() <= deadline) {
        return;
    }

    *timer = Some(Box::pin(tokio::time::sleep_until(deadline)));
}

/// Whether a timer is set and has fired. Polling it also arranges the wake-up.
fn fired(timer: &mut Option<Timer>, cx: &mut Context<'_>) -> bool {
    timer
        .as_mut()
        .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
}

fn timeout(detail: &'static str) -> Error {
    Error::new(ErrorKind::Timeout).with_detail(detail)
}
