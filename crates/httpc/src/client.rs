//! The client: one round trip over a pooled connection, timed.

use std::collections::HashMap;
use std::error::Error as _;
use std::io::{self, Write as _};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use base64::Engine;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::{http1, http2};
use hyper::header::{HeaderName, HeaderValue};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::ServerName;
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::cancel::CancelToken;
use crate::error::{io_message, Error, ErrorKind, Fail, TlsFailure};
use crate::headers::Headers;
use crate::proxy::{http_connect, socks5_connect, Proxy, ProxyScheme};
use crate::status::status_text;
use crate::target::{check_method, join_host_port, parse_target, Target};
use crate::tls::{io_fail, tls_error_in, tls_fail, TlsConfig, TlsConfigError, TlsParts};

/// How long an idle connection is kept for the next request.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

type ReqBody = Full<Bytes>;

/// A byte stream a connection runs over: TCP, or TLS over anything.
trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Io for T {}
type BoxIo = Box<dyn Io>;

/// Which HTTP versions a client speaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Protocols {
    /// HTTP/2 when TLS negotiates it, else HTTP/1.1.
    #[default]
    Any,
    /// HTTP/1.1 only: HTTP/2 is not offered.
    Http1,
    /// HTTP/2 only, over TLS: the request fails when the server does not
    /// offer it.
    Http2,
}

/// One request. Build it with `Request::new` and the chained setters, or
/// fill the fields in directly.
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// The method, any token: `GET`, `PURGE`.
    pub method: String,
    /// The absolute `http://` or `https://` URL. The path and the query
    /// are sent as written; a fragment is dropped; `user:password@`
    /// becomes basic `Authorization` unless the header is set.
    pub url: String,
    /// The headers. On the wire they go in canonical case, sorted by
    /// name. `Content-Length`, `Transfer-Encoding` and `Trailer` are
    /// ignored (the body decides them); a `Host` header acts as `host`
    /// when `host` is unset; without `User-Agent` the client sends
    /// `Go-http-client/1.1` (an empty `User-Agent` sends none); without
    /// `Accept-Encoding` (and without `Range`, for any method but HEAD)
    /// it asks for gzip and decodes the answer.
    pub headers: Headers,
    /// Overrides the Host header (`:authority` over HTTP/2); the
    /// connection still goes to the URL's host.
    pub host: Option<String>,
    /// The body. An empty body sends `Content-Length: 0` for POST, PUT
    /// and PATCH and no length for other methods.
    pub body: Vec<u8>,
    /// Fail with `ErrorKind::Timeout` when the response, body included,
    /// has not arrived within this time.
    pub timeout: Option<Duration>,
    /// As `timeout`, but as a point in time; the earlier of the two wins.
    /// The redirect helper uses it to give every hop one shared deadline.
    pub deadline: Option<Instant>,
    /// Fail with `ErrorKind::Cancelled` as soon as this is cancelled.
    pub cancel: Option<CancelToken>,
    /// The proxy to go through; `None` connects directly. The client
    /// never reads the proxy environment variables itself.
    pub proxy: Option<Proxy>,
    /// Read at most this many bytes of (decoded) body; a longer body sets
    /// `Response::truncated`. `None` reads it all.
    pub max_body: Option<u64>,
}

impl Request {
    /// A request with a method and a URL and nothing else.
    pub fn new(method: &str, url: &str) -> Request {
        Request {
            method: method.to_string(),
            url: url.to_string(),
            ..Request::default()
        }
    }

    /// A GET request.
    pub fn get(url: &str) -> Request {
        Request::new("GET", url)
    }

    /// A POST request with a body.
    pub fn post(url: &str, body: impl Into<Vec<u8>>) -> Request {
        Request::new("POST", url).body(body)
    }

    /// Adds a header, keeping earlier values of the name.
    pub fn header(mut self, name: &str, value: &str) -> Request {
        self.headers.add(name, value);
        self
    }

    /// Sets the body.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Request {
        self.body = body.into();
        self
    }

    /// Overrides the Host header.
    pub fn host(mut self, host: &str) -> Request {
        self.host = Some(host.to_string());
        self
    }

    /// Sets the timeout for the whole exchange.
    pub fn timeout(mut self, timeout: Duration) -> Request {
        self.timeout = Some(timeout);
        self
    }

    /// Attaches a cancellation token.
    pub fn cancel(mut self, token: &CancelToken) -> Request {
        self.cancel = Some(token.clone());
        self
    }

    /// Sends the request through a proxy.
    pub fn proxy(mut self, proxy: Proxy) -> Request {
        self.proxy = Some(proxy);
        self
    }

    /// Caps the bytes of body read.
    pub fn max_body(mut self, max: u64) -> Request {
        self.max_body = Some(max);
        self
    }

    /// Checks the method and the URL without sending anything, with the
    /// message a request that cannot be built has always had:
    /// `net/http: invalid method "G ET"`, `parse "http://h:x/": invalid
    /// port ":x" after host`.
    pub fn check(&self) -> Result<(), String> {
        check_method(&self.method)?;
        parse_target(&self.url).map(|_| ()).map_err(|f| f.message)
    }
}

