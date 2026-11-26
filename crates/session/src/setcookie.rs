//! `Set-Cookie` header lines as plain data, read the way browsers and HTTP
//! clients read them (RFC 6265 section 5.2, leniently).

use apic_gocompat::{days_in_month, Time};

/// One cookie a response set, as its `Set-Cookie` line says. Nothing has
/// been defaulted: an empty domain or path means the attribute was absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetCookie {
    pub name: String,
    pub value: String,
    /// The value was written in double quotes.
    pub quoted: bool,
    pub path: String,
    pub domain: String,
    /// The `Expires` attribute, when it held a date that could be read.
    pub expires: Option<Time>,
    /// The `Max-Age` attribute: 0 when absent, negative for "delete this
    /// cookie now" (`Max-Age=0` or less), else the lifetime in seconds.
    pub max_age: i64,
    pub secure: bool,
    pub http_only: bool,
}

/// The most cookies one response may set; past it, none are read.
const MAX_COOKIES: usize = 3000;

fn trim(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\n', '\r'])
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}

pub(crate) fn valid_cookie_value_byte(b: u8) -> bool {
    (0x20..0x7f).contains(&b) && b != b'"' && b != b';' && b != b'\\'
}

fn parse_cookie_value(raw: &str, allow_double_quote: bool) -> Option<(&str, bool)> {
    let b = raw.as_bytes();
    let (raw, quoted) =
        if allow_double_quote && b.len() > 1 && b[0] == b'"' && b[b.len() - 1] == b'"' {
            (&raw[1..raw.len() - 1], true)
        } else {
            (raw, false)
        };
    raw.bytes()
        .all(valid_cookie_value_byte)
        .then_some((raw, quoted))
}

/// Reads one `Set-Cookie` header value. None means the line sets no
/// cookie: it is blank, has no `=`, or its name or value holds a character
/// a cookie cannot. An attribute that cannot be read is skipped.
pub fn parse_set_cookie(line: &str) -> Option<SetCookie> {
    let mut parts = trim(line).split(';');
    let first = trim(parts.next()?);
    let (name, value) = first.split_once('=')?;
    let name = trim(name);
    if !is_token(name) {
        return None;
    }
    let (value, quoted) = parse_cookie_value(value, true)?;
    let mut c = SetCookie {
        name: name.to_string(),
        value: value.to_string(),
        quoted,
        ..SetCookie::default()
    };
    for part in parts {
        let part = trim(part);
        if part.is_empty() {
            continue;
        }
        let (attr, val) = part.split_once('=').unwrap_or((part, ""));
        if !attr.is_ascii() {
            continue;
        }
        let Some((val, _)) = parse_cookie_value(val, false) else {
            continue;
        };
        match attr.to_ascii_lowercase().as_str() {
            "secure" => c.secure = true,
            "httponly" => c.http_only = true,
            "domain" => c.domain = val.to_string(),
            "max-age" => {
                let digits = val.strip_prefix(['+', '-']).unwrap_or(val);
                if digits.is_empty() || !digits.bytes().all(|d| d.is_ascii_digit()) {
                    continue;
                }
                // A number too large to hold is not a lifetime.
                let Ok(secs) = val.parse::<i64>() else {
                    continue;
                };
                if secs != 0 && val.starts_with('0') {
                    continue;
                }
                c.max_age = if secs <= 0 { -1 } else { secs };
            }
            "expires" => c.expires = parse_cookie_time(val).map(Time::utc),
            "path" => c.path = val.to_string(),
            _ => {}
        }
    }
    Some(c)
}

/// Reads every `Set-Cookie` line of a response, in order, passing over the
/// ones that set nothing.
pub fn read_set_cookies<S: AsRef<str>>(lines: &[S]) -> Vec<SetCookie> {
    if lines.len() > MAX_COOKIES {
        return Vec::new();
    }
    lines
        .iter()
        .filter_map(|l| parse_set_cookie(l.as_ref()))
        .collect()
}

const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Reads a cookie date: `Mon, 02 Jan 2006 15:04:05 GMT`, or the same with
/// dashes in the date (`02-Jan-2006`).
pub(crate) fn parse_cookie_time(s: &str) -> Option<Time> {
    parse_date(s, b' ').or_else(|| parse_date(s, b'-'))
}

struct Cursor<'a> {
    b: &'a [u8],
}

impl Cursor<'_> {
    fn lit(&mut self, c: u8) -> Option<()> {
        if self.b.first() == Some(&c) {
            self.b = &self.b[1..];
            Some(())
        } else {
            None
        }
    }

    fn name(&mut self, names: &[&str]) -> Option<u32> {
        let word = std::str::from_utf8(self.b.get(..3)?)
            .ok()?
            .to_ascii_lowercase();
        let i = names.iter().position(|n| *n == word)?;
        self.b = &self.b[3..];
        Some(i as u32)
    }

    /// A number of exactly `n` digits, or of one or two when `n` is 0.
    fn num(&mut self, n: usize) -> Option<u32> {
        let len = if n == 0 {
            let two = self.b.len() >= 2 && self.b[1].is_ascii_digit();
            if two {
                2
            } else {
                1
            }
        } else {
            n
        };
        let digits = self.b.get(..len)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.b = &self.b[len..];
        std::str::from_utf8(digits).ok()?.parse().ok()
    }
}

