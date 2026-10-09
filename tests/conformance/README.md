# Conformance suite

Behavior vectors as data: recorded inputs and the outcome svir must produce for each. The suite
does not depend on svir's API. A Rust runner arrives with the code it tests, and any other
implementation can run the same vectors.

Expectations follow [docs/decisions.md](../../docs/decisions.md). A case whose expectations depend
on an open decision lists it in `open`.

| Directory | Covers | Status |
| --- | --- | --- |
| `decoder/` | SSE framing, deltas, reasoning, refusals, tool calls, usage, limits, errors inside the stream, strict and lenient | 81 cases, run by `tests/conformance_decoder.rs` |
| `encoder/` | Request bodies, attachments, exact length, reasoning and tool round trips, tool choice and response format, admission | 39 cases, run by `tests/conformance_encoder.rs` |
| `transport/` | Authentication, extra headers, status mapping, timeouts, cancellation, base URLs, model listing, compatibility learning | 58 cases, run by `tests/conformance_transport.rs` |

## Decoder cases

One JSON file per case. Recorded fixtures (`*.sse`) sit next to the cases that use them.

### Input

| Field | Meaning |
| --- | --- |
| `description` | What the case is about. Required. |
| `file` | A fixture in the same directory. Exactly one of `file` and `chunks` is present. |
| `chunks` | The input as a list of pieces. A string is sent as its UTF-8 bytes; `{"hex": "ff"}` is raw bytes, for input that is not valid UTF-8. |
| `replace` | `file` only. `[[from, to], ...]`, applied in order to the fixture text, replacing every occurrence. Each `from` must occur at least once. |
| `cut_before` | `file` only. The input ends just before the first occurrence of this text, which must occur. |
| `line_endings` | `"lf"` and/or `"crlf"`. `"crlf"` turns every LF into CRLF. Default: both for `file`, `["lf"]` for `chunks`. |
| `strides` | The whole input is joined and re-split into pieces of this many bytes, once per stride. Default: `[1, 3, 8192]` for `file`. For `chunks`, the default is to push each chunk as given. |
| `options` | Decoder options (see below). |
| `open` | IDs of open decisions the expectations depend on. A failure is reported as pending, not as a failure. |
| `expect` | `{"strict": <outcome>, "lenient": <outcome> or "same"}`. `"same"` repeats the strict outcome. |

`options`:

- `think`: `"split"` routes `<think>...</think>` in the content to reasoning; `"keep"` leaves it in
  the text. The default is `"split"` (D23); cases with `<think>` tags state it anyway.
- `limits`: any of `wire_bytes` (default 67108864), `event_bytes` (default 262144), and
  `tool_calls` (default 64).

A case runs once for every combination of mode, line ending, and stride.

### Running a case

1. Build the input: read `file` and apply `replace` and `cut_before`, or take `chunks`. Convert
   line endings. Re-split by stride if `strides` applies.
2. Push the pieces in order, collecting what the decoder delivers after each push.
3. Stop pushing once `Completed` is delivered: bytes after `[DONE]` are not read (P9).
4. After the last piece, signal the end of input. No `Completed` by then is `truncated_stream`.

The outcome must not depend on line endings or strides (P9): events arrive in wire order, and an
error is delivered after every event decoded before it.

### Outcome

A completion:

```json
{"completion": {"finish": "stop", "text": "", "reasoning": [], "calls": [], "usage": null}}
```

| Field | Meaning |
| --- | --- |
| `finish` | `stop`, `tool_calls`, `length`, `content_filter`, or `refusal`. Required. |
| `text` | The answer. Default `""`. |
| `reasoning` | `[{"source": ..., "text": ...}]`, one entry per carrier in order of first appearance, text concatenated. `source` is `reasoning_content`, `reasoning`, or `think`. Default `[]`. |
| `calls` | `[{"id": ..., "name": ..., "arguments": ...}]` in index order; `arguments` is the raw string. Default `[]`. |
| `usage` | `{"input": n, "output": n}` plus `total` and `reasoning` when the server reported them. Default `null`. |

Fields are compared exactly after defaults are filled in. This notation is the serde form of
svir's `Completion`, and error kinds are the serde form of `ErrorKind`; `tests/serde_contract.rs`
keeps them equal.

An error:

```json
{"error": "truncated_stream", "partial": {"text": "", "reasoning": ""}, "message": "..."}
```

| Field | Meaning |
| --- | --- |
| `error` | The kind: `transport`, `timeout`, `rate_limited`, `truncated_stream`, `authentication`, `context_overflow`, `content_filter`, `protocol`, `server`, `unsupported`, or `response_limit`. |
| `partial` | Text and reasoning deltas delivered before the error, each concatenated. Each field is compared only when present. |
| `message` | The server's own message. Compared only when present. |