/// Where the time of a round trip went. A reused connection has no DNS,
/// connect or TLS time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    /// Resolving the host name; zero for an IP address.
    pub dns: Duration,
    /// Making the TCP connection (to the proxy, when there is one).
    pub connect: Duration,
    /// The TLS handshake (both, through an `https://` proxy).
    pub tls: Duration,
    /// From the start of the request to the first byte of the response.
    pub ttfb: Duration,
    /// From the start of the request to the end of the body.
    pub total: Duration,
    /// Whether the request went over a connection an earlier one opened.
    pub reused: bool,
    /// When the request started.
    pub started: Instant,
}

impl Timings {
    /// Folds the timings of a later round trip into those of an earlier
    /// one, for an exchange that took several (a redirect, a digest
    /// challenge): the lookups and handshakes add up, the connection time
    /// is that of the first connection made, the first byte and the total
    /// are measured from the start of `self` to the end of `later`, and
    /// `reused` is the later one's.
    pub fn then(&self, later: &Timings) -> Timings {
        let gap = later.started.saturating_duration_since(self.started);
        Timings {
            dns: self.dns + later.dns,
            connect: if self.connect.is_zero() {
                later.connect
            } else {
                self.connect
            },
            tls: self.tls + later.tls,
            ttfb: gap + later.ttfb,
            total: gap + later.total,
            reused: later.reused,
            started: self.started,
        }
    }
}

/// A response, its body read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// The status code.
    pub status_code: u16,
    /// The status line without the protocol: `200 OK`. Over HTTP/1.1 the
    /// reason is the server's own; over HTTP/2 it is the standard text.
    pub status: String,
    /// The protocol the response came over: `HTTP/1.1`, `HTTP/2.0`.
    pub proto: String,
    /// The headers, names in canonical form, in the order received.
    pub headers: Headers,
    /// The body, gzip already decoded when the client asked for it.
    pub body: Vec<u8>,
    /// Set when the body was longer than the request's `max_body`; `body`
    /// then holds the first `max_body` bytes.
    pub truncated: bool,
    /// Set when the client asked for gzip itself and decoded the body;
    /// `Content-Encoding` and `Content-Length` are then removed from
    /// `headers`.
    pub uncompressed: bool,
    /// Where the time went.
    pub timings: Timings,
    /// The method of the request this answers (after redirects, the last).
    pub method: String,
    /// The URL of the request this answers (after redirects, the last).
    pub url: String,
}

impl Response {
    /// The reason phrase: `OK` for a status of `200 OK`.
    pub fn status_text(&self) -> &str {
        match self.status.split_once(' ') {
            Some((_, text)) => text,
            None => &self.status,
        }
    }

    /// The first value of a header.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    /// The body as text, invalid UTF-8 replaced.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Something that performs one HTTP round trip: the `Client`, or a layer
/// over it (digest auth answers a challenge by sending twice).
pub trait RoundTrip {
    /// Sends one request and reads its response, without following
    /// redirects.
    fn round_trip(&self, req: &Request) -> Result<Response, Error>;
}

/// Builds a `Client`.
#[derive(Clone, Debug, Default)]
pub struct ClientBuilder {
    tls: TlsConfig,
    protocols: Protocols,
}

impl ClientBuilder {
    /// Sets the TLS settings.
    pub fn tls(mut self, tls: TlsConfig) -> ClientBuilder {
        self.tls = tls;
        self
    }

    /// Sets which HTTP versions the client speaks.
    pub fn protocols(mut self, protocols: Protocols) -> ClientBuilder {
        self.protocols = protocols;
        self
    }

    /// Builds the client, checking the PEM of the TLS settings. Building
    /// is cheap: nothing is connected and the system's roots are read
    /// once per process, on the first HTTPS request.
    pub fn build(self) -> Result<Client, TlsConfigError> {
        Ok(Client {
            inner: Arc::new(Inner {
                tls: TlsParts::new(&self.tls)?,
                protocols: self.protocols,
                tls_main: OnceLock::new(),
                tls_proxy: OnceLock::new(),
                pool: Mutex::new(HashMap::new()),
            }),
        })
    }
}

/// An HTTP client with its own connection pool. It is cheap to clone
/// (clones share the pool) and safe to use from several threads. One
/// client has one set of TLS settings and protocols; build one per
/// distinct set and keep it, so its connections are reused.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("protocols", &self.inner.protocols)
            .finish_non_exhaustive()
    }
}

