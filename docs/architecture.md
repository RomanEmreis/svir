# Architecture

Status: design. Nothing here is implemented yet. Decisions referenced as `D<n>`, `P<n>`, and
`O<n>` are recorded in [decisions.md](decisions.md); protocol facts are in
[wire-protocol.md](wire-protocol.md).

## 1. What svir is

svir is the wire protocol between an application and a language model server, packaged as a Rust
library: request and response types, an encoder, a decoder, a transport, and compatibility
handling. On top of that core sit opt-in building blocks: layers and tool sets (D1). It is not
an agent framework: it does not run a tool loop (D6), keep a session history, store anything, or
speak MCP (D7).

The first protocol is OpenAI-compatible Chat Completions streaming (D9), as served by LM Studio,
mlx-lm, llama.cpp, vLLM, and hosted endpoints.

## 2. Where the design comes from

Two independent, working implementations of the same protocol were studied. Each was built for a
different consumer, and each is strong where the other is weak:

| Concern | Chat backend | Agent harness |
| --- | --- | --- |
| Content | Text, images, and text files, streamed from disk | Text only; images rejected |
| Request body | Streamed, with an exact `Content-Length` | Serialized in memory; its length is the context estimate |
| Reading the stream | Lenient: unknown fields are ignored | Strict: unknown fields are errors; byte and event limits; `id` and `model` must stay constant |
| Reasoning | Streamed live, including `<think>` inside content | `reasoning_content` and `reasoning` kept opaque and sent back; `<think>` not handled |
| Tools | None | `tool_calls` assembled across deltas; calls released only after a finish reason and `[DONE]` |
| Errors | Upstream text relayed as-is | Typed: retryable, retry-after, context overflow, truncated stream, ... |
| Compatibility | Learns that a server rejects optional fields | Static setting (`include_usage`) |
| API shape | HTTP client plus a byte `Stream` | Stateful pull interface (`start`, `next_event`), safe to drop |

svir takes the stronger half of each. The consumers keep everything that is theirs.

## 3. Boundaries

In svir:

- **Types**: messages with content parts, tool descriptors, tool calls, tool results, usage,
  finish reasons, and the typed error.
- **Encoder**: a request becomes a byte stream with an exact length known before the first byte
  is sent. The length doubles as the context-admission estimate (D10).
- **Decoder**: SSE framing, deltas, tool-call assembly, all three reasoning carriers, usage,
  mid-stream errors, and limits.
- **Transport**: the HTTP request, authentication, status mapping, timeouts, and cancellation.
- **Compatibility**: learning what a particular server accepts, once per server (D11).
- **Client**: one type over every wire API, with requests, messages, and streams (D13).

Opt-in, on top of the core:

- **Layers**: middleware around the provider-neutral call, including `Retry`, `Timeout`, and
  `Trace` (D15, D16).
- **Tool sets**: the `Toolbox` trait and `Tools`, a plain registry without macros (D18).

Not in svir, and owned by the consumer:

- the agent loop that feeds tool results back, effect journal, replay, budgets, context
  compaction, workspace tools (D6);
- MCP client and its policy (allow lists, configuration), and tool definitions for MCP; the bridge
  from MCP tools to a `Toolbox` lives in neva (D7, D18);
- conversation storage, a registry of running generations, attachment storage, authentication
  of end users;
- retry policy beyond the opt-in `Retry` layer (D16).

## 4. Components

### 4.1 Types

Provider-neutral types describe what a caller needs, not the union of every server's JSON. Wire
types stay private to the adapter that speaks them. They are in `src/`; this is the summary.
Every public type is `#[non_exhaustive]` and serializable (D22), built with constructors and
builder methods, with public fields for reading.

- A **request** has a model, an optional system prompt, messages, tools, and parameters. The
  system prompt is a field, not a message: each adapter puts it where its API expects it.
- A **message** has a role (`User`, `Assistant`, `Tool`; no system role) and parts in order:
  text, image, text file, reasoning and tool calls (assistant), and a tool result (tool role).
  Which parts a role may carry is checked when the request is encoded. A `Completion` converts
  into an assistant message with its reasoning, text, and calls.
