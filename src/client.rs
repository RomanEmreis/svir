//! The client: one type over a wire API, a transport, and what was learned about the server.

use std::{
    borrow::Borrow,
    fmt,
    future::{Future, poll_fn},
    net::IpAddr,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Completion, Error, ErrorKind, EventStream, Limits, Mode, RawStream, Request, Think,
    http::{Backend, HttpBody, HttpRequest, HttpResponse, Hyper, Method},
    layer::{Erased, Layer, Next, Wrap},
    openai::chat::{Decoder, Encoder, status},
};

/// Connecting takes at most this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A response that sends nothing for this long has stalled. Generous: a local model can take
/// minutes over a long prompt before its first token.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How long an error's body is waited for when only its message is at stake.
const MESSAGE_WAIT: Duration = Duration::from_millis(300);
/// The most of an error body that is read.
const ERROR_BODY_LIMIT: usize = 64 * 1024;
/// The most of an API key file that is read.
const KEY_FILE_LIMIT: u64 = 4096;

/// A client for one model server.
///
/// Cheap to clone; clones share the connection pool and what was learned about the server, such
/// as that it rejects optional fields.
///
/// `B` is the HTTP backend: the built-in [`Hyper`] unless the builder is given another. Most code
/// never names it and writes `Client`.
///
/// ```no_run
/// use svir::prelude::*;
///
/// # async fn run() -> Result<(), svir::Error> {
/// let client = Client::openai("http://127.0.0.1:1234").build()?;
/// let request = Request::new("qwen3-27b").user("Explain ownership in one paragraph.");
///
/// let mut stream = client.stream(&request).await?;
/// while let Some(event) = stream.next().await {
///     if let Event::Text(delta) = event? {
///         print!("{delta}");
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct Client<B = Hyper> {
    inner: Arc<Inner<B>>,
}

struct Inner<B> {
    http: B,
    /// Set once when the client is built, as everything here but `lean`.
    chat_url: Box<str>,
    models_url: Box<str>,
    authorization: Option<Secret>,
    /// The layers, outermost first.
    layers: Box<[Box<dyn Erased<B>>]>,
    idle: Option<Duration>,
    include_usage: bool,
    mode: Mode,
    limits: Limits,
    think: Think,
    context: Option<u64>,
    /// The server rejected the optional fields once; they are left out from now on.
    lean: AtomicBool,
}

impl<B> Clone for Client<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Client {
    /// A client for an OpenAI-compatible server at `url`, which may be written with or without
    /// `/v1`. Finish with [`build`](ClientBuilder::build).
    pub fn openai(url: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            url: url.into(),
            key: Key::None,
            allow_http: false,
            include_usage: true,
            mode: Mode::default(),
            limits: Limits::default(),
            think: Think::default(),
            context: None,
            connect_timeout: CONNECT_TIMEOUT,
            idle_timeout: Some(IDLE_TIMEOUT),
            http: Http::BuiltIn(Hyper::new),
            layers: Vec::new(),
            misuse: None,
        }
    }
}

impl<B: Backend> Client<B> {
    /// Sends `request` and returns the answer as a stream of events.
    ///
    /// Dropping the stream cancels the request and closes the connection.
    pub async fn stream(&self, request: impl Borrow<Request>) -> Result<EventStream<B>, Error> {
        if self.inner.layers.is_empty() {
            return self.direct(request.borrow()).await;
        }

        self.run_from(0, request.borrow().clone()).await
    }

    /// Runs the layers from `index` on, then the call itself.
    pub(crate) async fn run_from(
        &self,
        index: usize,
        request: Request,
    ) -> Result<EventStream<B>, Error> {
        match self.inner.layers.get(index) {
            Some(layer) => {
                let next = Next::new(self.clone(), index + 1);
                layer.call(request, next).await
            }
            None => self.direct(&request).await,
        }
    }

    /// The call itself, under every layer.
    async fn direct(&self, request: &Request) -> Result<EventStream<B>, Error> {
        let bytes = self.open(request).await?;
        let inner = &self.inner;
        let decoder = Decoder::new(inner.mode)
            .limits(inner.limits)
            .think(inner.think);

        Ok(EventStream::new(bytes, decoder))
    }

