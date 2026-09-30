//! What a response status means, and what the server said about it.

use std::time::Duration;

use serde_json::Value;

use super::overflow;
use crate::{Error, ErrorKind};

/// A retry delay the server asks for is honored up to this.
const RETRY_AFTER_LIMIT: Duration = Duration::from_secs(30);

/// Whether the kind of this status depends on the response body: a rejected request may be a
/// context overflow, which only the body tells.
pub(crate) fn kind_is_in_body(status: u16) -> bool {
    matches!(status, 400 | 413 | 422)
}

/// The error for a response that is not a success.
pub(crate) fn classify(status: u16, retry_after: Option<&str>, body: &[u8]) -> Error {
    let parsed = serde_json::from_slice::<Value>(body).ok();
    let reported = parsed.as_ref().and_then(|body| body.get("error"));

    // The server's own words: the message of a JSON error, or a plain-text body as it is.
    let message = match &parsed {
        Some(_) => reported
            .and_then(|error| error.get("message").or(Some(error)))
            .and_then(Value::as_str)
            .map(str::to_owned),
        None => std::str::from_utf8(body)
            .ok()
            .map(|text| text.trim().to_owned()),
    };

    // A rejected request is an overflow when its error names one, or its message says so (D30).
    let overflowed = || {
        reported.is_some_and(overflow::is_overflow)
            || message.as_deref().is_some_and(overflow::says_overflow)
    };

    let kind = match status {
        401 | 403 => ErrorKind::Authentication,
        429 => ErrorKind::RateLimited,
        408 | 504 => ErrorKind::Timeout,
        500 | 502 | 503 => ErrorKind::Transport,
        400 | 413 | 422 if overflowed() => ErrorKind::ContextOverflow,
        _ => ErrorKind::Unsupported,
    };

    let mut error = Error::new(kind).with_detail(format!("HTTP {status}"));

    let delay = retry_after
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds).min(RETRY_AFTER_LIMIT));

    if let Some(delay) = delay {
        error = error.with_retry_after(delay);
    }

    match message.filter(|message| !message.is_empty()) {
        Some(message) => error.with_server_message(message),
        None => error,
    }
}

/// Whether a `Content-Type` is an event stream, whatever its parameters.
pub(crate) fn is_event_stream(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("text/event-stream"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(status: u16, body: &str) -> ErrorKind {
        classify(status, None, body.as_bytes()).kind()
    }

    #[test]
    fn statuses_map_to_kinds() {
        assert_eq!(kind(401, ""), ErrorKind::Authentication);
        assert_eq!(kind(403, ""), ErrorKind::Authentication);
        assert_eq!(kind(429, ""), ErrorKind::RateLimited);
        assert_eq!(kind(408, ""), ErrorKind::Timeout);
        assert_eq!(kind(504, ""), ErrorKind::Timeout);
        for status in [500, 502, 503] {
            assert_eq!(kind(status, ""), ErrorKind::Transport);
        }
        for status in [307, 404, 400, 422] {
            assert_eq!(kind(status, ""), ErrorKind::Unsupported);
        }
    }

    #[test]
    fn an_overflow_is_told_by_its_code() {
        let overflow = r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#;
        assert_eq!(kind(400, overflow), ErrorKind::ContextOverflow);
        assert_eq!(kind(413, overflow), ErrorKind::ContextOverflow);
        // The code means nothing on a status that is not a rejected request.
        assert_eq!(kind(500, overflow), ErrorKind::Transport);
    }

    #[test]
    fn an_overflow_is_told_by_its_type_or_its_message() {
        let typed = r#"{"error":{"code":400,"type":"exceed_context_size_error","message":"x"}}"#;
        let said = r#"{"error":{"message":"This model's maximum context length is 8192 tokens."}}"#;
        let bare = r#"{"error":"the request exceeds the available context size"}"#;
        let plain = "Input exceeds the context window\n";

        for body in [typed, said, bare, plain] {
            assert_eq!(kind(400, body), ErrorKind::ContextOverflow, "{body}");
            assert_eq!(kind(422, body), ErrorKind::ContextOverflow, "{body}");
            assert_eq!(kind(500, body), ErrorKind::Transport, "{body}");
            assert_eq!(kind(404, body), ErrorKind::Unsupported, "{body}");
        }
        assert_eq!(
            kind(400, r#"{"error":{"message":"bad field"}}"#),
            ErrorKind::Unsupported
        );
    }

    #[test]
    fn retry_after_is_seconds_capped() {
        let delay = |value| classify(429, Some(value), b"").retry_after();
        assert_eq!(delay("2"), Some(Duration::from_secs(2)));
        assert_eq!(delay("120"), Some(Duration::from_secs(30)));
        assert_eq!(delay("Wed, 21 Oct 2037 07:28:00 GMT"), None);
    }

    #[test]
    fn the_servers_words_are_kept_behind_the_accessor() {
        let json = classify(400, None, br#"{"error":{"message":"bad field"}}"#);
        assert_eq!(json.server_message(), Some("bad field"));
        let text = classify(404, None, b" Model not loaded\n");
        assert_eq!(text.server_message(), Some("Model not loaded"));
        assert_eq!(
            text.to_string(),
            "the request or the response uses something svir cannot represent: HTTP 404"
        );
        assert_eq!(classify(500, None, b"").server_message(), None);
    }

    #[test]
    fn event_streams_are_recognized_with_parameters() {
        assert!(is_event_stream(Some("text/event-stream")));
        assert!(is_event_stream(Some("Text/Event-Stream; charset=utf-8")));
        assert!(!is_event_stream(Some("application/json")));
        assert!(!is_event_stream(None));
    }
}
