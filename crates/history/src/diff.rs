use std::collections::BTreeMap;

use apic_gocompat::{check_json, json_string};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;

/// One difference between two recorded responses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// "status"; a JSONPath into a JSON body, such as `$.user.roles[2]`;
    /// "line N" for a text body; or "body" when the bodies cannot be
    /// compared piece by piece (binary, or too long to diff).
    pub path: String,
    /// "added", "removed" or "changed".
    pub op: &'static str,
    /// The values as JSON text: a JSON body's values as they are, a text
    /// line as a string. One of them is absent for an addition or a
    /// removal, and both for a "body" change.
    pub from: Option<String>,
    pub to: Option<String>,
}

impl Serialize for Change {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let raw = |v: &Option<String>| -> Result<Option<Box<RawValue>>, S::Error> {
            v.as_ref()
                .map(|text| RawValue::from_string(text.clone()).map_err(serde::ser::Error::custom))
                .transpose()
        };
        let (from, to) = (raw(&self.from)?, raw(&self.to)?);
        let n = 2 + usize::from(from.is_some()) + usize::from(to.is_some());
        let mut st = s.serialize_struct("Change", n)?;
        st.serialize_field("path", &self.path)?;
        st.serialize_field("op", self.op)?;
        if let Some(from) = &from {
            st.serialize_field("from", from)?;
        }
        if let Some(to) = &to {
            st.serialize_field("to", to)?;
        }
        st.end()
    }
}

// Change operations.
pub const ADDED: &str = "added";
pub const REMOVED: &str = "removed";
pub const CHANGED: &str = "changed";

/// Bounds the line diff of text bodies: past it, two bodies that differ
/// are reported as one "body" change.
const MAX_LINE_CELLS: usize = 4_000_000;

#[derive(Deserialize)]
struct Stored {
    #[serde(default)]
    response: Option<StoredResponse>,
}

#[derive(Deserialize)]
struct StoredResponse {
    #[serde(default)]
    status: Option<i64>,
    #[serde(default)]
    body: Option<Box<RawValue>>,
    #[serde(default)]
    body_encoding: Option<String>,
}

fn stored(text: &str) -> Result<Option<StoredResponse>, String> {
    check_json(text.as_bytes())?;
    let kind = match text.trim_start().as_bytes().first() {
        Some(b'{') => "",
        Some(b'n') => return Ok(None),
        Some(b'[') => "array",
        Some(b'"') => "string",
        Some(b't' | b'f') => "bool",
        _ => "number",
    };
    if !kind.is_empty() {
        return Err(format!(
            "json: cannot unmarshal {kind} into Go value of type history.stored"
        ));
    }
    let s: Stored = serde_json::from_str(text).map_err(|e| format!("json: {e}"))?;
    Ok(s.response)
}

/// Lists what changed from one recorded result to another: the status,
/// then the body. JSON bodies are compared by structure, object keys in
/// sorted order and arrays index by index; text bodies line by line.
/// Headers are left out, since a Date or a request id differs every time.
pub fn compare(from: &str, to: &str) -> Result<Vec<Change>, String> {
    let (a, b) = (stored(from)?, stored(to)?);
    let (Some(a), Some(b)) = (a, b) else {
        return Err("an entry has no response".to_string());
    };
    let mut out = Vec::new();
    let (sa, sb) = (a.status.unwrap_or(0), b.status.unwrap_or(0));
    if sa != sb {
        out.push(Change {
            path: "status".to_string(),
            op: CHANGED,
            from: Some(sa.to_string()),
            to: Some(sb.to_string()),
        });
    }
    let (ab, bb) = (
        a.body.as_ref().map_or("", |r| r.get()),
        b.body.as_ref().map_or("", |r| r.get()),
    );
    let (ae, be) = (
        a.body_encoding.unwrap_or_default(),
        b.body_encoding.unwrap_or_default(),
    );
    if !ae.is_empty() || !be.is_empty() {
        if ae != be || ab != bb {
            out.push(Change {
                path: "body".to_string(),
                op: CHANGED,
                from: None,
                to: None,
            });
        }
    } else if is_string(ab) && is_string(bb) {
        let text = |raw: &str| serde_json::from_str::<String>(raw).unwrap_or_default();
        out.extend(diff_lines(&text(ab), &text(bb)));
    } else {
        diff_value("$", &decode(ab), &decode(bb), &mut out);
    }
    Ok(out)
}

fn is_string(raw: &str) -> bool {
    raw.trim().starts_with('"')
}

/// A JSON value with its numbers kept as they were written, so 1.50 and
/// 1.5 are told apart.
#[derive(Clone, Debug, PartialEq)]
enum Node {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Node>),
    Object(BTreeMap<String, Node>),
}

/// Reads a JSON value; an absent body is null.
fn decode(raw: &str) -> Node {
    if raw.trim().is_empty() {
        return Node::Null;
    }
    let mut p = Parser {
        b: raw.as_bytes(),
        text: raw,
        i: 0,
    };
    p.value()
}

