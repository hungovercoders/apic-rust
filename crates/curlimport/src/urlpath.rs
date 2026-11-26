//! Reading the path out of a URL, leniently: a relative reference, a bare
//! path and an opaque `scheme:rest` are all accepted, and the checks that
//! make a URL unreadable (a control character, a bad percent escape, a
//! host or port that cannot be one) are the ones the names and titles of
//! imported requests were first written against.

use std::net::Ipv6Addr;

/// Which part of a URL is being unescaped: the host and an IPv6 zone
/// restrict what may appear, the other parts only need well-formed escapes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Host,
    Zone,
    Other,
}

/// Returns the unescaped path of `raw`, or `None` when `raw` is not a URL
/// that can be read. The path is bytes: an escape may stand for any byte.
pub(crate) fn url_path(raw: &str) -> Option<Vec<u8>> {
    let (u, frag) = match raw.split_once('#') {
        Some((u, frag)) => (u, frag),
        None => (raw, ""),
    };
    let path = parse(u)?;
    if !frag.is_empty() {
        unescape(frag.as_bytes(), Mode::Other)?;
    }
    Some(path)
}

fn parse(raw: &str) -> Option<Vec<u8>> {
    if raw.bytes().any(|b| b < b' ' || b == 0x7f) {
        return None;
    }
    if raw == "*" {
        return Some(b"*".to_vec());
    }
    let (scheme, rest) = scheme(raw)?;
    let scheme = scheme.to_ascii_lowercase();

    let rest = if rest.ends_with('?') && rest.matches('?').count() == 1 {
        &rest[..rest.len() - 1]
    } else {
        rest.split_once('?').map_or(rest, |(before, _)| before)
    };

    if !rest.starts_with('/') {
        if !scheme.is_empty() {
            // A rootless path after a scheme is opaque: there is no path.
            return Some(Vec::new());
        }
        // A relative reference whose first segment has a colon would be
        // confused with a malformed scheme.
        let segment = rest.split_once('/').map_or(rest, |(segment, _)| segment);
        if segment.contains(':') {
            return None;
        }
    }

    let mut path = rest;
    if (!scheme.is_empty() || !rest.starts_with("///")) && rest.starts_with("//") {
        let authority = &rest[2..];
        let (authority, after) = match authority.find('/') {
            Some(i) => (&authority[..i], &authority[i..]),
            None => (authority, ""),
        };
        check_authority(&scheme, authority)?;
        path = after;
    }
    unescape(path.as_bytes(), Mode::Other)
}

/// Splits `scheme:rest`, when `raw` starts with a scheme
/// (`[a-zA-Z][a-zA-Z0-9+.-]*`); otherwise the scheme is empty and the rest
/// is all of `raw`. `None` when the URL starts with a colon.
fn scheme(raw: &str) -> Option<(&str, &str)> {
    for (i, c) in raw.bytes().enumerate() {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'0'..=b'9' | b'+' | b'-' | b'.' => {
                if i == 0 {
                    return Some(("", raw));
                }
            }
            b':' => {
                if i == 0 {
                    return None;
                }
                return Some((&raw[..i], &raw[i + 1..]));
            }
            _ => return Some(("", raw)),
        }
    }
    Some(("", raw))
}

/// Checks `[userinfo@]host[:port]`.
fn check_authority(scheme: &str, authority: &str) -> Option<()> {
    let Some(i) = authority.rfind('@') else {
        return check_host(scheme, authority);
    };
    check_host(scheme, &authority[i + 1..])?;
    let userinfo = &authority[..i];
    if !valid_userinfo(userinfo) {
        return None;
    }
    match userinfo.split_once(':') {
        None => {
            unescape(userinfo.as_bytes(), Mode::Other)?;
        }
        Some((username, password)) => {
            unescape(username.as_bytes(), Mode::Other)?;
            unescape(password.as_bytes(), Mode::Other)?;
        }
    }
    Some(())
}