- An **attachment** (`Image`, `TextFile`) has a `Source`: a path, read while the body streams
  with its size measured when the body is built, or bytes in memory (serialized as base64).
- **Effort** is the requested reasoning effort (`Off` to `XHigh`); each adapter maps it to its
  API (D13).
- A **tool descriptor** is a name, a description, and a JSON Schema for the input.
- A **tool call** is a provider call ID, a name, and the arguments as the raw string received.
- **Usage** is input and output tokens, and total and reasoning tokens when the server reports
  them. svir reports only what the server said; estimates are the caller's.
- **Timing** is when the first and last visible tokens arrived, from the start of the response;
  `Completion::tokens_per_second` applies the rule of P7.
- **Finish reason** is `stop`, `tool_calls`, or `length`. Anything else is an error in strict mode.
- **Reasoning** carries its source (`reasoning_content`, `reasoning`, or `think`), which is all
  Chat Completions needs to send it back. Opaque continuation data, such as signed thinking
  blocks, arrives with the first wire API that needs it (O6).
- **Decoding options**: `Mode` (`Strict` by default), `Limits`, and `Think` (`Split` or `Keep`;
  its default is O2).

### 4.2 Encoder

The body is built as a sequence of segments: literal JSON bytes, serialized up front, and file
segments read from disk on demand. Each segment's encoded length is known in advance:

- base64 of `n` bytes is `4 * ceil(n / 3)`;
- a text file's JSON-escaped length is measured once, when the file is accepted.

So the total length is exact before streaming starts, and the request is sent with
`Content-Length` rather than chunked. Nothing is read from disk until the stream is polled, so a
body can be produced again cheaply for a retry, byte for byte. A file that no longer has its
recorded size fails the stream with `Attachment` (P13) instead of sending a body that disagrees
with its `Content-Length`.

Files are read in blocks whose size is a multiple of 3, so base64 blocks concatenate without
inner padding. JSON escaping is byte-wise: only ASCII bytes ever need escaping, and every byte of
a multi-byte UTF-8 character passes through unchanged, so a block boundary inside a character is
harmless. Escapes match `serde_json` exactly.

What goes into the body:

- only what the request sets, plus `stream: true`; nothing is implied (P11);
- messages laid out as in P12: plain string content unless there is an image, text and files
  joined into one text part in the order given, images after it as data URLs, tool results as
  the caller's string, reasoning sent back only on request under the key it arrived with;
- tools as `{"type": "function", "function": {"name", "description", "parameters"}}`.

Admission (P14): the declared length plus `max_tokens` must fit in the configured context size,
and `max_tokens` must be above 0, or the request fails with `ContextOverflow` before a byte is
sent.

### 4.3 Decoder

The decoder is push-based and does no I/O: bytes go in, events come out. That lets it read a
stream that is being forwarded elsewhere unchanged (a proxy that relays bytes to a browser and
decodes them on the way past), as well as a stream it owns.

Output is a `Stream` of `Result<Event, Error>` (D4). The types are in `src/response.rs`:

```rust
enum Event {
    Text(String),
    Reasoning(Reasoning),   // { source: ReasoningSource, text: String }
    ToolCallDelta(ToolCallDelta), // { index, id: Option<String>, name: Option<String>, arguments }
    Completed(Completion),
}

struct Completion {
    finish: FinishReason,
    text: String,
    reasoning: Vec<Reasoning>,
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
    timing: Option<Timing>,
}
```

- `Text` and `Reasoning` are live deltas for display.
- `ToolCallDelta` is display data only. Executable calls exist only in `Completed`, which is
  emitted after a finish reason and `[DONE]` (D5). A stream that ends before that is a
  `TruncatedStream` error, never a partial completion.
- `Completed` is the last item. Bytes after `[DONE]` are not read (P9).

The outcome does not depend on how the bytes were chunked (P9). Events arrive in wire order, and
an error is delivered after every event decoded before it, even when both came in the same chunk.
Otherwise the same response could complete when read byte by byte and fail when read in one piece,
or lose the text that preceded an error.

A consumer that needs a pull interface (for scripted tests or replay) wraps the stream in its own
trait; `next_event()` becomes `stream.next()`. That trait belongs to the consumer, not to svir.

### 4.4 Strict and lenient

