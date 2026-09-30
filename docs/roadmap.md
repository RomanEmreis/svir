# Roadmap

Order of work for the first release. Each step is reviewable on its own. Decisions are in
[decisions.md](decisions.md).

1. **Conformance suite first.** Done: recorded fixtures and loopback-server scenarios, written as
   data before the code they test. The runner is written with each component, in Rust.
2. **Types and errors.** Done: messages with parts, attachments, tool descriptors, calls and
   results, events and completions, usage and timing, the error taxonomy, `Limits`, `Mode`, and
   `Think`. Their serde form is pinned against the conformance notation.
3. **Decoder.** Done: `svir::openai::chat::Decoder` with SSE framing, tool-call assembly,
   limits, `<think>` splitting, live reasoning, strict and lenient modes, and timing. All 49 decoder
   cases pass in `tests/conformance_decoder.rs`; the two lenient O12 cases are pending.
4. **Encoder.** Done: `svir::openai::chat::Encoder` and `Body`, with the exact length,
   attachments from memory and from disk, tools, tool calls and results, reasoning sent back, and
   admission. All 22 encoder cases pass in `tests/conformance_encoder.rs`, at every block size
   and from memory.
5. **Transport and compatibility.** Done: `Client` and its builder, `EventStream`, the HTTP seam
   with the hyper backend, authentication, status mapping, timeouts, cancellation, optional-field
   learning, and model listing. All 42 transport cases pass in `tests/conformance_transport.rs`;
   the `Retry-After` date case (O7) is pending.
6. **Layers.** Done: `svir::layer::{Layer, Next}`, `.layer(..)` and `.wrap(..)`, `Retry`,
   `Timeout`, and `Trace`, with the stream methods they use (D28). Tested against a scripted
   backend in `tests/layers.rs`.
7. **Tools.** Done: the `Toolbox` trait and the `Tools` registry, with schemas from types behind
   `schemars`. `tests/tool_loop.rs` runs the loop that feeds results back. The MCP bridge is
   neva's work, behind its `svir` feature (D18).
8. **Examples and live runs.** Done: `examples/` for each level of use, and `tests/live.rs`, an
   opt-in run against a real model server: a whole answer, a stream, a conversation, a tool call,
   an image and a text file, an output limit, cancellation, a deadline. Run against LM Studio, it
   found the `event: error` event (wire-protocol 3.5) and raised O15 and O16. HTTPS was exercised
   by hand against public endpoints.
9. **First consumer.** A chat backend moves over; its existing tests must pass unchanged. An agent
   engine follows when convenient.
10. **`testing`.** The scripted server and scripted event streams, published for callers' tests.

## Conformance suite

The suite is data: see [tests/conformance](../tests/conformance/README.md) for the format. It is
written for all three components:

- `decoder/`: 49 cases. Framing, reasoning and `<think>` splitting, tool-call assembly, usage,
  limits, errors inside the stream, and strict and lenient outcomes for each.
- `encoder/`: 22 cases. Content layout, attachments at every size around the block size, exact
  length and repeatability, reasoning and tool round trips, admission.
- `transport/`: 42 cases. Authentication, status mapping, the stream, timeouts, cancellation,
  base URLs, model listing, and compatibility learning, against a scripted loopback server.

Cases that depend on an open decision name it in `open`. Not expressible as data:

### Timing (unit test, needs a clock)

- Tokens per second only with more than one visible token over at least 50 ms (P7).

### Live (opt-in)

`tests/live.rs`, never part of the default run:

```sh
SVIR_MODEL=<model> cargo test --test live -- --ignored --test-threads=1
```

It includes a bounded session with a tool-capable model: at least one tool call before the final
answer, with the reported usage counted.