impl Default for Client {
    fn default() -> Client {
        Client::new()
    }
}

impl Client {
    /// A client with the default TLS settings that speaks HTTP/2 when the
    /// server offers it.
    pub fn new() -> Client {
        match ClientBuilder::default().build() {
            Ok(c) => c,
            // The default settings hold no PEM, so there is nothing to
            // reject.
            Err(e) => unreachable!("default TLS settings rejected: {e}"),
        }
    }

    /// Starts building a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Sends one request and reads its response. Redirects are not
    /// followed: a 302 is returned as it is. Blocks until the response
    /// has been read, the deadline passes or the request is cancelled.
    pub fn send(&self, req: &Request) -> Result<Response, Error> {
        let inner = self.inner.clone();
        let owned = req.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        runtime().spawn(async move {
            let _ = tx.send(run(inner, owned).await);
        });
        rx.recv().unwrap_or_else(|_| {
            Err(Error::new(
                ErrorKind::Other,
                &req.method,
                &req.url,
                "the client's runtime stopped",
            ))
        })
    }

    /// Sends a request and follows redirects the default way: up to ten,
    /// `Authorization` and `Cookie` dropped when a redirect leaves the
    /// host. See `follow_redirects` for a policy of the caller's own.
    pub fn execute(&self, req: &Request) -> Result<Response, Error> {
        crate::redirect::follow_redirects(self, req, &crate::redirect::RedirectPolicy::default())
    }

    /// Closes the connections that are idle in the pool.
    pub fn close_idle_connections(&self) {
        self.inner
            .pool
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

impl RoundTrip for Client {
    fn round_trip(&self, req: &Request) -> Result<Response, Error> {
        self.send(req)
    }
}

/// The runtime every client's connections live on: one thread for the
/// process, started on first use.
fn runtime() -> &'static tokio::runtime::Handle {
    static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("apic-httpc".to_string())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match rt {
                    Ok(rt) => {
                        let _ = tx.send(Ok(rt.handle().clone()));
                        rt.block_on(std::future::pending::<()>());
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            });
        match spawned.map(|_| rx.recv()) {
            Ok(Ok(Ok(handle))) => handle,
            other => panic!("cannot start the HTTP client's runtime: {other:?}"),
        }
    })
}

struct Inner {
    tls: TlsParts,
    protocols: Protocols,
    /// The TLS configuration for servers, built on the first HTTPS dial.
    tls_main: OnceLock<Result<Arc<ClientConfig>, Fail>>,
    /// The same offering HTTP/1.1 only, for an `https://` proxy.
    tls_proxy: OnceLock<Result<Arc<ClientConfig>, Fail>>,
    pool: Mutex<HashMap<PoolKey, Vec<Idle>>>,
}

/// What a pooled connection can be reused for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PoolKey {
    https: bool,
    /// `host:port` of the server; empty for plain HTTP through an HTTP
    /// proxy, where one connection to the proxy serves any server.
    host_port: String,
    proxy: Option<Proxy>,
}

impl PoolKey {
    fn new(target: &Target, proxy: Option<&Proxy>) -> PoolKey {
        let via_http_proxy = proxy.is_some_and(|p| !p.is_socks()) && !target.is_https();
        PoolKey {
            https: target.is_https(),
            host_port: if via_http_proxy {
                String::new()
            } else {
                target.host_port()
            },
            proxy: proxy.cloned(),
        }
    }
}

enum Sender {
    H1(http1::SendRequest<ReqBody>),
    H2(http2::SendRequest<ReqBody>),
}

impl Sender {
    async fn send(
        &mut self,
        req: hyper::Request<ReqBody>,
    ) -> hyper::Result<hyper::Response<Incoming>> {
        match self {
            Sender::H1(s) => s.send_request(req).await,
            Sender::H2(s) => s.send_request(req).await,
        }
    }

    fn is_closed(&self) -> bool {
        match self {
            Sender::H1(s) => s.is_closed(),
            Sender::H2(s) => s.is_closed(),
        }
    }
}

/// A connection ready for a request.
struct Conn {
    sender: Sender,
    /// The request line carries the absolute URL: plain HTTP through an
    /// HTTP proxy.
    absolute_form: bool,
    /// The `Proxy-Authorization` each request on the connection carries.
    proxy_auth: Option<String>,
    /// Why the connection itself failed, once it has. A request on a
    /// connection that broke is only told that it broke; the reason (a
    /// TLS alert, say) is known to the connection.
    broke: Arc<Mutex<Option<Fail>>>,
}