/// Checks `host[:port]`: a bracketed host must be an IPv6 address, a port
/// must be digits, and the host may only hold the bytes a host can.
fn check_host(scheme: &str, host: &str) -> Option<()> {
    match host.rfind('[') {
        Some(0) => {
            let close = host.rfind(']')?;
            let colon_port = &host[close + 1..];
            if !valid_optional_port(colon_port) {
                return None;
            }
            // `%25` introduces the zone identifier, which may use more
            // escapes than the address itself.
            let hostname = &host[1..close];
            let unescaped = match hostname.find("%25") {
                Some(zone) => {
                    let mut address = unescape(&hostname.as_bytes()[..zone], Mode::Host)?;
                    address.extend(unescape(&hostname.as_bytes()[zone..], Mode::Zone)?);
                    address
                }
                None => unescape(hostname.as_bytes(), Mode::Host)?,
            };
            // Only an IPv6 address may be enclosed by square brackets.
            if !is_ipv6(&unescaped) {
                return None;
            }
            return Some(());
        }
        Some(_) => return None,
        None => {}
    }
    if let Some(first) = host.find(':') {
        // An http or https host has no colon but the one before its port;
        // other schemes historically allowed more (a list of hosts), so
        // only the last colon counts there.
        let i = if scheme == "http" || scheme == "https" {
            first
        } else {
            host.rfind(':').unwrap_or(first)
        };
        if !valid_optional_port(&host[i..]) {
            return None;
        }
    }
    unescape(host.as_bytes(), Mode::Host)?;
    Some(())
}

/// Reports whether `s` is an IPv6 address, with an optional non-empty
/// `%zone`.
fn is_ipv6(s: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(s) else {
        return false;
    };
    let address = match s.split_once('%') {
        Some((_, "")) => return false,
        Some((address, _)) => address,
        None => s,
    };
    address.parse::<Ipv6Addr>().is_ok()
}

/// Reports whether `port` is empty or a colon followed by digits.
fn valid_optional_port(port: &str) -> bool {
    match port.strip_prefix(':') {
        Some(digits) => digits.bytes().all(|b| b.is_ascii_digit()),
        None => port.is_empty(),
    }
}

/// Reports whether `s` holds only the characters userinfo may: unreserved,
/// sub-delims, `:`, `%` and `@`. The escapes themselves are checked by
/// [`unescape`].
fn valid_userinfo(s: &str) -> bool {
    s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '-' | '.'
                    | '_'
                    | ':'
                    | '~'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
                    | '%'
                    | '@'
            )
    })
}

/// Reports whether `c` cannot appear as itself in a host.
fn host_must_escape(c: u8) -> bool {
    !(c.is_ascii_alphanumeric() || b"!$&'()*+,;=:[]<>\"-_.~".contains(&c))
}

fn unhex(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => c - b'A' + 10,
    }
}

/// Decodes the percent escapes of `s`, or returns `None` when one is
/// malformed or `s` holds something its part of the URL cannot.
fn unescape(s: &[u8], mode: Mode) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c != b'%' {
            if mode != Mode::Other && c < 0x80 && host_must_escape(c) {
                return None;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if i + 2 >= s.len() || !s[i + 1].is_ascii_hexdigit() || !s[i + 2].is_ascii_hexdigit() {
            return None;
        }
        let escape = &s[i..i + 3];
        let v = unhex(s[i + 1]) << 4 | unhex(s[i + 2]);
        match mode {
            // In a host, escapes are only for non-ASCII bytes, except the
            // `%25` that introduces a zone.
            Mode::Host if unhex(s[i + 1]) < 8 && escape != b"%25" => return None,
            // In a zone, an escape may not introduce a byte that could not
            // be written directly, bar the space.
            Mode::Zone if escape != b"%25" && v != b' ' && host_must_escape(v) => return None,
            _ => {}
        }
        out.push(v);
        i += 3;
    }
    Some(out)
}
