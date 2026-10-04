# Decisions

The design record. Each entry is **Accepted** (agreed; change it only deliberately and record the
change here), **Proposed** (the working default until someone objects), or **Open** (needs a
decision before the code that depends on it).

## Accepted

### D1. A protocol core with opt-in building blocks, not an agent framework

svir is the protocol between an application and a model server: types, encoder, decoder,
transport, and compatibility handling. On top of that core sit opt-in building blocks: layers
(D15) and tool sets (D18). The core does not depend on them. There is no agent loop and no
session state (D6).

svir should be small, composable, and pleasant to use, fit both a chat backend and an agent
engine without either bending around it, and be useful to anyone with a similar job.

*Revised 2026-09-29: originally "a wire-protocol SDK" only; layers and a tool router were added
with the API design (D13-D19). Revised 2026-09-30: the router lost its `#[tool]` macro (D18).*

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

svir provides tool descriptors, calls, and results, and the `Toolbox` trait (D18). svir does not
depend on an MCP SDK. The bridge from MCP tools to a `Toolbox` lives on the MCP side: in neva,
behind a `svir` feature (D18). MCP policy (allow lists, configuration, credentials) is always the
consumer's.

*Revised 2026-09-30: the bridge was "in the consumer or behind an optional svir feature".*

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
configuration at run time. It is generic over the HTTP backend, `Client<B = Hyper>` (D27): with
the built-in backend it is written `Client`, with no parameter. A constructor per wire API returns
a builder:

- `Client::openai(url)` for OpenAI-compatible servers. The URL is required: compatible servers
  live anywhere.
- `Client::anthropic()` and others later (O6): the hosted API by default, `.base_url()` to
  override.

`.build()?` validates the URL (P4), resolves the key (D17), and assembles the layers (D15), once.
Builder methods have plain names (`api_key`, `layer`, `wrap`); `with()` is reserved for adding
parts to a message.

**Calls.** `client.complete(request)` returns one `Completion`; it collects a single answer and is
not a loop. `client.stream(request)` returns an `EventStream`: a `Stream<Item = Result<Event,
Error>>` that is `Send + Unpin + 'static`, so it can be moved into a spawned task, and that has its
own `next()`, so reading it needs no extension trait. `client.list_models()` lists models. Calls
accept a `Request` or a `&Request`.

**Requests.** `Request::new(model)`, then:

- `.system(text)`: the system prompt is a field of the request, not a message; each adapter puts
  it where its API expects it (first message, or a top-level field);
- `.user(text)` as a shortcut, `.message(Message)` in general;
- `.assistant(completion)` adds a model answer with its text, tool calls, and reasoning, so
  nothing needed for the next turn can be lost; `.tool_result(id, content)` and
  `.tool_results(results)` answer calls;
- `.tools(&toolbox)` with anything implementing `Toolbox` (D18), or `.tool(Tool)`;
- `.reasoning(Effort)`, `.max_tokens(n)`, `.temperature(t)`, `.send_reasoning(bool)`.

`Effort` is `Off`, `Low`, `Medium`, `High`, or `XHigh`. Each adapter maps it explicitly: Chat
Completions sends `reasoning_effort` (`Off` as `none`); an API that takes a thinking budget uses a
documented, tested table.

**Messages.** `Message::user(text).with(part)`, parts kept in the order given (P12). Attachments do
no I/O when created: `Image::path(p)` and `TextFile::path(p)` record the path, and the size is
measured when the body is built and verified while it streams (P13). The media type comes from the
extension or `.media_type(..)`. In memory: `Image::bytes(buf, media_type)`,
`TextFile::text(name, text)`. An empty text next to images is left out. A text file's escaped
length can be declared, `.escaped_len(n)`, so the file is not read before it is sent (D39).

**Advanced use.** `svir::openai::chat::{Encoder, Decoder, Body}` for a custom transport or a proxy;
`Decoder::strict()` and `Decoder::lenient()` with `.limits(..)` and `.think(..)`.

Everyday imports come from `svir::prelude`.

### D14. One crate, no procedural macros

`svir` is one crate with features. It has no procedural macros and no `svir-macros` crate (D18).

