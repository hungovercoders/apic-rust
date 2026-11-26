//! Compares JSON documents for the `the response body is:` and `contains:`
//! steps. Numbers are kept as written so large integers compare exactly.

use std::collections::BTreeMap;

use apic_gocompat::{check_json, json_string};

use crate::state::truncate;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    /// The number as written.
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while matches!(self.data.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Option<Json> {
        if self.data[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Some(value)
        } else {
            None
        }
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > 10_000 {
            return None;
        }
        self.skip_ws();
        match *self.data.get(self.pos)? {
            b'n' => self.literal("null", Json::Null),
            b't' => self.literal("true", Json::Bool(true)),
            b'f' => self.literal("false", Json::Bool(false)),
            b'"' => self.string().map(Json::String),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                self.skip_ws();
                if self.data.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Some(Json::Array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.skip_ws();
                    match self.data.get(self.pos)? {
                        b',' => self.pos += 1,
                        b']' => {
                            self.pos += 1;
                            return Some(Json::Array(items));
                        }
                        _ => return None,
                    }
                }
            }
            b'{' => {
                self.pos += 1;
                let mut members = BTreeMap::new();
                self.skip_ws();
                if self.data.get(self.pos) == Some(&b'}') {
                    self.pos += 1;
                    return Some(Json::Object(members));
                }
                loop {
                    self.skip_ws();
                    if self.data.get(self.pos) != Some(&b'"') {
                        return None;
                    }
                    let key = self.string()?;
                    self.skip_ws();
                    if self.data.get(self.pos) != Some(&b':') {
                        return None;
                    }
                    self.pos += 1;
                    let value = self.value(depth + 1)?;
                    members.insert(key, value);
                    self.skip_ws();
                    match self.data.get(self.pos)? {
                        b',' => self.pos += 1,
                        b'}' => {
                            self.pos += 1;
                            return Some(Json::Object(members));
                        }
                        _ => return None,
                    }
                }
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.data.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.data.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        if self.data.get(self.pos) == Some(&b'0') {
            self.pos += 1;
        } else if self.digits() == 0 {
            return None;
        }
        if self.data.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if self.digits() == 0 {
                return None;
            }
        }
        if matches!(self.data.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.data.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return None;
            }
        }
        let text = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
        Some(Json::Number(text))
    }

    fn hex4(&mut self) -> Option<u32> {
        let hex = self.data.get(self.pos..self.pos + 4)?;
        let n = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
        self.pos += 4;
        Some(n)
    }

    fn string(&mut self) -> Option<String> {
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.data.get(self.pos)?;
            self.pos += 1;
            match c {
                b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
                b'\\' => {
                    let e = *self.data.get(self.pos)?;
                    self.pos += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{08}',
                        b'f' => '\u{0c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) {
                                // A surrogate pair is one character; half
                                // of one is not a character at all.
                                let rest = self.data.get(self.pos..self.pos + 2);
                                let save = self.pos;
                                let pair = if rest == Some(b"\\u") {
                                    self.pos += 2;
                                    self.hex4().filter(|lo| (0xDC00..0xE000).contains(lo))
                                } else {
                                    None
                                };
                                match pair {
                                    Some(lo) => char::from_u32(
                                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00),
                                    )
                                    .unwrap_or('\u{FFFD}'),
                                    None => {
                                        self.pos = save;
                                        '\u{FFFD}'
                                    }
                                }
                            } else {
                                char::from_u32(hi).unwrap_or('\u{FFFD}')
                            }
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return None,
                c => out.push(c),
            }
        }
    }
}

/// Reads exactly one JSON value: a second value or trailing syntax is an
/// error.
pub(crate) fn decode_json(data: &[u8]) -> Result<Json, String> {
    let mut p = Parser { data, pos: 0 };
    let Some(value) = p.value(0) else {
        if data.iter().all(u8::is_ascii_whitespace) {
            return Err("EOF".to_string());
        }
        return Err(match check_json(data) {
            Err(msg) if msg == "unexpected end of JSON input" => "unexpected EOF".to_string(),
            Err(msg) => msg,
            Ok(()) => "invalid JSON".to_string(),
        });
    };
    p.skip_ws();
    if p.pos != data.len() {
        return Err("unexpected data after the JSON value".to_string());
    }
    Ok(value)
}