Either outcome may add `visible_by_chunk`: one boolean per pushed chunk, true when that push
delivered at least one non-empty text or reasoning delta. Valid only for `chunks` pushed as given.

`{"open": "O7"}` in place of an outcome means there is no expectation yet for that mode; the
runner skips it.

### Fixtures

| File | Content |
| --- | --- |
| `tools.sse` | Reasoning, then two tool calls whose names and arguments are split and whose pieces interleave out of index order; `tool_calls` finish; trailing usage chunk. |
| `final.sse` | A short text answer, `stop` finish, trailing usage chunk. |
| `final-no-usage.sse` | `final.sse` without the usage chunk. |
| `azure.sse` | Recorded from Azure OpenAI: a prompt report with no choices and an empty `id` and `model`, content-filter results on every choice, `stop` finish, usage chunk with reasoning tokens. |
| `azure-async.sse` | Recorded from Azure OpenAI with the asynchronous content filter: annotations that block nothing interleave with the answer and follow its `stop`, before the usage chunk. |
| `azure-async-flagged.sse` | Recorded from Azure OpenAI with the asynchronous content filter and a blocklist: the answer holds the blocked word, the model finishes with `stop`, and an annotation after it marks the blocklist `filtered: true` with no finish reason. |
| `mlx-lm.sse` | Recorded from mlx-lm: SSE comments while the prompt is read, reasoning in the `reasoning` key, the role on every delta, `stop` finish, usage in a chunk with no choices whose `object` is `chat.completion`. The home directory in the model path is replaced. |
| `mlx-vlm.sse` | Recorded from mlx-vlm: every reasoning delta carries the same text under `reasoning_content` and `reasoning`, every delta carries all its keys, `null` when unused, and every chunk adds `timings`; `stop` finish, usage chunk with no choices. The home directory in the model path is replaced. |
| `llama-cpp-error.sse` | Recorded from llama.cpp: reasoning in `reasoning_content`, then an error object with a numeric `code` and type `server_error` where the server's parser failed, and no `[DONE]`. The home directory in the model path is replaced. |
| `vllm-named-tool.sse` | Recorded from vLLM with its reasoning and tool-call parsers, asked to call a named function: reasoning in `reasoning`, one call, a `stop` finish, usage with reasoning tokens. |

## Requests

Encoder and transport cases describe a request in a neutral form. It is the suite's notation, not
svir's API:

```json
{
  "model": "m",
  "max_tokens": 16,
  "temperature": 0.5,
  "reasoning_effort": "low",
  "include_usage": true,
  "tools": [{"name": "lookup", "description": "...", "input_schema": {"type": "object"}}],
  "tool_choice": {"tool": "lookup"},
  "response_format": {"schema": {"name": "weather", "schema": {"type": "object"}, "strict": true}},
  "messages": [{"role": "user", "parts": [{"text": "hi"}]}]
}
```

Everything but `model` and `messages` is optional and present only when the request sets it.
`tool_choice` is `auto`, `none`, `required`, or `{"tool": name}`. `response_format` is `text`,
`json`, or `{"schema": {"name": ..., "schema": ..., "strict": true}}`, where `strict` is present
only when true. Both are the serde form of svir's `ToolChoice` and `ResponseFormat`;
`tests/serde_contract.rs` keeps them equal. `role` is `system`, `user`, `assistant`, or `tool`.
Parts:

| Part | Where | Meaning |
| --- | --- | --- |
| `{"text": ...}` | any | Text |
| `{"image": key}` | user | An image from `attachments` |
| `{"file": key}` | user | A text file from `attachments` |
| `{"reasoning": {"source": ..., "text": ...}}` | assistant | Reasoning received with this answer |
| `{"tool_call": {"id": ..., "name": ..., "arguments": ...}}` | assistant | A call the model made; `arguments` is the raw string |
| `{"tool_result": {"call_id": ..., "content": ..., "is_error": true}}` | tool | The caller's result for a call, as a string; `is_error` marks a failed call and is present only when true |

## Encoder cases

| Field | Meaning |
| --- | --- |
| `description` | What the case is about. Required. |
| `request` | The request, as above. Required. |
| `attachments` | `key -> attachment`: `media_type` (images) or `name` (files), content as `hex` or `text`, and optionally `declared_size` and, for a file, `escaped_len`: its length once escaped into a JSON string, as the caller declares it. |
| `options` | `lean`: leave out the optional fields. `send_reasoning`: send reasoning back. `block_bytes`: block sizes to read attachments in (multiples of 3). `context_tokens`: check admission. |
| `open` | As for decoder cases. |
| `expect` | `{"body": <JSON>}`, `{"error": "attachment", "context_overflow", or "unsupported"}`, or `{"open": id}`. |

