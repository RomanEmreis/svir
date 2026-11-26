//! What every encoder shares: a body put together from JSON bytes and attachments, attachments
//! measured before the body streams, and context admission.

use bytes::Bytes;
use serde::Serialize;

use crate::{
    Error, ErrorKind, Image, Request, Source, TextFile, ToolChoice,
    body::{
        Body, Encoding, Segment, attachment,
        escape::{escape_into, escaped_len},
        not_utf8,
    },
};

/// Attachments are read this many bytes at a time. A multiple of 3, so every base64 block but the
/// last encodes without padding and the blocks join into one valid string.
const BLOCK: usize = 48 * 1024;

/// The most one image counts for in admission by default: what the common vision encoders make
/// of one image at their default settings, at most. Hosted APIs charge far less.
const IMAGE_TOKENS: u64 = 16_384;

/// How attachments are read and requests admitted: the settings every encoder has.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Settings {
    block: usize,
    context: Option<u64>,
    image_tokens: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            block: BLOCK,
            context: None,
            image_tokens: IMAGE_TOKENS,
        }
    }
}

impl Settings {
    /// Reads attachments `bytes` at a time, rounded down to a multiple of 3 and at least 3.
    pub(crate) fn block_bytes(&mut self, bytes: usize) {
        self.block = (bytes / 3).max(1) * 3;
    }

    pub(crate) fn context_tokens(&mut self, tokens: u64) {
        self.context = Some(tokens);
    }

    pub(crate) fn image_tokens(&mut self, tokens: u64) {
        self.image_tokens = tokens;
    }

    #[cfg(feature = "client")]
    pub(crate) fn block(&self) -> usize {
        self.block
    }

    /// The body `out` holds, once the request it encodes is admitted.
    pub(crate) fn body(&self, out: Segments, max_tokens: Option<u64>) -> Result<Body, Error> {
        let (segments, length, images) = out.finish();
        self.admit(max_tokens, length, &images)?;

        Ok(Body::new(segments, length, self.block))
    }

    /// Checks that the request fits in the context size, when there is one (P14). `images` are
    /// the base64 lengths of the images in the body, each counted as no more than
    /// `image_tokens` (D49).
    fn admit(&self, max_tokens: Option<u64>, length: u64, images: &[u64]) -> Result<(), Error> {
        let Some(context) = self.context else {
            return Ok(());
        };

        let uncounted: u64 = images
            .iter()
            .map(|encoded| encoded.saturating_sub(self.image_tokens))
            .sum();
        let estimate = length - uncounted;
        let reserved = max_tokens.unwrap_or(0);

        if max_tokens == Some(0) || estimate.saturating_add(reserved) > context {
            return Err(Error::new(ErrorKind::ContextOverflow)
                .with_detail("the request and max_tokens do not fit in the context size"));
        }
        Ok(())
    }
}

/// Measures one attachment: its size, and what it encodes to. The third argument is the escaped
/// length a text file declares.
pub(crate) type Measure<'a> =
    dyn FnMut(&Source, Encoding, Option<u64>) -> Result<Measured, Error> + 'a;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Measured {
    size: u64,
    encoded: u64,
}

/// Measures an attachment held in memory. One held as a file path is an error: it needs the
/// encoder's `encode_files`.
pub(crate) fn in_memory(
    source: &Source,
    encoding: Encoding,
    declared: Option<u64>,
) -> Result<Measured, Error> {
    match source {
        Source::Bytes(data) => measure_bytes(data, encoding, declared),
        _ => Err(attachment(
            "a file attachment needs Encoder::encode_files (feature `client`)",
        )),
    }
}

/// Measures the attachments held as file paths, in the order the body uses them.
#[cfg(feature = "client")]
pub(crate) async fn measure_files<'a>(
    attachments: impl Iterator<Item = (&'a Source, Encoding, Option<u64>)>,
    block: usize,
) -> Result<std::collections::VecDeque<Measured>, Error> {
    let mut measured = std::collections::VecDeque::new();
    for (source, encoding, declared) in attachments {
        if let Source::Path(path) = source {
            measured.push_back(files::measure(path, encoding, declared, block).await?);
        }
    }

    Ok(measured)
}

