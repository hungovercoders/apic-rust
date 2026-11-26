use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;

use apic_gocompat::quote;
use regex::Regex;

use crate::json::{self, Node, Type};
use crate::path::parse_path;

/// The headers of a response, in the order they arrived. A name may appear
/// more than once (`Set-Cookie` usually does); names compare without
/// regard to ASCII case.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers(pub Vec<(String, String)>);

impl Headers {
    pub fn new() -> Headers {
        Headers(Vec::new())
    }

    /// Adds a value under `name`, after any it already has.
    pub fn add(&mut self, name: &str, value: &str) {
        self.0.push((name.to_string(), value.to_string()));
    }

    /// Every value of `name`, in order.
    pub fn values(&self, name: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// The first value of `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl<N: Into<String>, V: Into<String>> FromIterator<(N, V)> for Headers {
    fn from_iter<I: IntoIterator<Item = (N, V)>>(iter: I) -> Self {
        Headers(
            iter.into_iter()
                .map(|(n, v)| (n.into(), v.into()))
                .collect(),
        )
    }
}

/// The subset of an HTTP response the selector language sees. It is plain
/// data: whatever sent the request fills it in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    /// The status code, such as 200.
    pub status: i64,
    /// The reason phrase alone, such as `OK`.
    pub status_text: String,
    pub headers: Headers,
    /// The body as received, after any content decoding.
    pub body: Vec<u8>,
    /// The round-trip time.
    pub duration: Duration,
}

/// Why a selector could not be evaluated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A selector that is none of the known forms (as opposed to a known
    /// form whose body path cannot be read).
    Unknown { selector: String },
    /// A known form that cannot be read or evaluated; the message names
    /// what was wrong.
    Invalid(String),
}

impl Error {
    /// Whether the selector was none of the known forms.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Error::Unknown { .. })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unknown { selector } => write!(
                f,
                "unknown selector {} (expected status, statusText, header.<name>, cookie.<name>, body, body.$.<path> or duration)",
                quote(selector)
            ),
            Error::Invalid(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}

fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

/// The JSON type of a selected value. Status, duration and counts are
/// numbers; headers, cookies, statusText and the raw body are strings; a
/// body path has the type of what it selects, and a set of matches is an
/// array.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    String,
    Number,
    Bool,
    Null,
    Array,
    Object,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::String => "string",
            Kind::Number => "number",
            Kind::Bool => "boolean",
            Kind::Null => "null",
            Kind::Array => "array",
            Kind::Object => "object",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A selected value: its text as captures and assertions see it (a string
/// unquoted, anything else as JSON), and its type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Value {
    pub text: String,
    pub kind: Kind,
}

impl Value {
    fn new(text: impl Into<String>, kind: Kind) -> Value {
        Value {
            text: text.into(),
            kind,
        }
    }
}

/// Evaluates `sel` against `resp`. `Ok(None)` means the selector was valid
/// but nothing was there (a missing header or JSON path).
pub fn select(resp: &Response, sel: &str) -> Result<Option<String>, Error> {
    Ok(select_value(resp, sel)?.map(|v| v.text))
}

fn is_body_path(sel: &str) -> bool {
    sel == "body.$" || sel.starts_with("body.$.") || sel.starts_with("body.$[")
}

/// [`select`] with the value's JSON type, for type checks.
pub fn select_value(resp: &Response, sel: &str) -> Result<Option<Value>, Error> {
    let sel = sel.trim();
    match sel {
        "status" => return Ok(Some(Value::new(resp.status.to_string(), Kind::Number))),
        "statusText" => return Ok(Some(Value::new(resp.status_text.clone(), Kind::String))),
        "duration" => {
            return Ok(Some(Value::new(
                resp.duration.as_millis().to_string(),
                Kind::Number,
            )))
        }
        "body" => {
            return Ok(Some(Value::new(
                String::from_utf8_lossy(&resp.body),
                Kind::String,
            )))
        }
        _ => {}
    }
    if sel.starts_with("header.") || sel.starts_with("headers.") {
        let h = parse_header(sel)?;
        let v = resp.headers.values(&h.name);
        if h.count {
            return Ok(Some(Value::new(v.len().to_string(), Kind::Number)));
        }
        if let Some(i) = h.index {
            let i = if i < 0 { i + v.len() as i64 } else { i };
            if i < 0 || i >= v.len() as i64 {
                return Ok(None);
            }
            return Ok(Some(Value::new(v[i as usize], Kind::String)));
        }
        return Ok(v.first().map(|first| Value::new(*first, Kind::String)));
    }
    if let Some(name) = sel.strip_prefix("cookie.") {
        if name.is_empty() {
            return Err(invalid("cookie selector needs a name"));
        }
        return Ok(response_cookies(&resp.headers)
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, value)| Value::new(value, Kind::String)));
    }
    if is_body_path(sel) {
        let path = parse_path(&sel["body.$".len()..]).map_err(Error::Invalid)?;
        if !json::valid(&resp.body) {
            return Err(invalid("body is not valid JSON"));
        }
        let text = String::from_utf8_lossy(&resp.body);
        let Some(doc) = json::parse(&text) else {
            return Ok(None);
        };
        let found = path.eval(doc);
        if let Some(n) = found.count {
            return Ok(Some(Value::new(n.to_string(), Kind::Number)));
        }
        if found.nodes.is_empty() {
            return Ok(None);
        }
        if found.many {
            let raws: Vec<&str> = found.nodes.iter().map(|n| n.raw.as_ref()).collect();
            return Ok(Some(Value::new(
                format!("[{}]", raws.join(",")),
                Kind::Array,
            )));
        }
        return Ok(Some(value_of(&found.nodes[0])));
    }
    Err(Error::Unknown {
        selector: sel.to_string(),
    })
}

