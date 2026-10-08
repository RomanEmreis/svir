//! The Chat Completions answer stream, decoded.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    time::{Duration, Instant},
};

use serde_json::{Map, Value};

use super::{
    overflow,
    sse::Framer,
    think::ThinkSplitter,
    wire::{
        CHOICES, CONTENT, DELTA, ERROR, FINISH_REASON, FUNCTION, FUNCTION_TYPE, ID, INDEX, MESSAGE,
        REASONING, REASONING_CONTENT, REFUSAL, ROLE, TOOL_CALLS, TYPE,
    },
};
use crate::{
    Completion, Error, ErrorKind, Event, FinishReason, Limits, Mode, Reasoning, ReasoningSource,
    Think, Timing, ToolCall, ToolCallDelta, Usage,
};

/// Reads a Chat Completions answer stream: bytes in, events out.
///
/// It does no I/O, so it can read a stream it owns or one it forwards elsewhere unchanged. Feed
/// bytes with [`push`](Self::push) in the order they arrive, then call [`finish`](Self::finish)
/// at the end of the input.
///
/// The outcome does not depend on how the bytes are chunked: events come in wire order, an error
/// comes after every event decoded before it, and [`Event::Completed`] is the last item. Bytes
/// after `[DONE]` are not read.
///
/// ```
/// use svir::openai::chat::Decoder;
/// use svir::Event;
///
/// let mut decoder = Decoder::strict();
/// let wire = concat!(
///     "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
///     "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
///     "data: [DONE]\n\n",
/// );
/// let events: Vec<Event> = decoder.push(wire.as_bytes()).into_iter().collect::<Result<_, _>>()?;
/// decoder.finish()?;
///
/// let Some(Event::Completed(done)) = events.last() else { unreachable!() };
/// assert_eq!(done.text, "Hi");
/// # Ok::<(), svir::Error>(())
/// ```
#[derive(Debug)]
pub struct Decoder {
    mode: Mode,
    limits: Limits,
    think: Option<ThinkSplitter>,
    framer: Framer,
    state: State,
    started: Instant,
    first_token: Option<Duration>,
    last_token: Option<Duration>,
    id: Option<String>,
    model: Option<String>,
    text: String,
    reasoning: Vec<Reasoning>,
    calls: BTreeMap<u64, PartialCall>,
    finish: Option<FinishReason>,
    /// An asynchronous content filter blocked something it had let through: the answer's finish
    /// is `ContentFilter`, whatever the model's own.
    filtered: bool,
    /// The answer came as content, which a refusal may not join.
    answered: bool,
    /// The answer came as a refusal: the text is the refusal, and the finish is `Refusal`.
    refused: bool,
    usage: Option<Usage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    Completed,
    Failed,
}

#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

impl Decoder {
    /// A decoder in `mode`, with the default limits, splitting `<think>` tags.
    pub fn new(mode: Mode) -> Self {
        let limits = Limits::default();
        Self {
            mode,
            limits,
            think: Some(ThinkSplitter::default()),
            framer: Framer::new(mode, limits),
            state: State::Open,
            started: Instant::now(),
            first_token: None,
            last_token: None,
            id: None,
            model: None,
            text: String::new(),
            reasoning: Vec::new(),
            calls: BTreeMap::new(),
            finish: None,
            filtered: false,
            answered: false,
            refused: false,
            usage: None,
        }
    }

    /// A decoder that treats anything unknown or inconsistent as an error.
    pub fn strict() -> Self {
        Self::new(Mode::Strict)
    }

    /// A decoder that skips unknown input and tolerates inconsistent metadata. It still enforces
    /// limits, truncation, and tool-call consistency.
    pub fn lenient() -> Self {
        Self::new(Mode::Lenient)
    }

