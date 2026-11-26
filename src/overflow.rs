//! Telling a context overflow from any other failure the server reports, and the error for a
//! failure reported inside an open stream.
//!
//! Servers agree on no single sign of an overflow. Some set an error code, some an error type,
//! and some only say it in words, so all three are read (D30).

use serde_json::Value;

use crate::{
    Error, ErrorKind,
    wire::{CODE, ERROR, MESSAGE, TYPE},
};

/// Values of `error.code` or `error.type` that mean the request did not fit.
const NAMES: [&str; 3] = [
    "context_length_exceeded",
    "context_window_exceeded",
    "exceed_context_size_error",
];

/// What a server that names nothing says about it, in lowercase. Every server observed speaks of
/// the context's length, size, window, or budget, or of compacted context, or says the prompt is
/// too long, as the Messages API does.
const PHRASES: [&str; 6] = [
    "context length",
    "context size",
    "context window",
    "context budget",
    "compacted context",
    "prompt is too long",
];

/// Whether an error the server reported is a context overflow. `error` is its error object, or
/// its message alone.
pub(crate) fn is_overflow(error: &Value) -> bool {
    let field = |key: &str| error.get(key).and_then(Value::as_str);
    let named = [field(CODE), field(TYPE)]
        .into_iter()
        .flatten()
        .any(|name| NAMES.contains(&name));

    named
        || field(MESSAGE)
            .or_else(|| error.as_str())
            .is_some_and(says_overflow)
}

/// Whether a message speaks of the context being exceeded, in any letter case.
pub(crate) fn says_overflow(message: &str) -> bool {
    let message = message.to_ascii_lowercase();

    PHRASES.iter().any(|phrase| message.contains(phrase))
}

/// An error the server reported inside an open stream: a failure of its own (D29), unless it
/// says that the request did not fit in the context (D30).
pub(crate) fn server_error(error: &Value) -> Error {
    let message = error
        .get(MESSAGE)
        .and_then(Value::as_str)
        .or_else(|| error.as_str());
    let reported = if is_overflow(error) {
        Error::new(ErrorKind::ContextOverflow).with_detail("reported inside the stream")
    } else {
        Error::new(ErrorKind::Server)
    };

    match message.filter(|message| !message.is_empty()) {
        Some(message) => reported.with_server_message(message),
        None => reported,
    }
}

/// An `error` event. Whatever its data, the stream has failed: the data is the server's error
/// object, or an object with the message, or the message itself.
pub(crate) fn error_event(data: &[u8]) -> Error {
    let text = String::from_utf8_lossy(data);
    let text = text.trim();

    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(body)) => match body.get(ERROR).filter(|error| !error.is_null()) {
            Some(error) => server_error(error),
            None => server_error(&Value::Object(body)),
        },
        Ok(Value::String(message)) => server_error(&Value::String(message)),
        _ => server_error(&Value::String(text.to_owned())),
    }
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
            "Protected conversation exceeds the available context budget.",
            "Output reservation leaves no room for compacted context.",
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
