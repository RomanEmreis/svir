//! The Messages answer stream, decoded.

use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::wire::{
    DATA, DELTA, INDEX, INPUT_TOKENS, OUTPUT_TOKENS, REDACTED_THINKING, SIGNATURE, TEXT, THINKING,
    TOOL_USE, USAGE,
};
use crate::{
    Completion, Error, ErrorKind, Event, FinishReason, Limits, Mode, Part, Reasoning,
    ReasoningSource, Think, Timing, ToolCall, ToolCallDelta, Usage,
    decode::{PartialCall, over_call_limit, settle},
    overflow::{error_event, server_error},
    sse::{self, Framer},
    think::ThinkSplitter,
    wire::{ERROR, TYPE},
};

/// Reads a Messages answer stream: bytes in, events out.
///
/// It does no I/O, so it can read a stream it owns or one it forwards elsewhere unchanged. Feed
/// bytes with [`push`](Self::push) in the order they arrive, then call [`finish`](Self::finish)
/// at the end of the input.
///
/// The outcome does not depend on how the bytes are chunked: events come in wire order, an error
/// comes after every event decoded before it, and [`Event::Completed`] is the last item. Bytes
/// after `message_stop` are not read. The completion keeps its blocks in the order they came, in
/// [`Completion::parts`], so that they go back to the server as they were.
///
/// ```
/// use svir::anthropic::messages::Decoder;
/// use svir::Event;
///
/// let mut decoder = Decoder::strict();
/// let wire = concat!(
///     "event: message_start\n",
///     "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
///     "event: content_block_start\n",
///     "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
///     "event: content_block_delta\n",
///     "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
///     "event: content_block_stop\n",
///     "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
///     "event: message_delta\n",
///     "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
///     "event: message_stop\n",
///     "data: {\"type\":\"message_stop\"}\n\n",
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
    /// `message_start` arrived.
    opened: bool,
    /// How many blocks have started: the index the next one has.
    blocks: u64,
    /// The block streaming now. Blocks come one after another.
    block: Option<Block>,
    text: String,
    reasoning: Vec<Reasoning>,
    calls: Vec<PartialCall>,
    parts: Vec<Part>,
    finish: Option<FinishReason>,
    input: Option<u64>,
    output: Option<u64>,
    thinking_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    Completed,
    Failed,
}

/// A block as it streams.
#[derive(Debug)]
struct Block {
    index: u64,
    /// How many parts the answer had when the block started: the parts after them are the
    /// block's.
    first_part: usize,
    kind: Kind,
}

/// What a delta shows of the answer, once the block has taken it.
enum Shown {
    Nothing,
    Text(String),
    Thinking(String),
    Arguments(String),
}

#[derive(Debug)]
enum Kind {
    Text,
    Thinking {
        text: String,
        signature: String,
    },
    Redacted {
        data: String,
    },
    Tool {
        call: PartialCall,
        /// The input the block started with, the arguments when no pieces come.
        input: String,
    },
    /// A block type outside the protocol, skipped in lenient mode.
    Skipped,
}

impl Decoder {
    /// A decoder in `mode`, with the default limits, splitting `<think>` tags in the text.
    pub fn new(mode: Mode) -> Self {
        let limits = Limits::default();
        Self {
            mode,
            limits,
            think: Some(ThinkSplitter::default()),
            framer: Framer::named(mode, limits),
            state: State::Open,
            started: Instant::now(),
            first_token: None,
            last_token: None,
            opened: false,
            blocks: 0,
            block: None,
            text: String::new(),
            reasoning: Vec::new(),
            calls: Vec::new(),
            parts: Vec::new(),
            finish: None,
            input: None,
            output: None,
            thinking_tokens: None,
        }
    }

    /// A decoder that treats anything unknown or inconsistent as an error, but for event types
    /// the API adds, which it skips (D55).
    pub fn strict() -> Self {
        Self::new(Mode::Strict)
    }

    /// A decoder that skips unknown input and tolerates inconsistent metadata. It still enforces
    /// limits, truncation, and the consistency of blocks and tool calls.
    pub fn lenient() -> Self {
        Self::new(Mode::Lenient)
    }

    /// Sets the limits. Call before the first [`push`](Self::push).
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self.framer = Framer::named(self.mode, limits);
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
        let text = sse::text(data, self.mode)?;
        let Some(event) = sse::object(&text, self.mode)? else {
            return Ok(());
        };

