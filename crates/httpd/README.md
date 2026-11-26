# apic-httpd

The small HTTP server behind `apic demo`, `apic mock` and the tests
(`apic_httpd`). Handlers are synchronous: `Fn(&Request) -> Response +
Send + Sync + 'static`. The server runs on a thread of its own and calls
each handler on a worker thread, so a handler may block.

```rust
let mut mux = Mux::new();
mux.handle_fn("GET /todos/{id}", |r| Response::json(200, format!("{{\"id\":\"{}\"}}", r.path_value("id"))))?;
let srv = TestServer::new(mux.into_handler());
let resp = apic_httpc::Client::new().send(&apic_httpc::Request::get(&format!("{}/todos/7", srv.url())))?;
```

## API

```rust
pub use apic_httpc::{canonical_header_key, status_text, CancelToken, Headers};

// --- handlers and routing -------------------------------------------
pub type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync + 'static>;
pub fn handler(f: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Handler;

pub struct Mux<H = Handler>;             // Default
impl<H> Mux<H> {
    pub fn new() -> Mux<H>;
    pub fn handle(&mut self, pattern: &str, handler: H) -> Result<(), String>;
    pub fn patterns(&self) -> Vec<&str>;
    pub fn lookup(&self, req: &Request) -> Lookup<'_, H>;   // what would happen, without doing it
}
impl Mux<Handler> {
    pub fn handle_fn(&mut self, pattern: &str,
        f: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Result<(), String>;
    pub fn serve(&self, req: &Request) -> Response;         // route and call, or 301/404/405
    pub fn into_handler(self) -> Handler;
}
pub enum Lookup<'a, H> {
    Found { handler: &'a H, pattern: &'a str, path_values: Vec<(String, String)> },
    Redirect { location: String },                          // 301
    MethodNotAllowed { allow: Vec<String> },                // 405, sorted
    NotFound,                                               // 404
}

// --- request --------------------------------------------------------
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,                    // decoded: "/todos/a b"
    pub raw_path: String,                // as sent: "/todos/a%20b"
    pub raw_query: String,               // without '?'
    pub target: String,                  // "/a?b", or the absolute URL sent to a proxy
    pub host: String,                    // Host header / :authority
    pub headers: Headers,                // canonical names, without Host
    pub body: Arc<[u8]>,
    pub remote_addr: String,             // "127.0.0.1:54321"
    pub proto: String,                   // "HTTP/1.1", "HTTP/2.0"
    pub tls: Option<TlsState>,
    pub pattern: String,                 // the mux pattern that matched
}
impl Request {
    pub fn new(method: &str, target: &str) -> Request;      // by hand, for calling a handler directly
    pub fn with_header(self, name: &str, value: &str) -> Request;
    pub fn with_body(self, body: impl AsRef<[u8]>) -> Request;
    pub fn with_host(self, host: &str) -> Request;
    pub fn header(&self, name: &str) -> Option<&str>;
    pub fn text(&self) -> String;                           // body, lossy UTF-8
    pub fn url(&self) -> &str;                              // = target
    pub fn path_value(&self, name: &str) -> &str;           // "" when absent
    pub fn set_path_value(&mut self, name: &str, value: &str);
    pub fn query(&self) -> Vec<(String, String)>;
    pub fn query_get(&self, name: &str) -> Option<String>;
    pub fn query_values(&self, name: &str) -> Vec<String>;
    pub fn form(&self) -> Vec<(String, String)>;            // urlencoded body, then query
    pub fn form_value(&self, name: &str) -> String;         // "" when absent; multipart fields too
    pub fn basic_auth(&self) -> Option<(String, String)>;
    pub fn cookie(&self, name: &str) -> Option<String>;
    pub fn multipart(&self) -> Result<Vec<Part>, String>;   // multipart/form-data
}
pub struct TlsState { pub peer_certificates: Vec<Vec<u8>> /* DER */, pub alpn: String }
impl TlsState { pub fn peer_common_name(&self) -> Option<String>; }
pub struct Part { pub name: String, pub filename: Option<String>, pub headers: Headers, pub data: Vec<u8> }

pub fn parse_query(raw: &str) -> Vec<(String, String)>;
pub fn path_unescape(s: &str) -> String;
pub fn certificate_common_name(der: &[u8]) -> Option<String>;
pub fn detect_content_type(data: &[u8]) -> &'static str;

// --- response -------------------------------------------------------
#[derive(Debug)]
pub struct Response { pub status: u16, pub headers: Headers, pub body: Vec<u8> }  // Default: 200
impl Response {
    pub fn new(status: u16) -> Response;
    pub fn with_header(self, name: &str, value: &str) -> Response;   // set
    pub fn add_header(self, name: &str, value: &str) -> Response;    // add (several Set-Cookie)
    pub fn with_body(self, body: impl Into<Vec<u8>>) -> Response;
    pub fn text(status: u16, body: impl Into<Vec<u8>>) -> Response;  // text/plain; charset=utf-8
    pub fn json(status: u16, body: impl Into<Vec<u8>>) -> Response;  // application/json
    pub fn html(status: u16, body: impl Into<Vec<u8>>) -> Response;  // text/html; charset=utf-8
    pub fn error(status: u16, message: &str) -> Response;            // message + "\n", nosniff
    pub fn not_found() -> Response;                                  // "404 page not found\n"
    pub fn redirect(req: &Request, location: &str, status: u16) -> Response;
    pub fn hang_up() -> Response;                                    // close without answering
    pub fn streaming(status: u16) -> (Response, BodySender);         // SSE and the like
}
#[derive(Clone, Debug)]
pub struct BodySender;
impl BodySender {
    pub fn send(&self, data: impl Into<Vec<u8>>) -> bool;            // false: client gone
    pub fn is_closed(&self) -> bool;
}

// --- server ---------------------------------------------------------
pub struct Listener;                                                 // bound, not yet serving
impl Listener {
    pub fn bind(addr: &str) -> Result<Listener, BindError>;          // "127.0.0.1:0" picks a port
    pub fn addr(&self) -> SocketAddr;
}
pub struct BindError { pub addr: String, pub reason: String }        // "listen tcp ADDR: bind: address already in use"

pub struct Server;                                                   // stops on drop
impl Server {
    pub fn bind<H>(addr: &str, handler: H) -> Result<Server, BindError>;
    pub fn start<H>(listener: Listener, handler: H) -> Server;
    pub fn builder() -> ServerBuilder;
    pub fn addr(&self) -> SocketAddr;
    pub fn url(&self) -> String;                                     // "http://127.0.0.1:54321", no trailing slash
    pub fn stop_token(&self) -> CancelToken;
    pub fn wait(&self);                                              // block until stopped
    pub fn shutdown(&self);                                          // graceful: up to 5 s for requests in flight
}
pub struct ServerBuilder;                                            // Clone + Default
impl ServerBuilder {
    pub fn tls(self, tls: TlsOptions) -> ServerBuilder;
    pub fn stop_on(self, token: &CancelToken) -> ServerBuilder;
    pub fn start<H>(self, listener: Listener, handler: H) -> Result<Server, String>;
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsOptions {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
    pub client_ca_pem: Option<Vec<u8>>,                              // require and verify client certs
    pub http2: bool,                                                 // offer h2 as well as http/1.1
}

/// Blocking: serve plain HTTP until `stop` is cancelled, then shut down.
pub fn serve<H>(listener: Listener, handler: H, stop: &CancelToken);

// --- tests ----------------------------------------------------------
pub struct TestServer;                                               // 127.0.0.1:0, stops on drop
impl TestServer {
    pub fn new<H>(handler: H) -> TestServer;                         // http
    pub fn new_tls<H>(handler: H) -> TestServer;                     // https, HTTP/1.1 only
    pub fn builder() -> TestServerBuilder;
    pub fn url(&self) -> String;
    pub fn addr(&self) -> SocketAddr;
    pub fn cert_pem(&self) -> &'static str;                          // = testcerts::CA_PEM
    pub fn close(&self);
}
pub struct TestServerBuilder;                                        // Clone + Default
impl TestServerBuilder {
    pub fn tls(self) -> TestServerBuilder;
    pub fn http2(self, on: bool) -> TestServerBuilder;               // turns TLS on
    pub fn require_client_cert(self) -> TestServerBuilder;           // turns TLS on
    pub fn tls_options(self, tls: TlsOptions) -> TestServerBuilder;
    pub fn addr(self, addr: &str) -> TestServerBuilder;
    pub fn start<H>(self, handler: H) -> TestServer;
}
pub mod testcerts {                                                  // all valid until 2052
    pub const CA_PEM: &str;           pub const CA_KEY_PEM: &str;
    pub const SERVER_CERT_PEM: &str;  pub const SERVER_KEY_PEM: &str;    // localhost, example.com, *.example.com, 127.0.0.1, ::1
    pub const CLIENT_CERT_PEM: &str;  pub const CLIENT_KEY_PEM: &str;    // CN=alice, signed by the CA
    pub const STRANGER_CERT_PEM: &str; pub const STRANGER_KEY_PEM: &str; // CN=mallory, self-signed
}
```

