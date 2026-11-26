//! Reading a request URL. The URL is taken apart by hand rather than
//! normalised, so the path and the query go out exactly as written.

use crate::error::Fail;
use crate::headers::is_token_byte;

/// A request URL, split into what the client needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// `http` or `https`.
    pub scheme: String,
    /// The user name and password of a `user:pass@` prefix, decoded.
    pub userinfo: Option<(String, String)>,
    /// The host to connect to: lower case, an IPv6 address without its
    /// brackets.
    pub host: String,
    /// The port to connect to, the scheme's default when none is written.
    pub port: u16,
    /// `host[:port]` as written (lower case), for the Host header.
    pub authority: String,
    /// The path and the query as written, `/` when there is none.
    pub path_and_query: String,
}

impl Target {
    pub fn is_https(&self) -> bool {
        self.scheme == "https"
    }

    /// `host:port`, with an IPv6 address in brackets.
    pub fn host_port(&self) -> String {
        join_host_port(&self.host, self.port)
    }
}

/// `host:port`, with an IPv6 address in brackets.
pub(crate) fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Checks a method the way the request line needs it: a non-empty token.
pub(crate) fn check_method(method: &str) -> Result<(), String> {
    if method.is_empty() || !method.bytes().all(is_token_byte) {
        return Err(format!("net/http: invalid method {}", quoted(method)));
    }
    Ok(())
}

fn quoted(s: &str) -> String {
    format!("{s:?}")
}

/// Parses an absolute `http` or `https` URL.
pub(crate) fn parse_target(raw: &str) -> Result<Target, Fail> {
    if raw.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(Fail::other(format!(
            "parse {}: net/url: invalid control character in URL",
            quoted(raw)
        )));
    }
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Err(Fail::other("unsupported protocol scheme \"\""));
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(Fail::other(format!(
            "unsupported protocol scheme {}",
            quoted(&scheme)
        )));
    }
    let rest = rest.split('#').next().unwrap_or("");
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let userinfo = userinfo.map(|u| match u.split_once(':') {
        Some((user, pass)) => (percent_decode(user), percent_decode(pass)),
        None => (percent_decode(u), String::new()),
    });
    let (host, port) = split_host_port(hostport)
        .map_err(|e| Fail::other(format!("parse {}: {e}", quoted(raw))))?;
    if host.is_empty() {
        return Err(Fail::other("http: no Host in request URL"));
    }
    let mut host = host.to_ascii_lowercase();
    if !host.is_ascii() {
        host = match url::Host::parse(&host) {
            Ok(h) => h.to_string(),
            Err(_) => {
                return Err(Fail::other(format!(
                    "parse {}: invalid host name",
                    quoted(raw)
                )))
            }
        };
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let bracketed = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    let authority = match port {
        Some(p) => format!("{bracketed}:{p}"),
        None => bracketed,
    };
    let path_and_query = match tail {
        "" => "/".to_string(),
        t if t.starts_with('?') => format!("/{}", escape_request_target(t)),
        t => escape_request_target(t),
    };
    Ok(Target {
        scheme,
        userinfo,
        host,
        port: port.unwrap_or(default_port),
        authority,
        path_and_query,
    })
}

/// Splits `host[:port]`; the host of `[::1]:80` is `::1`.
pub(crate) fn split_host_port(hostport: &str) -> Result<(&str, Option<u16>), String> {
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err("missing ']' in host".to_string());
        };
        (&rest[..end], &rest[end + 1..])
    } else {
        match hostport.rfind(':') {
            Some(i) => (&hostport[..i], &hostport[i..]),
            None => (hostport, ""),
        }
    };
    let port = match port {
        "" | ":" => None,
        p => match p.strip_prefix(':').and_then(|d| {
            if d.bytes().all(|b| b.is_ascii_digit()) {
                d.parse::<u16>().ok()
            } else {
                None
            }
        }) {
            Some(n) => Some(n),
            None => return Err(format!("invalid port {} after host", quoted(p))),
        },
    };
    Ok((host, port))
}

/// Percent-encodes the bytes a request line cannot carry (a space, a
/// quote, anything outside ASCII) and leaves the rest, existing escapes
/// included, as written.
fn escape_request_target(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let keep = (0x21..=0x7e).contains(&b) && !b"\"<>\\^`{|}".contains(&b);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes; a malformed escape is kept as written.
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let t = parse_target("HTTPS://Alice:p%40ss@Example.com:8443/a/../b?q=a b#frag").unwrap();
        assert_eq!(t.scheme, "https");
        assert_eq!(t.userinfo, Some(("Alice".to_string(), "p@ss".to_string())));
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, 8443);
        assert_eq!(t.authority, "example.com:8443");
        assert_eq!(t.path_and_query, "/a/../b?q=a%20b");
        let t = parse_target("http://[::1]:8080").unwrap();
        assert_eq!((t.host.as_str(), t.port, t.authority.as_str()), ("::1", 8080, "[::1]:8080"));
        assert_eq!(t.host_port(), "[::1]:8080");
        let t = parse_target("http://example.com?x=1").unwrap();
        assert_eq!((t.port, t.path_and_query.as_str()), (80, "/?x=1"));
    }

    #[test]
    fn rejects() {
        for (raw, want) in [
            ("ftp://x/", "unsupported protocol scheme \"ftp\""),
            ("example.com/x", "unsupported protocol scheme \"\""),
            ("http:///x", "http: no Host in request URL"),
            ("http://h:x/", "parse \"http://h:x/\": invalid port \":x\" after host"),
        ] {
            assert_eq!(parse_target(raw).unwrap_err().message, want);
        }
        assert!(check_method("GET").is_ok());
        assert_eq!(check_method("G ET").unwrap_err(), "net/http: invalid method \"G ET\"");
    }

    #[test]
    fn decodes() {
        assert_eq!(percent_decode("a%20b%zz%4"), "a b%zz%4");
    }
}
