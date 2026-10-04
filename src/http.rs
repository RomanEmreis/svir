//! The HTTP seam: the little of HTTP that svir needs, as a trait.
//!
//! The built-in backend, [`Hyper`], follows no redirects, knows no proxies, and has no TLS options
//! beyond the defaults. A caller who needs a proxy, client certificates, or other roots
//! implements [`Backend`] over an HTTP client that has them and passes it to the client builder.
//!
//! The seam is static: a client is generic over its backend, so nothing here is boxed or
//! dispatched dynamically.

use std::{fmt, future::Future, pin::Pin};

use ::hyper::header::{ACCEPT, CONTENT_TYPE};
use bytes::Bytes;
use futures_core::Stream;

use crate::{Error, body::BodyStream};

mod hyper;

pub use self::hyper::{Hyper, HyperBody};

/// What `Debug` output shows in place of a credential.
pub(crate) const WITHHELD: &str = "<withheld>";

/// A boxed response body, for a [`Backend`] whose HTTP client gives a stream it cannot name.
pub type BoxBody = Pin<Box<dyn Stream<Item = Result<Bytes, Error>> + Send + 'static>>;

/// An HTTP client svir can run on.
///
/// An implementation sends the request as given and returns the response as received: it must
/// not follow redirects, retry, or alter the body. It reports a failure to connect or to read as
/// [`ErrorKind::Transport`](crate::ErrorKind::Transport) and its own timeouts as
/// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout); svir maps status codes itself.
///
/// ```
/// use svir::Error;
/// use svir::http::{Backend, BoxBody, HttpRequest, HttpResponse};
///
/// struct Mine;
///
/// impl Backend for Mine {
///     type Body = BoxBody;
///
///     async fn send(&self, request: HttpRequest) -> Result<HttpResponse<BoxBody>, Error> {
///         // Send `request` with an HTTP client of your choice.
///         # let _ = request;
///         # unimplemented!()
///     }
/// }
/// ```
pub trait Backend: Send + Sync + 'static {
    /// The response body: its bytes as they arrive. Dropping it must close the exchange.
    type Body: Stream<Item = Result<Bytes, Error>> + Send + Unpin + 'static;

    /// Sends `request` and resolves once the response headers have arrived.
    fn send(
        &self,
        request: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse<Self::Body>, Error>> + Send;
}

/// The methods svir uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Method {
    /// `GET`
    Get,
    /// `POST`
    Post,
}

/// A request for a [`Backend`] to send.
#[non_exhaustive]
pub struct HttpRequest {
    /// The method.
    pub method: Method,
    /// The absolute URL.
    pub url: String,
    /// Header names, lowercase, and their values. Every value but those of `content-type` and
    /// `accept` may be a credential, in `authorization` or in a header the caller added: keep it
    /// out of logs, and send it marked sensitive where the HTTP client can.
    pub headers: Vec<(String, String)>,
    /// The body, if there is one.
    pub body: Option<HttpBody>,
}

/// A request body of a known length. Send it with `Content-Length`, not chunked: not every model
/// server accepts a chunked request.
#[derive(Debug)]
pub struct HttpBody {
    /// The exact number of bytes `stream` yields.
    pub length: u64,
    /// The bytes.
    pub stream: BodyStream,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let shown = if is_plain(name) {
                    value.as_str()
                } else {
                    WITHHELD
                };
                (name.as_str(), shown)
            })
            .collect();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body_length", &self.body.as_ref().map(|body| body.length))
            .finish()
    }
}

/// Whether a request header holds nothing secret: `content-type` or `accept`, which svir writes
/// itself. Any other value may be a credential, from the API key or a header the caller added.
pub(crate) fn is_plain(name: &str) -> bool {
    [CONTENT_TYPE, ACCEPT].iter().any(|plain| plain == name)
}

/// A response from a [`Backend`], with a body of type `B`.
#[non_exhaustive]
pub struct HttpResponse<B> {
    /// The status code.
    pub status: u16,
    /// Header names, lowercase, and their values.
    pub headers: Vec<(String, String)>,
    /// The body, as it arrives.
    pub body: B,
}

impl<B> HttpResponse<B> {
    /// A response.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: B) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// The first value of header `name`, which must be lowercase.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

impl<B> fmt::Debug for HttpResponse<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}
