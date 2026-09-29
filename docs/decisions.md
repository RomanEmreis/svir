# Decisions

The design record. Each entry is **Accepted** (agreed; change it only deliberately and record the
change here), **Proposed** (the working default until someone objects), or **Open** (needs a
decision before the code that depends on it).

## Accepted

### D1. A protocol core with opt-in building blocks, not an agent framework

svir is the protocol between an application and a model server: types, encoder, decoder,
transport, and compatibility handling. On top of that core sit opt-in building blocks: layers
(D15) and a tool router (D18). The core does not depend on them. There is no agent loop and no
session state (D6).

svir should be small, composable, and pleasant to use, fit both a chat backend and an agent
engine without either bending around it, and be useful to anyone with a similar job.

*Revised 2026-09-29: originally "a wire-protocol SDK" only; layers and the tool router were added
with the API design (D13-D19).*

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
must not have to compete with a simpler loop in the library it depends on. Feeding tool results
back is a few lines of caller code (see [architecture.md](architecture.md#51-four-levels)). A
small optional loop helper may be reconsidered later (O9).

### D7. MCP stays outside

svir provides tool descriptors, calls, and results, and the tool router (D18). An MCP bridge (for
example neva tools into the router) lives in the consumer or, later, behind an optional feature.
MCP policy (allow lists, configuration, credentials) is always the consumer's.

### D8. License: MIT OR Apache-2.0

Dual-licensed, the Rust ecosystem default. See `LICENSE-MIT` and `LICENSE-APACHE`.

### D9. First protocol: OpenAI-compatible Chat Completions streaming

It is what local servers (LM Studio, mlx-lm, llama.cpp, vLLM) and many hosted endpoints speak.
Other wire APIs come through their own client constructors (D13); which ones is O6.

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

### D13. The API

Resolves O1. Shown whole in [architecture.md](architecture.md#5-api-and-composition).

**Client.** One `Client` type, not generic over the wire API, so the API can be chosen from
configuration at run time and a client stored without a type parameter. A constructor per wire
API returns a builder:

- `Client::openai(url)` for OpenAI-compatible servers. The URL is required: compatible servers
  live anywhere.
- `Client::anthropic()` and others later (O6): the hosted API by default, `.base_url()` to
  override.

`.build()?` validates the URL (P4), resolves the key (D17), and assembles the layers (D15), once.
Builder methods have plain names (`api_key`, `layer`, `wrap`); `with()` is reserved for adding
parts to a message.

**Calls.** `client.complete(request)` returns one `Completion`; it collects a single answer and is
not a loop. `client.stream(request)` returns an `EventStream`: a `Stream<Item = Result<Event,
Error>>` that is `Send + 'static`, so it can be moved into a spawned task. `client.list_models()`
lists models. Calls accept a `Request` or a `&Request`.

**Requests.** `Request::new(model)`, then:

- `.system(text)`: the system prompt is a field of the request, not a message; each adapter puts
  it where its API expects it (first message, or a top-level field);
- `.user(text)` as a shortcut, `.message(Message)` in general;
- `.assistant(completion)` adds a model answer with its text, tool calls, and reasoning, so
  nothing needed for the next turn can be lost; `.tool_result(id, content)` and
  `.tool_results(results)` answer calls;
- `.tools(&tools)` or `.tool(Tool)`;
- `.reasoning(Effort)`, `.max_tokens(n)`, `.temperature(t)`, `.send_reasoning(bool)`.

`Effort` is `Off`, `Low`, `Medium`, `High`, or `XHigh`. Each adapter maps it explicitly: Chat
Completions sends `reasoning_effort` (`Off` as `none`); an API that takes a thinking budget uses a
documented, tested table.

**Messages.** `Message::user(text).with(part)`, parts kept in the order given (P12). Attachments do
no I/O when created: `Image::path(p)` and `TextFile::path(p)` record the path, and the size is
measured when the body is built and verified while it streams (P13). The media type comes from the
extension or `.media_type(..)`. In memory: `Image::bytes(buf, media_type)`,
`TextFile::text(name, text)`. An empty text next to images is left out.

**Advanced use.** `svir::openai::chat::{Encoder, Decoder, Body}` for a custom transport or a proxy;
`Decoder::strict()` and `Decoder::lenient()` with `.limits(..)` and `.think(..)`.

Everyday imports come from `svir::prelude`.

### D14. One crate, plus macros

`svir` is one crate with features; `svir-macros` holds the procedural macros and is re-exported
under the `macros` feature.

| Feature | Adds |
| --- | --- |
| none | Types and the codec; attachments in memory only |
| `client` (default) | `Client`, `EventStream`, the reqwest 0.13 and tokio transport, attachments from disk |
| `macros` | `#[tool]` |
| `schemars` | Tool input schemas derived from types |
| `tracing` | The `Trace` layer |
| `testing` | `MockServer` (the conformance suite's scripted loopback server) and scripted event streams |

A crate family is for later, when a second wire API or a heavy optional part justifies it.

### D15. Layers and middleware

A client is a stack of layers around the adapter. A layer wraps the provider-neutral call,
`Request -> Result<EventStream, Error>`, not HTTP, so the same layer works for every wire API.
Status mapping and compatibility learning sit below the layers, in the adapter; HTTP-level
settings go through a caller-supplied reqwest client.

- `.layer(L)` adds a reusable layer; `.wrap(|req, next| async move { ... next.run(req).await })`
  adds a closure, in the style of volga's middleware.
- The first layer added is the outermost.
- A layer may wrap the returned stream as well as the call: first-token and idle timeouts, usage
  metrics, and logging of whole answers need the events, not just the response headers.
- svir's own trait, friendly to closures and `async`; a tower adapter can come later as a feature.
- Built in: `Retry` (D16), `Timeout::first_token`, `Timeout::idle`, `Timeout::total`, and `Trace`
  (feature `tracing`). A `Fallback` to another client is a natural later addition.

### D16. Retries are opt-in, and only before the first event

Supersedes P3. svir does not retry by default: an engine that accounts for retries in its own
budget and journal must not have them happen underneath it. The `Retry` layer adds them:

- `Retry::connect(n)` retries connection failures, where the request never reached the server;
- `Retry::transient(n)` retries retryable kinds, honoring `Retry-After`, with backoff.

Neither retries once an event has been delivered, or the caller would see the same text twice.
The compatibility retry (D11) stays in the adapter: nothing was generated.

### D17. API keys

`.api_key(key)`, `.api_key_env(name)` (read at `build()`, with a clear error when unset), or
`.api_key_file(path)` (size-bounded, trimmed). The key is held as a `Secret`: redacted in `Debug`,
sent as a sensitive header, never in errors or events. svir never reads environment variables or
`.env` files implicitly; loading `.env` (for example with dotenvy) belongs to the application.

### D18. A tool router, with explicit registration

`Tools` holds tool descriptors and their handlers. It is built explicitly,
`Tools::new().add(lookup)`, never from a global registry: different requests and agents use
different tool sets, unlike a server with one tool set per process.

- `tools.call(&call)` validates the arguments against the tool's schema and runs one handler.
  Failures become tool results for the model, not errors of the request.
- `#[tool(descr = "...")]` (feature `macros`) turns a function into a tool, its input schema
  derived through schemars, as in neva.
- `call.parse::<T>()` parses the raw arguments on demand. Resolves O8.
- The router answers calls; feeding the answers back is the caller's (D6).

### D19. The server's own message behind an accessor

Resolves O4. `error.server_message()` returns the server's text (bounded), so a chat UI can show
"Model not loaded". It is excluded from `Debug` and `Display`, so it does not reach logs or
journals by accident.

### D20. Raw passthrough for proxies

`client.send(request)` returns the server's bytes unchanged, after status mapping and compatibility
handling; a `Decoder` can read them on the way past. A proxy that relays the stream to a browser
keeps its wire format. Whether a given proxy should relay svir events instead is its own choice.

### D21. Usage is requested by default

`include_usage` is on unless the client or the request turns it off. Nearly every caller wants
usage, and compatibility learning (D11) drops it on servers that reject it.

### D22. Public types are `#[non_exhaustive]` and serializable

They can grow without breaking callers, and consumers can store messages and completions. The
serde form is then a public contract: changing it is a breaking change.

## Proposed

### P1. Edition 2024; MSRV 1.85

For a library meant for others, the lower the better, as long as nothing newer is needed. 1.85 is
the lowest version edition 2024 allows. Clippy's `incompatible_msrv` checks standard library use
against it; a CI job on 1.85 itself should confirm it.

### P2. The decoder does no I/O; the encoder reads files only while streaming

The decoder takes bytes and gives events, so it works with any transport and can decode a stream
that is being forwarded elsewhere. The encoder builds JSON segments and file descriptions without
I/O; files are read only while the body streams, which needs the `client` feature (D14). Without
it, attachments are in memory.

### P3. Superseded by D16

### P4. Hardened transport defaults

No redirects, no automatic retries, no environment proxy discovery, a connect timeout capped at 10
seconds, and an idle read timeout between bytes; first-token and total limits are `Timeout` layers
(D15), so a long generation is not cut off by a request-wide default. `text/event-stream`
required. Plain HTTP only on loopback (`127.0.0.1`, `localhost`, `[::1]`) unless the caller opts
in; a LAN model server is a legitimate reason. A caller can supply its own client.

The base URL is accepted with or without `/v1` and trailing slashes, since both habits are common.
A base URL with credentials, a query, or a fragment is refused.

### P5. Reasoning is both live and kept

Every reasoning carrier is emitted as a live `Reasoning` event tagged with its source, and also
kept in the completion. Sending it back is decided per request (`.send_reasoning(..)`).

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
can reject. D21 is the one exception.

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

### P15. Accepted as D20

### P16. Accepted as D21

### P17. Accepted as D22

## Open

- **O2. `<think>` splitting.** On by default or opt-in? Is reasoning split out of `<think>` sent
  back, and if so, as tags in `content` or not at all?
- **O3. Compatibility retry versus context overflow.** Today any 400/422 on a request with optional
  fields is retried once without them, including a context overflow, which cannot succeed. Skip the
  retry when `error.code` identifies an overflow? Servers that report it only in text remain.
- **O5. Admission for images.** Counting base64 bytes as tokens is conservative for text but
  overestimates images by orders of magnitude.
- **O6. Other wire APIs.** The client shape is settled (D13). Open: which APIs (OpenAI Responses,
  Anthropic Messages, ...), in what order, and how each maps the neutral types, for example
  `Effort` to a thinking budget.
- **O7. `Retry-After` as an HTTP date.** Only numeric seconds are handled today.
- **O9. An optional loop helper.** See D6.
- **O10. Model listing.** Is the non-chat model filter part of svir or of the application?
- **O11. Assistant `content` with tool calls.** An empty string or `null`? Servers differ.
- **O12. Answer-changing anomalies in lenient mode.** An unknown finish reason (`content_filter`)
  or content after the finish reason: complete with what was received, or fail?
- **O13. The kind of an error inside an open stream.** It is a server-side failure, possibly
  transient, not a malformed stream. Today it maps to `Protocol`, which is not retryable. A
  separate kind, and is it retryable?

Resolved: O1 (API names and DX) by D13-D18, O4 (server error messages) by D19, O8 (tool
arguments) by D18.