    /// Sets the limits. Call before the first [`push`](Self::push).
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self.framer = Framer::new(self.mode, limits);
        self
    }

    /// Sets what to do with `<think>` tags in the answer text. Call before the first
    /// [`push`](Self::push).
    pub fn think(mut self, think: Think) -> Self {
        self.think = (think == Think::Split).then(ThinkSplitter::default);
        self
    }

    /// Whether the stream is over: completed, or failed.
    pub fn is_done(&self) -> bool {
        self.state != State::Open
    }

    /// Feeds the next bytes and returns what they complete, in order.
    ///
    /// At most one item is an error, and it is the last. After [`Event::Completed`] or an error,
    /// nothing more is returned.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Event, Error>> {
        let mut out = Vec::new();
        for &byte in bytes {
            if self.state != State::Open {
                break;
            }
            let step = match self.framer.byte(byte) {
                Ok(Some(frame)) if frame.error => Err(error_event(&frame.data)),
                Ok(Some(frame)) => self.event(&frame.data, &mut out),
                Ok(None) => Ok(()),
                Err(error) => Err(error),
            };
            if let Err(error) = step {
                self.state = State::Failed;
                out.push(Err(error));
            }
        }
        out
    }

    /// Ends the input. A stream that has not completed is
    /// [`ErrorKind::TruncatedStream`]; one that completed, or already failed through
    /// [`push`](Self::push), has nothing more to report.
    pub fn finish(self) -> Result<(), Error> {
        match self.state {
            State::Open => Err(truncated()),
            State::Completed | State::Failed => Ok(()),
        }
    }

    fn is_strict(&self) -> bool {
        self.mode == Mode::Strict
    }

    fn event(&mut self, data: &[u8], out: &mut Vec<Result<Event, Error>>) -> Result<(), Error> {
        let text: Cow<'_, str> = match std::str::from_utf8(data) {
            Ok(text) => Cow::Borrowed(text),
            Err(_) if !self.is_strict() => String::from_utf8_lossy(data),
            Err(_) => return Err(protocol("an event is not valid UTF-8")),
        };
        let text = text.trim();
        if text == "[DONE]" {
            return self.done(out);
        }
        let chunk = match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(chunk)) => chunk,
            _ if !self.is_strict() => return Ok(()),
            _ => return Err(protocol("an event is not a JSON object")),
        };

        // A chunk contributes all of its events or, on an error, none.
        let mut events = Vec::new();
        self.chunk(&chunk, &mut events)?;
        out.extend(events.into_iter().map(Ok));
        Ok(())
    }

    fn chunk(&mut self, chunk: &Map<String, Value>, events: &mut Vec<Event>) -> Result<(), Error> {
        if let Some(error) = present(chunk, ERROR) {
            return Err(server_error(error));
        }

        // Azure reports its prompt filter in a chunk of its own, with an empty id and model.
        if is_prompt_report(chunk) {
            return Ok(());
        }

        // Azure's asynchronous filter vets text it already streamed, in annotations. One carries
        // nothing of the answer, and its empty id and model are not the stream's.
        let annotation = annotation(chunk);
        if annotation.is_none() {
            self.metadata(chunk)?;
        }

        if let Some(usage) = present(chunk, "usage") {
            self.usage(usage)?;
        }

        let choices = match chunk.get(CHOICES) {
            Some(Value::Array(choices)) => choices,
            _ if self.is_strict() => return Err(protocol("a chunk has no choices array")),
            _ => return Ok(()),
        };

        let Some(choice) = choices.first() else {
            if self.is_strict() && (self.finish.is_none() || self.usage.is_none()) {
                return Err(protocol("an empty choices array before the finish reason"));
            }
            return Ok(());
        };

        if self.is_strict()
            && (choices.len() != 1 || choice.get(INDEX).and_then(Value::as_u64) != Some(0))
        {
            return Err(unsupported(
                "more than one choice, or a choice other than 0",
            ));
        }

        if let Some(annotation) = annotation {
            return self.annotation(annotation);
        }

        if self.finish.is_some() {
            return Err(unsupported("a choice after the finish reason"));
        }

        match choice {
            Value::Object(choice) => self.choice(choice, events),
            _ if self.is_strict() => Err(protocol("a choice is not an object")),
            _ => Ok(()),
        }
    }

    fn metadata(&mut self, chunk: &Map<String, Value>) -> Result<(), Error> {
        let strict = self.is_strict();
        for (key, slot) in [(ID, &mut self.id), ("model", &mut self.model)] {
            let Some(value) = chunk.get(key).and_then(Value::as_str) else {
                continue;
            };

            match slot {
                Some(seen) if seen != value && strict => {
                    return Err(protocol("the chunk id or model changed mid-stream"));
                }
                Some(_) => {}
                None => *slot = Some(value.to_owned()),
            }
        }
        Ok(())
    }

    fn usage(&mut self, usage: &Value) -> Result<(), Error> {
        let count = |key: &str| usage.get(key).and_then(Value::as_u64);
        let (Some(input), Some(output)) = (count("prompt_tokens"), count("completion_tokens"))
        else {
            return if self.is_strict() {
                Err(protocol("usage without prompt_tokens or completion_tokens"))
            } else {
                Ok(())
            };
        };

        if self.usage.is_some() && self.is_strict() {
            return Err(protocol("usage reported twice"));
        }

        let mut reported = Usage::new(input, output);
        reported.total = count("total_tokens");
        reported.reasoning = usage
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64);
        self.usage = Some(reported);

        Ok(())
    }

    fn choice(
        &mut self,
        choice: &Map<String, Value>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        match choice.get(DELTA) {
            Some(Value::Object(delta)) => self.delta(delta, events)?,
            None | Some(Value::Null) if !self.is_strict() => {}
            _ => return Err(protocol("a choice has no delta object")),
        }

        if let Some(reason) = present(choice, FINISH_REASON) {
            self.finish = Some(finish_reason(reason)?);
        }
        Ok(())
    }

    /// An annotation from an asynchronous content filter: its verdict on text already streamed.
    /// A block, by its finish reason or by a verdict marked filtered, is the answer's finish, even
    /// after the model's own (D35); anything else it says is not read.
    fn annotation(&mut self, choice: &Map<String, Value>) -> Result<(), Error> {
        if choice.get("content_filter_results").is_some_and(blocks) {
            self.filtered = true;
        }

        let Some(reason) = present(choice, FINISH_REASON) else {
            return Ok(());
        };

        match finish_reason(reason)? {
            FinishReason::ContentFilter => {
                self.finish = Some(FinishReason::ContentFilter);
                Ok(())
            }
            _ => Err(unsupported(
                "an annotation with a finish reason other than content_filter",
            )),
        }
    }

    fn delta(&mut self, delta: &Map<String, Value>, events: &mut Vec<Event>) -> Result<(), Error> {
        let strict = self.is_strict();
        // Validate the whole delta before any of it becomes an event.
        for (key, value) in delta {
            let known = match key.as_str() {
                _ if value.is_null() => true,
                ROLE => value.as_str() == Some("assistant"),
                CONTENT | REASONING_CONTENT | REASONING | REFUSAL => value.is_string(),
                TOOL_CALLS if !value.is_array() => {
                    return Err(protocol("tool_calls is not an array"));
                }
                TOOL_CALLS => true,
                _ => false,
            };
            if !known && strict {
                return Err(unsupported("a delta field outside the protocol"));
            }
        }

        // The answer is content or a refusal; a server that sends both contradicts itself.
        let string = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or_default();
        let (content, refusal) = (string(CONTENT), string(REFUSAL));
        let answered = self.answered || !content.is_empty();
        let refused = self.refused || !refusal.is_empty();
        if answered && refused {
            return Err(protocol("an answer that is both content and a refusal"));
        }
        
        (self.answered, self.refused) = (answered, refused);

        for (key, source) in [
            (REASONING_CONTENT, ReasoningSource::ReasoningContent),
            (REASONING, ReasoningSource::Reasoning),
        ] {
            if let Some(text) = delta.get(key).and_then(Value::as_str) {
                self.reasoning(source, text, events);
            }
        }

        if !content.is_empty() {
            self.content(content, events);
        }
        if !refusal.is_empty() {
            // The server's own text, with no <think> tags to split.
            self.text(refusal.to_owned(), events);
        }

        if let Some(Value::Array(pieces)) = delta.get(TOOL_CALLS) {
            for piece in pieces {
                self.tool_call(piece, events)?;
            }
        }

        Ok(())
    }

    fn content(&mut self, text: &str, events: &mut Vec<Event>) {
        let Some(splitter) = self.think.as_mut() else {
            return self.text(text.to_owned(), events);
        };

        for (reasoning, piece) in splitter.push(text) {
            if reasoning {
                self.reasoning(ReasoningSource::Think, &piece, events);
            } else {
                self.text(piece, events);
            }
        }
    }

    fn text(&mut self, text: String, events: &mut Vec<Event>) {
        if text.is_empty() {
            return;
        }

        self.visible();
        self.text.push_str(&text);
        events.push(Event::Text(text));
    }

    fn reasoning(&mut self, source: ReasoningSource, text: &str, events: &mut Vec<Event>) {
        if text.is_empty() {
            return;
        }

        self.visible();

        match self.reasoning.iter_mut().find(|r| r.source == source) {
            Some(kept) => kept.text.push_str(text),
            None => self.reasoning.push(Reasoning::new(source, text)),
        }

        events.push(Event::Reasoning(Reasoning::new(source, text)));
    }

    fn visible(&mut self) {
        let now = self.started.elapsed();
        self.first_token.get_or_insert(now);
        self.last_token = Some(now);
    }

    fn tool_call(&mut self, piece: &Value, events: &mut Vec<Event>) -> Result<(), Error> {
        let Value::Object(fields) = piece else {
            return Err(protocol("a tool call is not an object"));
        };

        if self.is_strict()
            && fields
                .keys()
                .any(|key| !matches!(key.as_str(), INDEX | ID | TYPE | FUNCTION))
        {
            return Err(outside_call());
        }

        let index = fields
            .get(INDEX)
            .and_then(Value::as_u64)
            .ok_or_else(|| protocol("a tool call has no index"))?;

        if index >= self.limits.tool_calls as u64 {
            return Err(Error::new(ErrorKind::ResponseLimit)
                .with_detail("more tool calls than the tool-call limit"));
        }
        if present(fields, TYPE).is_some_and(|kind| kind != FUNCTION_TYPE) {
            return Err(unsupported("a tool call that is not a function"));
        }

        let strict = self.is_strict();
        let call = self.calls.entry(index).or_default();
        let mut delta = ToolCallDelta::new(index as usize, "");
        if let Some(id) = present(fields, ID) {
            let id = id
                .as_str()
                .ok_or_else(|| protocol("a tool call ID is not a string"))?;

            if call.id.is_empty() {
                call.id = id.to_owned();
            } else if call.id != id {
                return Err(protocol("a tool call ID changed mid-stream"));
            }

            delta.id = Some(id.to_owned());
        }

        if let Some(function) = present(fields, FUNCTION) {
            let Value::Object(function) = function else {
                return Err(protocol("a tool call function is not an object"));
            };

            for (key, value) in function {
                if value.is_null() {
                    continue;
                }

                let text = || {
                    value
                        .as_str()
                        .ok_or_else(|| protocol("a tool call name or arguments is not a string"))
                };

                match key.as_str() {
                    "name" => {
                        let name = text()?;
                        call.name.push_str(name);
                        delta.name = Some(name.to_owned());
                    }
                    "arguments" => {
                        let arguments = text()?;
                        call.arguments.push_str(arguments);
                        delta.arguments.push_str(arguments);
                    }
                    _ if strict => return Err(outside_call()),
                    _ => {}
                }
            }
        }

        if delta.id.is_some() || delta.name.is_some() || !delta.arguments.is_empty() {
            events.push(Event::ToolCallDelta(delta));
        }

        Ok(())
    }

    fn done(&mut self, out: &mut Vec<Result<Event, Error>>) -> Result<(), Error> {
        let finish = self.finish.ok_or_else(|| {
            Error::new(ErrorKind::TruncatedStream).with_detail("[DONE] before a finish reason")
        })?;
        let finish = match finish {
            _ if self.filtered => FinishReason::ContentFilter,
            // A refusal is the answer however the model stopped, unless a filter stopped it (D35).
            FinishReason::Stop | FinishReason::Length if self.refused => FinishReason::Refusal,
            finish => finish,
        };

        let mut ids = HashSet::new();
        let mut calls = Vec::with_capacity(self.calls.len());
        for (expected, (index, call)) in (0u64..).zip(std::mem::take(&mut self.calls)) {
            if index != expected {
                return Err(protocol("tool call indices are not contiguous"));
            }
            if call.id.is_empty() || call.name.is_empty() {
                return Err(protocol("a tool call has no ID or no name"));
            }
            if !ids.insert(call.id.clone()) {
                return Err(protocol("two tool calls share an ID"));
            }

            calls.push(ToolCall::new(call.id, call.name, call.arguments));
        }

        if self.refused && !calls.is_empty() {
            return Err(protocol("a refusal with tool calls"));
        }
        if finish != FinishReason::Length && (finish == FinishReason::ToolCalls) == calls.is_empty()
        {
            return Err(protocol("the finish reason does not match the tool calls"));
        }

        let mut events = Vec::new();
        if let Some((reasoning, tail)) = self.think.as_mut().and_then(ThinkSplitter::flush) {
            if reasoning {
                self.reasoning(ReasoningSource::Think, &tail, &mut events);
            } else {
                self.text(tail, &mut events);
            }
        }

        let mut done = Completion::new(finish);
        done.text = std::mem::take(&mut self.text);
        done.reasoning = std::mem::take(&mut self.reasoning);
        done.calls = calls;
        done.usage = self.usage;
        done.timing = self
            .first_token
            .zip(self.last_token)
            .map(|(first, last)| Timing::new(first, last));
        events.push(Event::Completed(done));

        out.extend(events.into_iter().map(Ok));
        self.state = State::Completed;
        Ok(())
    }
}

