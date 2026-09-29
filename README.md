# svir

A small, composable Rust SDK for talking to large language models: the wire protocol between your
application and a model server, and nothing it does not need.

> **Status**: design stage. There is no code yet, nothing is published, and names may still
> change. The design record is in [docs/](docs/).

## The name

The Svir is the river that joins Lake Onega to Lake Ladoga; the Neva then carries that water from
Ladoga to the Baltic. svir sits in the same family as [volga](https://github.com/RomanEmreis/volga)
(HTTP) and [neva](https://github.com/RomanEmreis/neva) (MCP), and like its river it joins two
bodies of water that already exist: two independent, working implementations of the same
protocol, each strong where the other is weak.

## What it covers

The first protocol is OpenAI-compatible Chat Completions streaming, as served by LM Studio,
mlx-lm, llama.cpp, vLLM, and hosted endpoints.

- **Types**: messages with text, image, and file parts; tool descriptors, calls, and results;
  usage; finish reasons; a typed error.
- **Encoder**: the request body streamed from disk, attachments included, with an exact
  `Content-Length` known before the first byte. That length is also the context estimate.
- **Decoder**: SSE framing, tool calls assembled across deltas, reasoning from
  `reasoning_content`, `reasoning`, or inline `<think>` tags, usage, errors inside the stream, and
  hard limits. Strict by default, lenient on request.
- **Transport**: HTTP with optional Bearer authentication, typed status mapping, timeouts, and
  cancellation by drop.
- **Compatibility**: a server that rejects optional fields such as `reasoning_effort` is detected
  once and remembered.

Not covered, on purpose: an agent loop, session history, storage, retry scheduling, and MCP. Those
belong to the application; svir gives it the pieces to build them.

## Principles

- **Protocol, not framework.** svir fits under a chat backend and under an agent engine without
  either bending around it.
- **Nothing is lost silently.** Tool-call IDs, reasoning, and provider continuation data survive
  a round trip. A feature an adapter cannot represent is an explicit error, not a dropped field.
- **Wire types stay in adapters.** Shared types describe what the caller needs, not the union of
  every provider's JSON.
- **Complete before executable.** A partially streamed tool call is display data only.
- **Strict and bounded by default.** Unknown input is an error unless the caller asks for
  leniency. Bytes, events, and tool calls always have limits, and reaching one is a typed outcome.
- **Credentials stay out of the record.** Keys never reach errors, events, or logs.
- **Testable without a model.** Recorded fixtures and a loopback server by default; live model
  tests are opt-in.

## Documentation

- [Architecture](docs/architecture.md) -- boundaries, components, events, strictness, errors
- [Wire protocol](docs/wire-protocol.md) -- Chat Completions facts and observed server behavior
- [Decisions](docs/decisions.md) -- accepted, proposed, and open
- [Roadmap](docs/roadmap.md) -- order of work and what is left to write
- [Conformance suite](tests/conformance/README.md) -- behavior vectors as data, independent of
  the API

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
