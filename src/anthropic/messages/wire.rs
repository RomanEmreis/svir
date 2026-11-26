//! Names in the Messages JSON that the adapter reads or writes in more than one place.

/// The API version svir speaks, and the header that names it (D50).
#[cfg(feature = "client")]
pub(crate) const VERSION: &str = "2026-10-21";
#[cfg(feature = "client")]
pub(crate) const VERSION_HEADER: &str = "anthropic-version";

// The objects of the stream.
pub(super) const INDEX: &str = "index";
pub(super) const DELTA: &str = "delta";
pub(super) const USAGE: &str = "usage";

// Content blocks, and the fields of their starts and deltas.
pub(super) const TEXT: &str = "text";
pub(super) const THINKING: &str = "thinking";
pub(super) const REDACTED_THINKING: &str = "redacted_thinking";
pub(super) const TOOL_USE: &str = "tool_use";
pub(super) const SIGNATURE: &str = "signature";
pub(super) const DATA: &str = "data";

// Usage, as the API counts it.
pub(super) const INPUT_TOKENS: &str = "input_tokens";
pub(super) const OUTPUT_TOKENS: &str = "output_tokens";