/// A field that is present and not null.
fn present<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).filter(|value| !value.is_null())
}

/// Whether a chunk is a report on the prompt: no choices, and the prompt filter's results under
/// their current name or the one earlier API versions used.
fn is_prompt_report(chunk: &Map<String, Value>) -> bool {
    (chunk.contains_key("prompt_filter_results") || chunk.contains_key("prompt_annotations"))
        && matches!(chunk.get(CHOICES), Some(Value::Array(choices)) if choices.is_empty())
}

/// The choice of a chunk that annotates text already sent: one with the offsets of what it
/// vetted, and no delta.
fn annotation(chunk: &Map<String, Value>) -> Option<&Map<String, Value>> {
    let Some(Value::Array(choices)) = chunk.get(CHOICES) else {
        return None;
    };

    match choices.first() {
        Some(Value::Object(choice))
            if choice.contains_key("content_filter_offsets")
                && present(choice, DELTA).is_none() =>
        {
            Some(choice)
        }
        _ => None,
    }
}

/// Whether a content filter's results block anything: a category, or a blocklist, marked
/// filtered. Detected and not filtered is what a filter set to annotate only reports.
fn blocks(results: &Value) -> bool {
    let filtered = |verdict: &Value| verdict.get("filtered").and_then(Value::as_bool) == Some(true);

    let Value::Object(results) = results else {
        return false;
    };

    results.values().any(|verdict| match verdict {
        Value::Array(verdicts) => verdicts.iter().any(filtered),
        verdict => filtered(verdict),
    })
}

