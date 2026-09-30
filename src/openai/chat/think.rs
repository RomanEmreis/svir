//! Splitting `<think>...</think>` out of streamed answer text.
//!
//! A marker can be cut anywhere by a delta boundary, so a tail that might start the next marker is
//! held back until the next delta decides it. At the end, a held tail that never became a marker
//! is plain text.

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// A piece of split text: `true` for reasoning, `false` for the answer.
pub(super) type Piece = (bool, String);

#[derive(Debug, Default)]
pub(super) struct ThinkSplitter {
    carry: String,
    inside: bool,
}

impl ThinkSplitter {
    /// Splits the next delta of answer text. Pieces come in order and are never empty.
    pub(super) fn push(&mut self, text: &str) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let mut buf = std::mem::take(&mut self.carry);
        buf.push_str(text);

        loop {
            let marker = if self.inside { CLOSE } else { OPEN };
            if let Some(at) = buf.find(marker) {
                self.emit(&buf[..at], &mut pieces);
                buf.drain(..at + marker.len());
                self.inside = !self.inside;
                continue;
            }

            let keep = partial_tail(&buf, marker);
            let head = buf.len() - keep;
            self.emit(&buf[..head], &mut pieces);
            self.carry = buf.split_off(head);

            return pieces;
        }
    }

    /// Writes out a held tail at the end of the stream.
    pub(super) fn flush(&mut self) -> Option<Piece> {
        let carry = std::mem::take(&mut self.carry);
        (!carry.is_empty()).then_some((self.inside, carry))
    }

    fn emit(&self, text: &str, pieces: &mut Vec<Piece>) {
        if !text.is_empty() {
            pieces.push((self.inside, text.to_owned()));
        }
    }
}

/// The length of the longest suffix of `text` that could begin `marker`, on a character boundary.
fn partial_tail(text: &str, marker: &str) -> usize {
    let max = marker.len().min(text.len());
    (1..max)
        .rev()
        .find(|k| text.is_char_boundary(text.len() - k) && text.ends_with(&marker[..*k]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(deltas: &[&str]) -> (String, String) {
        let mut splitter = ThinkSplitter::default();
        let mut pieces: Vec<Piece> = deltas.iter().flat_map(|d| splitter.push(d)).collect();
        pieces.extend(splitter.flush());
        let pick = |reasoning: bool| {
            pieces
                .iter()
                .filter(|(r, _)| *r == reasoning)
                .map(|(_, t)| t.as_str())
                .collect::<String>()
        };
        (pick(false), pick(true))
    }

    #[test]
    fn a_marker_cut_anywhere_is_still_a_marker() {
        let whole = "a<think>b</think>c";
        for at in 0..=whole.len() {
            let (head, tail) = whole.split_at(at);
            assert_eq!(
                split(&[head, tail]),
                ("ac".into(), "b".into()),
                "cut at {at}"
            );
        }
    }

    #[test]
    fn a_dangling_partial_marker_is_text() {
        assert_eq!(split(&["done<thi"]), ("done<thi".into(), String::new()));
        assert_eq!(
            split(&["<think>still thinking</thi"]),
            (String::new(), "still thinking</thi".into())
        );
    }

    #[test]
    fn holding_back_never_splits_a_character() {
        let (text, reasoning) = split(&["caf\u{e9}<", "think>\u{e9}t\u{e9}</think>!"]);
        assert_eq!(text, "caf\u{e9}!");
        assert_eq!(reasoning, "\u{e9}t\u{e9}");
    }
}
