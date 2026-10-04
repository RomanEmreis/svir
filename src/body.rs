//! A request body whose length is known before its first byte.

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;

use crate::{Error, ErrorKind, Source};

pub(crate) mod escape;

#[cfg(feature = "client")]
mod stream;

pub use escape::escaped_len;
#[cfg(feature = "client")]
pub use stream::BodyStream;

/// How an attachment is written into the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    /// Base64, for the data URL of an image.
    Base64,
    /// Escaped for the inside of a JSON string, for a text file.
    JsonString,
}

/// A piece of the body: bytes that are JSON already, or an attachment encoded as it is sent.
/// The recorded size and length are checked only when a file is read, with `client`.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "client"), allow(dead_code))]
pub(crate) enum Segment {
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
    pub(crate) fn new(segments: Vec<Segment>, length: u64, block: usize) -> Self {
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

    /// The body as a stream of blocks. Files are opened and read only as the stream is polled.
    /// A file that cannot be read, or is no longer what it was when the body was built, ends the
    /// stream with [`ErrorKind::Attachment`] rather than sending bytes that disagree with
    /// [`len`](Self::len).
    #[cfg(feature = "client")]
    pub fn into_stream(self) -> BodyStream {
        BodyStream::new(self.segments, self.block)
    }
}

fn encode(block: &[u8], encoding: Encoding, out: &mut Vec<u8>) {
    match encoding {
        Encoding::Base64 => out.extend_from_slice(STANDARD.encode(block).as_bytes()),
        Encoding::JsonString => escape::escape_into(block, out),
    }
}

pub(crate) fn attachment(detail: &'static str) -> Error {
    Error::new(ErrorKind::Attachment).with_detail(detail)
}

pub(crate) fn not_utf8() -> Error {
    attachment("a text file is not UTF-8")
}

#[cfg(feature = "client")]
pub(crate) fn unreadable(source: std::io::Error) -> Error {
    attachment("an attachment could not be read").with_source(source)
}