    /// Sends `request` and returns the whole answer.
    pub async fn complete(&self, request: impl Borrow<Request>) -> Result<Completion, Error> {
        self.stream(request).await?.completion().await
    }

    /// Sends `request` and returns the server's bytes unchanged, after status mapping and
    /// compatibility handling: for a proxy that relays the stream and reads it on the way past
    /// with a [`Decoder`]. Layers work on events, so they do not apply here.
    pub async fn send(&self, request: impl Borrow<Request>) -> Result<RawStream<B>, Error> {
        self.open(request.borrow()).await
    }

    /// Lists the models the server reports, unfiltered.
    pub async fn list_models(&self) -> Result<Vec<Model>, Error> {
        let inner = &self.inner;
        let request = self.request(Method::Get, inner.models_url.to_string(), None);
        let mut response = self.exchange(request).await?;
        if !(200..300).contains(&response.status) {
            return Err(self.rejected(&mut response).await);
        }
        let wait = inner.idle.unwrap_or(IDLE_TIMEOUT);
        let body = read_some(&mut response.body, inner.limits.wire_bytes, wait).await;
        let malformed =
            || Error::new(ErrorKind::Protocol).with_detail("the model listing is malformed");
        let listing: Value = serde_json::from_slice(&body).map_err(|_| malformed())?;
        let data = listing
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?;
        data.iter()
            .map(|model| serde_json::from_value(model.clone()).map_err(|_| malformed()))
            .collect()
    }

    /// Opens the answer stream, learning on the way whether the server takes optional fields.
    async fn open(&self, request: &Request) -> Result<RawStream<B>, Error> {
        let lean = self.inner.lean.load(Ordering::Relaxed);
        let (status, error) = match self.attempt(request, lean).await? {
            Ok(bytes) => return Ok(bytes),
            Err(rejected) => rejected,
        };

        // A 400 or 422 to a request with optional fields may be about those fields. Nothing was
        // generated, so one attempt without them is safe. A rejection the body explains, a context
        // overflow or a blocked prompt, is not about them (D26, D36).
        let optional = request.reasoning.is_some()
            || request.include_usage.unwrap_or(self.inner.include_usage);
        if !lean
            && optional
            && matches!(status, 400 | 422)
            && error.kind() == ErrorKind::Unsupported
        {
            if let Ok(Ok(bytes)) = self.attempt(request, true).await {
                self.inner.lean.store(true, Ordering::Relaxed);
                return Ok(bytes);
            }
        }
        Err(error)
    }

    /// One exchange. The outer error is a failure to send; the inner one is the server's refusal,
    /// with its status.
    async fn attempt(
        &self,
        request: &Request,
        lean: bool,
    ) -> Result<Result<RawStream<B>, (u16, Error)>, Error> {
        let inner = &self.inner;
        let mut encoder = Encoder::new().lean(lean).include_usage(inner.include_usage);
        if let Some(context) = inner.context {
            encoder = encoder.context_tokens(context);
        }
        let body = encoder.encode_files(request).await?;
        let body = HttpBody {
            length: body.len(),
            stream: body.into_stream(),
        };

        let request = self.request(Method::Post, inner.chat_url.to_string(), Some(body));
        let mut response = self.exchange(request).await?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            return Ok(Err((status, self.rejected(&mut response).await)));
        }
        if !status::is_event_stream(response.header("content-type")) {
            return Err(Error::new(ErrorKind::Unsupported)
                .with_detail("the response is not an event stream"));
        }
        Ok(Ok(RawStream::new(response.body, inner.idle)))
    }

    fn request(&self, method: Method, url: String, body: Option<HttpBody>) -> HttpRequest {
        let mut headers = Vec::new();
        if body.is_some() {
            headers.push(("content-type".to_owned(), "application/json".to_owned()));
            headers.push(("accept".to_owned(), "text/event-stream".to_owned()));
        }
        if let Some(key) = &self.inner.authorization {
            headers.push(("authorization".to_owned(), format!("Bearer {}", key.0)));
        }
        HttpRequest {
            method,
            url,
            headers,
            body,
        }
    }

    /// Sends a request and waits for the response headers, no longer than the idle timeout.
    async fn exchange(&self, request: HttpRequest) -> Result<HttpResponse<B::Body>, Error> {
        let sent = self.inner.http.send(request);
        match self.inner.idle {
            None => sent.await,
            Some(idle) => tokio::time::timeout(idle, sent).await.map_err(|_| {
                Error::new(ErrorKind::Timeout).with_detail("the server sent no response")
            })?,
        }
    }

    /// The error for a response that is not a success, with as much of its body as is worth
    /// waiting for.
    async fn rejected(&self, response: &mut HttpResponse<B::Body>) -> Error {
        let wait = if status::kind_is_in_body(response.status) {
            self.inner.idle.unwrap_or(IDLE_TIMEOUT)
        } else {
            MESSAGE_WAIT
        };
        let body = read_some(&mut response.body, ERROR_BODY_LIMIT, wait).await;
        status::classify(response.status, response.header("retry-after"), &body)
    }
}

