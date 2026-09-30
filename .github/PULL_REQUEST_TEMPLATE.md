## Summary
What does this change do and why?

## Type
- [ ] Bug fix
- [ ] Feature
- [ ] Enhancement
- [ ] Performance
- [ ] Documentation
- [ ] Refactor
- [ ] Security
- [ ] Breaking change

## Checklist
- [ ] Behavior I changed or added is covered by conformance vectors in `tests/conformance/`, or by unit tests where data cannot express it
- [ ] I updated `docs/` and the examples if needed; a new or changed decision is in `docs/decisions.md`
- [ ] This change is backwards-compatible, the serde form of public types and the error kinds included (or clearly marked as breaking)
- [ ] `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` pass with default features, with `--no-default-features`, and with `--all-features`
- [ ] No credential, request URL, or server message reaches an error's `Debug` or `Display`, an event, or a log

## Servers
Which model servers was this run against, if any (`tests/live.rs`, the examples)? "None" is a fine answer for changes the default test run covers.

## Notes for reviewers
Anything you'd like reviewers to pay extra attention to?
