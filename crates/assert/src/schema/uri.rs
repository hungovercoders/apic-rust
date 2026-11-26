//! URI references as `$id` and `$ref` use them: parsing, resolving one
//! against a base (RFC 3986 section 5.2) and writing the result back.

use apic_gocompat::quote;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Uri {
    pub scheme: String,
    /// What follows the scheme of a URI such as `urn:example:a`, which has
    /// no path to resolve against.
    pub opaque: String,
    pub host: String,
    pub path: String,
    pub query: Option<String>,
    /// The fragment with its percent-escapes resolved.
    pub fragment: String,
}

fn unescape(s: &str, whole: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
            match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => out.push(v),
                None => {
                    let end = (i + 3).min(b.len());
                    return Err(format!(
                        "parse {}: invalid URL escape {}",
                        quote(whole),
                        quote(&String::from_utf8_lossy(&b[i..end]))
                    ));
                }
            }
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

impl Uri {
    pub fn parse(raw: &str) -> Result<Uri, String> {
        let mut u = Uri::default();
        let (rest, frag) = match raw.split_once('#') {
            Some((rest, frag)) => (rest, Some(frag)),
            None => (raw, None),
        };
        if let Some(frag) = frag {
            u.fragment = unescape(frag, raw)?;
        }
        let mut rest = rest;
        // A scheme is a letter followed by letters, digits, +, - or . up to
        // the first colon.
        if let Some(colon) = rest.find(':') {
            let scheme = &rest[..colon];
            let ok = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
            if ok {
                u.scheme = scheme.to_ascii_lowercase();
                rest = &rest[colon + 1..];
            } else if colon == 0 {
                return Err(format!("parse {}: missing protocol scheme", quote(raw)));
            }
        }
        if let Some((before, query)) = rest.split_once('?') {
            u.query = Some(query.to_string());
            rest = before;
        }
        if !rest.starts_with('/') {
            if !u.scheme.is_empty() {
                u.opaque = rest.to_string();
                return Ok(u);
            }
            // A colon in the first segment would read as a scheme.
            let first = rest.split('/').next().unwrap_or("");
            if first.contains(':') {
                return Err(format!(
                    "parse {}: first path segment in URL cannot contain colon",
                    quote(raw)
                ));
            }
        }
        if let Some(authority) = rest.strip_prefix("//") {
            let end = authority.find('/').unwrap_or(authority.len());
            u.host = authority[..end].to_string();
            rest = &authority[end..];
        }
        unescape(rest, raw)?;
        u.path = rest.to_string();
        Ok(u)
    }

    pub fn is_abs(&self) -> bool {
        !self.scheme.is_empty()
    }

    /// This URI without its fragment.
    pub fn without_fragment(&self) -> Uri {
        Uri { fragment: String::new(), ..self.clone() }
    }

    /// Resolves `reference` against this URI.
    pub fn resolve(&self, reference: &Uri) -> Uri {
        let mut url = reference.clone();
        if reference.scheme.is_empty() {
            url.scheme = self.scheme.clone();
        }
        if !reference.scheme.is_empty() || !reference.host.is_empty() {
            url.path = resolve_path(&reference.path, "");
            return url;
        }
        if !reference.opaque.is_empty() {
            url.host = String::new();
            url.path = String::new();
            return url;
        }
        if reference.path.is_empty() && reference.query.is_none() {
            url.query = self.query.clone();
            if reference.fragment.is_empty() {
                url.fragment = self.fragment.clone();
            }
        }
        if reference.path.is_empty() && !self.opaque.is_empty() {
            url.opaque = self.opaque.clone();
            url.host = String::new();
            url.path = String::new();
            return url;
        }
        url.host = self.host.clone();
        url.path = resolve_path(&self.path, &reference.path);
        url
    }
}

impl std::fmt::Display for Uri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        if !self.scheme.is_empty() {
            out.push_str(&self.scheme);
            out.push(':');
        }
        if !self.opaque.is_empty() {
            out.push_str(&self.opaque);
        } else {
            if (!self.scheme.is_empty() || !self.host.is_empty()) && (!self.host.is_empty() || !self.path.is_empty()) {
                out.push_str("//");
                out.push_str(&self.host);
            }
            if !self.path.is_empty() && !self.path.starts_with('/') && !self.host.is_empty() {
                out.push('/');
            }
            if out.is_empty() && self.path.split('/').next().is_some_and(|s| s.contains(':')) {
                out.push_str("./");
            }
            out.push_str(&self.path);
        }
        if let Some(q) = &self.query {
            if !q.is_empty() {
                out.push('?');
                out.push_str(q);
            }
        }
        if !self.fragment.is_empty() {
            out.push('#');
            out.push_str(&self.fragment);
        }
        f.write_str(&out)
    }
}

/// Applies a reference path to a base path and removes the `.` and `..`
/// segments of the result.
fn resolve_path(base: &str, reference: &str) -> String {
    let full = if reference.is_empty() {
        base.to_string()
    } else if !reference.starts_with('/') {
        let i = base.rfind('/').map_or(0, |i| i + 1);
        format!("{}{}", &base[..i], reference)
    } else {
        reference.to_string()
    };
    if full.is_empty() {
        return String::new();
    }
    let mut dst = String::from("/");
    let mut first = true;
    let mut last = "";
    for elem in full.split('/') {
        last = elem;
        if elem == "." {
            first = false;
            continue;
        }
        if elem == ".." {
            let str = dst[1..].to_string();
            dst = String::from("/");
            match str.rfind('/') {
                None => first = true,
                Some(index) => dst.push_str(&str[..index]),
            }
        } else {
            if !first {
                dst.push('/');
            }
            dst.push_str(elem);
            first = false;
        }
    }
    if last == "." || last == ".." {
        dst.push('/');
    }
    if dst.len() > 1 && dst.as_bytes()[1] == b'/' {
        dst.remove(0);
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(base: &str, reference: &str) -> String {
        Uri::parse(base).unwrap().resolve(&Uri::parse(reference).unwrap()).to_string()
    }

    #[test]
    fn resolves_references() {
        assert_eq!(resolved("", "other.json"), "/other.json");
        assert_eq!(resolved("", "#/a/b"), "#/a/b");
        assert_eq!(resolved("http://x/y", "z"), "http://x/z");
        assert_eq!(resolved("http://x/a/b/c", "../d#f"), "http://x/a/d#f");
        assert_eq!(resolved("http://x/a/b", "/c?q=1"), "http://x/c?q=1");
        assert_eq!(resolved("http://x/a/b", "https://y/c/./d"), "https://y/c/d");
        assert_eq!(resolved("http://x/a/b?q", "#frag"), "http://x/a/b?q#frag");
        assert_eq!(resolved("http://x/a/b", "//h/p"), "http://h/p");
        assert_eq!(resolved("urn:example:root", "#a"), "urn:example:root#a");
        assert_eq!(resolved("http://x/a/", "urn:example:b"), "urn:example:b");
        assert_eq!(Uri::parse("#a%2Fb").unwrap().fragment, "a/b");
        assert_eq!(Uri::parse("%zz").unwrap_err(), "parse \"%zz\": invalid URL escape \"%zz\"");
        assert!(Uri::parse("http://x/y").unwrap().is_abs() && !Uri::parse("x/y").unwrap().is_abs());
        assert_eq!(Uri::parse("HTTP://x").unwrap().to_string(), "http://x");
    }
}
