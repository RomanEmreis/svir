# svir

A small, composable Rust SDK for talking to large language models: the wire protocol between your
application and a model server, and nothing it does not need.

[![latest](https://img.shields.io/badge/latest-0.1.2-blue)](https://crates.io/crates/svir)
[![rustc](https://img.shields.io/badge/rustc-1.85+-964B00)](https://releases.rs/docs/1.85.0/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-violet.svg)](#license)
[![CI](https://github.com/RomanEmreis/svir/actions/workflows/rust.yml/badge.svg)](https://github.com/RomanEmreis/svir/actions/workflows/rust.yml)
[![Release](https://github.com/RomanEmreis/svir/actions/workflows/release.yml/badge.svg)](https://github.com/RomanEmreis/svir/actions/workflows/release.yml)

> **Status**: svir is in preview. The public API may still change between `0.x` releases; what
> changed is in the [changelog](CHANGELOG.md), and why it is the way it is in [docs/](docs/).

[API Docs](https://docs.rs/svir/latest/svir/) | [Examples](examples/) | [Design record](docs/)

## The name

The Svir is the river that joins Lake Onega to Lake Ladoga; the Neva then carries that water from
Ladoga to the Baltic. svir sits in the same family as [volga](https://github.com/RomanEmreis/volga)
(HTTP) and [neva](https://github.com/RomanEmreis/neva) (MCP), and like its river it joins two
bodies of water that already exist: two independent, working implementations of the same
protocol, each strong where the other is weak.

## A first look

```toml
[dependencies]
svir = "0.1.2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use svir::prelude::*;

#[tokio::main]
async fn main() -> Result<(), svir::Error> {
    let client = Client::openai("http://127.0.0.1:1234").build()?;
    let request = Request::new("qwen3-27b")
        .system("Be precise.")
        .user("Why do rivers meander?");

    let mut stream = client.stream(&request).await?;
    while let Some(event) = stream.next().await {
        match event? {
            Event::Text(piece) => print!("{piece}"),
            Event::Completed(done) => println!("\n{:?}", done.usage),
            _ => {}
        }
    }

    Ok(())
}
```

`client.complete(&request)` returns the whole answer instead. Dropping the stream cancels the
request.

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

On top of that core, opt-in:

- **Layers**: middleware around every call, with `Retry`, `Timeout`, and `Trace` built in.
- **Tools**: a `Toolbox` trait for anything that describes tools to a model and answers its
  calls, and `Tools`, a plain registry of typed handlers. No macros: tools defined for MCP with
  neva can be handed to a model through neva's side of the bridge.

Not covered, on purpose: an agent loop, session history, storage, and MCP. Those belong to the
application; svir gives it the pieces to build them.

## Examples

[examples/](examples/) has one short program per way of using svir. They take the server from
`SVIR_URL` (`http://127.0.0.1:1234` unless set) and the model from `SVIR_MODEL`:

```sh
cargo run --example models
SVIR_MODEL=<model> cargo run --example stream
```

| Example | Shows |
| --- | --- |
| `models` | The models a server has |
| `complete` | The whole answer, its token counts and speed |
| `stream` | An answer as it arrives: reasoning, then text |
| `chat` | A conversation; the caller keeps the history |
| `attachments` | An image read from disk and a text file in one message |
| `tools` | A `Tools` registry and the loop that feeds results back |
| `tools_typed` | Tool schemas derived from types (feature `schemars`), calls shown as they stream |
| `toolbox` | A tool set of one's own, with shared state |
| `layers` | `Retry`, `Timeout`, a closure, and a layer of one's own |
| `relay` | A proxy: the server's bytes passed on unchanged and decoded on the way past |
| `codec` | The encoder and decoder alone, with no client and no server |

## Principles

- **Protocol at the core, building blocks on top.** svir fits under a chat backend and under an
  agent engine without either bending around it; layers and tools are opt-in.
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
- [Agent Skill](skill/svir/README.md) -- svir for coding agents: the API, its traps, and code
  that compiles

## Contributing

Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md). To report a vulnerability, follow
[SECURITY.md](SECURITY.md) instead of opening an issue. Releases are listed in the
[changelog](CHANGELOG.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
