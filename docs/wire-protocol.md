# Wire protocol: OpenAI-compatible Chat Completions

What svir needs to know about `POST /v1/chat/completions` with `stream: true`, as observed across
LM Studio, mlx-lm, llama.cpp, vLLM, Azure OpenAI, and the OpenAI streaming schema. This is a reference of facts
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
| `response_format` | See 2.6 |
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
  results are serialized into the string. There is no field that marks a failed call; the content
  has to say so.
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

`tool_choice` is `"auto"` (the model decides), `"none"` (no call), `"required"` (at least one
call), or `{"type": "function", "function": {"name": "..."}}` (a call of that tool). The default
is `none` without tools and `auto` with them. Azure OpenAI was reported to reject a
`tool_choice` sent without `tools` with 400.

LM Studio takes the strings only. A named function is rejected with 400 and
`{"error":"Invalid tool_choice type: 'object'. Supported string values: none, auto, required"}`.
`required` is accepted and not kept: asked to say hello with a tool required, the model said
hello, finished with `stop`, and called nothing.

A model without tool support may ignore tools or fail. Tool calling needs a tool-capable model
and must be verified per model.

### 2.5 Optional fields

LM Studio accepts `reasoning_effort` and `stream_options`. A stricter server (plain mlx-lm or
llama.cpp builds) may reject the whole request with 400 or 422 because of either field. Nothing
was generated in that case, so retrying without them is safe.

### 2.6 Structured output

```json
"response_format": {"type": "json_object"}
"response_format": {"type": "json_schema", "json_schema": {"name": "...", "schema": {}, "strict": true}}
```

- `json_object` asks for a JSON object of any shape. OpenAI rejects it unless the word "JSON"
  appears somewhere in the messages.
- `json_schema` asks for JSON that matches `schema`. `name` is required, of ASCII letters,
  digits, `_`, and `-`, at most 64 characters.
- With `strict: true`, OpenAI and Azure OpenAI guarantee an answer that matches, and accept only
  schemas in which every object lists all of its properties under `required` and sets
  `additionalProperties: false`. Without it the schema guides the model but binds nothing.
- Local servers constrain sampling to the schema: LM Studio compiles it into a grammar
  (llama.cpp) for GGUF models and uses Outlines for MLX models.
- LM Studio takes `json_schema` and `text` only. `json_object` is rejected with 400 and
  `{"error":"'response_format.type' must be 'json_schema' or 'text'"}`, its error a plain string.
- LM Studio holds a reasoning model's reasoning to the schema as well, from its first token. With
  reasoning on (`reasoning_effort` unset, `low`, or `medium`), the whole JSON arrives as
  `reasoning_content` and `content` stays empty; with `reasoning_effort: "none"` it arrives as
  `content`. Unset, the model also answered wrongly, as if it had not read the question. Known
  and open in LM Studio's tracker (#1698, #1773, #1971), for GGUF and MLX models alike.
- Otherwise the answer arrives as ordinary `content` deltas. An answer cut off by `length` is not
  valid JSON.

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
  `refusal`, `tool_calls`. Keys can be present with `null`. Other keys (`audio`, deprecated
  `function_call`) are features outside this protocol subset.
- `refusal` is the model's refusal to answer, in place of `content`: a string, in pieces as
  `content` comes, with `content` null and the finish `stop`. OpenAI sends it above all when it
  will not give an answer in the format asked for (2.6). Azure sends `"refusal": null` on its
  first delta (3.7).
- `finish_reason` appears once, on the last choice chunk: `stop`, `tool_calls`, `length`, or
  `content_filter`, when a content filter stopped the answer (3.7). Others exist in the wider
  schema, such as the deprecated `function_call`.
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

### 3.7 Content filtering

Azure OpenAI runs a content filter over the prompt and the answer, and reports on both in the
stream. Recorded from an Azure AI Foundry deployment in its default streaming mode
(`tests/conformance/decoder/azure.sse`):

- The first chunk reports on the prompt: `choices` is empty, `id` and `model` are empty strings,
  `created` is `0`, and `prompt_filter_results` holds the verdict per category. It carries
  nothing of the answer. Earlier API versions name the field `prompt_annotations`.
- Every choice carries `content_filter_results`, sometimes `{}`. The first delta carries
  `"refusal": null` next to `role` and an empty `content`.