impl Conn {
    /// The failure for a request on this connection: the connection's
    /// own TLS failure when it has one, else the request's.
    fn fail(&self, err: &hyper::Error, host: &str) -> Fail {
        let own = self.broke.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match own {
            Some(fail) if matches!(fail.kind, ErrorKind::Tls(_)) => fail,
            _ => hyper_fail(err, host),
        }
    }
}

struct Idle {
    conn: Conn,
    since: Instant,
}

#[derive(Clone, Copy, Default)]
struct DialTimes {
    dns: Duration,
    connect: Duration,
    tls: Duration,
}

impl Inner {
    /// Takes an idle connection for the key. An HTTP/2 connection stays
    /// in the pool: it carries any number of requests at once.
    fn checkout(&self, key: &PoolKey) -> Option<Conn> {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        let list = pool.get_mut(key)?;
        while let Some(idle) = list.pop() {
            if idle.since.elapsed() > IDLE_TIMEOUT || idle.conn.sender.is_closed() {
                continue;
            }
            if let Sender::H2(s) = &idle.conn.sender {
                let shared = Conn {
                    sender: Sender::H2(s.clone()),
                    absolute_form: idle.conn.absolute_form,
                    proxy_auth: idle.conn.proxy_auth.clone(),
                    broke: idle.conn.broke.clone(),
                };
                list.push(Idle {
                    conn: idle.conn,
                    since: Instant::now(),
                });
                return Some(shared);
            }
            return Some(idle.conn);
        }
        None
    }

    fn checkin(&self, key: &PoolKey, conn: Conn) {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        pool.entry(key.clone()).or_default().push(Idle {
            conn,
            since: Instant::now(),
        });
    }

    fn tls_config(&self, for_proxy: bool) -> Result<Arc<ClientConfig>, Fail> {
        let (cell, alpn): (_, &[&str]) = if for_proxy {
            (&self.tls_proxy, &["http/1.1"])
        } else {
            let alpn: &[&str] = match self.protocols {
                Protocols::Any => &["h2", "http/1.1"],
                Protocols::Http1 => &["http/1.1"],
                Protocols::Http2 => &["h2"],
            };
            (&self.tls_main, alpn)
        };
        cell.get_or_init(|| self.tls.client_config(alpn).map(Arc::new))
            .clone()
    }

    /// Opens a connection for the target, through the proxy when there
    /// is one, and starts HTTP on it.
    async fn dial(
        &self,
        target: &Target,
        proxy: Option<&Proxy>,
    ) -> Result<(Conn, DialTimes), Fail> {
        let mut times = DialTimes::default();
        let mut absolute_form = false;
        let mut proxy_auth = None;
        let mut io: BoxIo = match proxy {
            None => Box::new(tcp_connect(&target.host, target.port, &mut times).await?),
            Some(p) => {
                let proxy_addr = join_host_port(&p.host, p.port);
                let prefix = if p.is_socks() {
                    format!("socks connect tcp {proxy_addr}->{}: ", target.host_port())
                } else {
                    "proxyconnect tcp: ".to_string()
                };
                let prefixed = |mut f: Fail| {
                    f.message = format!("{prefix}{}", f.message);
                    f
                };
                let tcp = tcp_connect(&p.host, p.port, &mut times)
                    .await
                    .map_err(prefixed)?;
                let mut io: BoxIo = Box::new(tcp);
                if p.scheme == ProxyScheme::Https {
                    let config = self.tls_config(true)?;
                    io = tls_handshake(io, &p.host, config, &mut times)
                        .await
                        .map_err(prefixed)?
                        .0;
                }
                if p.is_socks() {
                    socks5_connect(&mut io, &target.host, target.port, p, &proxy_addr).await?;
                } else if target.is_https() {
                    http_connect(&mut io, &target.host_port(), p).await?;
                } else {
                    absolute_form = true;
                    proxy_auth = p.basic_auth();
                }
                io
            }
        };
        let mut h2 = false;
        if target.is_https() {
            let config = self.tls_config(false)?;
            let (stream, alpn) = tls_handshake(io, &target.host, config, &mut times).await?;
            io = stream;
            h2 = alpn.as_deref() == Some(b"h2");
            if self.protocols == Protocols::Http2 && !h2 {
                let got = String::from_utf8_lossy(alpn.as_deref().unwrap_or_default()).into_owned();
                return Err(Fail::other(format!(
                    "http2: unexpected ALPN protocol {got:?}; want \"h2\""
                )));
            }
        } else if self.protocols == Protocols::Http2 {
            return Err(Fail::other("http2: unsupported scheme"));
        }
        let broke = Arc::new(Mutex::new(None));
        let record = |slot: Arc<Mutex<Option<Fail>>>, host: String| {
            move |err: hyper::Error| {
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(hyper_fail(&err, &host));
            }
        };
        let sender = if h2 {
            let (sender, conn) = http2::Builder::new(TokioExecutor::new())
                .handshake::<_, ReqBody>(TokioIo::new(io))
                .await
                .map_err(|e| hyper_fail(&e, &target.host))?;
            let record = record(broke.clone(), target.host.clone());
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    record(e);
                }
            });
            Sender::H2(sender)
        } else {
            let (sender, conn) = http1::Builder::new()
                .title_case_headers(true)
                .handshake::<_, ReqBody>(TokioIo::new(io))
                .await
                .map_err(|e| hyper_fail(&e, &target.host))?;
            let record = record(broke.clone(), target.host.clone());
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    record(e);
                }
            });
            Sender::H1(sender)
        };
        Ok((
            Conn {
                sender,
                absolute_form,
                proxy_auth,
                broke,
            },
            times,
        ))
    }
}