| Feature | Adds |
| --- | --- |
| none | Types, the codec, and tool sets (`Toolbox`, `Tools`); attachments in memory only |
| `client` (default) | `Client`, `EventStream`, layers, the hyper and tokio transport (D25), attachments from disk |
| `tls` (default) | HTTPS: rustls with the ring provider and the webpki roots |
| `tls-aws-lc` | HTTPS with the aws-lc-rs provider in place of ring (D34) |
| `schemars` | `Tools::add`: tool input schemas derived from the types of handlers' arguments |
| `tracing` | The `Trace` layer |
| `testing` | `MockServer` (the conformance suite's scripted loopback server) and scripted event streams |

A crate family is for later, when a second wire API or a heavy optional part justifies it.

*Revised 2026-09-30: `svir-macros` and the `macros` feature were dropped with `#[tool]`. Revised
2026-09-30: the transport is hyper, not reqwest, and `tls` is its own feature (D25).*

### D15. Layers and middleware

A client is a stack of layers around the adapter. A layer wraps the provider-neutral call,
`Request -> Result<EventStream, Error>`, not HTTP, so the same layer works for every wire API.
Status mapping and compatibility learning sit below the layers, in the adapter; HTTP-level
settings go through a caller-supplied HTTP backend (D25).

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
In practice a retry covers a call that failed before the response started: a connection that could
not be made, a timeout waiting for it, or an error status. A failure inside the stream is never
retried, even one before its first event. The compatibility retry (D11) stays in the adapter:
nothing was generated.

A retry waits as long as the server asked (`Retry-After`), or else 500 ms, doubling each time
(`.backoff(..)` sets the start), and never more than 30 seconds. "Never reached the server" is a
mark on the error, `error.is_unsent()`, which a backend sets for a connection it could not make.

### D17. API keys

`.api_key(key)`, `.api_key_env(name)` (read at `build()`, with a clear error when unset), or
`.api_key_file(path)` (size-bounded, trimmed). The key is held as a `Secret`: redacted in `Debug`,
sent as a sensitive header, never in errors or events. svir never reads environment variables or
`.env` files implicitly; loading `.env` (for example with dotenvy) belongs to the application.

### D18. Tool sets: a trait, a plain registry, and no `#[tool]`

A tool is one concept wherever it is served: a name, a description, a JSON Schema for the input,
and a handler. An MCP tool and a function-calling tool match almost field for field. neva already
defines tools well: its `#[tool]` macro, schemas through schemars, argument names, sync and async
handlers, and dependency injection kept out of the schema. A second `#[tool]` in svir would be a
second implementation of the same thing, and a function served both over MCP and to a model would
carry two attributes. So svir has no tool macro.

**`Toolbox`.** svir defines the trait for anything that can describe tools to a model and answer
its calls:

```rust
pub trait Toolbox {
    fn tools(&self) -> Vec<Tool>;
    fn call(&self, call: &ToolCall) -> impl Future<Output = ToolResult> + Send;
}
```

`Request::tools(&toolbox)` takes the descriptors from it, and `toolbox.call(&call)` answers one
call. `toolbox.call_all(&calls)`, provided by the trait, answers several in order. Feeding the
answers back to the model is the caller's (D6).

**`Tools`.** A plain registry for callers without MCP, built explicitly and never from a global
registry, since different requests and agents use different tool sets:

```rust
let tools = Tools::new()
    .add("lookup", "Look up a value.", |args: Lookup| async move { lookup(args.value) });
```

`add(name, description, handler)` derives the input schema from the type of the handler's
arguments and needs the `schemars` feature; `add_tool(tool, handler)` takes a `Tool` that carries
its own schema and is always there. A tool added under a name already registered replaces the
earlier one.

Validating the arguments is deserializing them into the handler's type with serde: arguments that
do not fit never reach the handler. There is no separate JSON Schema validator, which would be a
heavy dependency for what the type already says. `tools.call(&call)` runs one handler; a failure
(an unknown tool, arguments that do not fit, an `Err` from the handler) becomes a tool result for
the model, flagged as a failure (D37), not an error of the request. A handler
returns a string, a JSON value, or a `Result` of either. `call.parse::<T>()` parses raw arguments
for callers without a registry. Resolves O8.

**The MCP bridge lives in neva**, behind a `svir` feature, and implements `Toolbox` twice:

- for neva's own tools, so a function written once with `#[neva::tool]` is served over MCP and
  handed to a model in the same process. Calling a tool in-process without an MCP session needs
  neva's internals, which is why the bridge belongs there;
- for `neva::Client`, so the tools of a remote MCP server are handed to a model: `list_tools`
  becomes the descriptors, `call_tool` forwards each call.

neva depends on svir optionally; svir never depends on neva. What the bridge has to settle, in
neva:

- tools that need an MCP session (elicitation, progress, sampling) get a detached context or a
  clear error when called in-process;
- an MCP result is content blocks and structured content, while a `ToolResult` is a string (P12):
  text is joined, structured content is sent as JSON text, and images or resources are an explicit
  error, not dropped silently;
- only tools visible to the model are offered to it.

The same pieces also compose without any bridge: an MCP tool whose handler calls svir, for
example one `ask(model, prompt)` tool that routes to different models by its arguments, needs only
`#[neva::tool]` and a `Client` injected through neva's dependency injection.

*Revised 2026-09-30: was "a tool router, with explicit registration" with svir's own `#[tool]`.*

### D19. The server's own message behind an accessor

Resolves O4. `error.server_message()` returns the server's text (bounded), so a chat UI can show
"Model not loaded". It is excluded from `Debug` and `Display`, so it does not reach logs or
journals by accident.

### D20. Raw passthrough for proxies

`client.send(request)` returns the server's bytes unchanged, as a `RawStream`, after status mapping
and compatibility handling; a `Decoder` can read them on the way past. A proxy that relays the stream to a browser
keeps its wire format. Whether a given proxy should relay svir events instead is its own choice.

### D21. Usage is requested by default

`include_usage` is on unless the client or the request turns it off. Nearly every caller wants
usage, and compatibility learning (D11) drops it on servers that reject it. The default lives in
the client: the `Encoder` on its own sends the field only when asked.

### D22. Public types are `#[non_exhaustive]` and serializable

They can grow without breaking callers, and consumers can store messages and completions. The
serde form is then a public contract: changing it is a breaking change.

### D23. `<think>` is split by default and never sent back

Resolves O2. `Think::Split` is the default: servers without a reasoning parser put the model's
reasoning inside the answer text, and without splitting it is shown as the answer and sent back
to the model on every turn. A caller whose answers legitimately contain the tag uses
`Think::Keep`.

Reasoning split out of `<think>` tags is not sent back, even with `.send_reasoning(true)`. Servers
that inline the tags read no reasoning field in a request, and the chat templates of such models
drop earlier reasoning from the history anyway; putting it back into the text would only spend
context. This is a documented rule, not a silent loss.

### D24. An assistant message with tool calls and no text has empty-string content

Resolves O11. `"content": ""`, not `null`. The chat templates of many local models join `content`
as a string and fail on `null`, while an empty string is accepted everywhere.

### D25. The transport is hyper, behind a seam

The built-in transport is hyper with hyper-util's pooled client, not a full HTTP client library.
svir needs one POST and one GET, and P4 wants off nearly everything such a library adds:
redirects, proxy discovery, retries. hyper simply lacks them. Measured on a minimal program, the
dependency tree and the clean build are about a third of the alternative's (46 crates against 89
with TLS), and the TLS build needs no cmake.

