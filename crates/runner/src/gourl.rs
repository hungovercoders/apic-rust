//! Reading the scheme and the host out of a request URL, as leniently and
//! as strictly as apic always has: the path and the query may hold
//! anything, the host may not.

/// The parts of a URL the runner decides by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UrlParts {
    /// Lower case, empty when the URL has none.
    pub scheme: String,
    /// `host[:port]` as written, an IPv6 address in its brackets; empty
    /// when the URL names no host.
    pub host: String,
}

impl UrlParts {
    /// Splits `raw`. The error is the reason alone, without the URL.
    pub fn parse(raw: &str) -> Result<UrlParts, String> {
        if raw.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err("net/url: invalid control character in URL".to_string());
        }
        let raw = raw.split('#').next().unwrap_or("");
        let (scheme, rest) = split_scheme(raw)?;
        let rest = rest.split('?').next().unwrap_or("");
        if !rest.starts_with('/') {
            if !scheme.is_empty() {
                // An opaque URL such as mailto:x.
                return Ok(UrlParts {
                    scheme,
                    host: String::new(),
                });
            }
            let segment = rest.split('/').next().unwrap_or("");
            if segment.contains(':') {
                return Err("first path segment in URL cannot contain colon".to_string());
            }
        }
        let Some(after) = rest.strip_prefix("//") else {
            return Ok(UrlParts {
                scheme,
                host: String::new(),
            });
        };
        if scheme.is_empty() && after.starts_with('/') {
            return Ok(UrlParts {
                scheme,
                host: String::new(),
            });
        }
        let authority = &after[..after.find('/').unwrap_or(after.len())];
        let host = match authority.rfind('@') {
            Some(i) => &authority[i + 1..],
            None => authority,
        };
        check_host(host)?;
        Ok(UrlParts {
            scheme,
            host: host.to_string(),
        })
    }

    /// The host without its port, and without the brackets of an IPv6
    /// address.
    pub fn hostname(&self) -> &str {
        split_port(&self.host).0
    }

    /// The port as written, empty when there is none.
    pub fn port(&self) -> &str {
        split_port(&self.host).1
    }
}

fn split_port(host: &str) -> (&str, &str) {
    if let Some(rest) = host.strip_prefix('[') {
        return match rest.find(']') {
            Some(i) => (&rest[..i], rest[i + 1..].strip_prefix(':').unwrap_or("")),
            None => (host, ""),
        };
    }
    match host.rfind(':') {
        Some(i) if host[i + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            (&host[..i], &host[i + 1..])
        }
        _ => (host, ""),
    }
}

/// The scheme, in lower case, and what follows its colon; no scheme when
/// the text before the first colon is not one.
fn split_scheme(raw: &str) -> Result<(String, &str), String> {
    for (i, c) in raw.char_indices() {
        match c {
            'a'..='z' | 'A'..='Z' => {}
            '0'..='9' | '+' | '-' | '.' => {
                if i == 0 {
                    return Ok((String::new(), raw));
                }
            }
            ':' => {
                if i == 0 {
                    return Err("missing protocol scheme".to_string());
                }
                return Ok((raw[..i].to_ascii_lowercase(), &raw[i + 1..]));
            }
            _ => return Ok((String::new(), raw)),
        }
    }
    Ok((String::new(), raw))
}

fn check_host(host: &str) -> Result<(), String> {
    let name = if host.starts_with('[') {
        let Some(end) = host.rfind(']') else {
            return Err("missing ']' in host".to_string());
        };
        let port = &host[end + 1..];
        if !valid_optional_port(port) {
            return Err(format!("invalid port {port:?} after host"));
        }
        return Ok(());
    } else if let Some(i) = host.rfind(':') {
        let port = &host[i..];
        if !valid_optional_port(port) {
            return Err(format!("invalid port {port:?} after host"));
        }
        &host[..i]
    } else {
        host
    };
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'%' {
            let hex = |x: u8| x.is_ascii_hexdigit();
            if i + 2 >= b.len() || !hex(b[i + 1]) || !hex(b[i + 2]) {
                let end = (i + 3).min(b.len());
                return Err(format!(
                    "invalid URL escape {:?}",
                    String::from_utf8_lossy(&b[i..end])
                ));
            }
            // In a host an escape may only stand for a byte that is not
            // ASCII (and for the percent sign itself).
            if b[i + 1] < b'8' && &b[i..i + 3] != b"%25" {
                return Err(format!(
                    "invalid URL escape {:?}",
                    String::from_utf8_lossy(&b[i..i + 3])
                ));
            }
            i += 3;
            continue;
        }
        let ok = c >= 0x80
            || c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'-' | b'_'
                    | b'.'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'['
                    | b']'
                    | b'<'
                    | b'>'
                    | b'"'
            );
        if !ok {
            return Err(format!(
                "invalid character {:?} in host name",
                (c as char).to_string()
            ));
        }
        i += 1;
    }
    Ok(())
}

fn valid_optional_port(port: &str) -> bool {
    match port.strip_prefix(':') {
        None => port.is_empty(),
        Some(digits) => digits.bytes().all(|b| b.is_ascii_digit()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        let u = UrlParts::parse("HTTPS://user:pw@API.example.com:8443/a?b=c#d").unwrap();
        assert_eq!(
            (u.scheme.as_str(), u.host.as_str()),
            ("https", "API.example.com:8443")
        );
        assert_eq!((u.hostname(), u.port()), ("API.example.com", "8443"));
        let u = UrlParts::parse("http://[::1]:80/").unwrap();
        assert_eq!((u.hostname(), u.port()), ("::1", "80"));
        assert_eq!(UrlParts::parse("{{baseUrl}}/users").unwrap().host, "");
        assert_eq!(UrlParts::parse("").unwrap(), UrlParts::default());
        assert_eq!(UrlParts::parse("example.com/x").unwrap().host, "");
        assert!(UrlParts::parse("http://{{host}}/x")
            .unwrap_err()
            .contains("in host name"));
        assert!(UrlParts::parse("http://h:x/")
            .unwrap_err()
            .contains("invalid port"));
        assert!(UrlParts::parse("{{a}}:80/x").is_err());
        assert_eq!(
            UrlParts::parse("http://x y/").unwrap_err(),
            "invalid character \" \" in host name"
        );
    }
}