/// Resolves the host and connects to the first address that answers.
async fn tcp_connect(host: &str, port: u16, times: &mut DialTimes) -> Result<TcpStream, Fail> {
    let addrs: Vec<std::net::SocketAddr> = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![std::net::SocketAddr::new(ip, port)],
        Err(_) => {
            let started = Instant::now();
            let found = tokio::net::lookup_host((host, port)).await;
            times.dns += started.elapsed();
            let addrs: Vec<_> = match found {
                Ok(addrs) => addrs.collect(),
                Err(e) => return Err(dns_fail(host, &io_message(&e))),
            };
            if addrs.is_empty() {
                return Err(dns_fail(host, "no such host"));
            }
            addrs
        }
    };
    let started = Instant::now();
    let mut first_error = None;
    for addr in addrs {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                times.connect = started.elapsed();
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => {
                first_error.get_or_insert_with(|| {
                    Fail::new(
                        ErrorKind::Connect,
                        format!("dial tcp {addr}: connect: {}", io_message(&e)),
                    )
                });
            }
        }
    }
    Err(first_error.unwrap_or_else(|| dns_fail(host, "no such host")))
}

fn dns_fail(host: &str, why: &str) -> Fail {
    let lower = why.to_lowercase();
    let why = if lower.contains("name or service not known")
        || lower.contains("nodename nor servname")
        || lower.contains("no address associated")
        || lower.contains("no such host")
    {
        "no such host"
    } else {
        // The resolver's own text follows a fixed prefix.
        why.rsplit(": ").next().unwrap_or(why)
    };
    Fail::new(ErrorKind::Dns, format!("dial tcp: lookup {host}: {why}"))
}

/// Runs a TLS handshake over `io` for `host` and reports the protocol
/// ALPN chose.
async fn tls_handshake(
    io: BoxIo,
    host: &str,
    config: Arc<ClientConfig>,
    times: &mut DialTimes,
) -> Result<(BoxIo, Option<Vec<u8>>), Fail> {
    let name = ServerName::try_from(host.to_string()).map_err(|_| {
        Fail::new(
            ErrorKind::Tls(TlsFailure::Other),
            format!("tls: invalid server name {host:?}"),
        )
    })?;
    let started = Instant::now();
    let stream = tokio_rustls::TlsConnector::from(config)
        .connect(name, io)
        .await
        .map_err(|e| io_fail(&e, host))?;
    times.tls += started.elapsed();
    let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
    Ok((Box::new(AlertFirst::new(stream)), alpn))
}

