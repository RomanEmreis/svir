# Wire protocol: OpenAI-compatible Chat Completions

What svir needs to know about `POST /v1/chat/completions` with `stream: true`, as observed across
LM Studio, mlx-lm, mlx-vlm, llama.cpp, vLLM, Azure OpenAI, and the OpenAI streaming schema. This is a reference of facts
and observed server behavior. How svir handles each fact is in [architecture.md](architecture.md).

What is said of mlx-lm and mlx-vlm was observed with mlx-lm 0.32.0 and mlx-vlm 0.7.6 serving
Qwen3.8 27B (MLX, 4-bit), a model that reasons, calls tools, and sees images. What is said of
llama.cpp was observed with build b11429 serving Muse Glimmer 30B (GGUF, Q4_K_M) in a 16384-token
context, with and without the model's image projector. What is said of vLLM was observed with
vLLM 0.30.0 on Apple Silicon (the vllm-metal 0.30.0 platform plugin) serving the same Qwen3.8
model, with and without its reasoning and tool-call parsers. What was read in a server's code
instead says so.

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
| `model` | Exact model ID as the server lists it. LM Studio answers a request for an ID it does not know with a loaded model instead of an error. mlx-lm and mlx-vlm list a model loaded from a folder under its absolute path; in their code, they load the model a request names when it is not the loaded one. llama.cpp, serving one model, answers a request for any ID with it, and names it in the chunks |
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
- mlx-lm takes text parts only. A message with an image is rejected with 404 and
  `{"error": "Only 'text' content type is supported."}`, a JSON body also for a streamed request.
  mlx-vlm takes images.
- llama.cpp takes images with the model's image projector loaded (`--mmproj`), and only then.
  Without it, an image is rejected with 500 and
  `{"error":{"code":500,"message":"image input is not supported - hint: if this is unexpected, you may need to provide the mmproj","type":"server_error"}}`,
  the status of a transient failure for a request the server will never take.
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

mlx-lm reads no `tool_choice`. Any value is accepted with 200 and changes nothing, even one the
protocol does not have (`"bogus"`): with `none`, asked to open a locker, the model called the
locker tool; with a named function, asked to say hello, it said hello.

mlx-vlm keeps `tool_choice` through the prompt, not by constraining sampling. In its code, `none`
leaves the tools out of it, and `required` or a named function adds an instruction to call one,
a named function offering only that tool. The model called the named tool. A value outside the
protocol is rejected with 400 and
`{"detail":"Invalid tool_choice. Expected 'none', 'auto', 'required', or a specific function."}`.

llama.cpp keeps `required` through its grammar: asked to say hello, the model reasoned that no
tool was needed and called one anyway. A named function is accepted and not kept: the model said
hello. With `none`, asked to open a locker, the model wrote a call all the same, and the server's
parser failed the stream (3.5). A value outside the protocol is rejected with 400 and
`{"error":{"code":400,"message":"Invalid tool_choice: bogus","type":"invalid_request_error"}}`.

vLLM calls tools only with a tool-call parser (`--enable-auto-tool-choice --tool-call-parser`).
Without one, a request with tools and no `tool_choice` is rejected with 400 and
`{"error":{"message":"\"auto\" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set","type":"BadRequestError","param":null,"code":400}}`,
and `required` or a named function with 400 and
`tool_choice=function "open_locker" requires --tool-call-parser to be set`; `none` is taken.
With the parser it keeps all four, and finishes a call of a named function with `stop` (3.3).

A model without tool support may ignore tools or fail. Tool calling needs a tool-capable model
and must be verified per model.

### 2.5 Optional fields

LM Studio, mlx-lm, mlx-vlm, llama.cpp, and vLLM accept `reasoning_effort` and `stream_options`. A
stricter server may reject the whole request with 400 or 422 because of either field. Nothing was
generated in that case, so retrying without them is safe.

What they do with them:

- mlx-lm reads no `reasoning_effort`, so the model reasons as its chat template does by default.
  Qwen3.8's template takes the effort, but mlx-lm hands the template only `chat_template_kwargs`;
  `"none"` did not stop the reasoning.
- mlx-vlm turns reasoning on and off with `reasoning_effort`: without it, and without the server's
  `--enable-thinking`, the model does not reason; with `low` it does.
- llama.cpp takes the effort, and the model reasoned at `none` as at `high`: its chat template
  reads its own variable, `reasoning_strength`, and not `reasoning_effort`.
- vLLM hands the effort to the chat template. With Qwen3.8's, `none` turned reasoning off, `low`
  was taken, and `high`, which the template does not name, was rejected with 400:
  "Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low."
  Sent again without the field, the request gets the template's default, `xhigh`.