        // An event contributes all of its events or, on an error, none.
        let mut events = Vec::new();
        self.dispatch(&event, &mut events)?;
        out.extend(events.into_iter().map(Ok));
        Ok(())
    }

    fn dispatch(
        &mut self,
        event: &Map<String, Value>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        let Some(kind) = event.get(TYPE).and_then(Value::as_str) else {
            return if self.is_strict() {
                Err(protocol("an event has no type"))
            } else {
                Ok(())
            };
        };

        match kind {
            "message_start" => self.message_start(event),
            "content_block_start" => self.block_start(event, events),
            "content_block_delta" => self.block_delta(event, events),
            "content_block_stop" => self.block_stop(event, events),
            "message_delta" => self.message_delta(event),
            "message_stop" => self.done(events),
            ERROR => Err(match event.get(ERROR) {
                Some(error) => server_error(error),
                None => server_error(&Value::Object(event.clone())),
            }),
            // `ping`, and the event types the API adds (D55).
            _ => Ok(()),
        }
    }

    /// The answer has begun with `message_start`. Strict mode reads nothing of it before.
    fn require_start(&self) -> Result<(), Error> {
        if !self.opened && self.is_strict() {
            return Err(protocol("the answer before message_start"));
        }
        Ok(())
    }

    fn message_start(&mut self, event: &Map<String, Value>) -> Result<(), Error> {
        if self.opened {
            return if self.is_strict() {
                Err(protocol("a second message_start"))
            } else {
                Ok(())
            };
        }

        self.opened = true;
        if let Some(usage) = event.get("message").and_then(|message| message.get(USAGE)) {
            self.usage(usage);
        }
        Ok(())
    }

    fn block_start(
        &mut self,
        event: &Map<String, Value>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        self.require_start()?;
        let index = index(event)?;

        if self.block.is_some() {
            return Err(protocol("a block started before the one before it stopped"));
        }
        if index != self.blocks {
            return Err(protocol("a block index out of order"));
        }

        self.blocks += 1;

        let Some(Value::Object(start)) = event.get("content_block") else {
            return Err(protocol("a block start without its block"));
        };

        let string = |key: &str| start.get(key).and_then(Value::as_str).unwrap_or_default();

        let kind = match start.get(TYPE).and_then(Value::as_str) {
            Some(TEXT) => Kind::Text,
            Some(THINKING) => Kind::Thinking {
                text: string(THINKING).to_owned(),
                signature: string(SIGNATURE).to_owned(),
            },
            Some(REDACTED_THINKING) => match start.get(DATA).and_then(Value::as_str) {
                Some(data) => Kind::Redacted {
                    data: data.to_owned(),
                },
                None => return Err(protocol("a redacted thinking block without its data")),
            },
            Some(TOOL_USE) => {
                if self.calls.len() >= self.limits.tool_calls {
                    return Err(over_call_limit());
                }

                let call = PartialCall {
                    id: string("id").to_owned(),
                    name: string("name").to_owned(),
                    arguments: String::new(),
                };
                let mut delta = ToolCallDelta::new(self.calls.len(), "");
                delta.id = Some(call.id.clone()).filter(|id| !id.is_empty());
                delta.name = Some(call.name.clone()).filter(|name| !name.is_empty());
                if delta.id.is_some() || delta.name.is_some() {
                    events.push(Event::ToolCallDelta(delta));
                }

                let input = start.get("input").map(Value::to_string).unwrap_or_default();
                Kind::Tool { call, input }
            }
            _ if self.is_strict() => {
                return Err(unsupported("a content block type outside the protocol"));
            }
            _ => Kind::Skipped,
        };

        // A text block may start with some of its text.
        let text = matches!(kind, Kind::Text).then(|| string(TEXT).to_owned());
        self.block = Some(Block {
            index,
            first_part: self.parts.len(),
            kind,
        });
        if let Some(text) = text {
            self.text_piece(&text, events);
        }
        Ok(())
    }

    fn block_delta(
        &mut self,
        event: &Map<String, Value>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        self.require_start()?;
        let index = index(event)?;
        let strict = self.is_strict();
        let call = self.calls.len();
        let Some(Value::Object(delta)) = event.get(DELTA) else {
            return Err(protocol("a block delta without its delta"));
        };
        let piece = |key: &str| {
            delta
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| protocol("a delta without its text"))
        };
        let Some(block) = self.block.as_mut().filter(|block| block.index == index) else {
            return Err(protocol("a delta for a block that is not open"));
        };

        let kind = delta.get(TYPE).and_then(Value::as_str).unwrap_or_default();
        let shown = match (&mut block.kind, kind) {
            (Kind::Skipped, _) => Shown::Nothing,
            (Kind::Text, "text_delta") => Shown::Text(piece(TEXT)?.to_owned()),
            (Kind::Thinking { text, .. }, "thinking_delta") => {
                let piece = piece(THINKING)?;
                text.push_str(piece);
                Shown::Thinking(piece.to_owned())
            }
            (Kind::Thinking { signature, .. }, "signature_delta") => {
                signature.push_str(piece(SIGNATURE)?);
                Shown::Nothing
            }
            (Kind::Tool { call, .. }, "input_json_delta") => {
                let piece = piece("partial_json")?;
                call.arguments.push_str(piece);
                Shown::Arguments(piece.to_owned())
            }
            (_, "text_delta" | "thinking_delta" | "signature_delta" | "input_json_delta") => {
                return Err(protocol("a delta of the wrong type for its block"));
            }
            _ if strict => return Err(unsupported("a delta type outside the protocol")),
            _ => Shown::Nothing,
        };

        match shown {
            Shown::Text(text) => self.text_piece(&text, events),
            Shown::Thinking(piece) if !piece.is_empty() => {
                self.visible();
                events.push(Event::Reasoning(Reasoning::new(
                    ReasoningSource::Thinking,
                    piece,
                )));
            }
            Shown::Arguments(piece) if !piece.is_empty() => {
                events.push(Event::ToolCallDelta(ToolCallDelta::new(call, piece)));
            }
            _ => {}
        }
        Ok(())
    }

    fn block_stop(
        &mut self,
        event: &Map<String, Value>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        self.require_start()?;
        let index = index(event)?;
        match self.block.take() {
            Some(block) if block.index == index => {
                self.close(block, events);
                Ok(())
            }
            _ => Err(protocol("a stop for a block that is not open")),
        }
    }

    /// Ends a block: what it was goes into the answer.
    fn close(&mut self, block: Block, events: &mut Vec<Event>) {
        match block.kind {
            Kind::Text => {
                // A block boundary decides a marker held back at its end: it is text.
                if let Some((reasoning, tail)) = self.think.as_mut().and_then(ThinkSplitter::flush)
                {
                    self.split_piece(reasoning, tail, block.first_part, events);
                }
            }
            Kind::Thinking { text, signature } => {
                let mut reasoning = Reasoning::new(ReasoningSource::Thinking, text);
                reasoning.signature = Some(signature).filter(|signature| !signature.is_empty());
                self.reasoning.push(reasoning.clone());
                self.parts.push(Part::Reasoning(reasoning));
            }
            Kind::Redacted { data } => {
                let reasoning =
                    Reasoning::new(ReasoningSource::RedactedThinking, "").signature(data);
                self.reasoning.push(reasoning.clone());
                self.parts.push(Part::Reasoning(reasoning));
            }
            Kind::Tool { mut call, input } => {
                if call.arguments.is_empty() {
                    call.arguments = input;
                }
                self.parts.push(Part::ToolCall(ToolCall::new(
                    call.id.clone(),
                    call.name.clone(),
                    call.arguments.clone(),
                )));
                self.calls.push(call);
            }
            Kind::Skipped => {}
        }
    }

    fn message_delta(&mut self, event: &Map<String, Value>) -> Result<(), Error> {
        self.require_start()?;
        if let Some(reason) = event
            .get(DELTA)
            .and_then(|delta| delta.get("stop_reason"))
            .filter(|reason| !reason.is_null())
        {
            self.finish = Some(stop_reason(reason)?);
        }
        if let Some(usage) = event.get(USAGE) {
            self.usage(usage);
        }
        Ok(())
    }

    /// Reads counts the API reports as running totals: the input with what the cache read and
    /// wrote, the output, and the thinking in it.
    fn usage(&mut self, usage: &Value) {
        let count = |key: &str| usage.get(key).and_then(Value::as_u64);

        if let Some(input) = count(INPUT_TOKENS) {
            let cached = ["cache_creation_input_tokens", "cache_read_input_tokens"]
                .into_iter()
                .filter_map(count)
                .sum::<u64>();
            self.input = Some(input + cached);
        }
        if let Some(output) = count(OUTPUT_TOKENS) {
            self.output = Some(output);
        }
        if let Some(thinking) = usage
            .pointer("/output_tokens_details/thinking_tokens")
            .and_then(Value::as_u64)
        {
            self.thinking_tokens = Some(thinking);
        }
    }

    /// A piece of a text block's text, split for `<think>` tags.
    fn text_piece(&mut self, text: &str, events: &mut Vec<Event>) {
        let first = self
            .block
            .as_ref()
            .map_or(self.parts.len(), |block| block.first_part);
        let Some(splitter) = self.think.as_mut() else {
            return self.split_piece(false, text.to_owned(), first, events);
        };

        for (reasoning, piece) in splitter.push(text) {
            self.split_piece(reasoning, piece, first, events);
        }
    }

    /// A piece of text, or of reasoning split out of it, from the text block whose parts start at
    /// `first`: it joins that block's last part when that part is the same kind.
    fn split_piece(
        &mut self,
        reasoning: bool,
        piece: String,
        first: usize,
        events: &mut Vec<Event>,
    ) {
        if piece.is_empty() {
            return;
        }

        self.visible();
        let own = self.parts.len() > first;
        let last = self.parts.last_mut().filter(|_| own);

        if reasoning {
            match last {
                Some(Part::Reasoning(kept)) if kept.source == ReasoningSource::Think => {
                    kept.text.push_str(&piece);
                }
                _ => self.parts.push(Part::Reasoning(Reasoning::new(
                    ReasoningSource::Think,
                    piece.clone(),
                ))),
            }
            match self
                .reasoning
                .iter_mut()
                .find(|kept| kept.source == ReasoningSource::Think)
            {
                Some(kept) => kept.text.push_str(&piece),
                None => self
                    .reasoning
                    .push(Reasoning::new(ReasoningSource::Think, piece.clone())),
            }
            events.push(Event::Reasoning(Reasoning::new(
                ReasoningSource::Think,
                piece,
            )));
        } else {
            match last {
                Some(Part::Text(kept)) => kept.push_str(&piece),
                _ => self.parts.push(Part::Text(piece.clone())),
            }
            self.text.push_str(&piece);
            events.push(Event::Text(piece));
        }
    }

    fn visible(&mut self) {
        let now = self.started.elapsed();
        self.first_token.get_or_insert(now);
        self.last_token = Some(now);
    }

    fn done(&mut self, events: &mut Vec<Event>) -> Result<(), Error> {
        self.require_start()?;
        let finish = self.finish.ok_or_else(|| {
            Error::new(ErrorKind::TruncatedStream).with_detail("message_stop before a stop reason")
        })?;

        // The output limit may stop the answer inside a block, which never stops then.
        if let Some(block) = self.block.take() {
            if finish != FinishReason::Length {
                return Err(protocol("a block was still open at message_stop"));
            }
            self.close(block, events);
        }

        let refused = finish == FinishReason::Refusal;
        let (finish, calls, cut_calls) = settle(finish, refused, std::mem::take(&mut self.calls))?;
        if !cut_calls.is_empty() {
            // Calls the output limit cut off are not part of the answer that goes back (D47).
            self.parts.retain(|part| !matches!(part, Part::ToolCall(_)));
        }

        let mut done = Completion::new(finish);
        done.text = std::mem::take(&mut self.text);
        done.reasoning = std::mem::take(&mut self.reasoning);
        done.calls = calls;
        done.cut_calls = cut_calls;
        done.parts = std::mem::take(&mut self.parts);
        done.usage = self.input.zip(self.output).map(|(input, output)| {
            let mut usage = Usage::new(input, output);
            usage.reasoning = self.thinking_tokens;
            usage
        });
        done.timing = self
            .first_token
            .zip(self.last_token)
            .map(|(first, last)| Timing::new(first, last));
        events.push(Event::Completed(done));

        self.state = State::Completed;
        Ok(())
    }
}

/// The index of a block event.
fn index(event: &Map<String, Value>) -> Result<u64, Error> {
    event
        .get(INDEX)
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol("a block event without its index"))
}

/// A stop reason, as the finish it is (P19).
fn stop_reason(reason: &Value) -> Result<FinishReason, Error> {
    match reason.as_str() {
        Some("end_turn" | "stop_sequence") => Ok(FinishReason::Stop),
        Some("max_tokens" | "model_context_window_exceeded") => Ok(FinishReason::Length),
        Some("tool_use") => Ok(FinishReason::ToolCalls),
        Some("refusal") => Ok(FinishReason::Refusal),
        Some(_) => Err(unsupported("a stop reason outside the protocol")),
        None => Err(protocol("a stop reason is not a string")),
    }
}

fn protocol(detail: &'static str) -> Error {
    Error::new(ErrorKind::Protocol).with_detail(detail)
}

fn unsupported(detail: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported).with_detail(detail)
}

/// The stream ended before the answer completed.
fn truncated() -> Error {
    Error::new(ErrorKind::TruncatedStream).with_detail("the stream ended before message_stop")
}