Strictness is a decoder policy, not two decoders (D3). Strict is the default. Lenient skips
unknown input and tolerates inconsistent metadata, but never anything that could make the answer
or a tool call wrong (P8).

| Situation | Strict | Lenient |
| --- | --- | --- |
| Unknown SSE field (not `data`, `id`, `retry`, `event: message`, or a comment) | `Unsupported` | Ignored |
| `data` that is not valid JSON | `Protocol` | Ignored |
| Invalid UTF-8 in an event | `Protocol` | Replaced lossily |
| Unknown key in a delta (for example `audio`, `refusal`) | `Unsupported` | Ignored |
| A chunk with an empty `choices` array before the finish reason | `Protocol` | Ignored |
| More than one choice, or a choice index other than 0 | `Unsupported` | First choice read |
| `id` or `model` changes mid-stream | `Protocol` | Ignored |
| Usage reported twice | `Protocol` | The last one kept |
| Usage without `prompt_tokens` or `completion_tokens` | `Protocol` | Usage ignored |
| Unknown finish reason (for example `content_filter`) | `Unsupported` | Open (O12) |
| Content after the finish reason | `Unsupported` | Open (O12) |
| Error object inside an open stream | Error (O13) | Error, with the server's message (O4, O13) |

Enforced the same way in both modes (P8):

| Situation | Outcome |
| --- | --- |
| Wire bytes, event bytes, or tool calls over limit | `ResponseLimit` |
| End of stream, or `[DONE]`, before a finish reason; end of stream before `[DONE]` | `TruncatedStream` |
| Tool calls with duplicate or changing IDs, missing IDs or names, or non-contiguous indices | `Protocol` |
| `tool_calls` finish without calls, or calls with a `stop` finish | `Protocol` |

Two SSE details are framing, not leniency, and hold in both modes: several `data` lines in one
event join with a newline, and both `reasoning_content` and `reasoning` are reasoning. A call ID
repeated on later pieces of the same call is consistent; one that changes is not (P10).

Limits and their current defaults: 4 MiB of wire bytes per attempt, 256 KiB per SSE event, and 64
tool calls per response.

### 4.5 Reasoning

