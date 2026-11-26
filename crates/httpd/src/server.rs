//! The server: a listener, a thread that runs the connections, and a
//! handler called for each request.

use std::convert::Infallible;
use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use apic_httpc::{status_text, CancelToken};
use http_body_util::BodyExt;
use hyper::body::{Bytes, Frame, Incoming, SizeHint};
use hyper::header::{HeaderName, HeaderValue};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::request::{path_unescape, Request, TlsState};
use crate::response::Response;
use crate::sniff::detect_content_type;

/// How long a shutdown waits for requests in flight before giving up.
const GRACE: Duration = Duration::from_secs(5);

/// A listener could not be bound. It displays as the message apic has
/// always printed: `listen tcp 127.0.0.1:8080: bind: address already in
/// use`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindError {
    /// The address asked for.
    pub addr: String,
    /// Why, in lower case and without an OS error number.
    pub reason: String,
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "listen tcp {}: {}", self.addr, self.reason)
    }
}

impl std::error::Error for BindError {}

/// A bound TCP listener that is not yet serving. Binding first lets a
/// command fail on a port in use before it prints or writes anything.
#[derive(Debug)]
pub struct Listener {
    inner: std::net::TcpListener,
    addr: SocketAddr,
}

impl Listener {
    /// Binds `addr` (`127.0.0.1:8080`; port 0 picks a free port).
    pub fn bind(addr: &str) -> Result<Listener, BindError> {
        let fail = |reason: String| BindError {
            addr: addr.to_string(),
            reason,
        };
        let parsed: SocketAddr = match addr.parse() {
            Ok(a) => a,
            Err(_) => {
                use std::net::ToSocketAddrs;
                match addr.to_socket_addrs().ok().and_then(|mut it| it.next()) {
                    Some(a) => a,
                    None => {
                        let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
                        return Err(fail(format!("lookup {host}: no such host")));
                    }
                }
            }
        };
        let inner = std::net::TcpListener::bind(parsed).map_err(|e| {
            let text = e.to_string();
            let text = text
                .split(" (os error")
                .next()
                .unwrap_or(&text)
                .to_lowercase();
            fail(format!("bind: {text}"))
        })?;
        let addr = inner.local_addr().map_err(|e| fail(e.to_string()))?;
        Ok(Listener { inner, addr })
    }

    /// The address bound, with the port the system picked for port 0.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// TLS for a server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsOptions {
    /// The server's certificate chain in PEM, leaf first.
    pub cert_pem: Vec<u8>,
    /// The certificate's private key in PEM.
    pub key_pem: Vec<u8>,
    /// When set, every client must present a certificate signed by one of
    /// these PEM certificates; the handler finds it in `Request::tls`.
    pub client_ca_pem: Option<Vec<u8>>,
    /// Offer HTTP/2 (`h2`, then `http/1.1`) rather than HTTP/1.1 alone.
    pub http2: bool,
}

/// Settings for a `Server`.
#[derive(Clone, Debug, Default)]
pub struct ServerBuilder {
    tls: Option<TlsOptions>,
    stop: Option<CancelToken>,
}

impl ServerBuilder {
    /// Serves over TLS.
    pub fn tls(mut self, tls: TlsOptions) -> ServerBuilder {
        self.tls = Some(tls);
        self
    }

    /// Stops the server when this token is cancelled, as well as on
    /// `Server::shutdown`: the token a command cancels on Ctrl-C.
    pub fn stop_on(mut self, token: &CancelToken) -> ServerBuilder {
        self.stop = Some(token.clone());
        self
    }

    /// Starts serving on a listener. The error is a TLS certificate or
    /// key that cannot be used.
    pub fn start<H>(self, listener: Listener, handler: H) -> Result<Server, String>
    where
        H: Fn(&Request) -> Response + Send + Sync + 'static,
    {
        let tls = match &self.tls {
            Some(options) => Some(Arc::new(server_config(options)?)),
            None => None,
        };
        let stop = self.stop.unwrap_or_default();
        let addr = listener.addr;
        let http2 = self.tls.as_ref().is_some_and(|t| t.http2);
        let shared = Arc::new(Shared {
            handler: Box::new(handler),
            tls,
            http2,
            stop: stop.clone(),
        });
        let thread = std::thread::Builder::new()
            .name("apic-httpd".to_string())
            .spawn(move || run(listener.inner, shared))
            .map_err(|e| format!("cannot start the server thread: {e}"))?;
        Ok(Server {
            addr,
            tls: self.tls.is_some(),
            stop,
            thread: Mutex::new(Some(thread)),
        })
    }
}

