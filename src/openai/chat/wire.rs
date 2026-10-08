//! Names in the Chat Completions JSON that the adapter reads or writes in more than one place.

// The error object a server reports, in an error response or inside the stream.
pub(super) const ERROR: &str = "error";
pub(super) const MESSAGE: &str = "message";
pub(super) const CODE: &str = "code";
pub(super) const TYPE: &str = "type";

// A chunk of the stream, its choice, and the delta in it.
pub(super) const ID: &str = "id";
pub(super) const CHOICES: &str = "choices";
pub(super) const INDEX: &str = "index";
pub(super) const DELTA: &str = "delta";
pub(super) const FINISH_REASON: &str = "finish_reason";
pub(super) const ROLE: &str = "role";
pub(super) const CONTENT: &str = "content";
pub(super) const REASONING_CONTENT: &str = "reasoning_content";
pub(super) const REASONING: &str = "reasoning";
pub(super) const TOOL_CALLS: &str = "tool_calls";
pub(super) const REFUSAL: &str = "refusal";
/// The key of a tool call's name and arguments.
pub(super) const FUNCTION: &str = "function";

/// The type of every tool and tool call: the only one the protocol has.
pub(super) const FUNCTION_TYPE: &str = "function";