Reasoning arrives in three carriers (see [wire-protocol.md](wire-protocol.md#4-reasoning)):
`reasoning_content`, `reasoning`, and `<think>...</think>` inside `content`. The decoder emits all
of them as live `Reasoning` events tagged with their source and also keeps them in the
completion (P5). The carrier matters when sending reasoning back: a server expects its own field
back, unchanged, as continuation. Whether reasoning split out of `<think>` tags is sent back, and
whether splitting is on by default, is open (O2).

Splitting `<think>` is a streaming problem: a marker can be cut by a chunk boundary, so a
possible partial marker at the end of a chunk is held back until the next chunk decides it. At the
end of the stream a held-back tail that never completed a marker is plain text. The held-back
length is measured in bytes on character boundaries, so multi-byte text is never split.

### 4.6 Transport

Behind the `client` feature, so the codec can be used with any HTTP client (P2, D14). Each wire
API has an adapter under the one `Client` type (D13). The adapter posts the encoded body, adds
`Authorization: Bearer` when a key is configured (D17), maps HTTP status codes to error kinds, and
requires a `text/event-stream` response before decoding. Layers wrap the adapter (D15).

Proposed defaults (P4): no redirects, no automatic retries, no environment proxy discovery, a
connect timeout of at most 10 seconds, and an idle read timeout. First-token and total limits are
`Timeout` layers, so a long generation is not cut off by a request-wide default. Plain HTTP only
on loopback unless explicitly allowed. A caller can supply its own client.

`client.send(request)` returns the server's bytes unchanged, after status mapping and
compatibility handling, for a proxy that relays them and decodes them on the way past (D20).

The base URL is accepted with or without `/v1` and trailing slashes; one with credentials, a query,
or a fragment is refused. Model listing returns what `GET /v1/models` reports, `id` and optional
`name`, without filtering (O10).

Cancellation is dropping the stream: that closes the connection, including between events that
were already buffered.

### 4.7 Compatibility

Some fields are widely but not universally supported: `reasoning_effort` and
`stream_options`. A strict server may reject the whole request with 400 or 422. The first such
rejection of a request that carried optional fields triggers one retry without them. If the retry
succeeds, the server is remembered as strict and later requests omit those fields from the first
attempt. If the retry fails too, the original error is reported. The memory lives in a
cheap-to-clone handle shared by every request to that server. A 400/422 means nothing was
generated, so this retry is safe. Its interaction with context-overflow errors is open (O3).

### 4.8 Errors

Every failure is a typed kind with a `retryable` flag and an optional retry delay:

| Kind | Retryable | Raised for |
| --- | --- | --- |
| `Transport` | yes | Connection failure; HTTP 500, 502, 503 |
| `Timeout` | yes | Client timeout; HTTP 408, 504 |
| `RateLimited` | yes | HTTP 429 |
| `TruncatedStream` | yes | Stream ended before a finish reason and `[DONE]` |
| `Authentication` | no | HTTP 401, 403 |
| `ContextOverflow` | no | Known overflow codes on 400/413/422; or local admission failed |
| `Protocol` | no | Malformed or inconsistent stream |
| `Unsupported` | no | A feature the adapter cannot represent; a success that is not `text/event-stream`; any other status, redirects included |
| `ResponseLimit` | no | A byte, event, or tool-call limit was reached |
| `Attachment` | no | An attachment could not be read or changed size since it was recorded (P13) |
| `Config` | no | The client configuration is invalid: the URL (P4) or the source of the API key (D17) |

`Retry-After` in seconds is honored and capped at 30 seconds. svir classifies; retrying is the
caller's decision, or the opt-in `Retry` layer's (D16). Error values never contain credentials,
request URLs, or headers. The server's own message is available through
`error.server_message()`, cut to 4 KiB, and kept out of `Debug` and `Display` (D19). Everything
else a caller or a layer needs is on the error: `kind()`, `is_retryable()`, `retry_after()`,
`detail()`, and the chained `source()`.

## 5. API and composition

The API is decided in D13-D19. This section shows it in use.

### 5.1 Four levels

Each level is needed less often than the one before and gives more control.

**Talk to a model.**

```rust
use svir::prelude::*;

let client = Client::openai("http://127.0.0.1:1234").build()?;
let request = Request::new("qwen3-27b")
    .system("Be precise.")
    .reasoning(Effort::Low)
    .user("Explain ownership in one paragraph.");

let answer = client.complete(&request).await?;
println!("{}", answer.text);

let mut stream = client.stream(&request).await?;
while let Some(event) = stream.next().await {
    match event? {
        Event::Text(delta) => print!("{delta}"),
        Event::Reasoning(r) => eprint!("{}", r.text),
        Event::Completed(done) => println!("\n{:?}", done.usage),
        _ => {}
    }
}
```

**Attachments, tools, and composition.** The loop that feeds tool results back is caller code
(D6):

```rust
#[derive(Deserialize, JsonSchema)]
struct Lookup {
    value: i64,
}

let llm = Client::openai("http://127.0.0.1:1234")
    .api_key_env("LMSTUDIO_API_KEY")
    .layer(Retry::connect(3))
    .layer(Timeout::first_token(Duration::from_secs(120)))
    .wrap(|req, next| async move {
        let started = Instant::now();
        let reply = next.run(req).await;
        tracing::info!(elapsed = ?started.elapsed(), "upstream answered");
        reply
    })
    .build()?;

let tools = Tools::new()
    .add("lookup", "Look up a value.", |args: Lookup| async move { (args.value * 2).to_string() });
let msg = Message::user("What changed between these two?")
    .with(Image::path("before.png"))
    .with(Image::path("after.png"))
    .with(TextFile::path("diff.patch"));

let mut request = Request::new("qwen3-27b").tools(&tools).message(msg);
loop {
    let done = llm.complete(&request).await?;
    if done.calls.is_empty() {
        break println!("{}", done.text);
    }
    let mut results = Vec::new();
    for call in &done.calls {
        results.push(tools.call(call).await);
    }
    request = request.assistant(done).tool_results(results);
}
```

`reply` in the `wrap` closure arrives with the response headers, not the whole answer; a layer
that needs the whole answer wraps the returned stream (D15).

**A proxy or a custom transport.** Relay the server's bytes unchanged and decode them on the way
past (D20):

```rust
let mut raw = client.send(&request).await?;
let mut decoder = Decoder::lenient().think(Think::Split);
while let Some(bytes) = raw.next().await {
    let bytes = bytes?;
    browser.send(bytes.clone()).await?;
    for event in decoder.push(&bytes) {
        store(event?);
    }
}
decoder.finish()?; // TruncatedStream without [DONE]
```

**Types and the stream only.** An engine with its own provider interface, journal, and budgets
wraps `EventStream` in its own trait: `next_event()` is `stream.next()`, and cancelling is dropping
the stream, which closes the connection. It adds no `Retry` layer, since it accounts for retries
itself (D16).

### 5.2 Layers

A layer wraps `Request -> Result<EventStream, Error>`, above the adapter, so it is independent of
the wire API. The first layer added is the outermost. A layer can act on the call (retry, time to
headers, logging) and on the returned stream (first-token and idle timeouts, usage metrics).

| Layer | Behavior |
| --- | --- |
| `Retry::connect(n)` | Retries connection failures, where the request never reached the server |
| `Retry::transient(n)` | Retries retryable kinds with backoff, honoring `Retry-After` |
| `Timeout::first_token(d)` | Fails with `Timeout` if no text or reasoning arrives within `d` |
| `Timeout::idle(d)` | Fails with `Timeout` if the stream is silent for `d` |
| `Timeout::total(d)` | Fails with `Timeout` if the answer is not complete within `d` |
| `Trace` | Spans and events through `tracing` (feature `tracing`) |

No retry happens after an event has been delivered (D16).

### 5.3 Tools

svir has no tool macro (D18). A tool set is anything implementing `Toolbox`: it describes tools
to the model (`Request::tools(&toolbox)`) and answers one call at a time (`toolbox.call(&call)`).

- **`Tools`**, in svir, is a plain registry: `Tools::new().add(name, description, handler)`, the
  handler taking arguments deserialized with serde, the schema derived with the `schemars` feature
  or given explicitly. It validates arguments and turns a failure into a tool result for the model.
  `call.parse::<T>()` parses raw arguments for callers without a registry.
- **MCP tools** come through neva, which implements `Toolbox` behind its `svir` feature. A function
  written once with `#[neva::tool]` can be served over MCP and handed to a model in the same
  process, and the tools of a remote MCP server can be handed to a model through `neva::Client`.
  The exact neva API is neva's to decide.

The pieces also compose with no bridge. An MCP tool whose handler talks to models needs only neva
and a `Client`, injected through neva's dependency injection:

```rust
#[neva::tool(descr = "Ask another model")]
async fn ask(models: Dc<Models>, model: String, prompt: String) -> Result<String, Error> {
    let answer = models.get(&model)?.complete(Request::new(model).user(prompt)).await?;
    Ok(answer.text)
}
```

### 5.4 Crates and features

One crate, `svir`, with no procedural macros (D14). Without features: types and the codec.
`client` (default): `Client`, `EventStream`, and the reqwest and tokio transport. `schemars`,
`tracing`, and `testing` (a `MockServer` and scripted event streams for the caller's own tests)
are opt-in.

## 6. Security

- Credentials are configured by the caller (`api_key`, `api_key_env`, `api_key_file`), held as a
  `Secret`, and sent only as a sensitive `Authorization` header. They never appear in `Debug`,
  `Display`, errors, or events. svir never reads environment variables or `.env` files
  implicitly (D17).
- Plain HTTP to a non-loopback host is refused unless the caller opts in (P4).
- Tool descriptions and tool arguments are data from the model. svir executes nothing on its own:
  a `Toolbox` runs only handlers the caller registered, and `Tools` validates arguments against
  their schemas first.

## 7. Testing

The default test run needs no model, network, or credentials: recorded SSE fixtures, and a local
loopback HTTP server for transport behavior. Tests against a live model are opt-in. The same
scripted server is published as `svir::testing::MockServer` for callers' own tests (D14).

The conformance suite comes before the code. It is data, independent of the API: see
[tests/conformance](../tests/conformance/README.md). What is still to be written is listed in
[roadmap.md](roadmap.md#conformance-suite).