/// Measures an attachment held in memory, or takes the next of the files measured before.
#[cfg(feature = "client")]
pub(crate) fn measured_before(
    measured: &mut std::collections::VecDeque<Measured>,
) -> impl FnMut(&Source, Encoding, Option<u64>) -> Result<Measured, Error> + '_ {
    move |source, encoding, declared| match source {
        Source::Bytes(data) => measure_bytes(data, encoding, declared),
        _ => measured
            .pop_front()
            .ok_or_else(|| attachment("an attachment was not measured")),
    }
}

fn measure_bytes(
    data: &Bytes,
    encoding: Encoding,
    declared: Option<u64>,
) -> Result<Measured, Error> {
    let size = data.len() as u64;
    let encoded = match encoding {
        Encoding::Base64 => base64_len(size),
        Encoding::JsonString => {
            if std::str::from_utf8(data).is_err() {
                return Err(not_utf8());
            }
            escaped_len(data)
        }
    };

    // Measured anyway, since it is in memory; a declared length must agree.
    if declared.is_some_and(|declared| declared != encoded) {
        return Err(not_declared());
    }

    Ok(Measured { size, encoded })
}

fn not_declared() -> Error {
    attachment("a text file is not the escaped length it declares")
}

/// How long the base64 of `size` bytes is, padding included.
pub(crate) fn base64_len(size: u64) -> u64 {
    size.div_ceil(3) * 4
}

/// An image's media type, which the body needs: one without is `Attachment`.
pub(crate) fn media_type(image: &Image) -> Result<&str, Error> {
    image
        .media_type
        .as_deref()
        .ok_or_else(|| attachment("an image has no media type"))
}

pub(crate) fn unsupported(detail: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported).with_detail(detail)
}

/// A part a user message cannot carry.
pub(crate) fn not_for_user() -> Error {
    unsupported("a user message carries text, files, and images")
}

/// A part a model message cannot carry.
pub(crate) fn not_for_model() -> Error {
    unsupported("a model message carries text, reasoning, and tool calls")
}

/// A part a tool message cannot carry.
pub(crate) fn not_for_tool() -> Error {
    unsupported("a tool message carries only tool results")
}

/// What a tool choice asks of the server, once checked against the tools the request offers.
pub(crate) enum Choice<'a> {
    /// No tool may be called.
    None,
    /// Some tool must be called.
    Required,
    /// This tool must be called.
    Tool(&'a str),
}

/// What the tool choice asks, or nothing for the server's default: the model decides, and with
/// no tools there is nothing to forbid. A call that cannot be made is `Unsupported` (D40).
pub(crate) fn tool_choice(request: &Request) -> Result<Option<Choice<'_>>, Error> {
    let offers = |name: &str| request.tools.iter().any(|tool| tool.name == name);

    match &request.tool_choice {
        ToolChoice::Auto => Ok(None),
        ToolChoice::None if request.tools.is_empty() => Ok(None),
        ToolChoice::None => Ok(Some(Choice::None)),
        ToolChoice::Required if request.tools.is_empty() => Err(unsupported(
            "a tool call is required, and the request offers no tools",
        )),
        ToolChoice::Required => Ok(Some(Choice::Required)),
        ToolChoice::Tool(name) if !offers(name) => Err(unsupported(
            "a call is required of a tool the request does not offer",
        )),
        ToolChoice::Tool(name) => Ok(Some(Choice::Tool(name))),
    }
}

/// The body as it is put together: literal bytes, run together until an attachment interrupts.
#[derive(Default)]
pub(crate) struct Segments {
    parts: Vec<Segment>,
    pending: Vec<u8>,
    length: u64,
    /// The base64 length of each image, for admission.
    images: Vec<u64>,
}