/// Parses a selector without a response, so validation reports a bad one
/// before anything is sent. The error names what could not be read.
pub fn check(sel: &str) -> Result<(), Error> {
    let sel = sel.trim();
    if matches!(sel, "status" | "statusText" | "duration" | "body") {
        return Ok(());
    }
    if sel.starts_with("header.") || sel.starts_with("headers.") {
        return parse_header(sel).map(|_| ());
    }
    if sel.starts_with("cookie.") {
        if sel == "cookie." {
            return Err(invalid("cookie selector needs a name"));
        }
        return Ok(());
    }
    if is_body_path(sel) {
        return parse_path(&sel["body.$".len()..])
            .map(|_| ())
            .map_err(Error::Invalid);
    }
    Err(Error::Unknown {
        selector: sel.to_string(),
    })
}

fn value_of(r: &Node<'_>) -> Value {
    match r.kind {
        Type::String => Value::new(r.str.as_ref(), Kind::String),
        Type::Number => Value::new(r.raw.as_ref(), Kind::Number),
        Type::True | Type::False => Value::new(r.raw.as_ref(), Kind::Bool),
        Type::Null => Value::new(r.raw.as_ref(), Kind::Null),
        Type::Json if r.is_array() => Value::new(r.raw.as_ref(), Kind::Array),
        Type::Json => Value::new(r.raw.as_ref(), Kind::Object),
    }
}

struct HeaderSel {
    name: String,
    count: bool,
    index: Option<i64>,
}

static RE_HEADER_INDEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+)\[(-?[0-9]+)\]$").unwrap());

/// Reads `header.<name>`, `header.<name>.#` (how many values) and
/// `header.<name>[n]` (the n-th, negative from the end). A header name may
/// itself contain dots, so only those two suffixes are special.
fn parse_header(sel: &str) -> Result<HeaderSel, Error> {
    let name = &sel[sel.find('.').map_or(0, |i| i + 1)..];
    let mut h = HeaderSel {
        name: name.to_string(),
        count: false,
        index: None,
    };
    if let Some(stripped) = name.strip_suffix(".#") {
        h.name = stripped.to_string();
        h.count = true;
    } else if let Some(m) = RE_HEADER_INDEX.captures(name) {
        h.name = m[1].to_string();
        // An index too large to read is out of range either way.
        let far = if m[2].starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        };
        h.index = Some(m[2].parse().unwrap_or(far));
    }
    if h.name.is_empty() {
        return Err(invalid("header selector needs a name"));
    }
    if h.name.contains(['[', ']']) {
        return Err(invalid(format!(
            "unsupported header selector {}: use header.<name>, header.<name>.# or header.<name>[n]",
            quote(sel)
        )));
    }
    Ok(h)
}

/// The most `Set-Cookie` headers of one response that are read; past it
/// the response is taken to have set none.
const MAX_COOKIES: usize = 3000;

fn is_token_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)
}

fn trim_ows(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\n', '\r'])
}

/// The name and value of each cookie the response set, in order. A
/// `Set-Cookie` line whose name is not a token, or whose value holds a
/// character a cookie value cannot, is passed over.
fn response_cookies(headers: &Headers) -> Vec<(String, String)> {
    let lines = headers.values("Set-Cookie");
    if lines.len() > MAX_COOKIES {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lines {
        let first = trim_ows(trim_ows(line).split(';').next().unwrap_or(""));
        let Some((name, value)) = first.split_once('=') else {
            continue;
        };
        let name = trim_ows(name);
        if name.is_empty() || !name.bytes().all(is_token_byte) {
            continue;
        }
        let vb = value.as_bytes();
        let value = if vb.len() > 1 && vb[0] == b'"' && vb[vb.len() - 1] == b'"' {
            &value[1..value.len() - 1]
        } else {
            value
        };
        if !value
            .bytes()
            .all(|b| (0x20..0x7f).contains(&b) && b != b'"' && b != b';' && b != b'\\')
        {
            continue;
        }
        out.push((name.to_string(), value.to_string()));
    }
    out
}