fn protocol(detail: &'static str) -> Error {
    Error::new(ErrorKind::Protocol).with_detail(detail)
}

fn unsupported(detail: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported).with_detail(detail)
}

fn outside_call() -> Error {
    unsupported("a tool call field outside the protocol")
}

/// The stream ended before the answer completed.
pub(crate) fn truncated() -> Error {
    Error::new(ErrorKind::TruncatedStream).with_detail("the stream ended before [DONE]")
}

/// A finish reason, which must be one the protocol has.
fn finish_reason(reason: &Value) -> Result<FinishReason, Error> {
    match reason.as_str() {
        Some("stop") => Ok(FinishReason::Stop),
        Some("tool_calls") => Ok(FinishReason::ToolCalls),
        Some("length") => Ok(FinishReason::Length),
        Some("content_filter") => Ok(FinishReason::ContentFilter),
        Some(_) => Err(unsupported("a finish reason outside the protocol")),
        None => Err(protocol("a finish reason is not a string")),
    }
}

/// An error the server reported inside an open stream: a failure of its own (D29), unless it
/// says that the request did not fit in the context (D30).
fn server_error(error: &Value) -> Error {
    let message = error
        .get(MESSAGE)
        .and_then(Value::as_str)
        .or_else(|| error.as_str());
    let reported = if overflow::is_overflow(error) {
        Error::new(ErrorKind::ContextOverflow).with_detail("reported inside the stream")
    } else {
        Error::new(ErrorKind::Server)
    };

    match message.filter(|message| !message.is_empty()) {
        Some(message) => reported.with_server_message(message),
        None => reported,
    }
}