- `client`: HTTP/1.1. `tls` (default): HTTPS through rustls with the ring provider and the webpki
  roots, and HTTP/2 by ALPN; `tls-aws-lc` is the same with aws-lc-rs (D34). Without either, an
  `https` URL is a `Config` error.
- What hyper does not give, a caller brings: `svir::http::Backend` is the little of HTTP svir
  uses (a request with a body of known length in, a status, headers, and a byte stream out), and
  `.http(backend)` on the client builder replaces the built-in transport. A proxy, client
  certificates, or other roots are an implementation of that trait over a client that has them.
  The seam is static (D27).
- Timeouts, status mapping, compatibility learning, and decoding sit above the seam, so they hold
  for any backend.
- Host names are ASCII: there is no IDNA conversion.

### D26. No compatibility retry for a context overflow

Resolves O3. A 400 or 422 whose `error.code` says the context overflowed is not about the optional
fields, so it is reported at once, without the lean retry of D11. The same holds for an overflow
recognized by its type or its message (D30), and for a prompt the content filter blocked (D36).

### D27. Nothing on the request path is boxed or dispatched dynamically

The types between a request and its answer have names, and the client is generic over its
backend:

- `Backend` has an associated `Body` type, the response's bytes, and
  `fn send(&self, request) -> impl Future<..> + Send`. An implementation writes an `async fn`; no
  future is boxed. One whose HTTP client gives a stream it cannot name uses the `BoxBody` alias
  for its `Body`, and pays for that itself.