/// A running HTTP server. It serves on a background thread of its own
/// from the moment it is started until `shutdown` (or until it is
/// dropped); each request's handler runs on a worker thread, so a slow
/// handler does not hold up other requests.
#[derive(Debug)]
pub struct Server {
    addr: SocketAddr,
    tls: bool,
    stop: CancelToken,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Server {
    /// Starts building a server with TLS or a stop token.
    pub fn builder() -> ServerBuilder {
        ServerBuilder::default()
    }

    /// Binds `addr` and serves `handler` over plain HTTP.
    pub fn bind<H>(addr: &str, handler: H) -> Result<Server, BindError>
    where
        H: Fn(&Request) -> Response + Send + Sync + 'static,
    {
        Ok(Server::start(Listener::bind(addr)?, handler))
    }

    /// Serves `handler` over plain HTTP on a listener already bound.
    pub fn start<H>(listener: Listener, handler: H) -> Server
    where
        H: Fn(&Request) -> Response + Send + Sync + 'static,
    {
        let addr = listener.addr.to_string();
        match ServerBuilder::default().start(listener, handler) {
            Ok(server) => server,
            Err(e) => panic!("cannot serve on {addr}: {e}"),
        }
    }

    /// The address the server listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The server's base URL, without a trailing slash:
    /// `http://127.0.0.1:54321`.
    pub fn url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}", self.addr)
    }

    /// The token that stops the server when cancelled. Clones can be
    /// handed to a signal handler or another thread.
    pub fn stop_token(&self) -> CancelToken {
        self.stop.clone()
    }

    /// Blocks until the server has stopped: until its stop token is
    /// cancelled and the requests in flight have finished (or five
    /// seconds have passed).
    pub fn wait(&self) {
        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        match thread {
            Some(t) => {
                let _ = t.join();
            }
            None => self.stop.wait(),
        }
    }

    /// Stops the server gracefully: no new connections, idle ones closed,
    /// requests in flight given up to five seconds to finish. Returns
    /// once it has stopped.
    pub fn shutdown(&self) {
        self.stop.cancel();
        self.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Serves `handler` on `listener` over plain HTTP and blocks until
/// `stop` is cancelled and the server has shut down: the whole life of a
/// long-running command such as `apic demo`.
pub fn serve<H>(listener: Listener, handler: H, stop: &CancelToken)
where
    H: Fn(&Request) -> Response + Send + Sync + 'static,
{
    let shared = Arc::new(Shared {
        handler: Box::new(handler),
        tls: None,
        http2: false,
        stop: stop.clone(),
    });
    run(listener.inner, shared);
}

fn server_config(options: &TlsOptions) -> Result<rustls::ServerConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let chain: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &options.cert_pem[..])
        .flatten()
        .collect();
    if chain.is_empty() {
        return Err("tls: failed to find any PEM data in certificate input".to_string());
    }
    let key = rustls_pemfile::private_key(&mut &options.key_pem[..])
        .ok()
        .flatten()
        .ok_or("tls: failed to find any PEM data in key input")?;
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls: {e}"))?;
    let builder = match &options.client_ca_pem {
        Some(pem) => {
            let mut roots = rustls::RootCertStore::empty();
            for cert in rustls_pemfile::certs(&mut &pem[..]).flatten() {
                roots
                    .add(cert)
                    .map_err(|e| format!("tls: client CA: {e}"))?;
            }
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots),
                provider,
            )
            .build()
            .map_err(|e| format!("tls: client CA: {e}"))?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let mut config = builder
        .with_single_cert(chain, key)
        .map_err(|e| format!("tls: {e}"))?;
    config.alpn_protocols = if options.http2 {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    Ok(config)
}