fn parse_date(s: &str, sep: u8) -> Option<Time> {
    let mut c = Cursor { b: s.as_bytes() };
    c.name(&DAYS)?;
    c.lit(b',')?;
    c.lit(b' ')?;
    let day = c.num(2)?;
    c.lit(sep)?;
    let month = c.name(&MONTHS)? + 1;
    c.lit(sep)?;
    let year = i64::from(c.num(4)?);
    c.lit(b' ')?;
    let hour = c.num(0)?;
    c.lit(b':')?;
    let minute = c.num(2)?;
    c.lit(b':')?;
    let second = c.num(2)?;
    // A fraction of a second is read even though the format has none.
    let mut nanos = 0u32;
    if c.b.len() >= 2 && (c.b[0] == b'.' || c.b[0] == b',') && c.b[1].is_ascii_digit() {
        let n = c.b[1..].iter().take_while(|d| d.is_ascii_digit()).count();
        let mut scale = 100_000_000u32;
        for d in &c.b[1..=n.min(9)] {
            nanos += u32::from(d - b'0') * scale;
            scale /= 10;
        }
        c.b = &c.b[1 + n..];
    }
    c.lit(b' ')?;
    let offset = zone_offset(c.b)?;
    if day < 1 || day > days_in_month(year, month) || hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }
    Some(Time::from_civil(
        year, month, day, hour, minute, second, nanos, offset,
    ))
}

/// The offset of a time-zone abbreviation that makes up the whole of `z`.
/// Only `GMT` with hours (`GMT+2`) has one that is not zero: what an
/// abbreviation such as `PST` means depends on where it is read.
fn zone_offset(z: &[u8]) -> Option<i32> {
    let text = std::str::from_utf8(z).ok()?;
    if text.len() < 3 {
        return None;
    }
    if text == "ChST" || text == "MeST" {
        return Some(0);
    }
    if let Some(rest) = text.strip_prefix("GMT") {
        if rest.is_empty() {
            return Some(0);
        }
        let digits = rest.strip_prefix(['+', '-'])?;
        if digits.is_empty() || !digits.bytes().all(|d| d.is_ascii_digit()) {
            return None;
        }
        let hours: i32 = digits.parse().ok().filter(|h| *h <= 23)?;
        return Some(if rest.starts_with('-') { -hours } else { hours } * 3600);
    }
    if !text.bytes().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    match text.len() {
        3 => Some(0),
        4 if text.ends_with('T') || text == "WITA" => Some(0),
        5 if text.ends_with('T') => Some(0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_set_cookie_lines() {
        let c = parse_set_cookie("sid=abc; Path=/auth; Domain=.Example.com; Max-Age=3600; Secure; HttpOnly; SameSite=Lax").unwrap();
        assert_eq!(
            c,
            SetCookie {
                name: "sid".into(),
                value: "abc".into(),
                path: "/auth".into(),
                domain: ".Example.com".into(),
                max_age: 3600,
                secure: true,
                http_only: true,
                ..SetCookie::default()
            }
        );
        let c = parse_set_cookie(" q =\"a b\" ; expires=Wed, 06 Oct 2027 09:00:00 GMT; max-age=0")
            .unwrap();
        assert!(
            c.name == "q" && c.value == "a b" && c.quoted && c.max_age == -1,
            "{c:?}"
        );
        assert_eq!(
            c.expires.unwrap().format_rfc3339_nano(),
            "2027-10-06T09:00:00Z"
        );
        for (line, age) in [
            ("a=1; Max-Age=-5", -1),
            ("a=1; Max-Age=007", 0),
            ("a=1; Max-Age=x", 0),
            ("a=1; Max-Age=+9", 9),
        ] {
            assert_eq!(parse_set_cookie(line).unwrap().max_age, age, "{line}");
        }
        for bad in ["", "novalue", "bad name=1", "a=x\"y", "=1", "a=b\u{7f}"] {
            assert!(parse_set_cookie(bad).is_none(), "{bad:?}");
        }
        assert_eq!(
            parse_set_cookie("a=1; Path=/x\"y; Secure").unwrap().path,
            ""
        );
        assert_eq!(read_set_cookies(&["a=1", "nope", "b=2"]).len(), 2);
        assert!(read_set_cookies(&vec!["a=1"; 3001]).is_empty());
    }

    #[test]
    fn reads_cookie_dates() {
        let ok = [
            ("Wed, 06 Oct 2027 09:00:00 GMT", "2027-10-06T09:00:00Z"),
            ("Wed, 06-Oct-2027 09:00:00 GMT", "2027-10-06T09:00:00Z"),
            ("wed, 06 oct 2027 9:00:00 UTC", "2027-10-06T09:00:00Z"),
            ("Mon, 06 Oct 2027 09:00:00 GMT+2", "2027-10-06T07:00:00Z"),
            ("Wed, 06 Oct 2027 09:00:00 PST", "2027-10-06T09:00:00Z"),
            ("Thu, 01 Jan 1970 00:00:00.5 GMT", "1970-01-01T00:00:00.5Z"),
        ];
        for (text, want) in ok {
            let t = parse_cookie_time(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(t.utc().format_rfc3339_nano(), want, "{text}");
        }
        for bad in [
            "",
            "Wed, 6 Oct 2027 09:00:00 GMT",
            "Wed, 06 Oct 27 09:00:00 GMT",
            "Wed, 31 Feb 2027 09:00:00 GMT",
            "Wed, 06 Oct 2027 24:00:00 GMT",
            "Wed, 06 Oct 2027 09:00:00",
            "Wed, 06 Oct 2027 09:00:00 gmt",
            "Wed, 06 Oct 2027 09:00:00 GMT x",
            "Wed 06 Oct 2027 09:00:00 GMT",
            "2027-10-06T09:00:00Z",
        ] {
            assert!(parse_cookie_time(bad).is_none(), "{bad:?}");
        }
    }
}
