//! Server-sent event framing, one byte at a time, with limits.

use crate::{Error, ErrorKind, Limits, Mode};

/// A complete event: its data, and whether the server sent it as an `error` event.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Frame {
    pub(super) data: Vec<u8>,
    pub(super) error: bool,
}

/// The type of the event being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Message,
    /// The server reports a failure; the data says which.
    Error,
    /// A type outside the protocol; lenient mode drops the event.
    Foreign,
}

/// Assembles the events of a byte stream: `message` events with their data, and `error` events.
#[derive(Debug)]
pub(super) struct Framer {
    mode: Mode,
    limits: Limits,
    wire: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    kind: Kind,
}

impl Framer {
    pub(super) fn new(mode: Mode, limits: Limits) -> Self {
        Self {
            mode,
            limits,
            wire: 0,
            line: Vec::new(),
            data: Vec::new(),
            kind: Kind::Message,
        }
    }

    /// Feeds one byte. Returns the event this byte ends, if it ends one.
    pub(super) fn byte(&mut self, byte: u8) -> Result<Option<Frame>, Error> {
        self.wire += 1;
        if self.wire > self.limits.wire_bytes {
            return Err(limit("the response is longer than the wire-byte limit"));
        }

        if byte != b'\n' {
            self.line.push(byte);
            if self.line.len() + self.data.len() > self.limits.event_bytes {
                return Err(event_too_long());
            }
            return Ok(None);
        }

        if self.line.last() == Some(&b'\r') {
            self.line.pop();
        }

        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            return Ok(self.dispatch());
        }

        self.field(&line)?;
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<Frame> {
        let data = std::mem::take(&mut self.data);
        let kind = std::mem::replace(&mut self.kind, Kind::Message);

        match kind {
            // An error needs no data to be one.
            Kind::Error => Some(Frame { data, error: true }),
            Kind::Message if !data.is_empty() => Some(Frame { data, error: false }),
            Kind::Message | Kind::Foreign => None,
        }
    }

    fn field(&mut self, line: &[u8]) -> Result<(), Error> {
        if line.starts_with(b":") {
            return Ok(());
        }

        let (name, value) = match line.iter().position(|b| *b == b':') {
            Some(at) => (&line[..at], &line[at + 1..]),
            None => (line, &[][..]),
        };

        let value = value.strip_prefix(b" ").unwrap_or(value);
        match name {
            b"data" => {
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
                if self.data.len() > self.limits.event_bytes {
                    return Err(event_too_long());
                }
            }
            b"id" | b"retry" => {}
            b"event" => match value {
                b"message" => {}
                b"error" => self.kind = Kind::Error,
                _ if self.mode == Mode::Lenient => self.kind = Kind::Foreign,
                _ => return Err(unsupported("a server-sent event type outside the protocol")),
            },
            _ if self.mode == Mode::Lenient => {}
            _ => {
                return Err(unsupported(
                    "a server-sent event field outside the protocol",
                ));
            }
        }
        Ok(())
    }
}

fn limit(detail: &'static str) -> Error {
    Error::new(ErrorKind::ResponseLimit).with_detail(detail)
}

fn event_too_long() -> Error {
    limit("an event is longer than the event-byte limit")
}

fn unsupported(detail: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported).with_detail(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(mode: Mode, wire: &str) -> Result<Vec<String>, Error> {
        let mut framer = Framer::new(mode, Limits::default());
        let mut out = Vec::new();
        for byte in wire.bytes() {
            if let Some(frame) = framer.byte(byte)? {
                assert!(!frame.error);
                out.push(String::from_utf8(frame.data).unwrap());
            }
        }
        Ok(out)
    }

    #[test]
    fn data_lines_join_and_other_fields_are_skipped() {
        let wire =
            ": ping\r\n\r\nid: 1\nretry: 5\nevent: message\ndata: a\ndata:b\n\ndata: [DONE]\n\n";
        assert_eq!(frames(Mode::Strict, wire).unwrap(), ["a\nb", "[DONE]"]);
    }

    #[test]
    fn foreign_events_fail_strict_and_are_dropped_lenient() {
        let wire = "event: ping\ndata: x\n\ndata: y\n\nweird\n\n";
        assert_eq!(
            frames(Mode::Strict, wire).unwrap_err().kind(),
            ErrorKind::Unsupported
        );
        assert_eq!(frames(Mode::Lenient, wire).unwrap(), ["y"]);
    }

    #[test]
    fn an_error_event_is_told_apart_with_or_without_data() {
        for mode in [Mode::Strict, Mode::Lenient] {
            let mut framer = Framer::new(mode, Limits::default());
            let mut frames = Vec::new();
            for byte in b"event: error\ndata: x\n\nevent: error\n\ndata: y\n\n" {
                frames.extend(framer.byte(*byte).unwrap());
            }

            let frame = |data: &str, error| Frame {
                data: data.into(),
                error,
            };
            assert_eq!(
                frames,
                [frame("x", true), frame("", true), frame("y", false)]
            );
        }
    }

    #[test]
    fn limits_count_every_byte() {
        let mut framer = Framer::new(Mode::Strict, Limits::default().wire_bytes(4));
        for byte in b"data" {
            assert!(framer.byte(*byte).unwrap().is_none());
        }
        assert_eq!(
            framer.byte(b':').unwrap_err().kind(),
            ErrorKind::ResponseLimit
        );
    }
}
