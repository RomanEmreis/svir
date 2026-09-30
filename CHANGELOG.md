# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

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
