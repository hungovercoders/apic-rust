# apic-httpc

The blocking HTTP client apic sends requests with (`apic_httpc`). hyper,
rustls and tokio underneath, on one background thread shared by every
client; nothing asynchronous in the public API except
`CancelToken::cancelled`.

```rust
let client = Client::builder()
    .tls(TlsConfig { ca_pem: Some(pem), ..TlsConfig::default() })
    .protocols(Protocols::Any)
    .build()?;                       // Err: TlsConfigError
let resp = client.send(&Request::get("https://api.example.com/x")
    .header("Accept", "application/json")
    .timeout(Duration::from_secs(30))
    .max_body(10 << 20))?;           // Err: Error
resp.status_code; resp.status; resp.proto; resp.headers.get("content-type");
resp.body; resp.truncated; resp.timings.reused;
```

## Rules of use

- One `Client` has one `TlsConfig` and one `Protocols`. Build one per
  distinct set and keep it: the pool lives in the client (clones share it),
  and a pooled connection is what makes `Timings::reused` true. Building is
  cheap; the system's roots are read once per process on the first HTTPS
  dial.
- `Client::send` is one round trip and never follows a redirect.
  `follow_redirects` (or `Client::execute`, the default policy) does.
- The proxy is chosen per request (`Request::proxy`). The client never
  reads `HTTP_PROXY` and friends.
- The body is read into memory, up to `Request::max_body`.

## API

```rust
// --- client ---------------------------------------------------------
pub struct Client;                       // Clone + Send + Sync + Default + Debug
impl Client {
    pub fn new() -> Client;              // default TLS, Protocols::Any
    pub fn builder() -> ClientBuilder;
    pub fn send(&self, req: &Request) -> Result<Response, Error>;    // one round trip
    pub fn execute(&self, req: &Request) -> Result<Response, Error>; // follows redirects, default policy
    pub fn close_idle_connections(&self);
}
impl RoundTrip for Client {}

pub struct ClientBuilder;                // Clone + Default
impl ClientBuilder {
    pub fn tls(self, tls: TlsConfig) -> ClientBuilder;
    pub fn protocols(self, protocols: Protocols) -> ClientBuilder;
    pub fn build(self) -> Result<Client, TlsConfigError>;
}

pub enum Protocols { Any /* default: h2 via ALPN, else 1.1 */, Http1, Http2 /* TLS only */ }

pub trait RoundTrip {                    // the Client, or a layer over it (digest auth)
    fn round_trip(&self, req: &Request) -> Result<Response, Error>;
}

// --- request --------------------------------------------------------
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub method: String,                  // any token
    pub url: String,                     // absolute http(s) URL; path and query sent as written
    pub headers: Headers,
    pub host: Option<String>,            // Host / :authority override
    pub body: Vec<u8>,
    pub timeout: Option<Duration>,       // whole exchange, body included
    pub deadline: Option<Instant>,       // same as a point in time; the earlier wins
    pub cancel: Option<CancelToken>,
    pub proxy: Option<Proxy>,
    pub max_body: Option<u64>,           // cap on decoded body bytes
}
impl Request {
    pub fn new(method: &str, url: &str) -> Request;
    pub fn get(url: &str) -> Request;
    pub fn post(url: &str, body: impl Into<Vec<u8>>) -> Request;
    pub fn header(self, name: &str, value: &str) -> Request;   // adds
    pub fn body(self, body: impl Into<Vec<u8>>) -> Request;
    pub fn host(self, host: &str) -> Request;
    pub fn timeout(self, timeout: Duration) -> Request;
    pub fn cancel(self, token: &CancelToken) -> Request;
    pub fn proxy(self, proxy: Proxy) -> Request;
    pub fn max_body(self, max: u64) -> Request;
    pub fn check(&self) -> Result<(), String>;  // method and URL, without sending
}

// --- response -------------------------------------------------------
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status_code: u16,
    pub status: String,                  // "200 OK"
    pub proto: String,                   // "HTTP/1.1", "HTTP/1.0", "HTTP/2.0"
    pub headers: Headers,                // canonical names, order received
    pub body: Vec<u8>,
    pub truncated: bool,                 // body was longer than max_body
    pub uncompressed: bool,              // client asked for gzip and decoded it
    pub timings: Timings,
    pub method: String,                  // of the request answered (last hop)
    pub url: String,                     // of the request answered (last hop)
}
impl Response {
    pub fn status_text(&self) -> &str;   // "OK"
    pub fn header(&self, name: &str) -> Option<&str>;
    pub fn text(&self) -> String;        // lossy UTF-8
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    pub dns: Duration,                   // zero for an IP literal or a reused connection
    pub connect: Duration,
    pub tls: Duration,
    pub ttfb: Duration,                  // request start -> response head
    pub total: Duration,                 // request start -> end of body
    pub reused: bool,
    pub started: Instant,
}
impl Timings { pub fn then(&self, later: &Timings) -> Timings; }  // fold two hops

// --- headers --------------------------------------------------------
pub fn canonical_header_key(name: &str) -> String;   // "content-type" -> "Content-Type"
pub fn status_text(code: u16) -> &'static str;       // 200 -> "OK", unknown -> ""

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers;                      // ordered multi-map, canonical names, FromIterator<(N, V)>
impl Headers {
    pub fn new() -> Headers;
    pub fn add(&mut self, name: &str, value: &str);
    pub fn set(&mut self, name: &str, value: &str);
    pub fn get(&self, name: &str) -> Option<&str>;        // first value, any case
    pub fn values(&self, name: &str) -> Vec<&str>;
    pub fn contains(&self, name: &str) -> bool;
    pub fn del(&mut self, name: &str);
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>;
    pub fn names(&self) -> Vec<&str>;                     // distinct, first-seen order
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn to_map(&self) -> BTreeMap<String, Vec<String>>; // sorted by name
}

// --- TLS ------------------------------------------------------------
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsConfig {
    pub ca_pem: Option<Vec<u8>>,         // trusted in addition to the system roots
    pub cert_pem: Option<Vec<u8>>,       // client certificate (+chain)
    pub key_pem: Option<Vec<u8>>,        // its key; None reads it from cert_pem
    pub insecure: bool,                  // skip server verification
}
pub enum TlsConfigError {                // Display + Error
    NoCaCertificates,                    // "no PEM certificates found"
    ClientCertificate(String),           // "tls: private key does not match public key", ...
}

// --- proxies --------------------------------------------------------
pub enum ProxyScheme { Http, Https, Socks5, Socks5h }
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Proxy {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,                       // default 80 / 443 / 1080
    pub username: Option<String>,
    pub password: Option<String>,
}
impl Proxy { pub fn parse(raw: &str) -> Result<Proxy, String>; }  // needs scheme://

// --- cancellation ---------------------------------------------------
#[derive(Clone, Debug, Default)]
pub struct CancelToken;                  // clones share one state
impl CancelToken {
    pub fn new() -> CancelToken;
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
    pub fn wait_timeout(&self, d: Duration) -> bool;  // interruptible sleep; true = cancelled
    pub fn wait(&self);                               // block until cancelled
    pub async fn cancelled(&self);
}

// --- redirects ------------------------------------------------------
pub fn follow_redirects(
    transport: &dyn RoundTrip,
    req: &Request,
    policy: &RedirectPolicy<'_>,
) -> Result<Response, Error>;

#[derive(Clone, Copy, Default)]
pub struct RedirectPolicy<'a> {
    pub check: Option<&'a CheckRedirect<'a>>,  // None: follow, fail after 10 requests
    pub jar: Option<&'a dyn CookieJar>,
}
pub type CheckRedirect<'a> =
    dyn Fn(&mut Request /* next */, &[Request] /* via, oldest first, bodies emptied */)
        -> Result<RedirectAction, String> + 'a;
pub enum RedirectAction { Follow, Stop /* return the 3xx itself */ }
pub trait CookieJar {
    fn cookies(&self, url: &str) -> Vec<(String, String)>;
    fn set_cookies(&self, url: &str, set_cookie: &[String]);  // raw Set-Cookie values
}

// --- errors ---------------------------------------------------------
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {                       // Display: `Get "url": message`
    pub kind: ErrorKind,
    pub op: String,                      // "Get", "Post"
    pub url: String,
    pub message: String,
    pub reading_body: bool,              // failed after the head arrived; Display is the message alone
}
impl Error { pub fn is_timeout(&self) -> bool; pub fn is_cancelled(&self) -> bool; }
pub enum ErrorKind { Dns, Connect, Tls(TlsFailure), Timeout, Cancelled, Proxy, Other }
pub enum TlsFailure { UnknownIssuer, Expired, HostnameMismatch, Other }
```