`H` is always `Fn(&Request) -> Response + Send + Sync + 'static`; a
`Handler` box is one.

## Mux patterns

`[METHOD ][HOST]/[PATH]`. A method restricts the pattern (`GET` also takes
HEAD). `{name}` matches one segment, `{name...}` the rest of the path,
`{$}` only the path with its trailing slash; a pattern ending in `/`
matches everything beneath it. The most specific pattern wins; two
patterns where neither is more specific cannot both be registered
(`handle` returns `pattern "…" conflicts with pattern "…":\n…`, and
`parsing "…": at offset N: …` for one that is malformed). A path that
only matches with a trailing slash, or once cleaned of `.`, `..` and `//`,
gets a 301; a path known under other methods a 405 with `Allow`; anything
else `404 page not found`.

## What the server fills in

- `Date`, always.
- `Content-Type`, sniffed from the body when the handler set neither it
  nor `Content-Encoding`: text is `text/plain; charset=utf-8` (JSON too),
  HTML `text/html; charset=utf-8`, binary `application/octet-stream`.
- `Content-Length` for a body of up to 2048 bytes (4096 over HTTP/2); a
  longer body is sent chunked unless the handler set the length.
- HEAD: the GET's headers, no body. 1xx, 204 and 304: no body, no length.
- The reason phrase of an unknown status is `status code N`.

## Protocols

HTTP/1.1 always. HTTP/2 only over TLS and only when asked for
(`TlsOptions::http2`, `TestServerBuilder::http2`): `TestServer::new_tls`
alone speaks HTTP/1.1. No cleartext HTTP/2.
