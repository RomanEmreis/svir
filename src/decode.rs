//! How a response is read: strictness, limits, and inline `<think>` tags.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{Error, ErrorKind, FinishReason, ToolCall, ToolCallDelta};

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
///
/// Servers without a reasoning parser put the model's reasoning there. Split by default, so it is
/// not shown as the answer. Reasoning split out of these tags is never sent back to the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Think {
    /// Route it to reasoning, with [`ReasoningSource::Think`](crate::ReasoningSource::Think).
    #[default]
    Split,
    /// Leave it in the text.
    Keep,
}

/// Bounds on one response. They apply in every mode; reaching one is
/// [`ErrorKind::ResponseLimit`](crate::ErrorKind::ResponseLimit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Limits {
    /// Bytes of the whole response, as they arrive: every event with its framing and metadata,
    /// a few hundred bytes a token. Default 64 MiB. The decoder keeps the answer, not these bytes.
    pub wire_bytes: usize,
    /// Bytes of one server-sent event. Default 256 KiB.
    pub event_bytes: usize,
    /// Tool calls in one answer. Default 64.
    pub tool_calls: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            wire_bytes: 64 * 1024 * 1024,
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

/// A tool call as it arrived: its ID, name, and arguments, each empty if it never came.
#[derive(Debug, Default)]
pub(crate) struct PartialCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

/// The calls of an answer that finished for `finish`, in order, checked: the finish they make,
/// the calls to run, and the calls the output limit cut off. `refused` says the answer is a
/// refusal, which calls may not join (D42).
pub(crate) fn settle(
    finish: FinishReason,
    refused: bool,
    partial: Vec<PartialCall>,
) -> Result<(FinishReason, Vec<ToolCall>, Vec<ToolCallDelta>), Error> {
    // The output limit may have cut any call short, even before its ID or name arrived: none
    // is executable, and each is kept as far as it arrived (D47).
    let cut = finish == FinishReason::Length;
    let mut ids = HashSet::new();
    let mut calls = Vec::with_capacity(partial.len());
    let mut cut_calls = Vec::new();
    for (index, call) in partial.into_iter().enumerate() {
        if !cut && (call.id.is_empty() || call.name.is_empty()) {
            return Err(protocol("a tool call has no ID or no name"));
        }
        if !call.id.is_empty() && !ids.insert(call.id.clone()) {
            return Err(protocol("two tool calls share an ID"));
        }

        if cut {
            let mut delta = ToolCallDelta::new(index, call.arguments);
            delta.id = Some(call.id).filter(|id| !id.is_empty());
            delta.name = Some(call.name).filter(|name| !name.is_empty());
            cut_calls.push(delta);
        } else {
            calls.push(ToolCall::new(call.id, call.name, call.arguments));
        }
    }

    if refused && !calls.is_empty() {
        return Err(protocol("a refusal with tool calls"));
    }

    // An answer that is calls finishes with them, whatever the server said, as OpenAI and vLLM
    // finish a call of a named function with stop (D45).
    let finish = match finish {
        FinishReason::Stop if !calls.is_empty() => FinishReason::ToolCalls,
        finish => finish,
    };
    if finish != FinishReason::Length && (finish == FinishReason::ToolCalls) == calls.is_empty() {
        return Err(protocol("the finish reason does not match the tool calls"));
    }

    Ok((finish, calls, cut_calls))
}

/// An answer with more tool calls than the limit allows.
pub(crate) fn over_call_limit() -> Error {
    Error::new(ErrorKind::ResponseLimit).with_detail("more tool calls than the tool-call limit")
}

fn protocol(detail: &'static str) -> Error {
    Error::new(ErrorKind::Protocol).with_detail(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_is_the_default_and_limits_match_the_documented_defaults() {
        assert_eq!(Mode::default(), Mode::Strict);
        assert_eq!(Think::default(), Think::Split);
        let limits = Limits::default();
        assert_eq!(
            (limits.wire_bytes, limits.event_bytes, limits.tool_calls),
            (67_108_864, 262_144, 64)
        );
        assert_eq!(Limits::default().tool_calls(1).tool_calls, 1);
    }
}
