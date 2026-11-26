# Wire protocol: Anthropic Messages

What the Anthropic Messages API does on the wire, as far as svir uses it. Decisions about it are in
[decisions.md](decisions.md) (D50-D55, P18, P19); Chat Completions is in
[wire-protocol.md](wire-protocol.md).

Upstream references: [Messages](https://platform.claude.com/docs/en/api/messages/create),
[Streaming](https://platform.claude.com/docs/en/build-with-claude/streaming),
[Thinking](https://platform.claude.com/docs/en/build-with-claude/thinking),
[Effort](https://platform.claude.com/docs/en/build-with-claude/effort),
[Stop reasons](https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons),
[Errors](https://platform.claude.com/docs/en/api/errors),
[Versions](https://platform.claude.com/docs/en/api/versioning),
[Models](https://platform.claude.com/docs/en/api/models/list).

## 1. Endpoints and headers

- `POST {base}/v1/messages`, with `stream: true` in the body for a server-sent event stream.
  The hosted base is `https://api.anthropic.com`.
- `GET {base}/v1/models` lists models (section 6).
- `anthropic-version` names the API version; it is required. `2026-10-21` is the current one.
  Unlike `2023-06-01`, it rejects `temperature`, `top_p`, and `top_k` on every model, a
  `document` block in an assistant message, and a `text` block with citations in a user message.
- The API key is sent as `Authorization: Bearer <key>`, as the SDKs that speak `2026-10-21` send
  it, or as `x-api-key: <key>`, the older form. `anthropic-beta` turns on beta features.

## 2. Request body

### 2.1 Fields

| Field | Meaning |
| --- | --- |
| `model` | Required. |
| `max_tokens` | Required. The most output tokens, thinking included. |
| `messages` | Required. Turns of role `user` or `assistant`; consecutive turns of one role are combined into one. |
| `system` | The system prompt: a string, or an array of `text` blocks. |
| `stream` | `true` for the event stream. |
| `temperature` | Rejected on `2026-10-21` (section 1). |
| `tools` | `[{name, description, input_schema}]`, plus optional fields such as `strict`. |
| `tool_choice` | `{type: "auto"}` (the default), `{type: "any"}`, `{type: "tool", name}`, or `{type: "none"}`. The newest models reject `any` and `tool` with 400. |
| `thinking` | `{type: "adaptive"}`, `{type: "disabled"}`, or, on older models, `{type: "enabled", budget_tokens}`. `display` is `"summarized"` or `"omitted"`. |
| `output_config` | `effort` (`low`, `medium`, `high`, `xhigh`, `max`) and `format` (`{type: "json_schema", schema}`). |

### 2.2 Content blocks

A message's `content` is a string or an array of blocks:

- `text`: `{type: "text", text}`.
- `image`: `{type: "image", source: {type: "base64", media_type, data}}`.
- `tool_use` (assistant): `{type: "tool_use", id, name, input}`, `input` a JSON object.
- `tool_result` (user): `{type: "tool_result", tool_use_id, content, is_error}`. In a user message
  that answers calls, the results come before any other block, and all results of one answer go
  in one message.
- `thinking` (assistant): `{type: "thinking", thinking, signature}`.
- `redacted_thinking` (assistant): `{type: "redacted_thinking", data}`, encrypted, no readable
  text.

Thinking blocks go back unchanged. Within the latest assistant message, they must be the blocks
the model produced, in their order: none rearranged, edited, or dropped. On the newest models
(Opus 5.5, Sonnet 5.5, the Fable line), a thinking block is bound to the conversation that
produced it, and a history whose earlier messages changed is rejected with 400 (`Invalid
signature in thinking block ... bound to a different conversation`). The API drops, unbilled,
blocks the current model cannot read.

### 2.3 Thinking and effort

- Current models take adaptive thinking, whose depth `output_config.effort` sets. A thinking
  budget is rejected with 400 on Opus 4.7 and later, Sonnet 5 and later, and the Fable line, and
  deprecated on Opus 4.6 and Sonnet 4.6.
- Without `thinking`, Opus 4.7 and 4.8 run without thinking, and the Claude 5 models run adaptive.
- Some models cannot turn thinking off: Opus 5.5, Sonnet 5.5, and the Fable line answer
  `{type: "disabled"}` with 400.
- `display` defaults to `"omitted"` on the Claude 5 models: thinking blocks arrive with empty text
  and a signature. `"summarized"` returns a summary of the reasoning. The display does not change
  what is billed, nor the signature.
- `effort` defaults to `high`, and to `medium` on Opus 5.5 and Haiku 5.5. `xhigh` exists from Opus
  4.7 on; there is no `minimal`.

## 3. Response stream

### 3.1 Events

Each event has a name (`event: message_start`) and data whose `type` is the same name:

1. `message_start`: `{message: {id, type, role, model, content: [], stop_reason: null, usage}}`.
2. Per content block: `content_block_start` (`{index, content_block}`), any number of
   `content_block_delta` (`{index, delta}`), and `content_block_stop` (`{index}`). The index is
   the block's place in the message's content, from 0.
3. One or more `message_delta`: `{delta: {stop_reason, stop_sequence}, usage}`.
4. `message_stop`.

`ping` events may come anywhere. New event types may be added; the versioning policy asks clients
to skip those they do not know, and lets the values in objects expand.

### 3.2 Blocks and deltas

| Block | Starts as | Deltas |
| --- | --- | --- |
| `text` | `{type: "text", text: ""}` | `text_delta` (`text`) |
| `thinking` | `{type: "thinking", thinking: "", signature: ""}` | `thinking_delta` (`thinking`), then one `signature_delta` (`signature`) just before the stop |
| `redacted_thinking` | `{type: "redacted_thinking", data}` | none: it arrives whole |
| `tool_use` | `{type: "tool_use", id, name, input: {}}` | `input_json_delta` (`partial_json`), pieces of the input's JSON text |

With `display: "omitted"`, a thinking block streams one `thinking_delta` with empty text, then
its signature. By default the API buffers each key and value of a tool input until it is whole,
so input pieces may arrive after a pause.

### 3.3 Stop reasons

`stop_reason` is null in `message_start` and given in `message_delta`:

| Value | When |
| --- | --- |
| `end_turn` | The model finished. |
| `max_tokens` | The answer reached `max_tokens`. |
| `stop_sequence` | The model wrote one of the request's `stop_sequences`. |
| `tool_use` | The model calls tools. |
| `pause_turn` | A server tool's loop reached its limit; the turn continues in a later request. |
| `refusal` | Safety classifiers declined; text already streamed stays. `stop_details` says why. |
| `model_context_window_exceeded` | The answer filled the model's context window. |

### 3.4 Usage

`message_start` carries `usage` with `input_tokens`, `cache_creation_input_tokens`,
`cache_read_input_tokens`, and `output_tokens`; the total input of a request is the sum of the
first three. The counts in `message_delta` are running totals. `output_tokens_details`
carries `thinking_tokens` where the API reports it.

### 3.5 Errors inside the stream

After a `200`, an error arrives as an event:

```text
event: error
data: {"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}
```

The stream ends there. Its `error.type` is one of those of section 5.

## 4. Message order and compatibility

- The first message is `user`. Consecutive turns of one role are combined by the API.
- An assistant message may end the list as a prefill on older models; the 4.6 family and later
  reject a prefill with 400.

## 5. HTTP errors

Errors are JSON: `{"type": "error", "error": {"type", "message"}, "request_id"}`, with an optional
`error.details.error_code`.

| Status | `error.type` | |
| --- | --- | --- |
| 400 | `invalid_request_error` | A prompt over the context window says `prompt is too long: N tokens > M maximum`. Spend limits set by the organization are also 400. |
| 401 | `authentication_error` | |
| 402 | `billing_error` | |
| 403 | `permission_error` | |
| 404 | `not_found_error` | An unknown model as well. |
| 413 | `request_too_large` | The body is over the size limit (128 MB on the Messages API). |
| 429 | `rate_limit_error` | `retry-after` in seconds; a spend cap has none and `error.details.error_code` `enforced_spend_limit_reached`. |
| 500 | `api_error` | Or `timeout_error`. |
| 504 | `timeout_error` | |
| 529 | `overloaded_error` | Retryable. |

A response may carry `x-should-retry: true` or `false`.

## 6. Model listing

`GET /v1/models` returns `{data: [{type: "model", id, display_name, created_at, ...}], has_more,
first_id, last_id}`, newest first. `limit` takes 1 to 1000 (default 20), and `after_id` the
`last_id` of the page before.
