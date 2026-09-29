//! How a response is read: strictness, limits, and inline `<think>` tags.

use serde::{Deserialize, Serialize};

/// How strictly a response is read.
///
/// Lenient skips unknown input and tolerates inconsistent metadata. Neither mode relaxes limits,
/// truncation, or tool-call consistency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Mode {
    /// Anything unknown or inconsistent is an error.
    #[default]
    Strict,
    /// Unknown input is skipped and inconsistent metadata tolerated.
    Lenient,
}

/// What to do with `<think>...</think>` inside the answer text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Think {
    /// Route it to reasoning, with [`ReasoningSource::Think`](crate::ReasoningSource::Think).
    Split,
    /// Leave it in the text.
    Keep,
}

/// Bounds on one response. They apply in every mode; reaching one is
/// [`ErrorKind::ResponseLimit`](crate::ErrorKind::ResponseLimit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Limits {
    /// Bytes of the whole response. Default 4 MiB.
    pub wire_bytes: usize,
    /// Bytes of one server-sent event. Default 256 KiB.
    pub event_bytes: usize,
    /// Tool calls in one answer. Default 64.
    pub tool_calls: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            wire_bytes: 4 * 1024 * 1024,
            event_bytes: 256 * 1024,
            tool_calls: 64,
        }
    }
}

impl Limits {
    /// Sets the bytes allowed for the whole response.
    pub fn wire_bytes(mut self, bytes: usize) -> Self {
        self.wire_bytes = bytes;
        self
    }

    /// Sets the bytes allowed for one server-sent event.
    pub fn event_bytes(mut self, bytes: usize) -> Self {
        self.event_bytes = bytes;
        self
    }

    /// Sets the tool calls allowed in one answer.
    pub fn tool_calls(mut self, calls: usize) -> Self {
        self.tool_calls = calls;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_is_the_default_and_limits_match_the_documented_defaults() {
        assert_eq!(Mode::default(), Mode::Strict);
        let limits = Limits::default();
        assert_eq!(
            (limits.wire_bytes, limits.event_bytes, limits.tool_calls),
            (4_194_304, 262_144, 64)
        );
        assert_eq!(Limits::default().tool_calls(1).tool_calls, 1);
    }
}
