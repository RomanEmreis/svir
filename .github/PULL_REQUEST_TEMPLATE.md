<!--
Title: the release it ships in (`0.2.0:`) or the kind of change (`fix:`, `doc:`), and the main
change in one clause. Keep the description proportional to the diff; CONTRIBUTING.md has more.
-->

## Summary
<!-- `Closes #N` if there is an issue, then a few dry sentences, one per change. Cite a decision (`D<n>`, `O<n>`) rather than retelling it. -->
What does this change do and why?

## Type
<!-- Tick what applies. -->
- [ ] Bug fix
- [ ] Feature
- [ ] Enhancement
- [ ] Performance
- [ ] Documentation
- [ ] Refactor
- [ ] Security
- [ ] Breaking change

## Checklist
<!-- Keep every box, ticked or not. A sub-bullet only where a box needs a qualifier: what breaks, what was not run. -->
- [ ] Behavior I changed or added is covered by conformance vectors in `tests/conformance/`, or by unit tests where data cannot express it
- [ ] I updated `docs/` and the examples if needed; a new or changed decision is in `docs/decisions.md`
- [ ] This change is backwards-compatible, the serde form of public types and the error kinds included (or clearly marked as breaking)
- [ ] `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` pass with default features, with `--no-default-features`, and with `--all-features`
- [ ] No credential, request URL, or server message reaches an error's `Debug` or `Display`, an event, or a log

## Servers
<!-- One line. -->
Which model servers was this run against, if any (`tests/live.rs`, the examples)? "None" is a fine answer for changes the default test run covers.

## Notes for reviewers
<!-- Short bullets, one line each: non-obvious choices, intentional omissions, what needs a closer look. Not the summary or the checklist again. -->
Anything you'd like reviewers to pay extra attention to?