- Chunks carry `obfuscation` (random padding), `service_tier`, `system_fingerprint: null`, and
  `usage: null` until the usage chunk, which adds `latency_checkpoint` and `routing`.
- `model` in the chunks is the model version (`gpt-6-luna-2026-09-22`), not the deployment name
  the request named.

A block in this mode, recorded with a blocklist the filter applied per request (the
`x-policy-id` header names a content filter for one request; it applies the filter's lists but
not its streaming mode):

- The answer streamed with the blocklisted word in it, three times, before the block. Azure's
  documentation says that in the default mode what came before a block passed the filter; the
  recording does not bear that out.
- The block is an ordinary chunk of the stream, with its `id` and `model`, an empty delta,
  `finish_reason: "content_filter"`, and the verdict in `content_filter_results`
  (`"custom_blocklists":[{"filtered":true,"id":"svir"}]`). `[DONE]` follows at once: no usage
  chunk, although the request asked for usage.

A deployment can opt into an asynchronous filter, set on the content filter assigned to it: the
answer streams unvetted, and annotation chunks report on it. Recorded from the same deployment
with such a filter (`azure-async.sse`, `azure-async-flagged.sse`):

- An annotation is a choice with `content_filter_offsets` and `content_filter_results` and no
  `delta`, in a chunk whose `id`, `model`, and `object` are empty and whose `created` is `0`. It
  has no `usage` key. Each carries the results of one filter:

  ```text
  data: {"choices":[{"content_filter_offsets":{"check_offset":140,"start_offset":140,"end_offset":259},"content_filter_results":{"custom_blocklists":[{"filtered":true,"id":"svir"}]},"finish_reason":null,"index":0}],"created":0,"id":"","model":"","object":""}
  ```

- Annotations interleave with the answer, repeat, and follow the model's finish: after `stop`
  come the last annotations, then the usage chunk, then `[DONE]`.
- A block caught while the answer streams is an annotation with
  `finish_reason: "content_filter"`, and `[DONE]` follows at once, with no usage chunk. The
  blocked word had streamed four times before it.
- A block caught after the model's `stop` comes with no finish reason: an annotation whose
  verdict is `filtered: true`, then the usage chunk and `[DONE]`. Only the verdict says that the
  answer, which held the blocked word ten times, was blocked. Azure's documentation shows a
  `content_filter` finish in an annotation after the model's finish instead; that was not seen.
- A filter set to annotate only reports `detected: true` with `filtered: false`.
- The offsets do not behave as documented. `check_offset` is documented as how much text is
  fully moderated, never decreasing; recorded, it stayed at one value for the whole answer,
  about the length of the prompt as the server rendered it. `start_offset` and `end_offset` mark
  the text an annotation applies to, counted from the same start, and go back and forth from one
  annotation to the next.

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

Azure OpenAI rejects a prompt its content filter blocks with 400 and `error.code` of
`content_filter`, as `application/json`, also for a streamed request. `innererror` holds the
verdict per category. Nothing was generated, but the prompt's evaluation is billed, so sending it
again costs again. Recorded from an Azure AI Foundry deployment, a prompt its jailbreak shield
blocked (the message shortened here):

```json
{"error":{"message":"The response was filtered due to the prompt triggering Azure OpenAI's content management policy. ...","type":null,"param":"prompt","code":"content_filter","status":400,"innererror":{"code":"ResponsibleAIPolicyViolation","content_filter_result":{"hate":{"filtered":false,"severity":"safe"},"jailbreak":{"detected":true,"filtered":true},...}}}}
```

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
[LM Studio structured output](https://lmstudio.ai/docs/developer/openai-compat/structured-output),
[LM Studio: the schema held to the reasoning](https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/1773),
[Structured outputs](https://developers.openai.com/api/docs/guides/structured-outputs),
[LM Studio authentication](https://lmstudio.ai/docs/developer/core/authentication),
[Chat Completions streaming events](https://developers.openai.com/api/reference/resources/chat/subresources/completions/streaming-events),
[Azure OpenAI content streaming](https://learn.microsoft.com/en-us/azure/foundry/openai/concepts/content-streaming),
[Azure content filtering](https://learn.microsoft.com/en-us/azure/ai-foundry/openai/concepts/content-filter).
