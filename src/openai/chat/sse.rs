//! Server-sent event framing, one byte at a time, with limits.

use crate::{Error, ErrorKind, Limits, Mode};

/// Assembles `data` payloads of `message` events from a byte stream.
#[derive(Debug)]
pub(super) struct Framer {
    mode: Mode,
    limits: Limits,
    wire: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    /// The current event has a type other than `message`; lenient mode drops it.
    foreign: bool,
}

impl Framer {
    pub(super) fn new(mode: Mode, limits: Limits) -> Self {
        Self {
            mode,
            limits,
            wire: 0,
            line: Vec::new(),
            data: Vec::new(),
            foreign: false,
        }
    }

    /// Feeds one byte. Returns the data of a complete event when this byte ends one.
    pub(super) fn byte(&mut self, byte: u8) -> Result<Option<Vec<u8>>, Error> {
        self.wire += 1;
        if self.wire > self.limits.wire_bytes {
            return Err(limit("the response is longer than the wire-byte limit"));
        }

        if byte != b'\n' {
            self.line.push(byte);
            if self.line.len() + self.data.len() > self.limits.event_bytes {
                return Err(limit("an event is longer than the event-byte limit"));
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

    fn dispatch(&mut self) -> Option<Vec<u8>> {
        let data = std::mem::take(&mut self.data);
        let foreign = std::mem::replace(&mut self.foreign, false);
        (!data.is_empty() && !foreign).then_some(data)
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
                    return Err(limit("an event is longer than the event-byte limit"));
                }
            }
            b"id" | b"retry" => {}
            b"event" if value == b"message" => {}
            _ if self.mode == Mode::Lenient => self.foreign |= name == b"event",
            _ => {
                return Err(Error::new(ErrorKind::Unsupported)
                    .with_detail("a server-sent event field outside the protocol"));
            }
        }
        Ok(())
    }
}

fn limit(detail: &'static str) -> Error {
    Error::new(ErrorKind::ResponseLimit).with_detail(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(mode: Mode, wire: &str) -> Result<Vec<String>, Error> {
        let mut framer = Framer::new(mode, Limits::default());
        let mut out = Vec::new();
        for byte in wire.bytes() {
            if let Some(data) = framer.byte(byte)? {
                out.push(String::from_utf8(data).unwrap());
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
