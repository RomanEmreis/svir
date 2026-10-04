//! Text as it goes into a JSON string, a block at a time.
//!
//! Escaping works byte by byte: the only bytes JSON needs escaped are ASCII, and every byte of a
//! multi-byte UTF-8 character is 0x80 or above and passes through. So it works on any slice of a
//! file, even one that ends inside a character, and the escaped length of a file can be measured
//! before the file is sent.

/// How many bytes `bytes` take once escaped into a JSON string, as svir escapes them: the length
/// [`TextFile::escaped_len`](crate::TextFile::escaped_len) declares.
///
/// Escaping works byte by byte, so the length of a file is the sum of the lengths of the blocks
/// it is read in, whatever they are, even a block that ends inside a character. It does not
/// check that the bytes are UTF-8.
///
/// ```
/// use svir::body::escaped_len;
///
/// let text = "say \"hi\"\n".as_bytes();
/// let (head, tail) = text.split_at(5);
///
/// assert_eq!(escaped_len(text), 12);
/// assert_eq!(escaped_len(head) + escaped_len(tail), 12);
/// ```
pub fn escaped_len(bytes: &[u8]) -> u64 {
    bytes.iter().map(|&byte| escaped_byte_len(byte)).sum()
}

fn escaped_byte_len(byte: u8) -> u64 {
    match byte {
        b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 2,
        0x00..=0x1f => 6,
        _ => 1,
    }
}

/// Appends `bytes`, escaped for the inside of a JSON string, exactly as `serde_json` escapes.
pub(crate) fn escape_into(bytes: &[u8], out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    for &byte in bytes {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            0x00..=0x1f => {
                out.extend_from_slice(b"\\u00");
                out.push(HEX[usize::from(byte >> 4)]);
                out.push(HEX[usize::from(byte & 0xf)]);
            }
            _ => out.push(byte),
        }
    }
}

/// Checks that a stream of blocks is UTF-8, holding only a character split between two blocks.
#[derive(Debug, Default)]
#[cfg_attr(not(feature = "client"), allow(dead_code))]
pub(crate) struct Utf8Check {
    carry: Vec<u8>,
}

#[cfg_attr(not(feature = "client"), allow(dead_code))]
impl Utf8Check {
    /// Feeds the next block. `false` means the bytes so far are not UTF-8.
    pub(crate) fn push(&mut self, block: &[u8]) -> bool {
        let joined;
        let bytes = if self.carry.is_empty() {
            block
        } else {
            self.carry.extend_from_slice(block);
            joined = std::mem::take(&mut self.carry);
            &joined
        };
        match std::str::from_utf8(bytes) {
            Ok(_) => true,
            Err(error) if error.error_len().is_none() => {
                self.carry = bytes[error.valid_up_to()..].to_vec();
                true
            }
            Err(_) => false,
        }
    }

    /// Whether the stream ended on a whole character.
    pub(crate) fn finish(&self) -> bool {
        self.carry.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        "say \"hi\"\tC:\\temp\r\n\u{1}\u{8}\u{c}\u{1f}caf\u{e9} \u{20ac} \u{1f980}";

    #[test]
    fn escapes_exactly_as_serde_json_does() {
        let mut out = vec![b'"'];
        escape_into(SAMPLE.as_bytes(), &mut out);
        out.push(b'"');
        assert_eq!(out, serde_json::to_vec(SAMPLE).unwrap());
        assert_eq!(escaped_len(SAMPLE.as_bytes()) + 2, out.len() as u64);
    }

    #[test]
    fn escapes_the_same_whatever_the_blocks_are() {
        let mut whole = Vec::new();
        escape_into(SAMPLE.as_bytes(), &mut whole);
        for size in 1..8 {
            let mut pieces = Vec::new();
            for block in SAMPLE.as_bytes().chunks(size) {
                escape_into(block, &mut pieces);
            }
            assert_eq!(pieces, whole, "block size {size}");
        }
    }

    #[test]
    fn utf8_is_checked_across_blocks() {
        for size in 1..8 {
            let mut check = Utf8Check::default();
            assert!(
                SAMPLE
                    .as_bytes()
                    .chunks(size)
                    .all(|block| check.push(block))
            );
            assert!(check.finish(), "block size {size}");
        }

        let mut check = Utf8Check::default();
        assert!(!check.push(b"ab\xffcd"));

        let mut cut = Utf8Check::default();
        assert!(cut.push(&"\u{20ac}".as_bytes()[..2]));
        assert!(!cut.finish());
    }
}