- All of them send the usage chunk `stream_options` asks for.

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
- mlx-lm reads no `response_format`: asked for the locker schema, the model answered with a
  Markdown table. mlx-vlm keeps `json_schema` (the answer parsed) and takes `json_object`.
  llama.cpp and vLLM keep `json_schema`.
- Otherwise the answer arrives as ordinary `content` deltas. An answer cut off by `length` is
  incomplete and may still be valid JSON: a number at the root cut short, for one. The guide
  to structured output says to treat `length` as incomplete before parsing.

## 3. Response stream

### 3.1 SSE framing

- Events are separated by a blank line. Lines end in LF or CRLF.
- `data:` lines carry the payload; one leading space after the colon is not part of it. Several
  `data:` lines in one event join with `\n`.
- Lines starting with `:` are comments (keep-alives). mlx-lm sends `: keepalive <read>/<total>`
  while it reads the prompt, before the first chunk (`tests/conformance/decoder/mlx-lm.sse`).
  `id:`, `retry:`, and `event: message` also occur. `event: error` reports a failure (3.5). Other
  event types are not part of this protocol.
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
- llama.cpp's first delta is `{"role":"assistant","content":null}`, vLLM's
  `{"role":"assistant","content":""}`. vLLM adds `prompt_token_ids` and `prompt_text` to the
  first chunk and `token_ids` to the choice, all `null`. mlx-lm sends `role` on every delta.
  mlx-vlm sends every key of its message type on every delta, `null` when unused, `tool_call_id`
  and `name` among them, and adds `logprobs: null` to the choice and `usage: null` and `timings`
  (generation speed) to the chunk (`tests/conformance/decoder/mlx-vlm.sse`).
- `refusal` is the model's refusal to answer, in place of `content`: a string, in pieces as
  `content` comes, with `content` null and the finish `stop`. OpenAI sends it above all when it
  will not give an answer in the format asked for (2.6). Azure sends `"refusal": null` on its
  first delta (3.7).
- `finish_reason` appears once, on the last choice chunk: `stop`, `tool_calls`, `length`, or
  `content_filter`, when a content filter stopped the answer (3.7). Others exist in the wider
  schema, such as the deprecated `function_call`.
- A server that separates reasoning itself leaves what followed it in the answer: LM Studio's
  first `content` delta after reasoning is `"\n\n"`, also ahead of tool calls, and so is
  mlx-lm's. It is part of the text the server sent. mlx-vlm's answer starts with its first word.

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
  cut off either, and for a call of the function the request named: OpenAI and vLLM finish it
  with `stop` (`tests/conformance/decoder/vllm-named-tool.sse`), and `tool_calls` for `auto` and
  `required` (D45).
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
- mlx-lm's usage chunk has `object: "chat.completion"`, not `chat.completion.chunk`. mlx-lm and
  mlx-vlm report `prompt_tokens_details.cached_tokens` and no reasoning tokens; the reasoning is
  counted in `completion_tokens`, and so does llama.cpp. The usage chunks of mlx-vlm and llama.cpp
  add `timings`, with prompt and generation speed, and mlx-vlm's peak memory. vLLM with its
  reasoning parser reports `completion_tokens_details.reasoning_tokens`.

### 3.5 Errors inside the stream

vLLM and llama.cpp send an error chunk inside an open stream when generation dies midway:

```json
{"error": {"message": "..."}}
```

Recorded from llama.cpp (`tests/conformance/decoder/llama-cpp-error.sse`), when the model wrote
a tool call its parser did not expect: the error has a numeric `code` and the type
`server_error`, it is the last thing in the stream, and no `[DONE]` follows:

