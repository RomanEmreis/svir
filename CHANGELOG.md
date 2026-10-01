# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## 0.1.2

### Added

* **`FinishReason::ContentFilter`**: the server's content filter stopped the
  answer. The completion keeps the text that came before it, so a caller can
  show it and say why it ends. It was `Unsupported` in both modes before. Tool
  calls with this finish are a `Protocol` error, as with `Stop`: the filter may
  have cut a call short.

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
