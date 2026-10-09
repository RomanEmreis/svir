# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## 0.1.6

### Fixed

* **Reasoning sent under both `reasoning_content` and `reasoning` arrives
  once.** mlx-vlm sends every piece of reasoning under both keys; it was two
  `Event::Reasoning` for each piece, and two copies in the completion. The
  same text under both keys in one delta is now one piece, from
  `ReasoningSource::ReasoningContent`. Different texts are still two
  carriers (D43).

### Changed

* **A server's message is read from `detail` when its error body has no
  `error`**, as mlx-vlm's are: the string, or the first validation error's
  `msg`. It is what `error.server_message()` returns, and a message there
  that speaks of a context overflow makes the error
  `ErrorKind::ContextOverflow` (D44).
* **More overflows are recognized by their words**: a message that speaks of
  the `context budget` or of `compacted context` is
  `ErrorKind::ContextOverflow` on 400, 413, and 422, and inside the stream,
  as mlx-vlm reports a conversation longer than its context limit (D30).

## 0.1.5

### Added

* **`Request::tool_choice(ToolChoice)`**: the model decides (`Auto`, the
  default, not sent), may not call a tool (`None`), must call one
  (`Required`), or must call a given one (`ToolChoice::tool(name)`). A call
  required of a request that offers no tools, or of a tool it does not
  offer, is `ErrorKind::Unsupported` when the request is encoded (D40).
* **`Request::response_format(..)`**: the answer as a JSON object
  (`ResponseFormat::Json`), or as JSON matching a `Schema`, a name and a
  JSON Schema, with `strict` sent only when it is set.
  `Schema::of::<T>()` derives the schema from a type with the `schemars`
  feature (D40, D41).
* **`Completion::parse::<T>()`**: the answer's text read as JSON into `T`,
  when it finished with `Stop`; any other finish is an error, even for valid
  JSON. Nothing else checks the answer against the schema (D41).
* **`FinishReason::Refusal`**: the model refused to answer, and the
  completion's text is its refusal, streamed as `Event::Text`. Chat
  Completions sends it in `refusal` in place of `content`, with a `stop`
  finish; OpenAI does, above all for a structured answer it will not give.
  Content and a refusal in one answer, or a refusal with tool calls, are
  `ErrorKind::Protocol` (D42).
* **The `structured` example**: an answer to the schema of a type, parsed
  back into it.

A tool choice and a response format are what the answer must meet, so the
compatibility retry, which leaves out `reasoning_effort` and
`stream_options`, keeps them. A server that does not take them fails the
request with `Unsupported` and its own message (D40).

The answer is not checked against either: the caller reads `calls` and
parses the text. LM Studio takes `required` without keeping it, rejects a
tool named in `tool_choice` and `ResponseFormat::Json`, and with reasoning
on sends JSON to a schema as reasoning, with no text; `Effort::Off` with
the format avoids that.

### Changed

* **A refusal is read.** Strict mode failed it with `Unsupported`, and
  lenient mode dropped it and completed with no text (D42).

## 0.1.4

### Added

* **`ToolResult::is_error`**, set by `ToolResult::error(call_id, message)`:
  the call failed, and the content says what went wrong. Chat Completions has
  no field for it, so the encoder sends `error: ` before the content. The
  flag is in the serde form only when it is true (D37).
* **`ClientBuilder::header(name, value)`**: a header sent with every request,
  the model listing too, for a gateway or a hosted endpoint that asks for one.
  Its value is treated as a credential: withheld from `Debug` and sent as a
  sensitive header. A name or value that cannot be sent, and the headers svir
  writes itself (`authorization`, `content-type`, `content-length`, `accept`,
  `host`, `transfer-encoding`, `connection`), are `ErrorKind::Config` when the
  client is built (D38).
* **`TextFile::escaped_len(n)` and `svir::body::escaped_len(bytes)`**: a text
  file's length once escaped into a JSON string, measured once by the caller,
  for example when the file was stored. The body is then built without
  reading the file; only its size is looked up. A length the file does not
  have fails with `ErrorKind::Attachment`, when the body is built or while it
  streams (D39).

### Changed

* **`Tools` flags a failed call instead of writing `error: ` into the
  content.** The result is `is_error`, and its content is the message alone.
  What goes on the wire for Chat Completions is the same as before.
* **A text file is checked to be UTF-8 while the body streams**, not only when
  it is measured, so a declared length cannot carry other bytes into the
  request. It fails with `ErrorKind::Attachment`.

## 0.1.3

### Added

* **`ErrorKind::ContentFilter`**: the server's content filter blocked the
  prompt, as Azure OpenAI reports with a 400 whose `error.code` is
  `content_filter`. It is not retryable: the prompt has to change. It was
  `Unsupported` before, and only `server_message()` said what happened.
* **Azure's asynchronous content filter is read.** It streams the answer
  unvetted and reports on it in annotations. An annotation that blocks
  nothing is skipped, before the finish reason or after it. One that blocks,
  by a `content_filter` finish or by a verdict marked `filtered: true`, makes
  the answer's finish `FinishReason::ContentFilter`, even after the model's
  own `stop`: Azure reports a word it caught only after the model finished
  that way, with no finish reason. The completion keeps the text as it was
  streamed, and a caller that shows it withdraws it. Annotations were
  `Unsupported` in both modes before.

