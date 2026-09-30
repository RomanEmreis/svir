# Wire protocol: OpenAI-compatible Chat Completions

What svir needs to know about `POST /v1/chat/completions` with `stream: true`, as observed across
LM Studio, mlx-lm, llama.cpp, vLLM, and the OpenAI streaming schema. This is a reference of facts
and observed server behavior. How svir handles each fact is in [architecture.md](architecture.md).

## 1. Endpoints and base URL

- `POST {base}/v1/chat/completions` generates; `GET {base}/v1/models` lists models.
- People write the base URL both with and without `/v1`, often with a trailing slash. Normalize
  both forms, or `/v1/v1/models` follows.
- A base URL must not carry userinfo, a query, or a fragment.
- Authentication, when enabled, is `Authorization: Bearer <token>`. LM Studio accepts
  unauthenticated requests by default.

## 2. Request body

### 2.1 Fields

| Field | Notes |
| --- | --- |
| `model` | Exact model ID as the server lists it. LM Studio answers a request for an ID it does not know with a loaded model instead of an error |
| `messages` | See 2.2 |
| `stream` | `true` |
| `max_tokens` | Output cap |
| `temperature` | `0.0..=2.0` |
| `n` | `1`; more than one choice is not supported |
| `tools`, `tool_choice` | See 2.4 |
| `stream_options` | `{"include_usage": true}` requests a trailing usage chunk. Optional: see 2.5 |
| `reasoning_effort` | `none`, `low`, `medium`, `high`, `xhigh`. Optional: see 2.5 |

### 2.2 Messages

- **Text only**: `content` is a plain string. Not every server accepts the array form, so use it
  only when an image requires it.
- **With images**: `content` is an array of `text` and `image_url` parts. svir's layout is P12 in
  [decisions.md](decisions.md): one text part, then the images; no empty text part.
- **Images** go inline as data URLs:
  `{"type":"image_url","image_url":{"url":"data:<media type>;base64,<data>"}}`.
- **Text files** go as text inside the message; local servers do not take files any other way.
  svir wraps each in `<file name="<name>">\n<contents>\n</file>` (P12).
- **Assistant turns with tool calls** carry
  `"tool_calls":[{"id":...,"type":"function","function":{"name":...,"arguments":"<raw string>"}}]`
  next to `content`.
- **Tool results** are `{"role":"tool","tool_call_id":"<id>","content":"<string>"}`. Structured
  results are serialized into the string.
- **Reasoning sent back**: when a server returned reasoning as `reasoning_content` or
  `reasoning`, the next request repeats it on that assistant message under the same key,
  unchanged.

### 2.3 Size

Images are the large part: a request with several photos is tens of megabytes of base64. Building
the body in memory holds each image twice (bytes and base64) for the whole request. Streaming it
from disk needs the exact length up front, because not every server accepts a chunked body:

- base64 of `n` bytes is `4 * ceil(n / 3)`;
- the JSON-escaped length of text is computable byte by byte: `"`, `\`, `\n`, `\r`, `\t`,
  backspace, and form feed take 2 bytes, other control bytes below `0x20` take 6 (`\u00XX`), every
  other byte takes 1.

### 2.4 Tools

```json
"tools": [{"type": "function", "function": {"name": "...", "description": "...", "parameters": {}}}],
"tool_choice": "auto"
```

A model without tool support may ignore tools or fail. Tool calling needs a tool-capable model
and must be verified per model.

### 2.5 Optional fields

LM Studio accepts `reasoning_effort` and `stream_options`. A stricter server (plain mlx-lm or
llama.cpp builds) may reject the whole request with 400 or 422 because of either field. Nothing
was generated in that case, so retrying without them is safe.

## 3. Response stream

### 3.1 SSE framing

- Events are separated by a blank line. Lines end in LF or CRLF.
- `data:` lines carry the payload; one leading space after the colon is not part of it. Several
  `data:` lines in one event join with `\n`.
- Lines starting with `:` are comments (keep-alives). `id:`, `retry:`, and `event: message` also
  occur. `event: error` reports a failure (3.5). Other event types are not part of this protocol.
- Chunk boundaries fall anywhere: inside a line, inside a JSON string, inside a UTF-8 character.
  A line break never falls inside a UTF-8 character, so a complete line is complete text.
- The stream ends with `data: [DONE]`.

### 3.2 Chunks

```json
{"id":"...","model":"...","choices":[{"index":0,"delta":{...},"finish_reason":null}]}
```

- `id` and `model` stay the same for the whole stream.
- With `n: 1` there is exactly one choice, index `0`.
- Delta keys: `role` (`"assistant"`, first chunk), `content`, `reasoning_content`, `reasoning`,
  `tool_calls`. Keys can be present with `null`. Other keys (`audio`, `refusal`, deprecated
  `function_call`) are features outside this protocol subset.
- `finish_reason` appears once, on the last choice chunk: `stop`, `tool_calls`, or `length`.
  `content_filter` and others exist in the wider schema.
- A server that separates reasoning itself leaves what followed it in the answer: LM Studio's
  first `content` delta after reasoning is `"\n\n"`, also ahead of tool calls. It is part of the
  text the server sent.

### 3.3 Tool calls

Tool calls arrive in pieces and must be assembled:

- Each piece has an `index`. The first piece of a call usually carries `id`, `type: "function"`,
  and the start of `function.name`; later pieces carry more of `function.name` and
  `function.arguments`, keyed only by `index`.
- Names can be split across pieces, not just arguments.
- Pieces of different calls interleave, in any index order.
- A complete response has contiguous indices from 0, a non-empty unique `id` and a non-empty name
  for every call.
- `finish_reason` is `tool_calls` exactly when there are calls, except for `length`, which can
  cut off either.
- Arguments are only meaningful once the stream is complete. A call is executable only after the
  finish reason and `[DONE]`; a stream cut before `[DONE]` may have incomplete arguments.

Recorded example (two calls, split names and arguments, interleaved):

```text
data: {"id":"turn-1","model":"m","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"..."},"finish_reason":null}]}

data: {"id":"turn-1","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-a","type":"function","function":{"name":"look","arguments":"{\"value\":"}},{"index":1,"id":"call-b","type":"function","function":{"name":"lookup","arguments":"{\"value\":"}}]},"finish_reason":null}]}

