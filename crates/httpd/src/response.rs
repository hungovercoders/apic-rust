//! The response a handler returns.

use apic_httpc::{status_text, Headers};
use tokio::sync::mpsc;

use crate::request::Request;

/// The sending half of a streamed response body. Each `send` goes to the
/// client as it is made; dropping the sender ends the body.
#[derive(Clone, Debug)]
pub struct BodySender {
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl BodySender {
    /// Sends the next bytes of the body. Returns `false` once the client
    /// has gone away.
    pub fn send(&self, data: impl Into<Vec<u8>>) -> bool {
        self.tx.send(data.into()).is_ok()
    }

    /// Reports whether the client has gone away.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// A response: a status, headers and a body.
///
/// The server fills in what a handler leaves out. `Date` is always set.
/// A body gets a `Content-Type` sniffed from its first bytes when the
/// handler set neither that nor `Content-Encoding` (see
/// `detect_content_type`). A body of up to 2048 bytes (4096 over HTTP/2)
/// gets a `Content-Length`; a longer one is sent chunked unless the
/// handler set the length itself. A HEAD request gets the headers of the
/// GET without the body, and a 1xx, 204 or 304 never has a body.
#[derive(Debug)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The headers.
    pub headers: Headers,
    /// The body.
    pub body: Vec<u8>,
    pub(crate) stream: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    pub(crate) hang_up: bool,
}

impl Default for Response {
    fn default() -> Response {
        Response::new(200)
    }
}

impl Response {
    /// An empty response with a status.
    pub fn new(status: u16) -> Response {
        Response {
            status,
            headers: Headers::new(),
            body: Vec::new(),
            stream: None,
            hang_up: false,
        }
    }

    /// Sets a header, replacing its earlier values (chained).
    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.set(name, value);
        self
    }

    /// Adds a header value, keeping earlier ones (chained): several
    /// `Set-Cookie` headers.
    pub fn add_header(mut self, name: &str, value: &str) -> Response {
        self.headers.add(name, value);
        self
    }

    /// Sets the body (chained).
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Response {
        self.body = body.into();
        self
    }

    /// A `text/plain; charset=utf-8` response.
    pub fn text(status: u16, body: impl Into<Vec<u8>>) -> Response {
        Response::new(status)
            .with_header("Content-Type", "text/plain; charset=utf-8")
            .with_body(body)
    }

    /// An `application/json` response.
    pub fn json(status: u16, body: impl Into<Vec<u8>>) -> Response {
        Response::new(status)
            .with_header("Content-Type", "application/json")
            .with_body(body)
    }

    /// A `text/html; charset=utf-8` response.
    pub fn html(status: u16, body: impl Into<Vec<u8>>) -> Response {
        Response::new(status)
            .with_header("Content-Type", "text/html; charset=utf-8")
            .with_body(body)
    }

    /// A plain-text error: the message and a newline, with
    /// `X-Content-Type-Options: nosniff`.
    pub fn error(status: u16, message: &str) -> Response {
        Response::new(status)
            .with_header("Content-Type", "text/plain; charset=utf-8")
            .with_header("X-Content-Type-Options", "nosniff")
            .with_body(format!("{message}\n"))
    }

    /// The standard `404 page not found`.
    pub fn not_found() -> Response {
        Response::error(404, "404 page not found")
    }

    /// A redirect to `location` with a 3xx status. A relative location is
    /// resolved against the request's path, and a GET gets the short HTML
    /// body that names the target.
    pub fn redirect(req: &Request, location: &str, status: u16) -> Response {
        let mut url = location.to_string();
        if !location.contains("://") && !location.starts_with("//") {
            if !url.starts_with('/') {
                let dir = match req.path.rfind('/') {
                    Some(i) => &req.path[..=i],
                    None => "/",
                };
                url = format!("{dir}{url}");
            }
            let (path, query) = match url.find('?') {
                Some(i) => (url[..i].to_string(), url[i..].to_string()),
                None => (url.clone(), String::new()),
            };
            let trailing = path.ends_with('/');
            let mut clean = crate::mux::path_clean(&path);
            if trailing && !clean.ends_with('/') {
                clean.push('/');
            }
            url = clean + &query;
        }
        let mut resp = Response::new(status).with_header("Location", &hex_escape_non_ascii(&url));
        if req.method == "GET" || req.method == "HEAD" {
            resp.headers.set("Content-Type", "text/html; charset=utf-8");
        }
        if req.method == "GET" {
            resp.body = format!(
                "<a href=\"{}\">{}</a>.\n\n",
                html_escape(&url),
                status_text(status)
            )
            .into_bytes();
        }
        resp
    }

    /// Closes the connection without answering, as a server that crashed
    /// mid-request would: the client sees the connection end.
    pub fn hang_up() -> Response {
        let mut resp = Response::new(0);
        resp.hang_up = true;
        resp
    }

    /// A response whose body is sent as the returned sender is fed, for
    /// server-sent events and other long answers. The handler returns the
    /// response at once and something else (a thread it started) keeps
    /// the sender.
    pub fn streaming(status: u16) -> (Response, BodySender) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut resp = Response::new(status);
        resp.stream = Some(rx);
        (resp, BodySender { tx })
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

fn hex_escape_non_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b < 0x80 {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        let e = Response::error(401, "{\"error\":\"unauthorised\"}");
        assert_eq!(e.body, b"{\"error\":\"unauthorised\"}\n");
        assert_eq!(e.headers.get("X-Content-Type-Options"), Some("nosniff"));
        assert_eq!(Response::not_found().body, b"404 page not found\n");

        let r = Response::redirect(&Request::new("GET", "/redirect"), "/status/200", 302);
        assert_eq!(r.headers.get("Location"), Some("/status/200"));
        assert_eq!(r.body, b"<a href=\"/status/200\">Found</a>.\n\n");
        let r = Response::redirect(&Request::new("POST", "/a/b"), "c?x=1", 303);
        assert_eq!(r.headers.get("Location"), Some("/a/c?x=1"));
        assert!(r.body.is_empty() && !r.headers.contains("Content-Type"));
    }
}
