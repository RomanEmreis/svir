# Roadmap

Order of work for the first release. Each step is reviewable on its own. Decisions are in
[decisions.md](decisions.md).

1. **Conformance suite first.** Recorded fixtures and loopback-server tests for every vector
   below, written before the code they test.
2. **Types and errors.** Messages with parts, tool descriptors, calls and results, usage, finish
   reasons, continuation, the error taxonomy, `Limits`, and the strictness policy.
3. **Decoder.** SSE framing, tool-call assembly and limits (strict), `<think>` splitting, live
   reasoning, and lenient mode.
4. **Encoder.** The streamed body with an exact length and attachments; tools, tool calls,
   tool results, and continuation sent back.
5. **Transport and compatibility.** HTTP, authentication, status mapping, timeouts,
   cancellation, optional-field learning, model listing.
6. **First consumer.** A chat backend moves over; its existing tests must pass unchanged. An agent
   engine follows when convenient.

## Conformance suite

Vectors run at several chunk strides (1, 3, and 8192 bytes) and with both LF and CRLF, unless the
vector is about one specific split.

### Framing

- An event split across two writes at an arbitrary byte.
- A multi-byte UTF-8 character split across chunks (`caf\u{e9}` at stride 1).
- Comments and keep-alives between events.
- Strict: an unknown SSE field, unparseable `data`, or invalid UTF-8 is an error.
- Lenient: the same inputs are skipped and the answer still arrives.
- Wire-byte and event-byte limits end the stream with `ResponseLimit`.

### Content and reasoning

- Content accumulated across chunks.
- `reasoning_content` and `reasoning` go to reasoning, kept as continuation, and are sent back
  unchanged on the next request.
- Inline `<think>why</think>because` gives reasoning `why` and answer `because`.
- A marker split across chunks (`a<thi` + `nk>b</think>c`) gives answer `ac`, reasoning `b`.
- A dangling partial marker at the end (`done<thi`) is text.
- A held-back tail next to multi-byte text never splits a character.
- A chunk that is only a marker produces no visible token.

### Tool calls

- Two interleaved calls with split names and split arguments assemble correctly (the example in
  [wire-protocol.md](wire-protocol.md#33-tool-calls)).
- `tool_call_id` and the assistant `tool_calls` round-trip into the next request.
- A stream cut before the finish reason, or before `[DONE]`, never yields calls.
- Rejected: duplicate call IDs, a call index at or over the limit, non-contiguous indices, a
  missing ID or name, `tool_calls` finish with no calls, calls with a `stop` finish.
- `length` finish is accepted with or without calls.

### Completion and usage

- Trailing usage chunk with empty `choices`.
- No usage at all: completion with usage absent.
- `reasoning_tokens` from `completion_tokens_details`.
- Tokens per second only with more than one token over at least 50 ms.
- Strict: usage twice, `id` or `model` changing, a second choice, a choice after the finish,
  anything after `[DONE]`, `[DONE]` with no finish reason, `content_filter`, an unknown delta key
  (`audio`): each is an error.
- An error chunk inside an open stream: strict fails; lenient surfaces the server's message and
  keeps the partial answer.

### Encoder

- A text-only conversation is plain string content.
- An image goes as a data URL, and the body length equals the promised length exactly.
- An image-only message carries no empty text part.
- A text file goes inside the message, named and escaped; files and images share one text part.
- A file larger than one block arrives whole, with multi-byte characters across block boundaries.
- Exact length at block boundaries: sizes 0, 1, 2, 3, block - 1, block, and block + 1.
- Escaping matches `serde_json` byte for byte, for any block split.
- A file whose size changed fails the stream instead of sending a wrong body.
- The lean body omits the optional fields and nothing else.
- Admission: the estimate equals the actual wire body; an oversized request fails before sending.

### Transport

- Anonymous and Bearer requests; the key never appears in errors or `Debug` output.
- 401 is `Authentication`, without waiting for a stalled error body.
- 400 with `context_length_exceeded` is `ContextOverflow`; 400 otherwise is `Unsupported`; none
  are retryable.
- 429 and 503 are retryable with `Retry-After` honored and capped; a truncated stream is
  retryable.
- A non-`text/event-stream` success is an error.
- Dropping the stream between buffered events closes the connection.
- A stalled stream times out as `Timeout`; cancelling a stalled read closes it.
- Base URL with and without `/v1` and trailing slashes resolves to the same endpoint.

### Compatibility

- A server that rejects optional fields: the first request retries lean and is remembered; the
  second goes lean on the first attempt; only the optional fields are dropped.
- An error unrelated to optional fields (404 `Model not loaded`) is reported as-is.
- When the lean retry also fails, the original error is reported.
- A request without optional fields is never retried.

### Live (opt-in)

- A bounded session against a local server with a tool-capable model: at least one tool call
  before the final answer; reported or estimated usage recorded. Never part of the default run.
