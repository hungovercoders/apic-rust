//! The request a handler is given.

use std::sync::Arc;

use apic_httpc::Headers;
use base64::Engine;

/// What is known about the TLS connection a request came over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsState {
    /// The client's certificate chain in DER, leaf first; empty when the
    /// client presented none.
    pub peer_certificates: Vec<Vec<u8>>,
    /// The protocol ALPN chose: `h2`, `http/1.1`, or empty.
    pub alpn: String,
}

impl TlsState {
    /// The common name of the client certificate's subject.
    pub fn peer_common_name(&self) -> Option<String> {
        certificate_common_name(self.peer_certificates.first()?)
    }
}

/// One part of a `multipart/form-data` body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The `name` of the part's Content-Disposition.
    pub name: String,
    /// The `filename` of the part's Content-Disposition, for a file.
    pub filename: Option<String>,
    /// The part's own headers.
    pub headers: Headers,
    /// The part's content.
    pub data: Vec<u8>,
}

/// A request as a handler sees it: plain owned values, the body read.
#[derive(Clone, Debug)]
pub struct Request {
    /// The method: `GET`.
    pub method: String,
    /// The path, percent-decoding done: `/todos/a b`.
    pub path: String,
    /// The path as it was sent: `/todos/a%20b`.
    pub raw_path: String,
    /// The query as it was sent, without the `?`; empty when there is none.
    pub raw_query: String,
    /// The request target as it was sent: `/todos?done=1`, or the
    /// absolute URL a client sends to a proxy.
    pub target: String,
    /// The host the request addressed: the Host header (`:authority`
    /// over HTTP/2), or the host of an absolute target.
    pub host: String,
    /// The headers, names in canonical form, without `Host`.
    pub headers: Headers,
    /// The body. Shared, so cloning a request does not copy it.
    pub body: Arc<[u8]>,
    /// The client's address: `127.0.0.1:54321`.
    pub remote_addr: String,
    /// The protocol: `HTTP/1.1`, `HTTP/2.0`.
    pub proto: String,
    /// Set when the request came over TLS.
    pub tls: Option<TlsState>,
    /// The mux pattern that matched, when a `Mux` routed the request.
    pub pattern: String,
    pub(crate) path_values: Vec<(String, String)>,
}

impl Request {
    /// A request made by hand, for calling a handler or a `Mux` directly
    /// in a test. `target` is `/path?query` or an absolute URL. The host
    /// is `example.com` unless the target names one, the remote address
    /// `192.0.2.1:1234` and the protocol `HTTP/1.1`.
    pub fn new(method: &str, target: &str) -> Request {
        let mut host = "example.com".to_string();
        let mut rest = target;
        if let Some((_, after)) = target.split_once("://") {
            let end = after.find(['/', '?']).unwrap_or(after.len());
            host = after[..end].to_string();
            rest = &after[end..];
        }
        let (raw_path, raw_query) = match rest.split_once('?') {
            Some((p, q)) => (p, q),
            None => (rest, ""),
        };
        let raw_path = if raw_path.is_empty() { "/" } else { raw_path };
        Request {
            method: method.to_string(),
            path: path_unescape(raw_path),
            raw_path: raw_path.to_string(),
            raw_query: raw_query.to_string(),
            target: target.to_string(),
            host,
            headers: Headers::new(),
            body: Arc::from(&[][..]),
            remote_addr: "192.0.2.1:1234".to_string(),
            proto: "HTTP/1.1".to_string(),
            tls: None,
            pattern: String::new(),
            path_values: Vec::new(),
        }
    }

    /// Adds a header (chained, for a request made by hand).
    pub fn with_header(mut self, name: &str, value: &str) -> Request {
        self.headers.add(name, value);
        self
    }

    /// Sets the body (chained, for a request made by hand).
    pub fn with_body(mut self, body: impl AsRef<[u8]>) -> Request {
        self.body = Arc::from(body.as_ref());
        self
    }

