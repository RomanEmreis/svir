//! Telling a context overflow from any other failure the server reports.
//!
//! Servers agree on no single sign of it. Some set an error code, some an error type, and some
//! only say it in words, so all three are read (D30).

use serde_json::Value;

/// Values of `error.code` or `error.type` that mean the request did not fit.
const NAMES: [&str; 3] = [
    "context_length_exceeded",
    "context_window_exceeded",
    "exceed_context_size_error",
];

/// What a server that names nothing says about it, in lowercase. Every server observed speaks of
/// the context's length, size, or window.
const PHRASES: [&str; 3] = ["context length", "context size", "context window"];

/// Whether an error the server reported is a context overflow. `error` is its error object, or
/// its message alone.
pub(crate) fn is_overflow(error: &Value) -> bool {
    let field = |key: &str| error.get(key).and_then(Value::as_str);
    let named = [field("code"), field("type")]
        .into_iter()
        .flatten()
        .any(|name| NAMES.contains(&name));

    named
        || field("message")
            .or_else(|| error.as_str())
            .is_some_and(says_overflow)
}

/// Whether a message speaks of the context being exceeded, in any letter case.
pub(crate) fn says_overflow(message: &str) -> bool {
    let message = message.to_ascii_lowercase();

    PHRASES.iter().any(|phrase| message.contains(phrase))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_code_or_a_type_names_an_overflow() {
        assert!(is_overflow(&json!({"code": "context_length_exceeded"})));
        assert!(is_overflow(&json!({"code": "context_window_exceeded"})));
        assert!(is_overflow(
            &json!({"code": 400, "type": "exceed_context_size_error"})
        ));
        assert!(!is_overflow(&json!({"code": "unsupported_parameter"})));
        assert!(!is_overflow(
            &json!({"code": 400, "type": "invalid_request_error"})
        ));
    }

    #[test]
    fn a_message_says_it_in_the_words_servers_use() {
        for message in [
            "This model's maximum context length is 8192 tokens.",
            "The number of tokens to keep from the initial prompt is greater than the context length.",
            "the request exceeds the available context size, try increasing it",
            "Input exceeds the Context Window of this model",
        ] {
            assert!(is_overflow(&json!({"message": message})), "{message}");
            assert!(is_overflow(&json!(message)), "{message}");
        }

        for message in ["engine died", "max_tokens is too large", ""] {
            assert!(!is_overflow(&json!({"message": message})), "{message}");
        }
        assert!(!is_overflow(&json!({})));
        assert!(!is_overflow(&json!(null)));
    }
}