/// A reader of text already known to be valid JSON.
struct Parser<'a> {
    b: &'a [u8],
    text: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn string(&mut self) -> String {
        let start = self.i;
        self.i += 1;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\\' => self.i += 1,
                b'"' => {
                    self.i += 1;
                    break;
                }
                _ => {}
            }
            self.i += 1;
        }
        let end = self.i.min(self.b.len());
        serde_json::from_str(&self.text[start..end]).unwrap_or_default()
    }

    fn value(&mut self) -> Node {
        self.ws();
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut m = BTreeMap::new();
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b'"') => {
                            let k = self.string();
                            self.ws();
                            self.i += 1; // the colon
                            let v = self.value();
                            m.insert(k, v);
                        }
                        Some(b',') => self.i += 1,
                        _ => {
                            self.i += 1; // the closing brace
                            break;
                        }
                    }
                }
                Node::Object(m)
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b']') | None => {
                            self.i += 1;
                            break;
                        }
                        Some(b',') => self.i += 1,
                        Some(_) => a.push(self.value()),
                    }
                }
                Node::Array(a)
            }
            Some(b'"') => Node::String(self.string()),
            Some(b't') => {
                self.i += 4;
                Node::Bool(true)
            }
            Some(b'f') => {
                self.i += 5;
                Node::Bool(false)
            }
            Some(b'n') | None => {
                self.i += 4;
                Node::Null
            }
            Some(_) => {
                let start = self.i;
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
                    )
                {
                    self.i += 1;
                }
                Node::Number(self.text[start..self.i].to_string())
            }
        }
    }
}

/// A value as compact JSON, its keys sorted and its numbers as written.
fn raw(v: &Node) -> String {
    match v {
        Node::Null => "null".to_string(),
        Node::Bool(b) => b.to_string(),
        Node::Number(n) => n.clone(),
        Node::String(s) => json_string(s, true),
        Node::Array(a) => format!("[{}]", a.iter().map(raw).collect::<Vec<_>>().join(",")),
        Node::Object(o) => {
            let members: Vec<String> = o
                .iter()
                .map(|(k, v)| format!("{}:{}", json_string(k, true), raw(v)))
                .collect();
            format!("{{{}}}", members.join(","))
        }
    }
}

fn change(path: String, op: &'static str, from: Option<&Node>, to: Option<&Node>) -> Change {
    Change {
        path,
        op,
        from: from.map(raw),
        to: to.map(raw),
    }
}

fn diff_value(path: &str, a: &Node, b: &Node, out: &mut Vec<Change>) {
    match (a, b) {
        (Node::Object(av), Node::Object(bv)) => {
            let mut keys: Vec<&String> = av
                .keys()
                .chain(bv.keys().filter(|k| !av.contains_key(*k)))
                .collect();
            keys.sort();
            for k in keys {
                let p = format!("{}{}", path, key(k));
                match (av.get(k), bv.get(k)) {
                    (None, y) => out.push(change(p, ADDED, None, y)),
                    (x, None) => out.push(change(p, REMOVED, x, None)),
                    (Some(x), Some(y)) => diff_value(&p, x, y, out),
                }
            }
        }
        (Node::Array(av), Node::Array(bv)) => {
            for i in 0..av.len().max(bv.len()) {
                let p = format!("{path}[{i}]");
                match (av.get(i), bv.get(i)) {
                    (None, y) => out.push(change(p, ADDED, None, y)),
                    (x, None) => out.push(change(p, REMOVED, x, None)),
                    (Some(x), Some(y)) => diff_value(&p, x, y, out),
                }
            }
        }
        _ => {
            if a != b {
                out.push(change(path.to_string(), CHANGED, Some(a), Some(b)));
            }
        }
    }
}

/// The JSONPath step to an object member, in the form apic's selectors
/// read: `.name`, or `['odd key']` for anything else.
fn key(k: &str) -> String {
    let mut chars = k.chars();
    let identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if identifier {
        return format!(".{k}");
    }
    format!("['{}']", k.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A line diff of two texts by longest common subsequence. An added line
/// is numbered as in the newer text, a removed one as in the older. The
/// lines both texts start and end with are set aside first, so two long
/// bodies that differ in a few lines cost little.
fn diff_lines(a: &str, b: &str) -> Vec<Change> {
    if a == b {
        return Vec::new();
    }
    let al: Vec<&str> = a.split('\n').collect();
    let bl: Vec<&str> = b.split('\n').collect();
    let mut pre = 0;
    while pre < al.len() && pre < bl.len() && al[pre] == bl[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < al.len() - pre
        && suf < bl.len() - pre
        && al[al.len() - 1 - suf] == bl[bl.len() - 1 - suf]
    {
        suf += 1;
    }
    let (am, bm) = (&al[pre..al.len() - suf], &bl[pre..bl.len() - suf]);
    if am.len() * bm.len() > MAX_LINE_CELLS {
        return vec![Change {
            path: "body".to_string(),
            op: CHANGED,
            from: None,
            to: None,
        }];
    }
    // lcs[i][j] is the common length of am[i..] and bm[j..].
    let width = bm.len() + 1;
    let mut lcs = vec![0u32; (am.len() + 1) * width];
    for i in (0..am.len()).rev() {
        for j in (0..bm.len()).rev() {
            lcs[i * width + j] = if am[i] == bm[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let line = |text: &str| Some(json_string(text, true));
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < am.len() || j < bm.len() {
        if i < am.len() && j < bm.len() && am[i] == bm[j] {
            i += 1;
            j += 1;
        } else if i < am.len()
            && (j == bm.len() || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
        {
            out.push(Change {
                path: format!("line {}", pre + i + 1),
                op: REMOVED,
                from: line(am[i]),
                to: None,
            });
            i += 1;
        } else {
            out.push(Change {
                path: format!("line {}", pre + j + 1),
                op: ADDED,
                from: None,
                to: line(bm[j]),
            });
            j += 1;
        }
    }
    out
}
