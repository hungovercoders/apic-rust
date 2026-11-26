//! The few pieces of URL and form handling auth needs: query escaping,
//! `application/x-www-form-urlencoded` bodies, and taking a URL apart
//! without normalising it, so what is signed is what is sent.

use std::collections::BTreeMap;

/// Escapes a string for a query component: a space is `+`, and every byte
/// outside the unreserved set is `%XX`.
pub(crate) fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Undoes [`query_escape`]; `None` for a malformed `%` escape.
pub(crate) fn query_unescape(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = hex(*b.get(i + 1)?)?;
                let l = hex(*b.get(i + 2)?)?;
                out.push(h << 4 | l);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

/// Decodes `%XX` escapes in a path; a malformed escape is kept as written.
pub(crate) fn path_unescape(s: &str) -> String {
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

/// Reads a raw query into its pairs, in order. A pair that is malformed
/// (a bad escape, or a `;`) is left out.
pub(crate) fn parse_query(raw: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for pair in raw.split('&') {
        if pair.is_empty() || pair.contains(';') {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let (Some(k), Some(v)) = (query_unescape(k), query_unescape(v)) else {
            continue;
        };
        out.push((k, v));
    }
    out
}

/// The fields of a form or a query, encoded sorted by name.
#[derive(Clone, Debug, Default)]
pub(crate) struct Form(BTreeMap<String, Vec<String>>);

impl Form {
    pub fn new(pairs: &[(&str, &str)]) -> Form {
        let mut f = Form::default();
        for (k, v) in pairs {
            f.set(k, v);
        }
        f
    }

    /// Sets a field, replacing what it held.
    pub fn set(&mut self, key: &str, value: &str) {
        self.0.insert(key.to_string(), vec![value.to_string()]);
    }

    /// Adds a value to a field.
    pub fn add(&mut self, key: &str, value: &str) {
        self.0
            .entry(key.to_string())
            .or_default()
            .push(value.to_string());
    }

    /// Takes every field of `other`, replacing those of the same name.
    pub fn replace_with(&mut self, other: &Form) {
        for (k, values) in &other.0 {
            self.0.insert(k.clone(), values.clone());
        }
    }

    /// `a=1&b=2`, sorted by name.
    pub fn encode(&self) -> String {
        let mut parts = Vec::new();
        for (k, values) in &self.0 {
            for v in values {
                parts.push(format!("{}={}", query_escape(k), query_escape(v)));
            }
        }
        parts.join("&")
    }
}

/// A URL split where its parts begin, each part as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UrlParts<'a> {
    /// Lower case; empty when the URL has no `scheme://`.
    pub scheme: String,
    /// `host[:port]`, without any `user:password@`.
    pub host: &'a str,
    pub path: &'a str,
    /// What follows the `?`, when there is one.
    pub query: Option<&'a str>,
    pub fragment: Option<&'a str>,
}

pub(crate) fn split_url(raw: &str) -> UrlParts<'_> {
    let (rest, fragment) = match raw.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (raw, None),
    };
    let (scheme, rest) = match rest.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => (String::new(), rest),
    };
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let host = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    UrlParts {
        scheme,
        host,
        path,
        query,
        fragment,
    }
}

/// Percent-encodes the bytes a request line cannot carry (a space, a
/// quote, anything outside ASCII) and leaves the rest, existing escapes
/// included, as written: the form the client puts on the wire.
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

impl UrlParts<'_> {
    /// The path as it is sent: `/` when there is none.
    pub fn wire_path(&self) -> String {
        if self.path.is_empty() {
            "/".to_string()
        } else {
            escape_request_target(self.path)
        }
    }

    /// The path and query as they are sent on the request line.
    pub fn request_uri(&self) -> String {
        match self.query {
            Some(q) => format!("{}?{}", self.wire_path(), escape_request_target(q)),
            None => self.wire_path(),
        }
    }

    /// `scheme://host`, the host in lower case.
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.host.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_parses() {
        assert_eq!(query_escape("a b&c=d/é~"), "a+b%26c%3Dd%2F%C3%A9~");
        assert_eq!(query_unescape("a+b%26").unwrap(), b"a b&");
        assert!(query_unescape("%zz").is_none());
        let q = parse_query("b=2&a=%2Fpath;v=1&&c&d=%4");
        assert_eq!(
            q,
            vec![(b"b".to_vec(), b"2".to_vec()), (b"c".to_vec(), Vec::new())]
        );
        let mut f = Form::new(&[("z", "1"), ("a", "x y")]);
        f.add("a", "2");
        assert_eq!(f.encode(), "a=x+y&a=2&z=1");
    }

    #[test]
    fn splits_urls() {
        let u = split_url("HTTPS://user:pw@Example.com:8443/a b/c?q=a b#frag");
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "Example.com:8443");
        assert_eq!(u.path, "/a b/c");
        assert_eq!(u.query, Some("q=a b"));
        assert_eq!(u.fragment, Some("frag"));
        assert_eq!(u.request_uri(), "/a%20b/c?q=a%20b");
        assert_eq!(u.origin(), "https://example.com:8443");
        let u = split_url("http://h?x=1");
        assert_eq!(
            (u.wire_path().as_str(), u.request_uri().as_str()),
            ("/", "/?x=1")
        );
        assert_eq!(path_unescape("/c%20b%zz"), "/c b%zz");
    }
}