data: {"id":"turn-1","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"2}"}},{"index":0,"function":{"name":"up","arguments":"1}"}}]},"finish_reason":null}]}

data: {"id":"turn-1","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"id":"turn-1","model":"m","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}}

data: [DONE]
```

Result: `call-a` is `lookup({"value":1})`, `call-b` is `lookup({"value":2})`.

### 3.4 Usage

- With `stream_options.include_usage`, usage arrives in a separate chunk after the finish reason,
  with an empty `choices` array, before `[DONE]`.
- Fields: `prompt_tokens`, `completion_tokens`, `total_tokens`, and optionally
  `completion_tokens_details.reasoning_tokens`.
- A server that does not support it sends no usage at all; the answer is still complete.
- Usage is reported once.

### 3.5 Errors inside the stream

vLLM and llama.cpp send an error chunk inside an open stream when generation dies midway:

```json
{"error": {"message": "..."}}
```

LM Studio sends an SSE event of type `error`, with status 200, and nothing after it, not even
`[DONE]`:

```text
event: error
data: {"error":{"message":"..."},"message":"..."}
```

A prompt longer than the loaded context is reported this way, before anything is generated, and
without an error code: only the message says what happened ("The number of tokens to keep from
the initial prompt is greater than the context length...").

Everything received before an error is a partial answer, not a complete one.

### 3.6 Speed

Tokens per second is meaningful only from the first visible token to the last, so the wait for
the first token does not drag it down. A chunk that was only a `<think>` marker, or part of one,
produced nothing visible and does not count as a token. With one token, or a window shorter than
about 50 ms, there is no honest rate.

## 4. Reasoning

Three carriers exist:

| Carrier | Where | Sent back as |
| --- | --- | --- |
| `reasoning_content` | Delta key; LM Studio and servers with a reasoning parser | The same key on the assistant message |
| `reasoning` | Delta key; some servers | The same key on the assistant message |
| `<think>...</think>` | Inside `content`, from servers without a reasoning parser (llama.cpp, mlx) | Nothing in a request field; svir does not send it back (D23 in [decisions.md](decisions.md)) |

Treating inline `<think>` as answer text shows the reasoning to the user as the answer, and sends
it back to the model on every turn. The markers can be split across chunks anywhere, for example
`a<thi` then `nk>b</think>c`, which is answer `ac` and reasoning `b`. An unfinished marker at the
very end (`done<thi`) is text.

## 5. HTTP errors

| Status | Meaning |
| --- | --- |
| 401, 403 | Authentication. Some servers stall the error body; do not wait for it. |
| 429 | Rate limited. `Retry-After` may be present, in seconds. |
| 408, 504 | Timeout |
| 500, 502, 503 | Transient server or gateway failure |
| 400, 413, 422 | Request rejected. The body is `{"error":{"code":...,"message":...}}` on most servers |
| other | Not expected from this API |

Servers agree on no single sign of a context overflow:

| Server | How it says it |
| --- | --- |
| Servers that follow the OpenAI error schema | 400, 413, or 422 with `error.code` of `context_length_exceeded` or `context_window_exceeded`; the message speaks of the "maximum context length" |
| llama.cpp | 400 with a numeric `error.code`, `error.type` of `exceed_context_size_error`, and "the request exceeds the available context size" |
| LM Studio | Status 200 and an `event: error` inside the stream (3.5), with only a message: "...greater than the context length..." |

An upstream `401` in a proxy is ambiguous: it can mean the proxy's own session or the model
server's key. A proxy has to keep the two apart.

A successful response must be `text/event-stream`. Anything else is not a stream, whatever the
status says.

## 6. Model listing

`GET /v1/models` returns `{"data":[{"id":"...","name":"..."}]}`; `name` is optional. Servers list
embedding, reranking, speech, and image models next to chat models. There is no standard field
telling them apart; the practical filter is a name heuristic (`embed`, `rerank`, `bge`, `clip`,
`whisper`, `tts`, `asr`, `speech`, `audio`, `image`, `flux`, `sdxl`, `diffusion`).

Sources: [LM Studio Chat Completions](https://lmstudio.ai/docs/developer/openai-compat/chat-completions),
[LM Studio tool use](https://lmstudio.ai/docs/developer/openai-compat/tools),
[LM Studio authentication](https://lmstudio.ai/docs/developer/core/authentication),
[Chat Completions streaming events](https://developers.openai.com/api/reference/resources/chat/subresources/completions/streaming-events).