- `Client<B = Hyper>`, `EventStream<B = Hyper>`, and `RawStream<B = Hyper>`. The default keeps
  the parameter out of sight for everyone on the built-in backend.
- The streams are state machines written by hand, with `poll_next`: `EventStream`, `RawStream`
  (the idle timeout), `HyperBody` (the response body), and `BodyStream` (the request body,
  reading attachments from disk).
- The one allocation left is the idle timer, a `Pin<Box<Sleep>>`: a box, not a trait object. It
  is what keeps the streams `Unpin`, so `stream.next().await` needs no pinning by the caller.

This buys no measurable speed: a request costs a network round trip and a model's generation. It
buys a request path with no hidden indirection, and backends that are plain `async fn`s.

Layers (D15) are a separate matter. A closure passed to `wrap` has a type no one can write, so a
client with layers needs its types erased somewhere to be stored in a struct. D28 decides where,
and leaves the path without layers as it is here.

*Revises D13 (the client was not generic) and D25 (the seam was a trait object), 2026-09-30.*

### D28. Layers are erased at their boundary, and shape the stream through its own methods

A client has the same type whatever its layers: `Client<B>`, storable in a struct. That is worth
more than a static stack, whose type grows with every layer and cannot be written at all once a
closure is in it.

- A layer is `Layer<B>`: `fn call(&self, request: Request, next: Next<B>) -> impl Future<..>`.
  An implementation writes an `async fn`. The client keeps its layers as trait objects and boxes
  one future per layer per request. A client without layers takes the path of D27 and boxes
  nothing.
- `Next<B>` is the rest of the stack. It owns what it needs, so a closure passed to `wrap` has no
  lifetimes to name, and it is cheap to clone, so `Retry` can run it again.
- A layer cannot change the type of the stream it passes on, so `EventStream` has the methods a
  layer needs: `first_token_by(deadline)`, `complete_by(deadline)`, `idle_timeout(limit)`, and
  `inspect(|item| ..)`, which watches every item without changing it. The timers and the
  inspectors are allocated only when a layer asks for them.
- "First token" is anything of the answer: text, reasoning, or a piece of a tool call. An answer
  that opens with a tool call has started.
- Layers work on events. `client.send()`, which returns raw bytes, and `client.list_models()` do
  not pass through them.
- `.http(backend)` comes before the layers, which are tied to the backend they were added for.
  Calling it after them is a `Config` error from `build()`, not a silent loss of the layers.
- With layers, a call clones its request once on the way in, since a layer owns the request it
  is given and may change it.

### D29. An error inside the stream is `Server`

Resolves O13. A server that reports a failure inside an open stream, as an error object in a
chunk or as an `event: error` event, has failed while answering. That is not a malformed stream,
so it is its own kind, `Server`, in strict and lenient mode alike, with the server's message
behind `server_message()`. It is not retryable: svir cannot tell a crash that will pass from a
request the server will refuse again, and part of the answer may already have been delivered. A
caller who knows its server better reads the message and decides.

An `error` event fails the stream whatever its data is: an error object, an object with a
message, or plain text.

### D30. A context overflow is recognized by code, type, or message

Resolves O15. Servers agree on no single sign of an overflow, and the application has to know:
it is the one failure answered by shortening the conversation. So an error the server reports
is `ContextOverflow` when any of these holds:

- `error.code` or `error.type` is `context_length_exceeded`, `context_window_exceeded`, or
  `exceed_context_size_error`;
- the message speaks of the `context length`, the `context size`, or the `context window`, in
  any letter case.

This applies to the body of a 400, 413, or 422, and to an error inside the stream (D29), which is
how LM Studio reports an overflow. On any other status the body does not change the kind: a 500
that mentions the context is still a transient failure.

Matching words is looser than matching a code, on purpose. The lists live in one place
(`openai/chat/overflow.rs`) and grow as servers are observed. A false match turns one
non-retryable error into another, and the server's own message is kept either way.

### D31. The answer text is what the server sent

Resolves O16. svir does not trim or otherwise tidy the text. A server that separates reasoning
itself may start the answer with the line breaks that followed it; a proxy has to relay them, and
a stored answer has to equal what was streamed. Trimming for display is the caller's choice.

### D32. The default wire limit is 64 MiB