/// A TLS stream whose failed writes look for the reason first. A server
/// that refuses the client's certificate under TLS 1.3 says so only after
/// the handshake has finished here, and then closes; a write that follows
/// fails with a reset connection while the alert that explains it is
/// still on its way in. Waiting for it turns `connection reset by peer`
/// into `certificate required`.
struct AlertFirst<S> {
    inner: S,
    /// A write that failed, held back until the read side has spoken.
    broken: Option<io::Error>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> AlertFirst<S> {
    fn new(inner: S) -> AlertFirst<S> {
        AlertFirst {
            inner,
            broken: None,
        }
    }

    /// Settles a failed write: with the TLS failure the peer sent, if the
    /// read side has one, else with the write's own error.
    fn explain<T>(&mut self, cx: &mut Context<'_>, err: io::Error) -> Poll<io::Result<T>> {
        if !matches!(
            err.kind(),
            io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
        ) {
            return Poll::Ready(Err(err));
        }
        let mut scratch = [0u8; 512];
        let mut buf = ReadBuf::new(&mut scratch);
        match Pin::new(&mut self.inner).poll_read(cx, &mut buf) {
            Poll::Ready(Err(reason)) if tls_error_in(&reason).is_some() => Poll::Ready(Err(reason)),
            Poll::Ready(_) => Poll::Ready(Err(err)),
            Poll::Pending => {
                // The read wakes this task when the socket has something
                // to say, and a reset socket always does.
                self.broken = Some(err);
                Poll::Pending
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for AlertFirst<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for AlertFirst<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Some(err) = self.broken.take() {
            return self.explain(cx, err);
        }
        match Pin::new(&mut self.inner).poll_write(cx, data) {
            Poll::Ready(Err(e)) => self.explain(cx, e),
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(err) = self.broken.take() {
            return self.explain(cx, err);
        }
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(Err(e)) => self.explain(cx, e),
            other => other,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The failure for an error from the HTTP connection.
fn hyper_fail(err: &hyper::Error, host: &str) -> Fail {
    let mut source = err.source();
    while let Some(s) = source {
        if let Some(io) = s.downcast_ref::<io::Error>() {
            return io_fail(io, host);
        }
        if let Some(tls) = s.downcast_ref::<rustls::Error>() {
            return tls_fail(tls, host);
        }
        source = s.source();
    }
    if err.is_incomplete_message() || err.is_canceled() || err.is_closed() {
        return Fail::other("EOF");
    }
    if err.is_parse() {
        return Fail::other(format!("malformed HTTP response: {err}"));
    }
    // An HTTP/2 error keeps its I/O cause as text only.
    let text = err.to_string();
    let detail = match err.source() {
        Some(s) => format!("{text}: {s}"),
        None => text,
    };
    if let Some(fail) = crate::tls::alert_in_text(&detail) {
        return fail;
    }
    Fail::other(detail)
}

/// Reports whether a request that failed on a reused connection may go
/// again on a new one: the connection had gone away before the request
/// was taken, or the request is one that is safe to repeat.
fn may_retry(err: &hyper::Error, method: &str) -> bool {
    if err.is_canceled() {
        return true;
    }
    let idempotent = matches!(method, "GET" | "HEAD" | "OPTIONS" | "TRACE");
    idempotent && (err.is_closed() || err.is_incomplete_message() || has_io_source(err))
}

fn has_io_source(err: &hyper::Error) -> bool {
    let mut source = err.source();
    while let Some(s) = source {
        if let Some(io) = s.downcast_ref::<io::Error>() {
            return tls_error_in(io).is_none();
        }
        source = s.source();
    }
    false
}

/// Runs one request under its deadline and its cancellation token.
async fn run(inner: Arc<Inner>, mut req: Request) -> Result<Response, Error> {
    let started = Instant::now();
    let deadline = [req.deadline, req.timeout.map(|t| started + t)]
        .into_iter()
        .flatten()
        .min();
    let body = Bytes::from(std::mem::take(&mut req.body));
    let in_body = AtomicBool::new(false);
    let cancel = async {
        match &req.cancel {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let timer = async {
        match deadline {
            Some(at) => tokio::time::sleep_until(at.into()).await,
            None => std::future::pending().await,
        }
    };
    // The timer rounds up to its own tick, so a deadline already past is
    // checked by hand: such a request must not get as far as the wire.
    let expired = deadline.is_some_and(|at| at <= Instant::now());
    let outcome = tokio::select! {
        biased;
        () = cancel => {
            let cause = req.cancel.as_ref().and_then(CancelToken::cause);
            Err(Fail::new(
                ErrorKind::Cancelled,
                cause.as_deref().unwrap_or("context canceled"),
            ))
        }
        () = timer => Err(Fail::new(ErrorKind::Timeout, "context deadline exceeded")),
        () = std::future::ready(()), if expired => {
            Err(Fail::new(ErrorKind::Timeout, "context deadline exceeded"))
        }
        r = round_trip(&inner, &req, body, started, &in_body) => r,
    };
    outcome.map_err(|f| {
        let mut e = Error::new(f.kind, &req.method, &req.url, f.message);
        e.reading_body = in_body.load(Ordering::Relaxed);
        e
    })
}

async fn round_trip(
    inner: &Inner,
    req: &Request,
    body: Bytes,
    started: Instant,
    in_body: &AtomicBool,
) -> Result<Response, Fail> {
    check_method(&req.method).map_err(Fail::other)?;
    let target = parse_target(&req.url)?;
    let proxy = req.proxy.as_ref();
    let key = PoolKey::new(&target, proxy);
    let mut retried = false;
    let (mut conn, reused, times, head, asked_gzip) = loop {
        let (mut conn, reused, times) = match inner.checkout(&key) {
            Some(conn) => (conn, true, DialTimes::default()),
            None => {
                let (conn, times) = inner.dial(&target, proxy).await?;
                if let Sender::H2(s) = &conn.sender {
                    // Shared from the start: the next request rides it
                    // even while this one is in flight.
                    inner.checkin(
                        &key,
                        Conn {
                            sender: Sender::H2(s.clone()),
                            absolute_form: conn.absolute_form,
                            proxy_auth: conn.proxy_auth.clone(),
                            broke: conn.broke.clone(),
                        },
                    );
                }
                (conn, false, times)
            }
        };
        let (http_req, asked_gzip) = build_request(req, &target, &conn, body.clone())?;
        match conn.sender.send(http_req).await {
            Ok(head) => break (conn, reused, times, head, asked_gzip),
            Err(e) if reused && !retried && may_retry(&e, &req.method) => retried = true,
            Err(e) => return Err(conn.fail(&e, &target.host)),
        }
    };
    let first_byte = Instant::now();
    in_body.store(true, Ordering::Relaxed);

    let (parts, mut incoming) = head.into_parts();
    let is_h2 = parts.version == hyper::Version::HTTP_2;
    let proto = match parts.version {
        hyper::Version::HTTP_10 => "HTTP/1.0",
        hyper::Version::HTTP_2 => "HTTP/2.0",
        _ => "HTTP/1.1",
    };
    let code = parts.status.as_u16();
    let reason = if is_h2 {
        status_text(code).to_string()
    } else {
        match parts.extensions.get::<hyper::ext::ReasonPhrase>() {
            Some(r) => String::from_utf8_lossy(r.as_bytes()).into_owned(),
            None => parts.status.canonical_reason().unwrap_or("").to_string(),
        }
    };
    let mut headers = Headers::new();
    for (name, value) in &parts.headers {
        headers.add(name.as_str(), &String::from_utf8_lossy(value.as_bytes()));
    }
    let gzip = asked_gzip
        && headers
            .get("Content-Encoding")
            .is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    if gzip {
        headers.del("Content-Encoding");
        headers.del("Content-Length");
    }

    let mut sink = if gzip {
        Sink::Gzip(Box::new(flate2::write::MultiGzDecoder::new(Vec::new())))
    } else {
        Sink::Plain(Vec::new())
    };
    let mut truncated = false;
    let mut received = false;
    let complete = loop {
        match incoming.frame().await {
            None => break true,
            Some(Err(e)) => {
                let fail = conn.fail(&e, &target.host);
                return Err(if fail.message == "EOF" {
                    Fail::other("unexpected EOF")
                } else {
                    fail
                });
            }
            Some(Ok(frame)) => {
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                if data.is_empty() {
                    continue;
                }
                received = true;
                sink.write(&data)?;
                if req.max_body.is_some_and(|max| sink.len() as u64 > max) {
                    truncated = true;
                    break false;
                }
            }
        }
    };
    let mut data = sink.finish(received && !truncated)?;
    if let (true, Some(max)) = (truncated, req.max_body) {
        data.truncate(usize::try_from(max).unwrap_or(usize::MAX));
    }
    let ended = Instant::now();

    if complete {
        if let Sender::H1(mut sender) = conn.sender {
            // The connection says when it is ready for another request;
            // one that the server is closing never does.
            let ready = tokio::time::timeout(Duration::from_secs(1), sender.ready()).await;
            if matches!(ready, Ok(Ok(()))) {
                conn.sender = Sender::H1(sender);
                inner.checkin(&key, conn);
            }
        }
    }

    Ok(Response {
        status_code: code,
        status: format!("{code} {reason}"),
        proto: proto.to_string(),
        headers,
        body: data,
        truncated,
        uncompressed: gzip,
        timings: Timings {
            dns: times.dns,
            connect: times.connect,
            tls: times.tls,
            ttfb: first_byte - started,
            total: ended - started,
            reused,
            started,
        },
        method: req.method.clone(),
        url: req.url.clone(),
    })
}

/// Where the body goes as it arrives: as it is, or through gzip.
enum Sink {
    Plain(Vec<u8>),
    Gzip(Box<flate2::write::MultiGzDecoder<Vec<u8>>>),
}

impl Sink {
    fn write(&mut self, data: &[u8]) -> Result<(), Fail> {
        match self {
            Sink::Plain(out) => out.extend_from_slice(data),
            Sink::Gzip(decoder) => {
                // Flushed, so that what has been decoded can be counted
                // against the cap.
                decoder
                    .write_all(data)
                    .and_then(|()| decoder.flush())
                    .map_err(|e| gzip_fail(&e))?;
            }
        }
        Ok(())
    }

    fn len(&self) -> usize {
        match self {
            Sink::Plain(out) => out.len(),
            Sink::Gzip(decoder) => decoder.get_ref().len(),
        }
    }

    /// Returns what was written. `check` asks a gzip stream to have
    /// ended properly; an empty or cut-short one is taken as it is.
    fn finish(self, check: bool) -> Result<Vec<u8>, Fail> {
        match self {
            Sink::Plain(out) => Ok(out),
            Sink::Gzip(mut decoder) => {
                if check {
                    decoder.try_finish().map_err(|e| gzip_fail(&e))?;
                }
                Ok(std::mem::take(decoder.get_mut()))
            }
        }
    }
}

fn gzip_fail(err: &io::Error) -> Fail {
    if err.kind() == io::ErrorKind::UnexpectedEof {
        return Fail::other("unexpected EOF");
    }
    Fail::other(format!("gzip: {}", io_message(err)))
}

/// Builds the request as it goes on the wire and reports whether the
/// client asked for gzip itself.
fn build_request(
    req: &Request,
    target: &Target,
    conn: &Conn,
    body: Bytes,
) -> Result<(hyper::Request<ReqBody>, bool), Fail> {
    let is_h2 = matches!(conn.sender, Sender::H2(_));
    let host_override = req
        .host
        .clone()
        .filter(|h| !h.is_empty())
        .or_else(|| req.headers.get("Host").map(str::to_string));
    let authority = host_override.as_deref().unwrap_or(&target.authority);
    let uri = if is_h2 {
        format!("{}://{}{}", target.scheme, authority, target.path_and_query)
    } else if conn.absolute_form {
        format!("http://{}{}", target.authority, target.path_and_query)
    } else {
        target.path_and_query.clone()
    };

    // The order on the wire: Host, User-Agent, the length, the caller's
    // headers sorted by name, then Accept-Encoding.
    let mut wire: Vec<(String, String)> = Vec::new();
    if !is_h2 {
        wire.push(("Host".to_string(), authority.to_string()));
    }
    match req.headers.get("User-Agent") {
        Some("") => {}
        Some(agent) => wire.push(("User-Agent".to_string(), agent.to_string())),
        None => {
            let agent = if is_h2 {
                "Go-http-client/2.0"
            } else {
                "Go-http-client/1.1"
            };
            wire.push(("User-Agent".to_string(), agent.to_string()));
        }
    }
    if !body.is_empty() || matches!(req.method.as_str(), "POST" | "PUT" | "PATCH") {
        wire.push(("Content-Length".to_string(), body.len().to_string()));
    }
    let mut rest: Vec<(String, String)> = req
        .headers
        .iter()
        .filter(|(name, _)| {
            !matches!(
                *name,
                "Host" | "User-Agent" | "Content-Length" | "Transfer-Encoding" | "Trailer"
            )
        })
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if let Some((user, pass)) = &target.userinfo {
        if !req.headers.contains("Authorization") {
            let pair = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
            rest.push(("Authorization".to_string(), format!("Basic {pair}")));
        }
    }
    if let Some(auth) = &conn.proxy_auth {
        if !req.headers.contains("Proxy-Authorization") {
            rest.push(("Proxy-Authorization".to_string(), auth.clone()));
        }
    }
    rest.sort_by(|a, b| a.0.cmp(&b.0));
    wire.extend(rest);
    let asks_gzip = req.headers.get("Accept-Encoding").unwrap_or("").is_empty()
        && req.headers.get("Range").unwrap_or("").is_empty()
        && req.method != "HEAD";
    if asks_gzip {
        wire.retain(|(name, _)| name != "Accept-Encoding");
        wire.push(("Accept-Encoding".to_string(), "gzip".to_string()));
    }

    let method = hyper::Method::from_bytes(req.method.as_bytes())
        .map_err(|_| Fail::other(format!("net/http: invalid method {:?}", req.method)))?;
    let uri: hyper::Uri = uri
        .parse()
        .map_err(|e| Fail::other(format!("parse {:?}: {e}", req.url)))?;
    let mut out = hyper::Request::new(Full::new(body));
    *out.method_mut() = method;
    *out.uri_mut() = uri;
    for (name, value) in wire {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| Fail::other(format!("net/http: invalid header field name {name:?}")))?;
        let value = HeaderValue::from_bytes(value.as_bytes()).map_err(|_| {
            Fail::other(format!("net/http: invalid header field value for {name:?}"))
        })?;
        out.headers_mut().append(header, value);
    }
    Ok((out, asks_gzip))
}