### Fixed

* **A prompt the content filter blocked is no longer sent twice.** A 400 to
  a request with `reasoning_effort` or `stream_options`, which is nearly every
  request since usage is asked for by default, was retried without them, as a
  server that does not know them would need. The filter blocked the second
  attempt too, and its evaluation was billed again. The retry now follows only
  a rejection whose body explains nothing.

## 0.1.2

### Added

* **`FinishReason::ContentFilter`**: the server's content filter stopped the
  answer. The completion keeps the text that came before it, so a caller can
  show it and say why it ends. It was `Unsupported` in both modes before. Tool
  calls with this finish are a `Protocol` error, as with `Stop`: the filter may
  have cut a call short. Azure's asynchronous content filter, which vets text
  after streaming it, is not read yet: its annotations are `Unsupported` in
  both modes, so text it has not vetted never completes as an answer.

### Changed

* **Lenient mode fails on an unknown finish reason and on content after the
  finish reason**, as strict mode does. Both were pending a decision and
  already failed; this is now the settled behavior (D35).

### Fixed

* **Azure OpenAI streams decode in strict mode.** Azure starts the stream with
  a report on the prompt: a chunk with no choices, an empty `id` and `model`,
  and `prompt_filter_results`. Strict mode failed on it with "an empty choices
  array before the finish reason". It carries nothing of the answer and is now
  skipped in both modes, under its older name `prompt_annotations` too.

## 0.1.1

### Added

* **`Error::status()`**: the HTTP status of a response that was not a
  success, for a proxy that passes the upstream status on. `None` for every
  other failure, including an error inside a stream that began with `200`.
* **The `tls-aws-lc` feature**: HTTPS with the aws-lc-rs crypto provider in
  place of ring. A build that already has aws-lc-rs can take
  `default-features = false, features = ["client", "tls-aws-lc"]` and compile
  one provider only. With two providers compiled in, rustls has no default
  one, and other code that relies on it (`ClientConfig::builder()`) panics.

### Changed

* **The default wire limit is 64 MiB**, up from 4 MiB. A streamed answer
  spends a few hundred bytes a token on the wire, so 4 MiB cut off an answer
  after about 16,000 tokens, which a reasoning model can pass. The decoder
  keeps the answer, not the wire bytes.

### Fixed

* **Inline `<think>` tags are split out when a delta is nothing but part of
  one.** A delta such as `<thi` or `</`, with no text around it, was passed on
  as answer text instead of being held until the next delta decided it. The
  tags then stayed in the answer (`<thi`, `nk>hm</think>`), or the rest of the
  answer went to reasoning (`<think>hm`, `</`, `think>`). Models whose tokenizer
  has no single token for the tag send it this way, on servers without a
  reasoning parser. Reasoning from `reasoning_content` or `reasoning` was not
  affected.

## 0.1.0

The first release: OpenAI-compatible Chat Completions streaming, as served by LM Studio,
mlx-lm, llama.cpp, vLLM, and hosted endpoints.

### Added

* **Types.** `Request`, `Message` and its parts (text, `Image`, `TextFile`, reasoning, tool
  calls and results), `Tool`, `ToolCall`, `ToolResult`, `Event`, `Completion`, `Usage`,
  `Timing`, and `Effort`. Every public data type is `#[non_exhaustive]` and serializable.
* **Errors.** One `Error` with an `ErrorKind` a caller can act on, `is_retryable()`,
  `retry_after()`, and the server's own message behind `server_message()`, kept out of
  `Debug` and `Display`.
* **Encoder.** `openai::chat::Encoder` builds a request body whose exact length is known
  before its first byte. Attachments are read from disk a block at a time while the body is
  sent. `context_tokens` refuses a request that cannot fit before it is sent.
* **Decoder.** `openai::chat::Decoder`: SSE framing, tool calls assembled across deltas,
  reasoning from `reasoning_content`, `reasoning`, or inline `<think>` tags, usage, errors
  inside the stream, and limits on bytes, events, and tool calls. Strict by default, lenient
  on request; the outcome does not depend on how the bytes were chunked.
* **Client** (feature `client`, on by default). `Client::openai(url)` with `stream`,
  `complete`, `send` (the server's bytes unchanged, for a proxy), and `list_models`. Bearer
  authentication from a value, an environment variable, or a file; status codes mapped to
  error kinds; connect and idle timeouts; cancellation by dropping the stream. A server that
  rejects `reasoning_effort` or `stream_options` is detected once and remembered.
* **Context overflow** is recognized by the error's code, type, or message, in an error
  response and inside the stream.
* **HTTP seam.** The transport is hyper behind `http::Backend`; `.http(backend)` replaces it
  for a proxy, client certificates, or other roots. HTTPS through rustls is the `tls`
  feature, on by default.
* **Layers.** `layer::Layer` and `.wrap(closure)` around every call, with `Retry`, `Timeout`,
  and `Trace` (feature `tracing`) built in.
* **Tools.** The `Toolbox` trait and the `Tools` registry of typed handlers, with input
  schemas derived from types under the `schemars` feature.