The wire limit bounds the bytes of one response as they arrive, and a Chat Completions stream
spends a few hundred of them on every token: each event repeats the ID, the model, and the
choice around a delta of a few characters. Observed against a local server, that is about 250
bytes a token, so the first default, 4 MiB, cut off an answer after some 16,000 tokens. A
reasoning model passes that on a hard question, and the first application built on svir met it
at once.

The limit is there to stop a server that never ends, not to hold memory down: the decoder keeps
the answer, not the wire bytes, and the answer is a small fraction of them. 64 MiB is about a
quarter of a million tokens. The limits on one event (256 KiB) and on tool calls (64) are
unchanged.

### D33. An error carries the HTTP status

`error.status()` is the status of a response that was not a success, and `None` for every other
failure: a connection that could not be made, a timeout, an error inside a stream that began with
`200`. It is for a proxy that answers with the upstream's status. Everything else acts on the
kind, which means the same whatever the server; the status is kept beside it, not in place of it.

### D34. The crypto provider is a feature, and always passed explicitly

rustls has two providers, ring and aws-lc-rs, and picks a process default only when exactly one
of them is compiled in. A build that has both, because another dependency brings aws-lc-rs, has
no default, and any code that calls `ClientConfig::builder()` without a provider panics. svir
passed its provider explicitly from the start, but its `tls` feature compiled ring in, and that
alone took the default away from the rest of the build.

- `tls` (default) keeps ring: it builds without a C toolchain on every platform.
- `tls-aws-lc` uses aws-lc-rs instead. A build that has aws-lc-rs already turns the default
  features off and takes `client` and `tls-aws-lc`, so one provider is compiled in and the
  default is back. With both features on, aws-lc-rs is used.
- Whichever it is, svir passes it to rustls explicitly and never depends on the default.

### D35. A filtered answer is its own finish reason; other answer-changing anomalies fail

A content filter that stops the answer is an outcome, not a malformed stream: the server says why
it stopped, and the text before that point was sent. It is `FinishReason::ContentFilter` in both
modes, and the completion keeps that text as the server sent it (D31), so a caller can show it
and say why it ends. That text may hold what the filter flagged: an asynchronous filter vets it
only after streaming it, and even without one Azure was recorded streaming a blocklisted word
before the block. A caller that shows the text withdraws it on this finish. Tool calls with it
are `Protocol`, as with `stop`: the filter may have cut a call short, or flagged it, and a call
is released only whole and clean (D5).

An asynchronous filter streams the answer unvetted and reports on it afterwards, in annotations:
a choice with `content_filter_offsets` and no delta, in a chunk with an empty `id` and `model`.
An annotation carries nothing of the answer, so it is not content, and its `id` and `model` are
not the stream's. Annotations interleave with the answer and follow its finish, and a block is
the answer's finish, `ContentFilter`, wherever it comes:

- an annotation whose `content_filter_results` mark a category or a blocklist `filtered: true`
  is a block, with or without a finish reason. Recorded, a blocklisted word the filter caught
  only after the model's `stop` came this way, with `finish_reason: null`, and the answer that
  held it ten times would otherwise complete as clean. The verdict is kept, not its place, so
  content after it is not content after the finish;
- `content_filter` as an annotation's finish reason is a block too, as the filter sends it while
  the answer is still streaming, and the stream ends there;
- `detected: true` without `filtered` is what a filter set to annotate only reports, and blocks
  nothing; an annotation that blocks nothing is skipped, before the finish or after it;
- any other finish reason in an annotation is `Unsupported`.