impl<B> fmt::Debug for Client<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("url", &self.inner.chat_url)
            .field("api_key", &self.inner.authorization)
            .field("mode", &self.inner.mode)
            .finish_non_exhaustive()
    }
}

/// Builds a [`Client`]. Created by [`Client::openai`].
#[must_use = "a builder does nothing until `build` is called"]
pub struct ClientBuilder<B = Hyper> {
    url: String,
    key: Key,
    allow_http: bool,
    include_usage: bool,
    mode: Mode,
    limits: Limits,
    think: Think,
    context: Option<u64>,
    connect_timeout: Duration,
    idle_timeout: Option<Duration>,
    http: Http<B>,
    layers: Vec<Box<dyn Erased<B>>>,
    /// A mistake in how the builder was used, reported by `build`.
    misuse: Option<&'static str>,
}

/// Where the backend comes from: built when the client is, or given by the caller.
enum Http<B> {
    BuiltIn(fn(Duration) -> Result<B, Error>),
    Given(B),
}

enum Key {
    None,
    Value(String),
    Env(String),
    File(PathBuf),
}

impl<B> ClientBuilder<B> {
    /// Sends `key` as a Bearer token.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.key = Key::Value(key.into());
        self
    }

    /// Reads the API key from environment variable `name` when the client is built.
    pub fn api_key_env(mut self, name: impl Into<String>) -> Self {
        self.key = Key::Env(name.into());
        self
    }

    /// Reads the API key from a file when the client is built. The file holds the key and
    /// nothing else; surrounding whitespace is dropped.
    pub fn api_key_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.key = Key::File(path.into());
        self
    }

    /// Allows plain HTTP to a host that is not loopback, for a model server on a trusted network.
    pub fn allow_http(mut self) -> Self {
        self.allow_http = true;
        self
    }

    /// Whether to ask for token usage when a request does not say. On by default; a server that
    /// rejects the field is detected once and remembered.
    pub fn include_usage(mut self, include: bool) -> Self {
        self.include_usage = include;
        self
    }

    /// Sets how strictly responses are read. Strict by default.
    pub fn mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Reads responses leniently: unknown input is skipped.
    pub fn lenient(self) -> Self {
        self.mode(Mode::Lenient)
    }

    /// Sets the bounds on one response.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets what to do with `<think>` tags in the answer text.
    pub fn think(mut self, think: Think) -> Self {
        self.think = think;
        self
    }

    /// Sets the model's context size, so a request that cannot fit fails before it is sent.
    pub fn context_tokens(mut self, tokens: u64) -> Self {
        self.context = Some(tokens);
        self
    }

    /// Sets how long connecting may take. 10 seconds by default.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets how long the server may send nothing, before the response and between its pieces.
    /// 5 minutes by default.
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// Waits for the server as long as it takes.
    pub fn no_idle_timeout(mut self) -> Self {
        self.idle_timeout = None;
        self
    }

    /// Uses `backend` for HTTP instead of the built-in one: for a proxy, client certificates, or
    /// other roots. [`connect_timeout`](Self::connect_timeout) then does not apply.
    ///
    /// Call it before adding layers: a layer is tied to the backend it was added for.
    pub fn http<C: Backend>(self, backend: C) -> ClientBuilder<C> {
        let misuse = if self.layers.is_empty() {
            self.misuse
        } else {
            Some("http() must be called before layers are added")
        };

        ClientBuilder {
            url: self.url,
            key: self.key,
            allow_http: self.allow_http,
            include_usage: self.include_usage,
            mode: self.mode,
            limits: self.limits,
            think: self.think,
            context: self.context,
            connect_timeout: self.connect_timeout,
            idle_timeout: self.idle_timeout,
            http: Http::Given(backend),
            layers: Vec::new(),
            misuse,
        }
    }

    /// Validates the configuration and builds the client.
    ///
    /// Fails with [`ErrorKind::Config`] for a URL that is not acceptable or an API key that
    /// cannot be read.
    pub fn build(self) -> Result<Client<B>, Error> {
        if let Some(misuse) = self.misuse {
            return Err(config(misuse));
        }

        let base = base_url(&self.url, self.allow_http)?;
        let authorization = self.key.resolve()?;
        let http = match self.http {
            Http::Given(backend) => backend,
            Http::BuiltIn(make) => make(self.connect_timeout)?,
        };
        Ok(Client {
            inner: Arc::new(Inner {
                http,
                chat_url: format!("{base}/v1/chat/completions").into_boxed_str(),
                models_url: format!("{base}/v1/models").into_boxed_str(),
                authorization,
                layers: self.layers.into_boxed_slice(),
                idle: self.idle_timeout,
                include_usage: self.include_usage,
                mode: self.mode,
                limits: self.limits,
                think: self.think,
                context: self.context,
                lean: AtomicBool::new(false),
            }),
        })
    }
}