/// What every connection of a server shares.
struct Shared {
    handler: crate::mux::Handler,
    tls: Option<Arc<rustls::ServerConfig>>,
    http2: bool,
    stop: CancelToken,
}

/// What the requests of one connection share.
struct ConnInfo {
    shared: Arc<Shared>,
    remote_addr: String,
    tls: Option<TlsState>,
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Io for T {}

/// Runs the server on the calling thread until its stop token is
/// cancelled.
fn run(listener: std::net::TcpListener, shared: Arc<Shared>) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(async {
        if listener.set_nonblocking(true).is_err() {
            return;
        }
        let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
            return;
        };
        let mut conns = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                () = shared.stop.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let _ = stream.set_nodelay(true);
                        conns.spawn(serve_connection(stream, peer, shared.clone()));
                    }
                    // Out of descriptors, most likely: let some close.
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                },
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
        drop(listener);
        let _ =
            tokio::time::timeout(GRACE, async { while conns.join_next().await.is_some() {} }).await;
    });
    // A handler still running is left to finish on its own.
    rt.shutdown_background();
}

async fn serve_connection(stream: tokio::net::TcpStream, peer: SocketAddr, shared: Arc<Shared>) {
    let (io, tls): (Box<dyn Io>, Option<TlsState>) = match &shared.tls {
        None => (Box::new(stream), None),
        Some(config) => {
            let accept = tokio_rustls::TlsAcceptor::from(config.clone()).accept(stream);
            let stream = tokio::select! {
                () = shared.stop.cancelled() => return,
                accepted = accept => match accepted {
                    Ok(s) => s,
                    Err(_) => return,
                },
            };
            let session = stream.get_ref().1;
            let state = TlsState {
                peer_certificates: session
                    .peer_certificates()
                    .unwrap_or_default()
                    .iter()
                    .map(|c| c.to_vec())
                    .collect(),
                alpn: String::from_utf8_lossy(session.alpn_protocol().unwrap_or_default())
                    .into_owned(),
            };
            (Box::new(stream), Some(state))
        }
    };
    let info = Arc::new(ConnInfo {
        shared: shared.clone(),
        remote_addr: peer.to_string(),
        tls,
    });
    let service = service_fn(move |req| handle(req, info.clone()));
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .title_case_headers(true)
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30));
    let builder = if shared.http2 {
        builder
    } else {
        builder.http1_only()
    };
    let conn = builder.serve_connection(TokioIo::new(io), service);
    tokio::pin!(conn);
    tokio::select! {
        _ = conn.as_mut() => return,
        () = shared.stop.cancelled() => {}
    }
    conn.as_mut().graceful_shutdown();
    let _ = conn.await;
}

/// The reason a connection is dropped without an answer.
#[derive(Debug)]
struct HangUp;

impl fmt::Display for HangUp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("connection closed without a response")
    }
}

impl std::error::Error for HangUp {}

async fn handle(
    req: hyper::Request<Incoming>,
    info: Arc<ConnInfo>,
) -> Result<hyper::Response<OutBody>, HangUp> {
    let (parts, body) = req.into_parts();
    let body = body.collect().await.map_err(|_| HangUp)?.to_bytes();
    let is_h2 = parts.version == hyper::Version::HTTP_2;
    let mut headers = apic_httpc::Headers::new();
    let mut host = parts
        .uri
        .authority()
        .map(|a| a.as_str().to_string())
        .unwrap_or_default();
    for (name, value) in &parts.headers {
        let value = String::from_utf8_lossy(value.as_bytes());
        if name == hyper::header::HOST {
            if host.is_empty() {
                host = value.into_owned();
            }
            continue;
        }
        headers.add(name.as_str(), &value);
    }
    let raw_path = parts.uri.path().to_string();
    let target = if parts.uri.scheme().is_some() && !is_h2 {
        parts.uri.to_string()
    } else {
        parts
            .uri
            .path_and_query()
            .map_or_else(|| raw_path.clone(), |pq| pq.as_str().to_string())
    };
    let request = Request {
        method: parts.method.as_str().to_string(),
        path: path_unescape(&raw_path),
        raw_path,
        raw_query: parts.uri.query().unwrap_or("").to_string(),
        target,
        host,
        headers,
        body: Arc::from(&body[..]),
        remote_addr: info.remote_addr.clone(),
        proto: match parts.version {
            hyper::Version::HTTP_10 => "HTTP/1.0",
            hyper::Version::HTTP_2 => "HTTP/2.0",
            _ => "HTTP/1.1",
        }
        .to_string(),
        tls: info.tls.clone(),
        pattern: String::new(),
        path_values: Vec::new(),
    };
    let is_head = request.method == "HEAD";
    let shared = info.shared.clone();
    // A handler may block (it may sleep, or call out), so it runs off the
    // connection thread. One that panics drops the connection.
    let response = tokio::task::spawn_blocking(move || (shared.handler)(&request))
        .await
        .map_err(|_| HangUp)?;
    if response.hang_up {
        return Err(HangUp);
    }
    Ok(into_hyper(response, is_head, is_h2))
}

