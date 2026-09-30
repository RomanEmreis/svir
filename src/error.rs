//! Errors: one type, a kind the caller can act on, and nothing that leaks.

use std::{borrow::Cow, error::Error as StdError, fmt, time::Duration};

use serde::{Deserialize, Serialize};

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The longest server message kept, in bytes. Longer ones are cut on a character boundary.
const SERVER_MESSAGE_LIMIT: usize = 4096;

/// What went wrong, in terms a caller can act on.
///
/// [`is_retryable`](Self::is_retryable) says whether trying the same request again can succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorKind {
    /// The model server could not be reached, or failed transiently (HTTP 500, 502, 503).
    Transport,
    /// The model server did not answer in time (a client timeout, HTTP 408, 504).
    Timeout,
    /// The model server is rate limiting requests (HTTP 429).
    RateLimited,
    /// The stream ended before the answer was complete.
    TruncatedStream,
    /// The model server refused the credentials (HTTP 401, 403).
    Authentication,
    /// The request does not fit in the model's context.
    ContextOverflow,
    /// The model server sent a malformed or inconsistent response.
    Protocol,
    /// The model server reported a failure of its own while answering: an error inside the
    /// stream. Whether it will pass is not known, so it is not retryable.
    Server,
    /// The request or the response uses something svir cannot represent.
    Unsupported,
    /// The response exceeded a configured limit.
    ResponseLimit,
    /// An attachment could not be read, or changed size since it was recorded.
    Attachment,
    /// The client configuration is invalid: the URL, or the source of the API key.
    Config,
}

impl ErrorKind {
    /// Whether trying the same request again can succeed.
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Transport | Self::Timeout | Self::RateLimited | Self::TruncatedStream
        )
    }

    const fn describe(self) -> &'static str {
        match self {
            Self::Transport => "could not reach the model server",
            Self::Timeout => "the model server did not answer in time",
            Self::RateLimited => "the model server is rate limiting requests",
            Self::TruncatedStream => "the stream ended before the answer was complete",
            Self::Authentication => "the model server refused the credentials",
            Self::ContextOverflow => "the request does not fit in the model's context",
            Self::Protocol => "the model server sent a malformed or inconsistent response",
            Self::Server => "the model server failed while answering",
            Self::Unsupported => "the request or the response uses something svir cannot represent",
            Self::ResponseLimit => "the response exceeded a configured limit",
            Self::Attachment => "an attachment could not be read, or changed since it was recorded",
            Self::Config => "the client configuration is invalid",
        }
    }
}

/// An error from svir.
///
/// It never carries credentials, request URLs, or headers. The server's own message, when there
/// is one, is available through [`server_message`](Self::server_message) and left out of
/// `Display` and `Debug`, so it cannot reach a log or a journal by accident.
pub struct Error {
    kind: ErrorKind,
    detail: Option<Cow<'static, str>>,
    retry_after: Option<Duration>,
    server_message: Option<Box<str>>,
    source: Option<Box<dyn StdError + Send + Sync>>,
    /// The request never reached the server.
    unsent: bool,
}

impl Error {
    /// An error of this kind.
    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            detail: None,
            retry_after: None,
            server_message: None,
            source: None,
            unsent: false,
        }
    }

    /// Adds a description written by svir or by the caller, shown after the kind in `Display`.
    ///
    /// Never put text from the server here: use
    /// [`with_server_message`](Self::with_server_message).
    pub fn with_detail(mut self, detail: impl Into<Cow<'static, str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Adds how long the server asked to wait before trying again.
    pub fn with_retry_after(mut self, delay: Duration) -> Self {
        self.retry_after = Some(delay);
        self
    }

    /// Adds the server's own message, cut to 4 KiB on a character boundary.
    pub fn with_server_message(mut self, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > SERVER_MESSAGE_LIMIT {
            let mut at = SERVER_MESSAGE_LIMIT;
            while !message.is_char_boundary(at) {
                at -= 1;
            }
            message.truncate(at);
        }
        self.server_message = Some(message.into_boxed_str());
        self
    }

    /// Adds the underlying error.
    pub fn with_source(mut self, source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Marks the failure as one where the request never reached the server, such as a refused or
    /// timed-out connection. Sending the request again then cannot repeat anything.
    pub fn with_unsent(mut self) -> Self {
        self.unsent = true;
        self
    }

    /// What went wrong.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Whether trying the same request again can succeed.
    pub fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }

    /// Whether the request never reached the server: the connection could not be made. Such a
    /// request can always be sent again.
    pub fn is_unsent(&self) -> bool {
        self.unsent
    }

    /// How long the server asked to wait before trying again, if it said.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    /// The server's own message, if it sent one: for showing to a person, not for logging.
    pub fn server_message(&self) -> Option<&str> {
        self.server_message.as_deref()
    }

    /// The description added with [`with_detail`](Self::with_detail), if any.
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self::new(kind)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.describe())?;
        if let Some(detail) = &self.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("Error");
        out.field("kind", &self.kind);
        if let Some(detail) = &self.detail {
            out.field("detail", detail);
        }
        if let Some(delay) = &self.retry_after {
            out.field("retry_after", delay);
        }
        if let Some(message) = &self.server_message {
            out.field(
                "server_message",
                &format_args!("<{} bytes withheld>", message.len()),
            );
        }
        if self.unsent {
            out.field("unsent", &true);
        }
        if let Some(source) = &self.source {
            out.field("source", source);
        }
        out.finish()
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_kinds_are_exactly_the_transient_ones() {
        use ErrorKind::*;
        let retryable = [Transport, Timeout, RateLimited, TruncatedStream];
        let not_retryable = [
            Authentication,
            ContextOverflow,
            Protocol,
            Server,
            Unsupported,
            ResponseLimit,
            Attachment,
            Config,
        ];
        assert!(retryable.iter().all(|k| k.is_retryable()));
        assert!(not_retryable.iter().all(|k| !k.is_retryable()));
    }

    #[test]
    fn the_server_message_stays_out_of_display_and_debug() {
        let error = Error::new(ErrorKind::Unsupported)
            .with_detail("status 404")
            .with_server_message("Model not loaded: secret-server-detail");

        assert_eq!(
            error.server_message(),
            Some("Model not loaded: secret-server-detail")
        );
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains("secret-server-detail"), "{shown}");
        assert!(shown.contains("status 404"), "{shown}");
        assert!(shown.contains("bytes withheld"), "{shown}");
    }

    #[test]
    fn a_long_server_message_is_cut_on_a_character_boundary() {
        // Three-byte characters, so 4096 falls inside one.
        let message = "\u{20ac}".repeat(2000);
        let error = Error::new(ErrorKind::Protocol).with_server_message(message);
        let kept = error.server_message().unwrap();
        assert_eq!(kept.len(), 4095);
        assert!(kept.chars().all(|c| c == '\u{20ac}'));
    }

    #[test]
    fn display_is_the_kind_then_the_detail() {
        let error = Error::new(ErrorKind::RateLimited)
            .with_retry_after(Duration::from_secs(2))
            .with_detail("HTTP 429");
        assert_eq!(
            error.to_string(),
            "the model server is rate limiting requests: HTTP 429"
        );
        assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
        assert!(error.is_retryable());
    }

    #[test]
    fn the_source_is_chained() {
        let io = std::io::Error::new(std::io::ErrorKind::InvalidData, "size changed");
        let error = Error::new(ErrorKind::Attachment).with_source(io);
        assert_eq!(error.source().unwrap().to_string(), "size changed");
    }
}