impl<B: Backend> ClientBuilder<B> {
    /// Adds a layer around every call. The first layer added is the outermost: it sees the
    /// request first and the answer last.
    pub fn layer(mut self, layer: impl Layer<B>) -> Self {
        self.layers.push(Box::new(layer));
        self
    }

    /// Adds a closure as a layer. It gets the request and the rest of the stack, and returns the
    /// answer:
    ///
    /// ```no_run
    /// # use svir::prelude::*;
    /// # fn build() -> Result<(), svir::Error> {
    /// let client = Client::openai("http://127.0.0.1:1234")
    ///     .wrap(|request, next| async move {
    ///         let started = std::time::Instant::now();
    ///         let answer = next.run(request).await;
    ///         println!("the response started after {:?}", started.elapsed());
    ///         answer
    ///     })
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// `next.run` resolves when the response starts, not when the answer is complete; to see the
    /// whole answer, use [`EventStream::inspect`] on what it returns.
    pub fn wrap<F, Fut>(self, layer: F) -> Self
    where
        F: Fn(Request, Next<B>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<EventStream<B>, Error>> + Send + 'static,
    {
        self.layer(Wrap(layer))
    }
}

impl<B> fmt::Debug for ClientBuilder<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Key {
    fn resolve(self) -> Result<Option<Secret>, Error> {
        let key = match self {
            Key::None => return Ok(None),
            Key::Value(key) => key,
            Key::Env(name) => std::env::var(&name).map_err(|_| {
                config(format!(
                    "the API key variable {name} is not set or not text"
                ))
            })?,
            Key::File(path) => {
                let unreadable = |source: std::io::Error| {
                    config("the API key file cannot be read").with_source(source)
                };
                let size = std::fs::metadata(&path).map_err(unreadable)?.len();
                if size > KEY_FILE_LIMIT {
                    return Err(config("the API key file is too large to be a key"));
                }
                std::fs::read_to_string(&path).map_err(unreadable)?
            }
        };
        let key = key.trim();
        if key.is_empty() || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(config("the API key is empty or is not a single token"));
        }
        Ok(Some(Secret(key.into())))
    }
}

/// An API key. Never shown.
#[derive(Clone)]
struct Secret(Box<str>);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<withheld>")
    }
}

/// A model, as the server lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Model {
    /// The ID to put in a request.
    pub id: String,
    /// A display name, when the server gives one.
    #[serde(default)]
    pub name: Option<String>,
}