Running a case:

1. Write each attachment to a file. One with a `declared_size` changed after it was recorded:
   write a file of that size, build the body, then write the content the case gives before
   reading the body. An implementation that also takes attachments in memory runs those cases
   without a `declared_size` that way too.
2. Produce the body once for each size in `block_bytes`, and once with the implementation's
   default block size.
3. For `body`: the number of bytes streamed equals the declared length; the bytes parse as JSON
   and equal the expected value (object key order does not matter); producing the body again
   gives identical bytes.
4. For `attachment`: building the body fails, or its stream fails instead of completing with
   bytes that disagree with the declared length or are not UTF-8 where text is required.
5. For `unsupported`: building the body fails, before any byte is sent. The request asks for
   something the body cannot carry, such as a call of a tool it does not offer.
6. With `context_tokens`: the request is admitted exactly when `max_tokens` is above 0 and the
   declared length plus `max_tokens` is at most `context_tokens`. Otherwise the outcome is
   `context_overflow`, before any byte is sent.

## Transport cases

A scripted HTTP server on loopback plays the model server.

| Field | Meaning |
| --- | --- |
| `description` | What the case is about. Required. |
| `config` | `base_url` (a string, or a list to run the case once per value), `api_key`, `headers` (`name -> value`, added to every request), `timeout_ms`, `allow_http`. `{server}` stands for the scripted server's origin, such as `http://127.0.0.1:52811`. |
| `expect_config` | `"accepted"` or `"rejected"`: only construct the transport from `config`. No server, no calls. |
| `server` | Replies, in order, one per HTTP request the server receives. |
| `calls` | Calls made in order on one transport instance. |
| `expect_requests` | What the server received, one entry per request in order, or `{"open": id}`. |
| `expect_closed` | The server sees the connection closed within 1 second of the last client action. |
| `open` | As for decoder cases. |

A reply is `{"status", "headers", "body" or "body_file", "cut_before", "then"}` or
`{"stall": "headers"}`:

- `body_file` is relative to the case file; `cut_before` truncates the body before the first
  occurrence of its text.
- The server sends `connection: close` and no `Content-Length`; the body ends when it closes the
  connection. With `"then": "stall"` it keeps the connection open after the body instead, until
  the client closes it.
- `{"stall": "headers"}` reads the request and never answers.
- `{server}` in a header value is replaced with the server's origin.
- A request beyond the script gets a 500 and fails the case.

A call is `{"request", "list_models", "client", "expect"}`:

- `request` defaults to model `m` with one user message `hi`. `"list_models": true` lists models
  instead of generating.
- `client` is a list of actions, by default `["read_all"]`: `"read_all"` reads until a completion
  or an error, `{"read_events": n}` reads n events, `{"read_for_ms": n}` reads for at most n
  milliseconds, and `"cancel"` drops the stream.
- `expect` is a decoder outcome, with these additions:
  - errors carry `retryable`, `retry_after_ms` (`null` when there is no delay), and `status`:
    the HTTP status of a response that was not a success, `null` for a failure that is not a
    status, such as a timeout or an error inside a stream that began with `200`;
  - `{"cancelled": true, "partial": ...}` after `"cancel"`;
  - `{"models": [{"id": ..., "name": ...}]}` for a listing (`name` is `null` when absent);
  - `within_ms`: the outcome arrives within this many milliseconds of the call starting.

An entry of `expect_requests` may check `method`, `path`, `headers` (exact values, lowercase
names), `absent_headers`, `body_includes` (top-level body keys with equal values), and
`body_excludes` (top-level body keys that must be absent).

For every case, whatever it lists:

- every POST carries `content-type: application/json` and a `content-length` equal to the body,
  never `transfer-encoding: chunked`;
- the API key, and the value of a header from `headers`, never appear in the `Debug` or
  `Display` output of an error, an event, or the configuration;
- the stream is decoded in strict mode;
- the client's default of asking for usage is off, so a request carries an optional field only
  when the notation sets it.

## Conventions

- Files are ASCII. Non-ASCII test text is written with JSON `\u` escapes and reaches the decoder as
  UTF-8 bytes; raw invalid bytes use `{"hex": ...}`.
- Fixtures use LF; CRLF variants come from `line_endings`. Transport cases reuse the decoder
  fixtures through `body_file`.
- One behavior per case. The file name says which, and the description says why it matters.
- A new case states both modes explicitly, or `"same"`.

Not covered as data: tokens-per-second calculation needs a clock, so it is a unit test of the
timing helper (P7).
