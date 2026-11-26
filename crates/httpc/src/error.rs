//! The error a failed request returns.

use std::fmt;

/// Why a TLS handshake failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsFailure {
    /// The server's certificate chains to no trusted root.
    UnknownIssuer,
    /// The certificate has expired or is not yet valid.
    Expired,
    /// The certificate is for other names than the one addressed.
    HostnameMismatch,
    /// Anything else: an alert from the peer (a refused client
    /// certificate, no common protocol), a malformed record.
    Other,
}

/// The kind of failure, for callers that map failures to codes and hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The host name did not resolve.
    Dns,
    /// The connection could not be made or broke: a refused or reset
    /// connection, to the server or to a proxy, or a SOCKS proxy that
    /// could not reach the target.
    Connect,
    /// The TLS handshake failed.
    Tls(TlsFailure),
    /// The deadline passed.
    Timeout,
    /// The request's `CancelToken` was cancelled.
    Cancelled,
    /// An HTTP proxy refused the CONNECT tunnel.
    Proxy,
    /// Everything else: a connection closed without an answer, a malformed
    /// response, an unsupported URL, a redirect policy that said stop.
    Other,
}

/// A request that got no response. It displays as `Get "url": message`:
/// the operation is the method in title case and the URL is quoted, so the
/// text matches what apic has always printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// What kind of failure this is.
    pub kind: ErrorKind,
    /// The operation: the method with only its first letter in upper case
    /// (`Get`, `Post`).
    pub op: String,
    /// The URL of the request that failed.
    pub url: String,
    /// The failure itself, without the operation and the URL.
    pub message: String,
    /// Set when the response's status and headers had arrived and the
    /// failure came while reading the body. Such an error displays as the
    /// message alone, without the operation and the URL.
    pub reading_body: bool,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, method: &str, url: &str, message: impl Into<String>) -> Error {
        Error {
            kind,
            op: op_for(method),
            url: url.to_string(),
            message: message.into(),
            reading_body: false,
        }
    }

    /// Reports whether the deadline passed.
    pub fn is_timeout(&self) -> bool {
        self.kind == ErrorKind::Timeout
    }

    /// Reports whether the request was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.kind == ErrorKind::Cancelled
    }
}

/// The operation name for a method: `GET` is `Get`, and an empty method is
/// `Get` too.
fn op_for(method: &str) -> String {
    if method.is_empty() {
        return "Get".to_string();
    }
    let (first, rest) = method.split_at(method.chars().next().map_or(0, char::len_utf8));
    format!("{first}{}", rest.to_lowercase())
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.reading_body {
            return f.write_str(&self.message);
        }
        write!(f, "{} {}: {}", self.op, quote(&self.url), self.message)
    }
}

impl std::error::Error for Error {}

/// Quotes a string the way `%q` does for the characters a URL can hold.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A failure before it is tied to a request: the kind and the message.
#[derive(Clone, Debug)]
pub(crate) struct Fail {
    pub kind: ErrorKind,
    pub message: String,
}

impl Fail {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Fail {
        Fail {
            kind,
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Fail {
        Fail::new(ErrorKind::Other, message)
    }
}

/// The message of an I/O error without the `(os error N)` suffix and with
/// a lower-case first letter: `connection refused`.
pub(crate) fn io_message(err: &std::io::Error) -> String {
    let s = err.to_string();
    let s = match s.find(" (os error") {
        Some(i) => &s[..i],
        None => &s,
    };
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
