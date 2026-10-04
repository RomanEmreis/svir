//! The body as a stream. It is a state machine written by hand, so it is a type with a name: no
//! boxed future and no trait object between the encoder and the socket.

use std::{
    collections::VecDeque,
    fmt,
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures_core::Stream;
use tokio::{
    fs::File,
    io::{AsyncRead, ReadBuf},
    task::JoinHandle,
};

use super::{Encoding, Segment, attachment, encode, escape::Utf8Check, not_utf8, unreadable};
use crate::{Error, Source};

/// A [`Body`](super::Body) being read: its blocks, in order.
///
/// The bytes it yields add up to exactly [`Body::len`](super::Body::len), or it ends with an
/// error.
pub struct BodyStream {
    segments: VecDeque<Segment>,
    block: usize,
    /// One block, reused for every read. Its size never changes.
    buffer: Box<[u8]>,
    state: State,
}

enum State {
    /// Between segments.
    Idle,
    /// A file is being opened, off the async threads.
    Opening {
        task: JoinHandle<io::Result<std::fs::File>>,
        left: u64,
        encoded_left: u64,
        encoding: Encoding,
    },
    /// A file is being read into the block buffer.
    Reading(Reading),
    /// The recorded size is used up. One more read confirms the file ends there; then `out`, the
    /// last block, goes out.
    Probing { file: File, out: Bytes },
    /// An attachment held in memory.
    Memory {
        data: Bytes,
        at: usize,
        encoding: Encoding,
    },
    /// Finished, or failed.
    Done,
}

struct Reading {
    file: File,
    /// Raw bytes still to come, by the recorded size.
    left: u64,
    /// Encoded bytes still to come, by the recorded length.
    encoded_left: u64,
    encoding: Encoding,
    /// How much of the block buffer is filled.
    filled: usize,
    /// Text goes inside a JSON string, so it is checked to be UTF-8 as it is read: it may have
    /// been measured by the caller, or not read at all before.
    utf8: Utf8Check,
}

impl BodyStream {
    pub(super) fn new(segments: Vec<Segment>, block: usize) -> Self {
        Self {
            segments: segments.into(),
            block,
            buffer: vec![0; block].into_boxed_slice(),
            state: State::Idle,
        }
    }
}

impl Stream for BodyStream {
    type Item = Result<Bytes, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        let fail = |error: Error| Poll::Ready(Some(Err(error)));
        loop {
            // The state is `Done` while it is taken: an early return on an error ends the stream.
            match std::mem::replace(&mut this.state, State::Done) {
                State::Done => return Poll::Ready(None),

                State::Idle => match this.segments.pop_front() {
                    None => return Poll::Ready(None),
                    Some(Segment::Bytes(bytes)) => {
                        this.state = State::Idle;
                        return Poll::Ready(Some(Ok(bytes)));
                    }
                    Some(Segment::Data {
                        source: Source::Bytes(data),
                        encoding,
                        ..
                    }) => {
                        this.state = State::Memory {
                            data,
                            at: 0,
                            encoding,
                        };
                    }
                    Some(Segment::Data {
                        source: Source::Path(path),
                        size,
                        encoded,
                        encoding,
                    }) => {
                        this.state = State::Opening {
                            task: tokio::task::spawn_blocking(move || std::fs::File::open(path)),
                            left: size,
                            encoded_left: encoded,
                            encoding,
                        };
                    }
                },

                State::Opening {
                    mut task,
                    left,
                    encoded_left,
                    encoding,
                } => match Pin::new(&mut task).poll(cx) {
                    Poll::Pending => {
                        this.state = State::Opening {
                            task,
                            left,
                            encoded_left,
                            encoding,
                        };
                        return Poll::Pending;
                    }
                    Poll::Ready(Ok(Ok(file))) => {
                        this.state = State::Reading(Reading {
                            file: File::from_std(file),
                            left,
                            encoded_left,
                            encoding,
                            filled: 0,
                            utf8: Utf8Check::default(),
                        });
                    }
                    Poll::Ready(Ok(Err(source))) => return fail(unreadable(source)),
                    Poll::Ready(Err(source)) => return fail(unreadable(io::Error::other(source))),
                },

                State::Memory { data, at, encoding } => {
                    if at >= data.len() {
                        this.state = State::Idle;
                        continue;
                    }
                    let end = data.len().min(at + this.block);
                    let mut out = Vec::new();
                    encode(&data[at..end], encoding, &mut out);
                    this.state = State::Memory {
                        data,
                        at: end,
                        encoding,
                    };
                    return Poll::Ready(Some(Ok(Bytes::from(out))));
                }

                State::Reading(mut reading) => {
                    // A read may return less than asked for; only a full block, or the end of
                    // the file, keeps base64 free of padding in the middle.
                    let mut at_end = false;
                    while reading.filled < this.block {
                        let mut buffer = ReadBuf::new(&mut this.buffer[reading.filled..]);
                        match Pin::new(&mut reading.file).poll_read(cx, &mut buffer) {
                            Poll::Pending => {
                                this.state = State::Reading(reading);
                                return Poll::Pending;
                            }
                            Poll::Ready(Err(source)) => return fail(unreadable(source)),
                            Poll::Ready(Ok(())) if buffer.filled().is_empty() => {
                                at_end = true;
                                break;
                            }
                            Poll::Ready(Ok(())) => reading.filled += buffer.filled().len(),
                        }
                    }

                    let read = reading.filled as u64;
                    if read > reading.left || (at_end && read != reading.left) {
                        return fail(changed());
                    }
                    if reading.filled == 0 {
                        if reading.encoded_left != 0 {
                            return fail(changed());
                        }
                        this.state = State::Idle;
                        continue;
                    }

                    let block = &this.buffer[..reading.filled];
                    let left = reading.left - read;
                    if reading.encoding == Encoding::JsonString
                        && (!reading.utf8.push(block) || (left == 0 && !reading.utf8.finish()))
                    {
                        return fail(not_utf8());
                    }

                    let mut out = Vec::new();
                    encode(block, reading.encoding, &mut out);
                    let Some(encoded_left) = reading.encoded_left.checked_sub(out.len() as u64)
                    else {
                        return fail(changed());
                    };
                    let out = Bytes::from(out);
                    if left > 0 {
                        this.state = State::Reading(Reading {
                            left,
                            encoded_left,
                            filled: 0,
                            ..reading
                        });
                        return Poll::Ready(Some(Ok(out)));
                    }
                    // The recorded size is used up: the file must have encoded to exactly the
                    // promised length, and must end here.
                    if encoded_left != 0 {
                        return fail(changed());
                    }
                    if at_end {
                        this.state = State::Idle;
                        return Poll::Ready(Some(Ok(out)));
                    }
                    this.state = State::Probing {
                        file: reading.file,
                        out,
                    };
                }

                State::Probing { mut file, out } => {
                    let mut byte = [0u8; 1];
                    let mut buffer = ReadBuf::new(&mut byte);
                    match Pin::new(&mut file).poll_read(cx, &mut buffer) {
                        Poll::Pending => {
                            this.state = State::Probing { file, out };
                            return Poll::Pending;
                        }
                        Poll::Ready(Err(source)) => return fail(unreadable(source)),
                        Poll::Ready(Ok(())) if !buffer.filled().is_empty() => {
                            return fail(changed());
                        }
                        Poll::Ready(Ok(())) => {
                            this.state = State::Idle;
                            return Poll::Ready(Some(Ok(out)));
                        }
                    }
                }
            }
        }
    }
}

impl fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BodyStream")
            .field("segments_left", &self.segments.len())
            .finish_non_exhaustive()
    }
}

fn changed() -> Error {
    attachment("an attachment changed after the body was built, or is not the length it declares")
}