The filter's verdict on text already sent is the stronger statement: a block turns the model's
`stop`, `length`, or `tool_calls` into `ContentFilter`. The offsets are not passed on. They are
documented to count characters from the start of the prompt as the server rendered it, which
svir cannot map onto the answer's text, and recorded they do not behave as documented (see
[wire-protocol.md](wire-protocol.md#37-content-filtering)).

Any other finish reason svir does not know, and content after the finish reason, are
`Unsupported` in both modes. An unknown reason may mean the answer is not what it looks like;
content after the finish is a server that contradicts itself about where the answer ends. Lenient
mode does not produce an answer that may be wrong (P8). Resolves O12.

*Revised 2026-10-02: annotations were `Unsupported` in both modes, until they were read (#12);
the text before a `ContentFilter` finish was said to have passed the filter in Azure's default
mode, until a recording showed otherwise.*

### D36. A prompt the content filter blocked is its own error kind

A content filter that blocks the prompt rejects the request with 400 and `error.code` of
`content_filter` (Azure OpenAI). That is not a server that cannot do something, so it is not
`Unsupported`: it is `ContentFilter`, not retryable, since the same prompt is blocked again. It
sits next to `FinishReason::ContentFilter` (D35), the filter stopping an answer.

It is told by the code alone, in the body of a 400, 413, or 422, as an overflow is (D30); the
code is specific, so a match cannot be false. The words of the message are not read, and the
code means nothing on any other status.

Such a rejection is not about the optional fields, so it is reported without the compatibility
retry (D11), as an overflow is (D26). Otherwise the blocked prompt is sent twice, and its
evaluation billed twice. More generally, the retry follows only a rejection whose body explains
nothing, `Unsupported`.

### D37. A failed tool result is flagged

Resolves O14. `ToolResult` has `is_error`, set by `ToolResult::error(call_id, message)`. `Tools`
sets it for an unknown tool, arguments that do not fit, and an `Err` from a handler, and the
content is then what went wrong, with nothing added.

Each wire API tells the model as it can. Chat Completions has no field for it, so its encoder
writes `error: ` before the content. That is the text `Tools` sent before the flag existed, so
nothing changes on the wire for its users, and the flag is not dropped for a result the caller
built. An API with a field of its own sets that field and sends the content as it is, with no
prefix.

It is a field now, before a second wire API exists, because it changes the serde form of a public
type (D22), which only gets more expensive. The change is additive: `is_error` is written only
when it is true.

### D38. The caller's headers are set on the builder

`ClientBuilder::header(name, value)` adds a header to every request the client sends, the model
listing included: for a gateway or a hosted endpoint that asks for attribution, an organization or
a project, or a key under a name of its own. Names are not case-sensitive and are sent lowercase;
setting a name again replaces the earlier value.

`build()` validates them, `Config` otherwise: a name must be a header name; a value must be ASCII
with no control character but a tab, so it cannot end the header early and start another; and the
headers svir writes itself, or that frame the request, are refused: `authorization`,
`content-type`, `content-length`, `accept`, `host`, `transfer-encoding`, `connection`. The API
key stays with `api_key` (D17), where it is checked and withheld.

Any value may be a credential, so every one is treated as one: withheld from the `Debug` output
of the client and of an `HttpRequest`, and sent as a sensitive header by the built-in backend.
Only `content-type` and `accept` are shown. An error names the header, never its value.

Not part of this: a public constructor for the built-in `Hyper` backend, so that a caller's
backend could wrap it, and headers per request, which a layer cannot add since layers work above
HTTP. Each is its own decision, when it is needed. The headers are validated and kept with the
`http` crate's types, which hyper brings, and handed to the backend as text, as the seam carries
them (D25); whether the seam should carry those types instead is O17.

### D39. A text file's escaped length can be declared

The length of a text file once escaped into a JSON string takes a read of the whole file to
measure, before the file is read again to be sent. An application that keeps files, a chat
backend for one, can measure it once, as the file arrives, and store it.
`TextFile::escaped_len(n)` declares it, and `svir::body::escaped_len(bytes)` measures it as svir
escapes, summed over the blocks the file is read in. With it, `encode_files` looks up the file's
size and reads nothing. The declared length is checked where it is used:

- a length the size rules out (every byte escapes to 1, 2, or 6 bytes) fails when the body is
  built;
- otherwise the body stream counts what the file encodes to, as for any attachment, and fails
  with `Attachment` rather than send a body that disagrees with its `Content-Length` (P13);
- the stream checks that a text file is UTF-8 as it reads it, since nothing may have read it
  before. Without a declared length the file is checked twice, when measured and when sent,
  which also catches a file replaced in between by one of the same lengths that is not text;
- text in memory is measured anyway, and a declared length that disagrees fails when the body is
  built.


### P1. Edition 2024; MSRV 1.85

For a library meant for others, the lower the better, as long as nothing newer is needed. 1.85 is
the lowest version edition 2024 allows. Clippy's `incompatible_msrv` checks standard library use
against it; a CI job on 1.85 itself should confirm it.

### P2. The decoder does no I/O; the encoder touches files only behind `client`

The decoder takes bytes and gives events, so it works with any transport and can decode a stream
that is being forwarded elsewhere.

`Encoder::encode` does no I/O either, and takes attachments held in memory; `Body::into_bytes`
gives the whole body. Attachments held as file paths need the `client` feature (D14):
`Encoder::encode_files` measures them first (an image by its size, a text file by one read that
also checks it is UTF-8, or by its size when its escaped length is declared, D39), and
`Body::into_stream` reads them again, a block at a time, as the body is sent.

### P3. Superseded by D16

### P4. Hardened transport defaults

No redirects, no retries of a request that reached the server, no environment proxy discovery.
Connecting takes at most 10 seconds. The server may send nothing for at most 5 minutes, before
the response headers and between pieces of the body; both are settable, and the idle timeout can
be turned off. It is generous because a local model can take minutes over a long prompt;
first-token and total limits are `Timeout` layers (D15). `text/event-stream` required. Plain HTTP
only on loopback (`localhost` or a loopback address) unless the caller opts in with
`.allow_http()`; a LAN model server is a legitimate reason. A caller can supply its own HTTP
backend (D25).

An error response's body is read for the server's message (D19), up to 64 KiB. It is waited for
only briefly, 300 ms, unless the kind of the error depends on it (400, 413, 422): an error is not
held up by a body that never comes.

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
partial text that arrived before such an error. Anomalies that change the answer itself fail in
both modes, except a filtered answer, which is its own finish reason (D35).

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
- A tool result's content is the caller's string. svir does not wrap or serialize outcomes; a
  failed result (D37) has `error: ` before it.
- Reasoning goes back only when the request asks for it, under the key it arrived with;
  reasoning split out of `<think>` tags never goes back (D23).

### P13. Attachment failures are their own error kind

An attachment that cannot be read, no longer has its recorded size, does not encode to its
recorded or declared length, or is a text file that is not UTF-8, fails the body stream with
`Attachment`, which is not retryable. It is a local failure, not something the server did.

### P14. Admission

With a context size configured, a request is admitted when the body length plus `max_tokens`
fits in it and `max_tokens` is not 0; otherwise it fails with `ContextOverflow` before a byte is
sent. A request without `max_tokens` reserves nothing for the answer. The body length in bytes
stands in for its length in tokens, which it never underestimates (but see O5).

### P15. Accepted as D20

### P16. Accepted as D21

### P17. Accepted as D22

## Open

- **O5. Admission for images.** Counting base64 bytes as tokens is conservative for text but
  overestimates images by orders of magnitude.
- **O6. Other wire APIs.** The client shape is settled (D13). Open: which APIs (OpenAI Responses,
  Anthropic Messages, ...), in what order, and how each maps the neutral types, for example
  `Effort` to a thinking budget.
- **O7. `Retry-After` as an HTTP date.** Only numeric seconds are handled today.
- **O9. An optional loop helper.** See D6.
- **O10. Model listing.** Is the non-chat model filter part of svir or of the application?
- **O17. The HTTP seam on `http` types.** `HttpRequest` and `HttpResponse` carry headers as
  `(String, String)` pairs, so a backend converts them both ways. `HeaderMap` would pass straight
  through hyper or another client built on `http` 1.x, and keep the sensitive mark, but changes a
  public type and ties svir's public API to `http`'s major version. A change for 0.2 at the
  earliest.
- **O18. Binary attachments beyond images.** Audio, PDF, video, and whatever comes next. The
  working idea: one part for a binary attachment with its media type, of which `Image` is a
  case, rather than a type per kind, since the caller gives the same for each (bytes or a path,
  and a media type) and only the wire layout differs, which the encoder derives from the media
  type. `TextFile` stays its own part: sending a file as text inside the message is the caller's
  choice, not a kind of file, and a media type says too little about text. A media type an API
  has no layout for is `Unsupported` before anything is sent; converting formats (a PDF to text,
  a video to frames) is the application's. Open: the name (`Part::File` is the text file), the
  layout per wire API (Chat Completions `input_audio` and `file` parts; Anthropic's `document`
  and the Responses API's `input_file`, with O6), what servers accept, seen live, and admission
  for large media (O5).

Resolved: O1 (API names and DX) by D13-D18, O2 (`<think>` splitting) by D23, O4 (server error
messages) by D19, O3 (compatibility retry versus context overflow) by D26, O8 (tool arguments)
by D18, O11 (assistant content with tool calls) by D24, O13 (errors inside the stream) by D29,
O15 (an overflow without a code) by D30, O16 (whitespace before the answer) by D31, O12
(answer-changing anomalies in lenient mode) by D35, O14 (failed tool results) by D37.