    /// Sets the host (chained, for a request made by hand).
    pub fn with_host(mut self, host: &str) -> Request {
        self.host = host.to_string();
        self
    }

    /// The first value of a header, or `None`.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    /// The body as text, invalid UTF-8 replaced.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The URL of the request: the path and query, or the absolute URL
    /// when that is what was sent.
    pub fn url(&self) -> &str {
        &self.target
    }

    /// The value the mux matched for a `{name}` wildcard of the pattern,
    /// or `""` when the pattern has no such wildcard.
    pub fn path_value(&self, name: &str) -> &str {
        self.path_values
            .iter()
            .find(|(k, _)| k == name)
            .map_or("", |(_, v)| v.as_str())
    }

    /// Sets a path value, as the mux does when it routes.
    pub fn set_path_value(&mut self, name: &str, value: &str) {
        match self.path_values.iter_mut().find(|(k, _)| k == name) {
            Some(entry) => entry.1 = value.to_string(),
            None => self.path_values.push((name.to_string(), value.to_string())),
        }
    }

    /// The query parameters, decoded, in order.
    pub fn query(&self) -> Vec<(String, String)> {
        parse_query(&self.raw_query)
    }

    /// The first value of a query parameter.
    pub fn query_get(&self, name: &str) -> Option<String> {
        self.query().into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// Every value of a query parameter, in order.
    pub fn query_values(&self, name: &str) -> Vec<String> {
        self.query()
            .into_iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v)
            .collect()
    }

    /// The form parameters: those of an
    /// `application/x-www-form-urlencoded` body of a POST, PUT or PATCH
    /// first, then the query's.
    pub fn form(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let urlencoded = self
            .media_type()
            .is_some_and(|t| t == "application/x-www-form-urlencoded");
        if urlencoded && matches!(self.method.as_str(), "POST" | "PUT" | "PATCH") {
            out = parse_query(&String::from_utf8_lossy(&self.body));
        }
        out.extend(self.query());
        out
    }

    /// The first value of a form parameter (see `form`), or `""`. A
    /// `multipart/form-data` body's plain fields count too.
    pub fn form_value(&self, name: &str) -> String {
        if let Some((_, v)) = self.form().into_iter().find(|(k, _)| k == name) {
            return v;
        }
        if let Ok(parts) = self.multipart() {
            if let Some(p) = parts.iter().find(|p| p.name == name && p.filename.is_none()) {
                return String::from_utf8_lossy(&p.data).into_owned();
            }
        }
        String::new()
    }

