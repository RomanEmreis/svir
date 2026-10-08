//! What comes back: events while the answer streams, and the completion at the end.

use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::ToolCall;

/// Tokens per second is meaningless over a shorter window.
const MIN_RATE_WINDOW: Duration = Duration::from_millis(50);

/// One item of an answer stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// A piece of the answer, for display.
    Text(String),
    /// A piece of the model's reasoning, for display.
    Reasoning(Reasoning),
    /// A piece of a tool call, for display only: calls are executable once complete, in
    /// [`Completion::calls`].
    ToolCallDelta(ToolCallDelta),
    /// The whole answer. Always the last item.
    Completed(Completion),
}

/// A piece of a tool call as it streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolCallDelta {
    /// Which call this piece belongs to; pieces of different calls can interleave.
    pub index: usize,
    /// The call's ID, on the piece that carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A piece of the tool's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A piece of the arguments.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub arguments: String,
}

impl ToolCallDelta {
    /// A piece of the arguments of call `index`.
    pub fn new(index: usize, arguments: impl Into<String>) -> Self {
        Self {
            index,
            id: None,
            name: None,
            arguments: arguments.into(),
        }
    }
}

/// Reasoning, and where in the response it came from.
///
/// The source matters when reasoning is sent back: a server expects its own field back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Reasoning {
    /// Where it came from.
    pub source: ReasoningSource,
    /// The reasoning text.
    pub text: String,
}

impl Reasoning {
    /// Reasoning from `source`.
    pub fn new(source: ReasoningSource, text: impl Into<String>) -> Self {
        Self {
            source,
            text: text.into(),
        }
    }
}

/// Where reasoning came from in the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningSource {
    /// The `reasoning_content` field.
    ReasoningContent,
    /// The `reasoning` field.
    Reasoning,
    /// `<think>...</think>` inside the answer text.
    Think,
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    /// The answer is complete.
    Stop,
    /// The model called tools and waits for their results.
    ToolCalls,
    /// The output limit cut the answer off.
    Length,
    /// The server's content filter stopped the answer, or flagged it after streaming it; the
    /// text that was sent is kept.
    ///
    /// That text may hold what the filter flagged: an asynchronous filter vets it only after it
    /// was streamed, even after the model finished. A caller that shows it withdraws it.
    ContentFilter,
    /// The model refused to answer, and the text is its refusal, for the caller to show. It
    /// does not have the format the request asked for.
    Refusal,
}

/// Token counts, as the server reported them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Usage {
    /// Tokens in the request.
    pub input: u64,
    /// Tokens generated.
    pub output: u64,
    /// The total, when the server reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Tokens of reasoning within `output`, when the server reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
}

impl Usage {
    /// Input and output tokens.
    pub fn new(input: u64, output: u64) -> Self {
        Self {
            input,
            output,
            total: None,
            reasoning: None,
        }
    }
}

/// When the first and the last visible text or reasoning arrived, from the start of the response.
///
/// A delta that produced nothing visible, such as a lone `<think>` marker, does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Timing {
    /// The first visible token.
    #[serde(rename = "first_token_ms", with = "millis")]
    pub first_token: Duration,
    /// The last visible token.
    #[serde(rename = "last_token_ms", with = "millis")]
    pub last_token: Duration,
}

impl Timing {
    /// The first and last visible tokens.
    pub fn new(first_token: Duration, last_token: Duration) -> Self {
        Self {
            first_token,
            last_token,
        }
    }
}

/// A whole answer.
///
/// Turned into a [`Message`](crate::Message), or added with
/// [`Request::assistant`](crate::Request::assistant), it keeps everything the next request needs:
/// text, tool calls, and reasoning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Completion {
    /// Why the model stopped.
    pub finish: FinishReason,
    /// The answer.
    #[serde(default)]
    pub text: String,
    /// Reasoning, one entry per source, in order of first appearance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning: Vec<Reasoning>,
    /// Tool calls, complete and in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<ToolCall>,
    /// Token counts, when the server reported them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// When visible tokens arrived, when there were any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<Timing>,
}

impl Completion {
    /// An empty answer that stopped for `finish`.
    pub fn new(finish: FinishReason) -> Self {
        Self {
            finish,
            text: String::new(),
            reasoning: Vec::new(),
            calls: Vec::new(),
            usage: None,
            timing: None,
        }
    }

    /// Parses the answer's text as JSON into `T`, as asked for with
    /// [`Request::response_format`](crate::Request::response_format).
    ///
    /// Nothing else checks the answer against the format: `T` is what it is checked against. An
    /// answer the output limit cut off (`FinishReason::Length`) does not parse, nor a refusal
    /// (`FinishReason::Refusal`), nor an answer a server sent as reasoning with no text, which is
    /// read from the text alone.
    pub fn parse<T: DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_str(&self.text)
    }

    /// Output tokens per second, from the first visible token to the last.
    ///
    /// `None` without usage or timing, with one output token or fewer, or over a window shorter
    /// than 50 ms: there is no honest rate then.
    pub fn tokens_per_second(&self) -> Option<f64> {
        let usage = self.usage.as_ref()?;
        let timing = self.timing.as_ref()?;
        let window = timing.last_token.checked_sub(timing.first_token)?;
        (usage.output > 1 && window >= MIN_RATE_WINDOW)
            .then(|| usage.output as f64 / window.as_secs_f64())
    }
}

/// A `Duration` as whole milliseconds.
mod millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let millis = u64::try_from(value.as_millis()).unwrap_or(u64::MAX);
        serializer.serialize_u64(millis)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_millis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(output: u64, first_ms: u64, last_ms: u64) -> Completion {
        let mut done = Completion::new(FinishReason::Stop);
        done.usage = Some(Usage::new(10, output));
        done.timing = Some(Timing::new(
            Duration::from_millis(first_ms),
            Duration::from_millis(last_ms),
        ));
        done
    }

    #[test]
    fn the_rate_runs_from_the_first_visible_token_to_the_last() {
        let rate = timed(100, 2_000, 4_000).tokens_per_second().unwrap();
        assert!((rate - 50.0).abs() < 1e-9, "{rate}");
    }

    #[test]
    fn the_answer_parses_on_demand() {
        #[derive(Debug, PartialEq, Deserialize)]
        struct Weather {
            celsius: f64,
        }

        let mut done = Completion::new(FinishReason::Stop);
        // A server that separates reasoning may start the answer with line breaks (D31).
        done.text = "\n\n{\"celsius\": 12.5}".into();
        assert_eq!(done.parse::<Weather>().unwrap(), Weather { celsius: 12.5 });

        done.finish = FinishReason::Length;
        done.text = "{\"cel".into();
        assert!(done.parse::<Weather>().is_err());
    }

    #[test]
    fn there_is_no_honest_rate_for_one_token_or_a_short_window() {
        assert_eq!(timed(1, 0, 1_000).tokens_per_second(), None);
        assert_eq!(timed(12, 100, 149).tokens_per_second(), None);
        assert!(timed(12, 100, 150).tokens_per_second().is_some());
        assert_eq!(
            Completion::new(FinishReason::Stop).tokens_per_second(),
            None
        );
    }
}