```json
{"error":{"code":500,"message":"The model produced output that does not match the expected peg-native format","type":"server_error"}}
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
| `reasoning_content` | Delta key; LM Studio, llama.cpp, and servers with a reasoning parser | The same key on the assistant message |
| `reasoning` | Delta key; mlx-lm, vLLM with its reasoning parser, and some other servers | The same key on the assistant message |
| `<think>...</think>` | Inside `content`, from servers without a reasoning parser, or with it turned off (llama.cpp's `--reasoning-format none`) | Nothing in a request field; svir does not send it back (D23 in [decisions.md](decisions.md)) |

mlx-lm separates the reasoning of a model whose think markers its tokenizer knows, as it does for
Qwen3.8, and sends it as `reasoning`. mlx-vlm sends every piece of reasoning twice in one delta,
the same text under `reasoning_content` and under `reasoning`, its alias for it: one text, not
two (D43).

A template can open the tag itself: Qwen3.8's ends the prompt with `<think>`. A server without a
reasoning parser then sends the reasoning as `content`, with a closing marker and no opening one.
vLLM without `--reasoning-parser` sent
`"The user wants me to count from one to five, spelling out the numbers in words. This is straightforward.\n</think>\n\nOne, two, three, four, five."`.
svir keeps that text as the answer (D23); the server's reasoning parser separates it.

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

Not every server reports an `error` object:

- mlx-lm sends `{"error": "<message>"}`, the error a plain string, and answers any failure before
  generation starts with 404, a content part it does not take among them (2.2).
- mlx-vlm sends `{"detail": "<message>"}`, and for a body it cannot read, 422 with a list of
  validation errors: `{"detail":[{"type":"list_type","loc":["body","messages"],"msg":"Input should be a valid list","input":"hi"}]}`.
  The message is `detail`, or the first error's `msg` (D44).

Servers agree on no single sign of a context overflow:

| Server | How it says it |
| --- | --- |
| Servers that follow the OpenAI error schema | 400, 413, or 422 with `error.code` of `context_length_exceeded` or `context_window_exceeded`; the message speaks of the "maximum context length" |
| llama.cpp | 400 with a numeric `error.code`, `error.type` of `exceed_context_size_error`, a message such as "request (20056 tokens) exceeds the available context size (16384 tokens), try increasing it", and `n_prompt_tokens` and `n_ctx` next to them. Observed for a streamed request too, as JSON before any stream |
| LM Studio | Status 200 and an `event: error` inside the stream (3.5), with only a message: "...greater than the context length..." |
| mlx-vlm | Only with a context limit set (`MAX_KV_SIZE` in its environment): 400 with only a `detail`, in several wordings. "Protected conversation exceeds the available context budget." when the last message alone does not fit; "Output reservation leaves no room for compacted context." when `max_tokens` alone leaves no room |
| mlx-lm | Has no context limit to set, and its code checks none |
| vLLM | 400 with a numeric `error.code`, `error.type` of `BadRequestError`, `param` of `input_tokens`, and "This model's maximum context length is 262144 tokens. However, you requested 0 output tokens and your prompt contains at least 262145 input tokens, ...". A `max_tokens` above the model's length is 400 with `param` of `max_tokens` and "max_tokens=300000 cannot be greater than max_model_len=max_total_tokens=262144": a parameter rejected, not an overflow |

mlx-vlm counts `max_tokens` into the limit, its own `--max-tokens` for a request that sets none:
with `--max-tokens 8192` and `MAX_KV_SIZE=4096`, every request that sets no `max_tokens` was
rejected as too long. With a limit set, its code also shortens a conversation that does not fit
before it answers, replacing older exchanges with a summary the model writes, so that the
conversation the model answers is not the one sent; a shortening that succeeded was not seen. A
summary that runs out of output fails the request with 502 and
`{"detail":"Compaction summary hit its output limit; original context preserved."}`, a gateway
status for a failure that sending the request again repeats.

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

vLLM lists its model under `--served-model-name`, with `max_model_len`, `owned_by: "vllm"`, and
the path it was loaded from as `root`.

llama.cpp lists its one model under the path it was loaded from, with `meta` (`n_ctx`,
`n_ctx_train`, and more) and `architecture.input_modalities`, and the same model again in a
`models` array of another shape.

mlx-lm and mlx-vlm list every model in the local Hugging Face cache that looks loadable, an image
model among them (`unsloth/Qwen-Image-2.1`), and the loaded model under the path it was loaded
from.

Sources: [LM Studio Chat Completions](https://lmstudio.ai/docs/developer/openai-compat/chat-completions),
[LM Studio tool use](https://lmstudio.ai/docs/developer/openai-compat/tools),
[LM Studio structured output](https://lmstudio.ai/docs/developer/openai-compat/structured-output),
[LM Studio: the schema held to the reasoning](https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/1773),
[Structured outputs](https://developers.openai.com/api/docs/guides/structured-outputs),
[LM Studio authentication](https://lmstudio.ai/docs/developer/core/authentication),
[Chat Completions streaming events](https://developers.openai.com/api/reference/resources/chat/subresources/completions/streaming-events),
[Azure OpenAI content streaming](https://learn.microsoft.com/en-us/azure/foundry/openai/concepts/content-streaming),
[Azure content filtering](https://learn.microsoft.com/en-us/azure/ai-foundry/openai/concepts/content-filter),
[mlx-lm server](https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/SERVER.md),
[mlx-vlm](https://github.com/Blaizzy/mlx-vlm),
[vllm-metal](https://vllm.ai/blog/2026-09-22-vllm-metal-v0-28-0),
[OpenAI: a call of a named function finished with stop](https://community.openai.com/t/function-call-with-finish-reason-of-stop/437226).
