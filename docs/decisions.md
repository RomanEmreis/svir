# Decisions

The design record. Each entry is **Accepted** (agreed; change it only deliberately and record the
change here), **Proposed** (the working default until someone objects), or **Open** (needs a
decision before the code that depends on it).

## Accepted

### D1. A wire-protocol SDK, not an agent framework

svir is the protocol between an application and a model server: types, encoder, decoder,
transport, and compatibility handling. It should be small, composable, and pleasant to use, fit
both a chat backend and an agent engine without either bending around it, and be useful to
anyone with a similar job. Agent building blocks may come later, on top, without changing this
layer.

### D2. A standalone repository

svir has consumers in more than one repository and none of them owns it. It lives on its own, is
versioned independently, and is published on crates.io. Consumers develop against a local
checkout through `[patch]` with a path. Consumers adopt it incrementally: existing
implementations are not thrown away; what is appropriate from them moves into svir, and each
consumer switches over when convenient, keeping its own tests green.

### D3. Strict decoding by default

Strictness is a decoder policy, not two decoders. Strict (unknown means error) is the default,
with generous limits. Lenient (unknown means skip) is an explicit opt-in, for example for a chat
UI that prefers a partial answer to an error. The byte, event, and tool-call limits are on in both
modes; only their values change. See [architecture.md](architecture.md#44-strict-and-lenient).

### D4. The core is a Stream of events

The decoder produces a `Stream` of `Result<Event, Error>`. A consumer that needs a pull interface
(scripted tests, replay) keeps its own trait and implements it over the stream. No such trait in
svir.

### D5. Tool calls are released only on completion

`ToolCallDelta` events exist for display. Executable calls appear only in `Completed`, after a
finish reason and `[DONE]`. A stream that ends earlier is `TruncatedStream`; partial arguments
are never executable.

### D6. No agent loop, for now

An engine that journals intent before effects and never repeats an effect with an unknown outcome
must not have to compete with a simpler loop in the library it depends on. A small optional loop
helper for simple cases may be reconsidered later (O9).

### D7. MCP stays outside

svir provides tool descriptors, calls, and results. An MCP bridge (for example over neva) lives in
the consumer or, later, behind an optional feature. MCP policy (allow lists, configuration,
credentials) is always the consumer's.

### D8. License: MIT OR Apache-2.0

Dual-licensed, the Rust ecosystem default. See `LICENSE-MIT` and `LICENSE-APACHE`.

### D9. First protocol: OpenAI-compatible Chat Completions streaming

It is what local servers (LM Studio, mlx-lm, llama.cpp, vLLM) and many hosted endpoints speak.
Other wire APIs are open (O6).

### D10. A streamed body with an exact length

The encoder produces a body stream plus its exact length before the first byte, sent as
`Content-Length`. Attachments are read from disk block by block. The same length serves as the
conservative context-admission estimate, so admission needs no body in memory.

### D11. Compatibility learning is part of the transport

The first 400/422 on a request carrying optional fields triggers one retry without them; success
marks the server as strict for all later requests through the same handle.

### D12. A typed error taxonomy

Errors are typed kinds with a `retryable` flag and an optional retry delay. See
[architecture.md](architecture.md#48-errors).

## Proposed

### P1. Edition 2024; MSRV no higher than 1.94

For a library meant for others, the lower the better, as long as nothing newer is needed.

### P2. The codec does no I/O; the HTTP transport is a feature

Encoder and decoder depend on no HTTP client, so they work with any transport and can decode a
stream that is being forwarded elsewhere. The built-in transport uses reqwest 0.13 behind a
feature.

### P3. svir does not retry generation requests

It classifies failures and reports the retry delay; the caller schedules retries, because only
the caller knows its budgets and whether a retry could duplicate an effect. The compatibility
retry (D11) is the one exception: nothing was generated.

### P4. Hardened transport defaults

No redirects, no automatic retries, no environment proxy discovery, connect timeout capped at 10
seconds, request timeout covering headers and body, `text/event-stream` required. Plain HTTP only
on loopback (`127.0.0.1`, `localhost`, `[::1]`) unless the caller opts in; a LAN model server is a
legitimate reason. A caller can supply its own client.

The base URL is accepted with or without `/v1` and trailing slashes, since both habits are common.
A base URL with credentials, a query, or a fragment is refused.

### P5. Reasoning is both live and kept

Every reasoning carrier is emitted as a live `Reasoning` event tagged with its source, and also
kept in the completion. Sending it back is decided per request by the encoder.

### P6. `ToolCallDelta` carries the call index

So a UI can attribute interleaved argument fragments to the right call.

### P7. Timing in the completion

The completion records when the first and last visible tokens arrived, so a caller can compute
tokens per second without its own clock. A `<think>` marker alone is not a visible token.

### P8. What lenient relaxes

Lenient skips unknown input (SSE fields, unparseable `data`, unknown delta keys, extra choices,
empty `choices` chunks) and tolerates inconsistent metadata (`id` or `model` changes, usage
reported twice or without its required fields). It never relaxes answer integrity: limits,
truncation, and tool-call consistency are enforced in both modes. A chat UI can still show the
partial text that arrived before such an error. Anomalies that change the answer itself are O12.

### P9. The outcome does not depend on chunking

Events are delivered in wire order, and an error after every event decoded before it, even within
one chunk. `Completed` at `[DONE]` is final: bytes after it are not read. Without this, the same
bytes can complete when read one at a time and fail when read at once, or lose the text before an
error.

### P10. A tool-call ID may repeat, but not change

Repeating a call's `id` on later pieces of the same call is consistent and loses nothing, so it
is accepted rather than risking a rejected answer from a server that does it. An `id` that differs
from the one already seen for that index is `Protocol`.

### P11. The body carries what the request sets, and nothing implied

`model`, `messages`, and `stream: true` always; everything else (`max_tokens`, `temperature`,
`reasoning_effort`, `stream_options`, `tool_choice`, `n`) only when the request sets it. Server
defaults already cover what is left out, and every extra field is one more thing a strict server
can reject.

### P12. Message layout

- Content is a plain string unless the message has an image.
- Text and text files are joined into one text in the order given, separated by a blank line;
  each file is wrapped in `<file name="...">` tags with `"` in the name written as `&quot;`.
- With images, content is an array: that one text part (if there is any text), then the images
  as data URLs, in the order given.
- A tool result's content is the caller's string. svir does not wrap or serialize outcomes.
- Reasoning goes back only when the request asks for it, under the key it arrived with.

### P13. Attachment failures are their own error kind

An attachment that cannot be read, or no longer has its recorded size, fails the body stream with
`Attachment`, which is not retryable. It is a local failure, not something the server did.

### P14. Admission

A request is admitted when `max_tokens` is above 0 and the body length plus `max_tokens` fits in
the configured context size; otherwise it fails with `ContextOverflow` before a byte is sent.

## Open

- **O1. API names and DX.** Module layout, builder style, one crate or a small family
  (codec, transport), feature names.
- **O2. `<think>` splitting.** On by default or opt-in? Is reasoning split out of `<think>` sent
  back, and if so, as tags in `content` or not at all?
- **O3. Compatibility retry versus context overflow.** Today any 400/422 on a request with optional
  fields is retried once without them, including a context overflow, which cannot succeed. Skip the
  retry when `error.code` identifies an overflow? Servers that report it only in text remain.
- **O4. Server error messages.** A chat UI wants the server's own words (`Model not loaded`); an
  agent journal must not store raw server bodies. Carry the message in the error behind an explicit
  accessor, excluded from `Debug` and `Display`?
- **O5. Admission for images.** Counting base64 bytes as tokens is conservative for text but
  overestimates images by orders of magnitude.
- **O6. Other wire APIs.** OpenAI Responses, Anthropic Messages, and others, and how the neutral
  types stay neutral when they arrive.
- **O7. `Retry-After` as an HTTP date.** Only numeric seconds are handled today.
- **O8. Tool arguments.** Raw string only, or also a parsed JSON value with validation against the
  descriptor's schema?
- **O9. An optional loop helper.** See D6.
- **O10. Model listing.** Is the non-chat model filter part of svir or of the application?
- **O11. Assistant `content` with tool calls.** An empty string or `null`? Servers differ.
- **O12. Answer-changing anomalies in lenient mode.** An unknown finish reason (`content_filter`)
  or content after the finish reason: complete with what was received, or fail?
- **O13. The kind of an error inside an open stream.** It is a server-side failure, possibly
  transient, not a malformed stream. Today it maps to `Protocol`, which is not retryable. A
  separate kind, and is it retryable?