/// Fills in what the handler left out and hands the response to hyper.
fn into_hyper(mut response: Response, is_head: bool, is_h2: bool) -> hyper::Response<OutBody> {
    let status = hyper::StatusCode::from_u16(response.status)
        .unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR);
    let code = status.as_u16();
    let body_allowed = !(code < 200 || code == 204 || code == 304);
    let streaming = response.stream.is_some();
    let mut sized = false;
    if !body_allowed {
        response.body.clear();
        response.stream = None;
        response.headers.del("Content-Length");
    } else if !streaming {
        let body = &response.body;
        if !body.is_empty()
            && !response.headers.contains("Content-Type")
            && response
                .headers
                .get("Content-Encoding")
                .unwrap_or("")
                .is_empty()
        {
            response
                .headers
                .set("Content-Type", detect_content_type(body));
        }
        let buffered = if is_h2 { 4096 } else { 2048 };
        if response.headers.contains("Content-Length") {
            sized = true;
        } else if body.len() <= buffered {
            // The whole body fitted the write buffer, so its length is
            // known before the headers go out.
            response
                .headers
                .set("Content-Length", &body.len().to_string());
            sized = true;
        }
    }
    let body = match response.stream.take() {
        Some(rx) if !is_head => OutBody::Stream(rx),
        _ if is_head || !body_allowed => OutBody::Full {
            data: None,
            sized: true,
            len: 0,
        },
        _ => {
            let data = Bytes::from(std::mem::take(&mut response.body));
            OutBody::Full {
                len: data.len() as u64,
                data: Some(data),
                sized,
            }
        }
    };
    let mut out = hyper::Response::new(body);
    *out.status_mut() = status;
    for (name, value) in response.headers.iter() {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out.headers_mut().append(name, value);
        }
    }
    if !is_h2 {
        let text = status_text(code);
        let reason = if text.is_empty() {
            format!("status code {code}")
        } else {
            text.to_string()
        };
        if status.canonical_reason() != Some(reason.as_str()) {
            if let Ok(phrase) = hyper::ext::ReasonPhrase::try_from(reason.into_bytes()) {
                out.extensions_mut().insert(phrase);
            }
        }
    }
    out
}

/// A response body: the bytes in one piece, or a stream fed from a
/// channel.
enum OutBody {
    Full {
        data: Option<Bytes>,
        /// Whether the length is declared. A body without one is sent
        /// chunked over HTTP/1.1.
        sized: bool,
        len: u64,
    },
    Stream(mpsc::UnboundedReceiver<Vec<u8>>),
}

impl hyper::body::Body for OutBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match self.get_mut() {
            OutBody::Full { data, .. } => Poll::Ready(
                data.take()
                    .filter(|d| !d.is_empty())
                    .map(|d| Ok(Frame::data(d))),
            ),
            OutBody::Stream(rx) => rx
                .poll_recv(cx)
                .map(|next| next.map(|chunk| Ok(Frame::data(Bytes::from(chunk))))),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            OutBody::Full { data, .. } => data.as_ref().is_none_or(Bytes::is_empty),
            OutBody::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            OutBody::Full {
                sized: true, len, ..
            } => SizeHint::with_exact(*len),
            _ => SizeHint::default(),
        }
    }
}