/// An `error` event. Whatever its data, the stream has failed: the data is the server's error
/// object, or an object with the message, or the message itself.
fn error_event(data: &[u8]) -> Error {
    let text = String::from_utf8_lossy(data);
    let text = text.trim();

    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(body)) => match present(&body, ERROR) {
            Some(error) => server_error(error),
            None => server_error(&Value::Object(body)),
        },
        Ok(Value::String(message)) => server_error(&Value::String(message)),
        _ => server_error(&Value::String(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"<think>why</think>\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    fn completion(events: Vec<Result<Event, Error>>) -> Completion {
        match events.into_iter().last() {
            Some(Ok(Event::Completed(done))) => done,
            other => panic!("no completion: {other:?}"),
        }
    }

    #[test]
    fn timing_covers_visible_tokens_only() {
        let done = completion(Decoder::strict().push(HELLO.as_bytes()));
        let timing = done.timing.unwrap();
        assert!(timing.first_token <= timing.last_token);

        let bare = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(
            completion(Decoder::strict().push(bare.as_bytes())).timing,
            None
        );
    }

    #[test]
    fn think_can_be_kept_in_the_text() {
        let done = completion(Decoder::strict().think(Think::Keep).push(HELLO.as_bytes()));
        assert_eq!(done.text, "<think>why</think>hi");
        assert!(done.reasoning.is_empty());
    }

    #[test]
    fn nothing_is_returned_after_the_end() {
        let mut decoder = Decoder::strict();
        assert!(!decoder.push(HELLO.as_bytes()).is_empty());
        assert!(decoder.is_done());
        assert!(decoder.push(b"data: {}\n\n").is_empty());
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn the_server_message_of_an_in_stream_error_is_kept() {
        let wire = "data: {\"error\":{\"message\":\"engine died\"}}\n\n";
        let error = Decoder::strict()
            .push(wire.as_bytes())
            .pop()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Server);
        assert_eq!(error.server_message(), Some("engine died"));
    }

    #[test]
    fn an_error_event_fails_the_stream_in_either_mode() {
        let failure = |mode, wire: &str| {
            let mut decoder = Decoder::new(mode);
            let error = decoder.push(wire.as_bytes()).pop().unwrap().unwrap_err();

            assert!(decoder.is_done());
            assert!(decoder.finish().is_ok(), "the failure is reported once");
            (error.kind(), error.server_message().map(str::to_owned))
        };

        for mode in [Mode::Strict, Mode::Lenient] {
            for (wire, message) in [
                (
                    "event: error\ndata: {\"error\":{\"message\":\"too long\"}}\n\n",
                    Some("too long"),
                ),
                (
                    "event: error\ndata: {\"message\":\"too long\"}\n\n",
                    Some("too long"),
                ),
                ("event: error\ndata: too long\n\n", Some("too long")),
                ("event: error\n\n", None),
            ] {
                let expected = (ErrorKind::Server, message.map(str::to_owned));
                assert_eq!(failure(mode, wire), expected, "{mode:?} {wire:?}");
            }
        }
    }

    #[test]
    fn an_error_that_speaks_of_the_context_is_an_overflow() {
        let wire =
            "event: error\ndata: {\"error\":{\"message\":\"greater than the context length\"}}\n\n";
        let error = Decoder::lenient()
            .push(wire.as_bytes())
            .pop()
            .unwrap()
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::ContextOverflow);
        assert!(!error.is_retryable());
        assert_eq!(
            error.server_message(),
            Some("greater than the context length")
        );
    }

    #[test]
    fn an_unfinished_stream_is_truncated() {
        let mut decoder = Decoder::lenient();
        let _ = decoder.push(&HELLO.as_bytes()[..HELLO.len() / 2]);
        assert_eq!(
            decoder.finish().unwrap_err().kind(),
            ErrorKind::TruncatedStream
        );
    }
}
