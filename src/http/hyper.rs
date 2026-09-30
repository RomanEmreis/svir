//! The built-in backend: hyper, a pooled client, and nothing svir does not need.
//!
//! hyper follows no redirects and knows no proxies, which is what svir wants by default.

use std::{
    error::Error as StdError,
    fmt, io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use futures_core::Stream;
use hyper::{
    Request, Uri,
    body::{Body, Frame, Incoming, SizeHint},
    header::{HeaderName, HeaderValue},
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

use super::{Backend, HttpRequest, HttpResponse, Method};
use crate::{Error, ErrorKind, body::BodyStream};

#[cfg(feature = "tls")]
type Connector = hyper_rustls::HttpsConnector<HttpConnector>;
#[cfg(not(feature = "tls"))]
type Connector = HttpConnector;

/// The built-in HTTP backend: a pooled hyper client, with rustls when the `tls` feature is on.
///
/// It is what a client uses unless the builder is given another backend.
pub struct Hyper {
    client: Client<Connector, SizedBody>,
}

impl Hyper {
    pub(crate) fn new(connect_timeout: Duration) -> Result<Self, Error> {
        let mut http = HttpConnector::new();
        http.set_connect_timeout(Some(connect_timeout));
        http.set_nodelay(true);
        http.enforce_http(false);

        #[cfg(feature = "tls")]
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(rustls::crypto::ring::default_provider())
            .map_err(|source| {
                Error::new(ErrorKind::Config)
                    .with_detail("TLS could not be set up")
                    .with_source(source)
            })?
            .https_or_http()
            .enable_all_versions()
            .wrap_connector(http);
        #[cfg(not(feature = "tls"))]
        let connector = http;

        Ok(Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
        })
    }
}

impl fmt::Debug for Hyper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hyper").finish_non_exhaustive()
    }
}

impl Backend for Hyper {
    type Body = HyperBody;

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse<HyperBody>, Error> {
        let invalid = |what: &'static str| Error::new(ErrorKind::Config).with_detail(what);
        let uri: Uri = request
            .url
            .parse()
            .map_err(|_| invalid("the URL is not valid"))?;
        let mut builder = Request::builder().uri(uri).method(match request.method {
            Method::Get => hyper::Method::GET,
            Method::Post => hyper::Method::POST,
        });
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| invalid("a header is not valid"))?;
            let mut value =
                HeaderValue::from_str(value).map_err(|_| invalid("a header is not valid"))?;
            value.set_sensitive(name == hyper::header::AUTHORIZATION);
            builder = builder.header(name, value);
        }
        let body = match request.body {
            Some(body) => SizedBody {
                stream: Some(body.stream),
                remaining: body.length,
            },
            None => SizedBody {
                stream: None,
                remaining: 0,
            },
        };
        let request = builder
            .body(body)
            .map_err(|_| invalid("the request is not valid"))?;

        let response = self
            .client
            .request(request)
            .await
            .map_err(|error| failed(&error))?;

        let (parts, incoming) = response.into_parts();
        let headers = parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();

        let body = HyperBody {
            incoming: Some(incoming),
        };
        Ok(HttpResponse::new(parts.status.as_u16(), headers, body))
    }
}

/// The body of a response from [`Hyper`]: its data frames, as they arrive.
pub struct HyperBody {
    incoming: Option<Incoming>,
}

impl Stream for HyperBody {
    type Item = Result<Bytes, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let Some(incoming) = self.incoming.as_mut() else {
                return Poll::Ready(None);
            };
            match Pin::new(incoming).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(frame))) => {
                    // Trailers carry nothing svir reads.
                    if let Ok(data) = frame.into_data() {
                        return Poll::Ready(Some(Ok(data)));
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    self.incoming = None;
                    return Poll::Ready(Some(Err(failed(&error))));
                }
                Poll::Ready(None) => {
                    self.incoming = None;
                    return Poll::Ready(None);
                }
            }
        }
    }
}

impl fmt::Debug for HyperBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HyperBody").finish_non_exhaustive()
    }
}

/// A request body with a length hyper can put in `Content-Length`.
struct SizedBody {
    stream: Option<BodyStream>,
    remaining: u64,
}

impl Body for SizedBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let Some(stream) = self.stream.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(stream).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                self.remaining = self.remaining.saturating_sub(bytes.len() as u64);
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Poll::Ready(Some(Err(error))) => {
                self.stream = None;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.stream = None;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.stream.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}

/// Maps a hyper failure. A failure of the request body itself, such as an attachment that
/// changed, keeps its own kind; a timed-out connect is a timeout; the rest is transport. A failure
/// to connect is marked as never having reached the server.
fn failed(error: &(dyn StdError + 'static)) -> Error {
    let unsent = error
        .downcast_ref::<hyper_util::client::legacy::Error>()
        .is_some_and(hyper_util::client::legacy::Error::is_connect);

    let mut timed_out = false;
    let mut cause: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(current) = cause {
        if let Some(own) = current.downcast_ref::<Error>() {
            let copy = Error::new(own.kind());
            return match own.detail() {
                Some(detail) => copy.with_detail(detail.to_owned()),
                None => copy,
            };
        }
        if let Some(io) = current.downcast_ref::<io::Error>() {
            timed_out |= io.kind() == io::ErrorKind::TimedOut;
        }
        cause = current.source();
    }

    let mapped = if timed_out {
        Error::new(ErrorKind::Timeout).with_detail("connecting to the model server timed out")
    } else {
        Error::new(ErrorKind::Transport).with_detail(error.to_string())
    };
    if unsent { mapped.with_unsent() } else { mapped }
}