/// Reads up to `limit` bytes of a body for at most `wait`, keeping what arrived.
async fn read_some<S>(body: &mut S, limit: usize, wait: Duration) -> Vec<u8>
where
    S: Stream<Item = Result<Bytes, Error>> + Unpin,
{
    let mut out = Vec::new();
    let _ = tokio::time::timeout(wait, async {
        while out.len() < limit {
            let Some(Ok(chunk)) = poll_fn(|cx| Pin::new(&mut *body).poll_next(cx)).await else {
                break;
            };
            let room = limit - out.len();
            out.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
    })
    .await;
    out
}

fn config(detail: impl Into<std::borrow::Cow<'static, str>>) -> Error {
    Error::new(ErrorKind::Config).with_detail(detail)
}

/// Validates a base URL and returns it without a trailing slash or `/v1`.
fn base_url(url: &str, allow_http: bool) -> Result<String, Error> {
    let url = url.trim();
    if url.contains('#') {
        return Err(config("the base URL has a fragment"));
    }
    let uri: ::hyper::Uri = url
        .parse()
        .map_err(|_| config("the base URL is not a valid URL"))?;
    let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return Err(config("the base URL needs a scheme and a host"));
    };
    if authority.as_str().contains('@') {
        return Err(config("the base URL has credentials"));
    }
    if uri.query().is_some() {
        return Err(config("the base URL has a query"));
    }
    match scheme {
        "https" if cfg!(any(feature = "tls", feature = "tls-aws-lc")) => {}
        "https" => {
            return Err(config(
                "an https URL needs the `tls` or the `tls-aws-lc` feature",
            ));
        }
        "http" if allow_http || is_loopback(authority.host()) => {}
        "http" => {
            return Err(config(
                "plain HTTP to a host that is not loopback; use https, or allow_http()",
            ));
        }
        _ => return Err(config("the base URL is not http or https")),
    }
    let path = uri.path().trim_end_matches('/');
    let path = path.strip_suffix("/v1").unwrap_or(path);
    Ok(format!("{scheme}://{authority}{path}"))
}

fn is_loopback(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_url_is_normalized() {
        for url in [
            "http://127.0.0.1:1234",
            "http://127.0.0.1:1234/",
            "http://127.0.0.1:1234/v1",
            " http://127.0.0.1:1234/v1/ ",
        ] {
            assert_eq!(
                base_url(url, false).unwrap(),
                "http://127.0.0.1:1234",
                "{url}"
            );
        }
        assert_eq!(
            base_url("http://localhost:8080/openai/v1", false).unwrap(),
            "http://localhost:8080/openai"
        );
    }

    #[test]
    fn unsafe_base_urls_are_refused() {
        for url in [
            "http://user:pass@127.0.0.1:1234/v1",
            "http://127.0.0.1:1234/v1?key=x",
            "http://127.0.0.1:1234/v1#x",
            "http://example.com/v1",
            "ftp://127.0.0.1/v1",
            "127.0.0.1:1234",
        ] {
            let error = base_url(url, false).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Config, "{url}");
        }
        assert!(base_url("http://192.168.1.50:8080/v1", true).is_ok());
        assert!(base_url("http://[::1]:1234/v1", false).is_ok());
    }

    #[test]
    fn an_api_key_is_one_token_and_never_shown() {
        let key = Key::Value(" secret-key\n".into())
            .resolve()
            .unwrap()
            .unwrap();
        assert_eq!(&*key.0, "secret-key");
        assert_eq!(format!("{key:?}"), "<withheld>");
        assert!(Key::Value("two words".into()).resolve().is_err());
        assert!(Key::Value("  ".into()).resolve().is_err());
        assert!(
            Key::Env("SVIR_TEST_KEY_THAT_IS_NOT_SET".into())
                .resolve()
                .is_err()
        );
        assert!(Key::None.resolve().unwrap().is_none());
    }

    #[test]
    fn the_streams_need_no_pinning_and_can_cross_threads() {
        fn movable<T: Send + Unpin + 'static>() {}
        movable::<EventStream>();
        movable::<RawStream>();
        movable::<Client>();
    }
}
