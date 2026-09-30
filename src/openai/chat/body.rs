//! A request body whose length is known before its first byte.

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;

use super::escape::escape_into;
use crate::{Error, ErrorKind, Source};

/// How an attachment is written into the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Encoding {
    /// Base64, for the data URL of an image.
    Base64,
    /// Escaped for the inside of a JSON string, for a text file.
    JsonString,
}

/// A piece of the body: bytes that are JSON already, or an attachment encoded as it is sent.
/// The recorded size and length are checked only when a file is read, with `client`.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "client"), allow(dead_code))]
pub(super) enum Segment {
    Bytes(Bytes),
    Data {
        source: Source,
        /// The attachment's size when the body was built.
        size: u64,
        /// What it encodes to.
        encoded: u64,
        encoding: Encoding,
    },
}

/// A request body: a sequence of bytes whose exact length is known before the first one is sent.
///
/// Attachments are encoded while the body is read, a block at a time, so an image is never held
/// both as bytes and as base64. Cloning is cheap, and a clone produces the same bytes: a request
/// can be sent again.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "client"), allow(dead_code))]
pub struct Body {
    segments: Vec<Segment>,
    length: u64,
    block: usize,
}

impl Body {
    pub(super) fn new(segments: Vec<Segment>, length: u64, block: usize) -> Self {
        Self {
            segments,
            length,
            block,
        }
    }

    /// The exact length in bytes, for `Content-Length`.
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Whether the body has no bytes. A request body always has some.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// The whole body in memory. An attachment held as a file path is
    /// [`ErrorKind::Attachment`]: read those with `into_stream`.
    pub fn into_bytes(self) -> Result<Bytes, Error> {
        let mut out = Vec::with_capacity(usize::try_from(self.length).unwrap_or(0));
        for segment in self.segments {
            match segment {
                Segment::Bytes(bytes) => out.extend_from_slice(&bytes),
                Segment::Data {
                    source: Source::Bytes(data),
                    encoding,
                    ..
                } => encode(&data, encoding, &mut out),
                Segment::Data { .. } => {
                    return Err(attachment(
                        "a file attachment is read while the body streams",
                    ));
                }
            }
        }
        Ok(Bytes::from(out))
    }
}

fn encode(block: &[u8], encoding: Encoding, out: &mut Vec<u8>) {
    match encoding {
        Encoding::Base64 => out.extend_from_slice(STANDARD.encode(block).as_bytes()),
        Encoding::JsonString => escape_into(block, out),
    }
}

pub(super) fn attachment(detail: &'static str) -> Error {
    Error::new(ErrorKind::Attachment).with_detail(detail)
}

#[cfg(feature = "client")]
mod stream {
    use std::collections::VecDeque;

    use bytes::Bytes;
    use futures_util::{Stream, stream};
    use tokio::{fs::File, io::AsyncReadExt};

    use super::{Body, Encoding, Segment, attachment, encode};
    use crate::{Error, ErrorKind, Source};

    /// An attachment being read.
    enum Open {
        File {
            file: File,
            /// Raw bytes still to come, by the recorded size.
            left: u64,
            /// Encoded bytes still to come, by the recorded length.
            encoded_left: u64,
            encoding: Encoding,
        },
        Memory {
            data: Bytes,
            at: usize,
            encoding: Encoding,
        },
    }

    struct State {
        segments: VecDeque<Segment>,
        open: Option<Open>,
        block: usize,
        buffer: Vec<u8>,
        failed: bool,
    }

    impl Body {
        /// The body as a stream of blocks. Files are opened and read only as the stream is
        /// polled. A file that cannot be read, or is no longer the size it was when the body was
        /// built, ends the stream with [`ErrorKind::Attachment`] rather than sending bytes that
        /// disagree with [`len`](Self::len).
        pub fn into_stream(self) -> impl Stream<Item = Result<Bytes, Error>> + Send + 'static {
            let state = State {
                segments: self.segments.into(),
                open: None,
                block: self.block,
                buffer: vec![0; self.block],
                failed: false,
            };
            stream::unfold(state, |mut state| async move {
                if state.failed {
                    return None;
                }
                match state.next().await {
                    Ok(Some(bytes)) => Some((Ok(bytes), state)),
                    Ok(None) => None,
                    Err(error) => {
                        state.failed = true;
                        Some((Err(error), state))
                    }
                }
            })
        }
    }

    impl State {
        async fn next(&mut self) -> Result<Option<Bytes>, Error> {
            loop {
                if let Some(open) = self.open.take() {
                    if let Some(bytes) = self.read(open).await? {
                        return Ok(Some(bytes));
                    }
                    continue;
                }
                match self.segments.pop_front() {
                    None => return Ok(None),
                    Some(Segment::Bytes(bytes)) => return Ok(Some(bytes)),
                    Some(Segment::Data {
                        source,
                        size,
                        encoded,
                        encoding,
                    }) => {
                        self.open = Some(match source {
                            Source::Bytes(data) => Open::Memory {
                                data,
                                at: 0,
                                encoding,
                            },
                            Source::Path(path) => Open::File {
                                file: File::open(&path).await.map_err(unreadable)?,
                                left: size,
                                encoded_left: encoded,
                                encoding,
                            },
                        });
                    }
                }
            }
        }

        /// The next encoded block of an attachment, or `None` when it is finished.
        async fn read(&mut self, open: Open) -> Result<Option<Bytes>, Error> {
            match open {
                Open::Memory { data, at, encoding } => {
                    if at >= data.len() {
                        return Ok(None);
                    }
                    let end = data.len().min(at + self.block);
                    let mut out = Vec::new();
                    encode(&data[at..end], encoding, &mut out);
                    self.open = Some(Open::Memory {
                        data,
                        at: end,
                        encoding,
                    });
                    Ok(Some(Bytes::from(out)))
                }
                Open::File {
                    mut file,
                    left,
                    encoded_left,
                    encoding,
                } => {
                    // A read may return less than asked for; only a full block, or the end of
                    // the file, keeps base64 free of padding in the middle.
                    let mut filled = 0;
                    while filled < self.block {
                        match file
                            .read(&mut self.buffer[filled..])
                            .await
                            .map_err(unreadable)?
                        {
                            0 => break,
                            n => filled += n,
                        }
                    }
                    let read = filled as u64;
                    let at_end = filled < self.block;
                    if read > left || (at_end && read != left) {
                        return Err(changed());
                    }
                    if filled == 0 {
                        return if encoded_left == 0 {
                            Ok(None)
                        } else {
                            Err(changed())
                        };
                    }

                    let mut out = Vec::new();
                    encode(&self.buffer[..filled], encoding, &mut out);
                    let left = left - read;
                    let encoded_left = encoded_left
                        .checked_sub(out.len() as u64)
                        .ok_or_else(changed)?;
                    if left == 0 {
                        // The recorded size is used up: the file must end here, and must have
                        // encoded to exactly the promised length.
                        let more = file.read(&mut self.buffer[..1]).await.map_err(unreadable)?;
                        if more != 0 || encoded_left != 0 {
                            return Err(changed());
                        }
                    } else {
                        self.open = Some(Open::File {
                            file,
                            left,
                            encoded_left,
                            encoding,
                        });
                    }
                    Ok(Some(Bytes::from(out)))
                }
            }
        }
    }

    fn unreadable(source: std::io::Error) -> Error {
        Error::new(ErrorKind::Attachment)
            .with_detail("an attachment could not be read")
            .with_source(source)
    }

    fn changed() -> Error {
        attachment("an attachment changed after the body was built")
    }
}
