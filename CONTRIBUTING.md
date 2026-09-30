# Contributing

Thanks for your interest in contributing!
All kinds of contributions are welcome - ideas, bug reports, documentation fixes,
and code improvements.

This project is currently maintained by a small team (often just one person),
so please keep that in mind when interacting or proposing changes.

## What svir is, and is not

svir is the wire protocol between an application and a model server, with opt-in
layers and tool sets on top. It is not an agent framework: an agent loop, session
history, storage, and MCP belong to the application. A change that adds one of them
is out of scope, however well it is done. The reasons are in
[docs/decisions.md](docs/decisions.md).

## Reporting issues

Before opening an issue:
- Please check if a similar issue already exists.
- Include clear steps to reproduce (if applicable).
- For feature requests, describe the problem you're trying to solve.

When svir misreads a model server, the most useful report is what the server sent:
the output of `cargo run --example relay`, or of `curl -N` with the same request,
with anything private removed. Name the server and its version.

If you're unsure, feel free to open a discussion first.

## Pull Requests

A few simple guidelines:

- All changes should go through a pull request.
- Keep PRs focused and reasonably small.
- Follow existing code style and conventions.
- Add tests where it makes sense.
- Clearly describe *why* the change is needed, not just *what* changed.

Draft PRs are totally fine if you want early feedback.

### The design record

[docs/](docs/) is the design record, and it is kept true:

- Accepted decisions (`D<n>` in [docs/decisions.md](docs/decisions.md)) are binding.
  A PR that changes one says so and updates the record.
- Open questions (`O<n>`) are settled in an issue before the code that depends on them.
- A change to behavior that `docs/` describes updates `docs/` in the same PR.
- What a server was observed to do goes into
  [docs/wire-protocol.md](docs/wire-protocol.md), as a fact, with the server named.

### Tests

- Behavior is pinned by conformance vectors: data in
  [tests/conformance](tests/conformance/README.md), written before the code they cover.
  A unit test is for what data cannot express.
- The default test run is deterministic and needs no model, network, or credentials.
  Tests against a real server live in `tests/live.rs` and are ignored unless asked for.
- The serde form of a public type is a contract, pinned in `tests/serde_contract.rs`.

### Before you push

CI runs these on Linux, the tests on macOS and Windows as well, and a check on the oldest
supported Rust (`rust-version` in `Cargo.toml`):

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --no-default-features --all-targets -- -D warnings
cargo clippy --no-default-features --features client --all-targets -- -D warnings
cargo clippy --all-features --all-targets -- -D warnings
cargo test
cargo test --all-features
```

With a model server at hand, also:

```sh
SVIR_MODEL=<model> cargo test --test live -- --ignored --test-threads=1
```

### Style

- Code, comments, and conformance cases are ASCII: `-` or `--` for a dash, `->` for an
  arrow, `...` for an ellipsis, straight quotes. Prose in the docs follows the same habit.
- Every public item is documented. Every public data type is `#[non_exhaustive]` and
  serializable.
- No `unsafe`, and no `unwrap()` outside tests.
- Inside a function, a blank line separates logical steps.
- Nothing on the request path is boxed or dispatched dynamically; a stream is a named
  type with a hand-written `poll_next`.
- The dependency tree is small on purpose. Please discuss a new dependency before
  adding it.
- Credentials, request URLs, and the server's own messages stay out of errors' `Debug`
  and `Display`, events, and logs.

## Breaking changes

Breaking changes are welcome, but:
- Please open an issue or discussion first.
- Clearly explain the motivation and impact.
- Migration guidance is highly appreciated.

The public API includes the serde form of public types and the error kinds.

## Changelog and releases

A change a user of the crate can notice gets a line in [CHANGELOG.md](CHANGELOG.md), under
the version it will ship in.

A release is cut by a maintainer: the version in `Cargo.toml` and its section in the
changelog go to `main`, then a GitHub release is published with the version as its tag
(`0.1.0`, no prefix). Publishing the release sends the crate to crates.io, after checking
that the tag, the manifest, and the changelog agree.

## License

svir is licensed under MIT OR Apache-2.0. Unless you explicitly state otherwise, any
contribution you submit is dual licensed the same way, without additional terms.

## Communication

Be kind, constructive, and respectful.
By participating in this project, you agree to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

Thank you for helping make this project better!