fn decode_both(actual: &[u8], expected: &[u8]) -> Result<(Json, Json), String> {
    let a = decode_json(actual).map_err(|_| "response body is not JSON".to_string())?;
    let e = decode_json(expected).map_err(|err| format!("expected document is not JSON: {err}"))?;
    Ok((a, e))
}

/// Checks that two JSON documents are semantically equal; the error says
/// where they first differ.
pub(crate) fn json_equal(actual: &[u8], expected: &[u8]) -> Result<(), String> {
    let (a, e) = decode_both(actual, expected)?;
    diff("$", &a, &e, true)
}

/// Checks that `expected` is a subset of `actual`: every key in an
/// expected object must be present and match; arrays must have the same
/// length and match element by element; scalars must be equal.
pub(crate) fn json_contains(actual: &[u8], expected: &[u8]) -> Result<(), String> {
    let (a, e) = decode_both(actual, expected)?;
    diff("$", &a, &e, false)
}

fn diff(path: &str, a: &Json, e: &Json, exact: bool) -> Result<(), String> {
    match e {
        Json::Object(ev) => {
            let Json::Object(av) = a else {
                return Err(format!("{path}: expected an object, got {}", kind(a)));
            };
            for (k, expected) in ev {
                let Some(got) = av.get(k) else {
                    return Err(format!("{path}.{k}: missing"));
                };
                diff(&format!("{path}.{k}"), got, expected, exact)?;
            }
            if exact && av.len() != ev.len() {
                if let Some(k) = av.keys().find(|k| !ev.contains_key(*k)) {
                    return Err(format!("{path}.{k}: unexpected key"));
                }
            }
        }
        Json::Array(ev) => {
            let Json::Array(av) = a else {
                return Err(format!("{path}: expected an array, got {}", kind(a)));
            };
            if av.len() != ev.len() {
                return Err(format!(
                    "{path}: expected {} items, got {}",
                    ev.len(),
                    av.len()
                ));
            }
            for (i, (got, expected)) in av.iter().zip(ev).enumerate() {
                diff(&format!("{path}[{i}]"), got, expected, exact)?;
            }
        }
        Json::Number(en) => {
            let equal = matches!(a, Json::Number(an) if number_equal(an, en));
            if !equal {
                return Err(format!("{path}: expected {}, got {}", show(e), show(a)));
            }
        }
        _ => {
            if a != e {
                return Err(format!("{path}: expected {}, got {}", show(e), show(a)));
            }
        }
    }
    Ok(())
}

/// Compares JSON numbers exactly, so 1.0 equals 1 and 9007199254740993
/// differs from 9007199254740992. Numbers too large to expand safely
/// compare as text.
fn number_equal(a: &str, b: &str) -> bool {
    match (apic_assert::parse_number(a), apic_assert::parse_number(b)) {
        (Some(x), Some(y)) => x.cmp(&y).is_eq(),
        _ => a == b,
    }
}

fn kind(v: &Json) -> &'static str {
    match v {
        Json::Null => "null",
        Json::Object(_) => "an object",
        Json::Array(_) => "an array",
        Json::String(_) => "a string",
        Json::Number(_) => "a number",
        Json::Bool(_) => "a boolean",
    }
}

fn encode(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => out.push_str(n),
        Json::String(s) => out.push_str(&json_string(s, true)),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode(item, out);
            }
            out.push(']');
        }
        Json::Object(members) => {
            out.push('{');
            for (i, (k, item)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&json_string(k, true));
                out.push(':');
                encode(item, out);
            }
            out.push('}');
        }
    }
}

/// The compact JSON text of a value.
pub(crate) fn to_json(v: &Json) -> String {
    let mut out = String::new();
    encode(v, &mut out);
    out
}

fn show(v: &Json) -> String {
    truncate(&to_json(v), 80).trim().to_string()
}