## What goes on the wire

- HTTP/1.1 headers in canonical case, in this order: `Host`, `User-Agent`,
  `Content-Length`, the caller's headers sorted by name, `Accept-Encoding`.
- `User-Agent: Go-http-client/1.1` (`/2.0` over HTTP/2) unless the caller
  sets one; an empty `User-Agent` sends none.
- `Accept-Encoding: gzip` unless the caller sets `Accept-Encoding` or
  `Range`, or the method is HEAD; a gzip answer to that is decoded, and
  `Content-Encoding` and `Content-Length` are then removed from the
  response headers. A caller that asks for gzip itself gets the bytes as
  sent.
- `Content-Length` from the body; an empty body sends `Content-Length: 0`
  for POST, PUT and PATCH only. `Content-Length`, `Transfer-Encoding` and
  `Trailer` set by the caller are ignored.
- A `Host` header in `headers` is the same as the `host` field.
- `user:password@` in the URL becomes basic `Authorization` unless the
  header is set.
- Through an `http://` or `https://` proxy: absolute-form request line and
  `Proxy-Authorization` for an `http://` target, a CONNECT tunnel for an
  `https://` one. `socks5` and `socks5h` both send the host name to the
  proxy.

## Error messages

| Failure | `kind` | `message` |
| --- | --- | --- |
| name does not resolve | `Dns` | `dial tcp: lookup HOST: no such host` |
| refused connection | `Connect` | `dial tcp 127.0.0.1:1: connect: connection refused` |
| proxy unreachable | `Connect` | `proxyconnect tcp: dial tcp …` |
| SOCKS failure | `Connect` | `socks connect tcp PROXY->TARGET: …` |
| CONNECT refused | `Proxy` | the proxy's reason: `Proxy Authentication Required` |
| unknown CA | `Tls(UnknownIssuer)` | `tls: failed to verify certificate: x509: certificate signed by unknown authority` |
| expired | `Tls(Expired)` | `tls: failed to verify certificate: x509: certificate has expired or is not yet valid` |
| wrong name | `Tls(HostnameMismatch)` | `tls: failed to verify certificate: x509: certificate is valid for a, b, not c` |
| alert from the peer | `Tls(Other)` | `remote error: tls: certificate required` |
| deadline | `Timeout` | `context deadline exceeded` |
| cancelled | `Cancelled` | `context canceled` |
| closed without an answer | `Other` | `EOF` (`unexpected EOF` mid-body) |
| bad URL or method | `Other` | `unsupported protocol scheme "ftp"`, `net/http: invalid method "…"` |
| redirect limit | `Other` | `stopped after 10 redirects` (URL is the `Location`) |

For apic's error codes: `Dns` and `Connect` are the connect code, any
`Tls` the TLS code, `Proxy` and `Other` the generic transport code.
