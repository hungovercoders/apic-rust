//! The cookie rules of RFC 6265 for one set of cookies: which a response
//! may set, and which go with a request. There is no public-suffix list:
//! a project that talks to one API has no need for one.

use std::net::IpAddr;

use apic_gocompat::Time;

use crate::setcookie::SetCookie;

/// The parts of a request URL the cookie rules look at. It is plain data:
/// whatever sends the request fills it in from its own URL type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestUrl {
    /// `http` or `https`; cookies are neither set nor sent for others.
    pub scheme: String,
    /// The host, with the port when the URL has one: `api.example.com`,
    /// `localhost:8080`, `[::1]:8080`.
    pub host: String,
    /// The path, percent-escapes resolved; empty counts as `/`.
    pub path: String,
}

impl RequestUrl {
    /// Splits an absolute URL into the parts the cookie rules need. This
    /// is a convenience for callers without a URL type of their own: it
    /// drops any user information, query and fragment, and resolves the
    /// percent-escapes of the path.
    pub fn parse(url: &str) -> Option<RequestUrl> {
        let (scheme, rest) = url.split_once("://")?;
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let host = authority.rsplit('@').next().unwrap_or("");
        Some(RequestUrl {
            scheme: scheme.to_ascii_lowercase(),
            host: host.to_string(),
            path: percent_decode(path),
        })
    }