    /// The user name and password of a basic `Authorization` header.
    pub fn basic_auth(&self) -> Option<(String, String)> {
        let auth = self.headers.get("Authorization")?;
        if auth.len() < 6 || !auth[..6].eq_ignore_ascii_case("basic ") {
            return None;
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(auth[6..].trim())
            .ok()?;
        let text = String::from_utf8_lossy(&decoded).into_owned();
        let (user, pass) = text.split_once(':')?;
        Some((user.to_string(), pass.to_string()))
    }

    /// The value of a cookie the request carries.
    pub fn cookie(&self, name: &str) -> Option<String> {
        for line in self.headers.values("Cookie") {
            for pair in line.split(';') {
                if let Some((k, v)) = pair.trim().split_once('=') {
                    if k == name {
                        return Some(v.trim_matches('"').to_string());
                    }
                }
            }
        }
        None
    }

    /// The Content-Type without its parameters, in lower case.
    fn media_type(&self) -> Option<String> {
        let ct = self.headers.get("Content-Type")?;
        Some(ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
    }

    /// The parts of a `multipart/form-data` body.
    pub fn multipart(&self) -> Result<Vec<Part>, String> {
        let ct = self.headers.get("Content-Type").unwrap_or("");
        if self.media_type().as_deref() != Some("multipart/form-data") {
            return Err("request Content-Type isn't multipart/form-data".to_string());
        }
        let boundary = ct
            .split(';')
            .skip(1)
            .filter_map(|p| p.trim().split_once('='))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("boundary"))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
            .filter(|b| !b.is_empty())
            .ok_or("no multipart boundary param in Content-Type")?;
        parse_multipart(&self.body, &boundary)
    }
}

/// Decodes `%XX` escapes in a path; a malformed escape leaves the path as
/// it was written.
pub fn path_unescape(s: &str) -> String {
    unescape(s, false).unwrap_or_else(|| s.to_string())
}

fn unescape(s: &str, plus_is_space: bool) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hi = (*b.get(i + 1)? as char).to_digit(16)?;
                let lo = (*b.get(i + 2)? as char).to_digit(16)?;
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Parses a query string or a urlencoded form: `a=1&b=x+y`. A pair that
/// does not decode, or that holds a semicolon, is skipped.
pub fn parse_query(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pair in raw.split('&') {
        if pair.is_empty() || pair.contains(';') {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if let (Some(k), Some(v)) = (unescape(k, true), unescape(v, true)) {
            out.push((k, v));
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn parse_multipart(body: &[u8], boundary: &str) -> Result<Vec<Part>, String> {
    const BROKEN: &str = "multipart: NextPart: EOF";
    let delim = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    // The first delimiter starts a line; anything before it is preamble.
    let mut at = if body.starts_with(&delim) {
        0
    } else {
        let mut marker = b"\n".to_vec();
        marker.extend_from_slice(&delim);
        find(body, &marker, 0).ok_or(BROKEN)? + 1
    };
    loop {
        let mut pos = at + delim.len();
        if body[pos..].starts_with(b"--") {
            return Ok(parts);
        }
        // The rest of the delimiter line.
        pos = find(body, b"\n", pos).ok_or(BROKEN)? + 1;
        let mut headers = Headers::new();
        loop {
            let end = find(body, b"\n", pos).ok_or(BROKEN)?;
            let line = String::from_utf8_lossy(&body[pos..end]);
            let line = line.trim_end_matches('\r');
            pos = end + 1;
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.add(name.trim(), value.trim());
            }
        }
        let mut marker = b"\n".to_vec();
        marker.extend_from_slice(&delim);
        let next = find(body, &marker, pos.saturating_sub(1)).ok_or(BROKEN)?;
        let mut end = next.max(pos);
        if end > pos && body[end - 1] == b'\r' {
            end -= 1;
        }
        let disposition = headers.get("Content-Disposition").unwrap_or("").to_string();
        parts.push(Part {
            name: disposition_param(&disposition, "name").unwrap_or_default(),
            filename: disposition_param(&disposition, "filename"),
            headers,
            data: body[pos.min(end)..end].to_vec(),
        });
        at = next + 1;
    }
}

/// The value of a `key="value"` parameter of a Content-Disposition.
fn disposition_param(header: &str, key: &str) -> Option<String> {
    let mut rest = header;
    while let Some(i) = rest.find(';') {
        rest = rest[i + 1..].trim_start();
        let Some((name, after)) = rest.split_once('=') else {
            return None;
        };
        let value = if let Some(quoted) = after.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = quoted.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => out.extend(chars.next()),
                    '"' => break,
                    c => out.push(c),
                }
            }
            out
        } else {
            after.split(';').next().unwrap_or("").trim().to_string()
        };
        if name.trim().eq_ignore_ascii_case(key) {
            return Some(value);
        }
    }
    None
}

/// The common name of an X.509 certificate's subject, read from its DER.
pub fn certificate_common_name(der: &[u8]) -> Option<String> {
    // Certificate ::= SEQUENCE { tbsCertificate SEQUENCE { [0] version,
    // serial, signature, issuer, validity, subject, ... }, ... }
    let (_, cert, _) = tlv(der)?;
    let (_, mut tbs, _) = tlv(cert)?;
    let (tag, _, rest) = tlv(tbs)?;
    if tag == 0xA0 {
        tbs = rest;
    }
    for _ in 0..4 {
        tbs = tlv(tbs)?.2;
    }
    let (_, mut subject, _) = tlv(tbs)?;
    let mut found = None;
    while !subject.is_empty() {
        let (_, mut set, rest) = tlv(subject)?;
        subject = rest;
        while !set.is_empty() {
            let (_, attr, rest) = tlv(set)?;
            set = rest;
            let (_, oid, value) = tlv(attr)?;
            if oid == [0x55, 0x04, 0x03] {
                let (_, text, _) = tlv(value)?;
                found = Some(String::from_utf8_lossy(text).into_owned());
            }
        }
    }
    found
}

/// Reads one DER element: its tag, its content and what follows it.
fn tlv(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *data.first()?;
    let first = *data.get(1)? as usize;
    let (len, start) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = len << 8 | *data.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = start.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    Some((tag, &data[start..end], &data[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn made_by_hand() {
        let r = Request::new("GET", "/todos/a%20b?done=1&tag=x+y&tag=z")
            .with_header("authorization", "Basic YWxpY2U6czNjcmV0")
            .with_header("Cookie", "sid=abc; theme=dark");
        assert_eq!(r.path, "/todos/a b");
        assert_eq!(r.raw_path, "/todos/a%20b");
        assert_eq!(r.query_get("done").as_deref(), Some("1"));
        assert_eq!(r.query_values("tag"), ["x y", "z"]);
        assert_eq!(r.query_get("none"), None);
        assert_eq!(r.basic_auth(), Some(("alice".to_string(), "s3cret".to_string())));
        assert_eq!(r.cookie("theme").as_deref(), Some("dark"));
        assert_eq!(r.host, "example.com");
        let r = Request::new("GET", "http://api.example.test/ping?x=1");
        assert_eq!((r.host.as_str(), r.path.as_str(), r.raw_query.as_str()), ("api.example.test", "/ping", "x=1"));
        assert_eq!(r.url(), "http://api.example.test/ping?x=1");
    }

    #[test]
    fn forms() {
        let r = Request::new("POST", "/token?client_id=query")
            .with_header("Content-Type", "application/x-www-form-urlencoded; charset=utf-8")
            .with_body("client_id=demo&scope=a+b");
        assert_eq!(r.form_value("client_id"), "demo");
        assert_eq!(r.form_value("scope"), "a b");
        assert_eq!(r.form_value("missing"), "");
    }

    #[test]
    fn multipart() {
        let body = "--XX\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nhello\r\n--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a \\\"b\\\".txt\"\r\nContent-Type: text/plain\r\n\r\nline one\r\nline two\r\n--XX--\r\n";
        let r = Request::new("POST", "/upload")
            .with_header("Content-Type", "multipart/form-data; boundary=XX")
            .with_body(body);
        let parts = r.multipart().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].name.as_str(), &parts[0].data[..]), ("title", &b"hello"[..]));
        assert_eq!(parts[1].filename.as_deref(), Some("a \"b\".txt"));
        assert_eq!(parts[1].data, b"line one\r\nline two");
        assert_eq!(parts[1].headers.get("content-type"), Some("text/plain"));
        assert_eq!(r.form_value("title"), "hello");
        let bad = Request::new("POST", "/upload")
            .with_header("Content-Type", "multipart/form-data; boundary=XX")
            .with_body("--XX\r\nno end");
        assert!(bad.multipart().is_err());
        assert!(Request::new("POST", "/").multipart().is_err());
    }

    #[test]
    fn common_name() {
        let pem = include_str!("../testdata/client.pem");
        let der = rustls_pemfile::certs(&mut pem.as_bytes()).next().unwrap().unwrap();
        assert_eq!(certificate_common_name(&der).as_deref(), Some("alice"));
        assert_eq!(certificate_common_name(b"junk"), None);
    }
}
