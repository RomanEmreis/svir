# Roadmap

Order of work for the first release. Each step is reviewable on its own. Decisions are in
[decisions.md](decisions.md).

1. **Conformance suite first.** Done: recorded fixtures and loopback-server scenarios, written as
   data before the code they test. The runner is written with each component, in Rust.
2. **Types and errors.** Done: messages with parts, attachments, tool descriptors, calls and
   results, events and completions, usage and timing, the error taxonomy, `Limits`, `Mode`, and
   `Think`. Their serde form is pinned against the conformance notation.
3. **Decoder.** SSE framing, tool-call assembly and limits (strict), `<think>` splitting, live
   reasoning, and lenient mode.
4. **Encoder.** The streamed body with an exact length and attachments; tools, tool calls,
   tool results, and continuation sent back.
5. **Transport and compatibility.** HTTP, authentication, status mapping, timeouts,
   cancellation, optional-field learning, model listing.
6. **First consumer.** A chat backend moves over; its existing tests must pass unchanged. An agent
   engine follows when convenient.
7. **Layers.** The layer trait, `wrap`, `Retry`, `Timeout`, and `Trace`.
8. **Tools.** The router, `svir-macros` with `#[tool]`, and schemars schemas.

## Conformance suite

The suite is data: see [tests/conformance](../tests/conformance/README.md) for the format. It is
written for all three components:

- `decoder/`: 46 cases. Framing, reasoning and `<think>` splitting, tool-call assembly, usage,
  limits, and strict and lenient outcomes for each.
- `encoder/`: 22 cases. Content layout, attachments at every size around the block size, exact
  length and repeatability, reasoning and tool round trips, admission.
- `transport/`: 42 cases. Authentication, status mapping, the stream, timeouts, cancellation,
  base URLs, model listing, and compatibility learning, against a scripted loopback server.

Cases that depend on an open decision name it in `open`. Not expressible as data:

### Timing (unit test, needs a clock)

- Tokens per second only with more than one visible token over at least 50 ms (P7).

### Live (opt-in)

- A bounded session against a local server with a tool-capable model: at least one tool call
  before the final answer; reported or estimated usage recorded. Never part of the default run.