    /// The host without a port, and without the brackets of an IPv6
    /// address.
    pub fn hostname(&self) -> &str {
        let host = self.host.as_str();
        let Some(colon) = host.find(':') else {
            return host;
        };
        if let Some(i) = host.find(']') {
            return host[..i].strip_prefix('[').unwrap_or(&host[..i]);
        }
        &host[..colon]
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%')
            .then(|| b.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A cookie to send with a request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestCookie {
    pub name: String,
    pub value: String,
    /// The value was set in double quotes, and is sent in them.
    pub quoted: bool,
}

#[derive(Clone, Debug)]
struct Entry {
    /// The registrable-domain bucket the cookie was stored under.
    key: String,
    name: String,
    value: String,
    quoted: bool,
    domain: String,
    path: String,
    secure: bool,
    persistent: bool,
    host_only: bool,
    expires: Option<Time>,
    creation: Time,
    seq: u64,
}

impl Entry {
    fn same_cookie(&self, other: &Entry) -> bool {
        self.key == other.key
            && self.domain == other.domain
            && self.path == other.path
            && self.name == other.name
    }

    fn should_send(&self, https: bool, host: &str, path: &str) -> bool {
        self.domain_match(host) && self.path_match(path) && self.secure_match(https)
    }

    /// RFC 6265 section 5.1.3.
    fn domain_match(&self, host: &str) -> bool {
        self.domain == host || !self.host_only && has_dot_suffix(host, &self.domain)
    }

    /// RFC 6265 section 5.1.4.
    fn path_match(&self, request_path: &str) -> bool {
        if request_path == self.path {
            return true;
        }
        if let Some(rest) = request_path.strip_prefix(self.path.as_str()) {
            // "/any/" matches "/any/path", and so does "/any".
            return self.path.ends_with('/') || rest.starts_with('/');
        }
        false
    }

    /// A secure cookie goes over HTTPS, or to this machine.
    fn secure_match(&self, https: bool) -> bool {
        if !self.secure || https || is_localhost(&self.domain) {
            return true;
        }
        self.domain
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    }
}

fn is_localhost(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    let last = host.rsplit('.').next().unwrap_or(host);
    last.eq_ignore_ascii_case("localhost")
}

/// Reports whether `s` ends in "." + `suffix`.
fn has_dot_suffix(s: &str, suffix: &str) -> bool {
    s.len() > suffix.len()
        && s.as_bytes()[s.len() - suffix.len() - 1] == b'.'
        && s.ends_with(suffix)
}

/// The cookies of one environment.
#[derive(Debug, Default)]
pub(crate) struct CookieJar {
    entries: Vec<Entry>,
    next_seq: u64,
}

impl CookieJar {
    /// The cookies to send with a request to `u`, ordered as RFC 6265
    /// section 5.4 has them.
    pub fn cookies(&mut self, u: &RequestUrl, now: Time) -> Vec<RequestCookie> {
        if u.scheme != "http" && u.scheme != "https" {
            return Vec::new();
        }
        let Some(host) = canonical_host(&u.host) else {
            return Vec::new();
        };
        let key = jar_key(&host);
        let https = u.scheme == "https";
        let path = if u.path.is_empty() {
            "/"
        } else {
            u.path.as_str()
        };
        // Whatever has expired goes first.
        self.entries.retain(|e| {
            !(e.key == key && e.persistent && !e.expires.is_some_and(|t| t.after(now)))
        });
        let mut selected: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.key == key && e.should_send(https, &host, path))
            .collect();
        selected.sort_by(|a, b| {
            b.path
                .cmp(&a.path)
                .then(a.creation.cmp(&b.creation))
                .then(a.seq.cmp(&b.seq))
        });
        selected
            .into_iter()
            .map(|e| RequestCookie {
                name: e.name.clone(),
                value: e.value.clone(),
                quoted: e.quoted,
            })
            .collect()
    }

    /// Handles the cookies a response to a request for `u` set.
    pub fn set_cookies(&mut self, u: &RequestUrl, cookies: &[SetCookie], now: Time) {
        if cookies.is_empty() || (u.scheme != "http" && u.scheme != "https") {
            return;
        }
        let Some(host) = canonical_host(&u.host) else {
            return;
        };
        let key = jar_key(&host);
        let def_path = default_path(&u.path);
        for cookie in cookies {
            let Some((mut e, remove)) = new_entry(cookie, now, &def_path, &host, &key) else {
                continue;
            };
            let existing = self.entries.iter().position(|old| old.same_cookie(&e));
            if remove {
                if let Some(i) = existing {
                    self.entries.remove(i);
                }
                continue;
            }
            match existing {
                Some(i) => {
                    e.creation = self.entries[i].creation;
                    e.seq = self.entries[i].seq;
                    self.entries[i] = e;
                }
                None => {
                    e.creation = now;
                    e.seq = self.next_seq;
                    self.next_seq += 1;
                    self.entries.push(e);
                }
            }
        }
    }
}

/// Builds the entry for a cookie a response from `host` set. The flag says
/// the cookie is to be deleted; None is a cookie the host may not set.
fn new_entry(
    c: &SetCookie,
    now: Time,
    def_path: &str,
    host: &str,
    key: &str,
) -> Option<(Entry, bool)> {
    let path = if c.path.is_empty() || !c.path.starts_with('/') {
        def_path.to_string()
    } else {
        c.path.clone()
    };
    let (domain, host_only) = domain_and_type(host, &c.domain)?;
    let mut e = Entry {
        key: key.to_string(),
        name: c.name.clone(),
        value: String::new(),
        quoted: false,
        domain,
        path,
        secure: false,
        persistent: false,
        host_only,
        expires: None,
        creation: now,
        seq: 0,
    };
    // Max-Age takes precedence over Expires.
    if c.max_age < 0 {
        return Some((e, true));
    } else if c.max_age > 0 {
        e.expires = Some(now.add_nanos(i128::from(c.max_age) * 1_000_000_000));
        e.persistent = true;
    } else if let Some(expires) = c.expires {
        if !expires.after(now) {
            return Some((e, true));
        }
        e.expires = Some(expires);
        e.persistent = true;
    }
    e.value = c.value.clone();
    e.quoted = c.quoted;
    e.secure = c.secure;
    Some((e, false))
}

/// The domain a cookie is stored under and whether it is host-only, or
/// None when the host may not set a cookie for the domain attribute.
fn domain_and_type(host: &str, domain: &str) -> Option<(String, bool)> {
    if domain.is_empty() {
        // No domain attribute: a host cookie.
        return Some((host.to_string(), true));
    }
    if is_ip(host) {
        // An IP address has no domain cookies: an attribute naming the
        // address itself makes a host cookie, anything else is refused.
        return (host == domain).then(|| (host.to_string(), true));
    }
    // One leading dot is allowed and means nothing.
    let domain = domain.strip_prefix('.').unwrap_or(domain);
    if domain.is_empty() || domain.starts_with('.') || !domain.is_ascii() {
        return None;
    }
    let domain = domain.to_ascii_lowercase();
    if domain.ends_with('.') {
        return None;
    }
    // The domain must domain-match the host: www.mycompany.com cannot set
    // cookies for .ourcompetitors.com.
    if host != domain && !has_dot_suffix(host, &domain) {
        return None;
    }
    Some((domain, false))
}

/// The host of a URL as cookies know it: no port, no trailing dot, in
/// lower case, and in its ASCII form when it has letters outside ASCII.
fn canonical_host(host: &str) -> Option<String> {
    let mut host = host;
    if has_port(host) {
        host = split_host_port(host)?;
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    Some(to_ascii(host)?.to_ascii_lowercase())
}

/// Reports whether `host` has a port. A bare IPv6 address has colons but
/// no port.
fn has_port(host: &str) -> bool {
    match host.matches(':').count() {
        0 => false,
        1 => true,
        _ => host.starts_with('[') && host.contains("]:"),
    }
}

fn split_host_port(hostport: &str) -> Option<&str> {
    let colon = hostport.rfind(':')?;
    if let Some(rest) = hostport.strip_prefix('[') {
        let end = rest.find(']')?;
        // The port follows the bracket directly.
        if end + 2 != colon {
            return None;
        }
        return Some(&rest[..end]);
    }
    let host = &hostport[..colon];
    if host.contains([':', '[', ']']) || hostport[colon + 1..].contains([':', '[', ']']) {
        return None;
    }
    Some(host)
}

/// The key cookies of a host are stored under: the host itself for an IP
/// address or a single label, else its last two labels.
fn jar_key(host: &str) -> String {
    if is_ip(host) {
        return host.to_string();
    }
    let i = match host.rfind('.') {
        Some(i) if i > 0 => i,
        _ => return host.to_string(),
    };
    match host[..i - 1].rfind('.') {
        Some(prev) => host[prev + 1..].to_string(),
        None => host.to_string(),
    }
}

fn is_ip(host: &str) -> bool {
    host.contains([':', '%']) || host.parse::<IpAddr>().is_ok()
}

/// The default-path of a request path (RFC 6265 section 5.1.4), which a
/// cookie without a Path attribute takes.
pub(crate) fn default_path(path: &str) -> String {
    if !path.starts_with('/') {
        return "/".to_string(); // empty or malformed
    }
    match path.rfind('/') {
        Some(0) | None => "/".to_string(), // "/abc"
        Some(i) => path[..i].to_string(),  // "/abc/xyz" or "/abc/xyz/"
    }
}

/// Converts a host with letters outside ASCII to its ASCII form, label by
/// label (RFC 3492).
fn to_ascii(s: &str) -> Option<String> {
    if s.is_ascii() {
        return Some(s.to_string());
    }
    let labels: Option<Vec<String>> = s
        .split('.')
        .map(|label| {
            if label.is_ascii() {
                Some(label.to_string())
            } else {
                punycode(label)
            }
        })
        .collect();
    Some(labels?.join("."))
}

fn punycode(s: &str) -> Option<String> {
    const BASE: i32 = 36;
    const DAMP: i32 = 700;
    const INITIAL_BIAS: i32 = 72;
    const INITIAL_N: i32 = 128;
    const SKEW: i32 = 38;
    const TMAX: i32 = 26;
    const TMIN: i32 = 1;
    fn digit(d: i32) -> char {
        if d < 26 {
            char::from(b'a' + d as u8)
        } else {
            char::from(b'0' + (d - 26) as u8)
        }
    }
    fn adapt(mut delta: i32, num_points: i32, first_time: bool) -> i32 {
        delta /= if first_time { DAMP } else { 2 };
        delta += delta / num_points;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + (BASE - TMIN + 1) * delta / (delta + SKEW)
    }
    let mut output = String::from("xn--");
    let (mut delta, mut n, mut bias) = (0i32, INITIAL_N, INITIAL_BIAS);
    let (mut b, mut remaining) = (0i32, 0i32);
    for r in s.chars() {
        if r.is_ascii() {
            b += 1;
            output.push(r);
        } else {
            remaining += 1;
        }
    }
    let mut h = b;
    if b > 0 {
        output.push('-');
    }
    while remaining != 0 {
        let m = s.chars().map(|r| r as i32).filter(|&r| r >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(h + 1)?)?;
        n = m;
        for r in s.chars().map(|r| r as i32) {
            if r < n {
                delta = delta.checked_add(1)?;
                continue;
            }
            if r > n {
                continue;
            }
            let mut q = delta;
            let mut k = BASE;
            loop {
                let t = (k - bias).clamp(TMIN, TMAX);
                if q < t {
                    break;
                }
                output.push(digit(t + (q - t) % (BASE - t)));
                q = (q - t) / (BASE - t);
                k += BASE;
            }
            output.push(digit(q));
            bias = adapt(delta, h + 1, h == b);
            delta = 0;
            h += 1;
            remaining -= 1;
        }
        delta += 1;
        n += 1;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> RequestUrl {
        RequestUrl::parse(s).unwrap()
    }

    #[test]
    fn splits_urls() {
        let u = url("HTTPS://user:pw@Api.Example.com:8443/a%20b/c?q=1#f");
        assert_eq!(
            (u.scheme.as_str(), u.host.as_str(), u.path.as_str()),
            ("https", "Api.Example.com:8443", "/a b/c")
        );
        assert_eq!(u.hostname(), "Api.Example.com");
        assert_eq!(url("http://[::1]:8080/").hostname(), "::1");
        assert_eq!(url("http://x").path, "");
        assert!(RequestUrl::parse("not a url").is_none());
    }

    #[test]
    fn hosts_and_keys() {
        assert_eq!(
            canonical_host("WWW.Example.COM.:8080").as_deref(),
            Some("www.example.com")
        );
        assert_eq!(canonical_host("[::1]:80").as_deref(), Some("::1"));
        assert_eq!(canonical_host("::1").as_deref(), Some("::1"));
        assert_eq!(
            canonical_host("bücher.example").as_deref(),
            Some("xn--bcher-kva.example")
        );
        assert_eq!(canonical_host("a:b:c"), Some("a:b:c".to_string()));
        assert_eq!(jar_key("www.api.example.com"), "example.com");
        assert_eq!(jar_key("example.com"), "example.com");
        assert_eq!(jar_key("localhost"), "localhost");
        assert_eq!(jar_key("127.0.0.1"), "127.0.0.1");
        assert_eq!(default_path(""), "/");
        assert_eq!(default_path("/a"), "/");
        assert_eq!(default_path("/a/b"), "/a");
        assert_eq!(default_path("/a/b/"), "/a/b");
        assert_eq!(default_path("x"), "/");
    }
}