impl Segments {
    /// Bytes that are JSON already.
    pub(crate) fn raw(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// A value written as JSON.
    pub(crate) fn json<T: Serialize>(&mut self, value: &T) -> Result<(), Error> {
        serde_json::to_writer(&mut self.pending, value).map_err(|source| {
            Error::new(ErrorKind::Unsupported)
                .with_detail("the request cannot be written as JSON")
                .with_source(source)
        })
    }

    /// An object written as JSON and left open for more members: its closing brace is not
    /// written.
    pub(crate) fn open_object<T: Serialize>(&mut self, value: &T) -> Result<(), Error> {
        self.json(value)?;
        self.pending.pop();

        Ok(())
    }

    /// Text, escaped for the JSON string being written.
    pub(crate) fn escaped(&mut self, text: &[u8]) {
        escape_into(text, &mut self.pending);
    }

    /// Text as a whole JSON string.
    pub(crate) fn string(&mut self, text: &str) {
        self.raw(b"\"");
        self.escaped(text.as_bytes());
        self.raw(b"\"");
    }

    /// An attachment, encoded when the body is read.
    fn data(
        &mut self,
        source: &Source,
        encoding: Encoding,
        declared: Option<u64>,
        measure: &mut Measure<'_>,
    ) -> Result<u64, Error> {
        let measured = measure(source, encoding, declared)?;

        self.flush();
        self.length += measured.encoded;
        self.parts.push(Segment::Data {
            source: source.clone(),
            size: measured.size,
            encoded: measured.encoded,
            encoding,
        });

        Ok(measured.encoded)
    }

    /// An image, as base64 encoded when the body is read, inside the JSON string being written.
    pub(crate) fn image(
        &mut self,
        source: &Source,
        measure: &mut Measure<'_>,
    ) -> Result<(), Error> {
        let encoded = self.data(source, Encoding::Base64, None, measure)?;
        self.images.push(encoded);
        Ok(())
    }

    /// A text file inside the JSON string being written: its content wrapped in `<file>` tags
    /// with its name, `"` in the name written as `&quot;` (P12).
    pub(crate) fn file(&mut self, file: &TextFile, measure: &mut Measure<'_>) -> Result<(), Error> {
        let name = file.name.replace('"', "&quot;");

        self.escaped(format!("<file name=\"{name}\">\n").as_bytes());
        self.data(
            &file.source,
            Encoding::JsonString,
            file.escaped_len,
            measure,
        )?;
        self.escaped(b"\n</file>");

        Ok(())
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            self.length += self.pending.len() as u64;
            self.parts.push(Segment::Bytes(Bytes::from(std::mem::take(
                &mut self.pending,
            ))));
        }
    }

    /// The pieces, the length, and the base64 length of each image.
    fn finish(mut self) -> (Vec<Segment>, u64, Vec<u64>) {
        self.flush();
        (self.parts, self.length, self.images)
    }
}

#[cfg(feature = "client")]
mod files {
    use std::path::Path;

    use tokio::{fs::File, io::AsyncReadExt};

    use super::{Encoding, Measured, base64_len, not_declared};
    use crate::{
        Error,
        body::{
            escape::{Utf8Check, escaped_len},
            not_utf8, unreadable,
        },
    };

    /// Measures a file: an image by its size, a text file by reading it once, or by its size when
    /// it declares its escaped length. The body stream checks a declared length as it reads.
    pub(super) async fn measure(
        path: &Path,
        encoding: Encoding,
        declared: Option<u64>,
        block: usize,
    ) -> Result<Measured, Error> {
        match (encoding, declared) {
            (Encoding::Base64, _) => {
                let size = size_of(path).await?;
                Ok(Measured {
                    size,
                    encoded: base64_len(size),
                })
            }
            (Encoding::JsonString, Some(encoded)) => {
                // Every byte escapes to 1, 2, or 6 bytes; a length outside that is wrong already.
                let size = size_of(path).await?;
                if encoded < size || encoded > size.saturating_mul(6) {
                    return Err(not_declared());
                }
                Ok(Measured { size, encoded })
            }
            (Encoding::JsonString, None) => {
                let mut file = File::open(path).await.map_err(unreadable)?;
                let mut buffer = vec![0; block.max(8 * 1024)];
                let mut check = Utf8Check::default();
                let (mut size, mut encoded) = (0u64, 0u64);
                loop {
                    let read = file.read(&mut buffer).await.map_err(unreadable)?;
                    if read == 0 {
                        break;
                    }
                    if !check.push(&buffer[..read]) {
                        return Err(not_utf8());
                    }
                    size += read as u64;
                    encoded += escaped_len(&buffer[..read]);
                }
                if !check.finish() {
                    return Err(not_utf8());
                }
                Ok(Measured { size, encoded })
            }
        }
    }

    async fn size_of(path: &Path) -> Result<u64, Error> {
        Ok(tokio::fs::metadata(path).await.map_err(unreadable)?.len())
    }
}
