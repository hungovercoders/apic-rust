//! A reader of JSON text that hands out each value as the slice of the
//! document it was written in, so a selected object or array keeps its
//! text exactly as the server sent it. Values are found on demand: taking
//! the first element of a long array does not read the rest.

use std::borrow::Cow;

/// The type of a JSON value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Type {
    Null,
    False,
    Number,
    String,
    True,
    /// An object or an array.
    Json,
}

/// One value of a document.
#[derive(Clone, Debug)]
pub(crate) struct Node<'a> {
    pub kind: Type,
    /// The value as written, quotes and all.
    pub raw: Cow<'a, str>,
    /// A string's text with its escapes resolved; empty for other types.
    pub str: Cow<'a, str>,
}

impl<'a> Node<'a> {
    /// A number that is not in the document: a count.
    pub fn number(n: usize) -> Node<'static> {
        Node {
            kind: Type::Number,
            raw: Cow::Owned(n.to_string()),
            str: Cow::Borrowed(""),
        }
    }

    pub fn is_object(&self) -> bool {
        self.kind == Type::Json && self.raw.starts_with('{')
    }

    pub fn is_array(&self) -> bool {
        self.kind == Type::Json && self.raw.starts_with('[')
    }

    /// Calls `visit` with each element of an array or each key and value
    /// of an object, in document order, until it returns false.
    pub fn for_each(&self, mut visit: impl FnMut(Option<Cow<'a, str>>, Node<'a>) -> bool) {
        let Cow::Borrowed(json) = self.raw else {
            return;
        };
        if self.kind != Type::Json {
            return;
        }
        let b = json.as_bytes();
        let object = b.first() == Some(&b'{');
        let mut i = 1;
        loop {
            while i < b.len() && (b[i] <= b' ' || b[i] == b',') {
                i += 1;
            }
            if i >= b.len() || b[i] == b']' || b[i] == b'}' {
                return;
            }
            let mut key = None;
            if object {
                let (node, next) = value_at(json, i);
                key = Some(node.str);
                i = next;
                while i < b.len() && (b[i] <= b' ' || b[i] == b':') {
                    i += 1;
                }
                if i >= b.len() {
                    return;
                }
            }
            let (node, next) = value_at(json, i);
            i = next;
            if !visit(key, node) {
                return;
            }
        }
    }
}

/// Reads a document that [`valid`] accepted. An object or an array takes
/// the rest of the text, so white space after the body is part of it, as
/// it is part of the body the server sent.
pub(crate) fn parse(json: &str) -> Option<Node<'_>> {
    let b = json.as_bytes();
    let i = b.iter().position(|&c| c > b' ')?;
    if b[i] == b'{' || b[i] == b'[' {
        return Some(Node {
            kind: Type::Json,
            raw: Cow::Borrowed(&json[i..]),
            str: Cow::Borrowed(""),
        });
    }
    Some(value_at(json, i).0)
}

/// Reads the value starting at byte `i` of valid JSON and returns it with
/// the index just past it.
fn value_at(json: &str, i: usize) -> (Node<'_>, usize) {
    let b = json.as_bytes();
    let plain = |kind: Type, end: usize| {
        (
            Node {
                kind,
                raw: Cow::Borrowed(&json[i..end]),
                str: Cow::Borrowed(""),
            },
            end,
        )
    };
    match b[i] {
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => j = string_end(b, j) - 1,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return plain(Type::Json, j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            plain(Type::Json, b.len())
        }
        b'"' => {
            let end = string_end(b, i);
            let inner = &json[i + 1..(end - 1).max(i + 1)];
            let str = if inner.contains('\\') {
                Cow::Owned(unescape(inner))
            } else {
                Cow::Borrowed(inner)
            };
            (
                Node {
                    kind: Type::String,
                    raw: Cow::Borrowed(&json[i..end]),
                    str,
                },
                end,
            )
        }
        b't' | b'f' | b'n' => {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_lowercase() {
                j += 1;
            }
            let kind = match b[i] {
                b't' => Type::True,
                b'f' => Type::False,
                _ => Type::Null,
            };
            plain(kind, j)
        }
        _ => {
            let mut j = i + 1;
            while j < b.len() && !(b[j] <= b' ' || matches!(b[j], b',' | b']' | b'}')) {
                j += 1;
            }
            plain(Type::Number, j)
        }
    }
}

/// The index just past the string whose opening quote is at `i`.
fn string_end(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            b'"' => return j + 1,
            _ => {}
        }
        j += 1;
    }
    b.len()
}

/// Resolves the escapes of a string's text. A `\u` escape that is half of
/// a surrogate pair on its own becomes U+FFFD.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let hex4 = |at: usize| -> u32 {
        b.get(at..at + 4)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .unwrap_or(0)
    };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c < b' ' {
            break;
        }
        if c != b'\\' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&e) = b.get(i) else { break };
        match e {
            b'\\' | b'/' | b'"' => out.push(e),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'u' => {
                if i + 5 > b.len() {
                    break;
                }
                let mut r = hex4(i + 1);
                i += 5;
                if (0xD800..0xDC00).contains(&r)
                    && b.len() - i >= 6
                    && b[i] == b'\\'
                    && b[i + 1] == b'u'
                {
                    let r2 = hex4(i + 2);
                    if (0xDC00..0xE000).contains(&r2) {
                        r = 0x10000 + ((r - 0xD800) << 10) + (r2 - 0xDC00);
                        i += 6;
                    }
                }
                let ch = char::from_u32(r).unwrap_or(char::REPLACEMENT_CHARACTER);
                out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                continue;
            }
            _ => break,
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `data` is one JSON value with nothing but white space around
/// it. Strings are not checked for valid UTF-8.
pub(crate) fn valid(data: &[u8]) -> bool {
    let mut i = 0;
    while i < data.len() {
        if is_ws(data[i]) {
            i += 1;
            continue;
        }
        let Some(end) = valid_any(data, i) else {
            return false;
        };
        return data[end..].iter().all(|&c| is_ws(c));
    }
    false
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

fn skip_ws(data: &[u8], mut i: usize) -> usize {
    while i < data.len() && is_ws(data[i]) {
        i += 1;
    }
    i
}

/// Each `valid_` function takes the index of a value's first byte and
/// returns the index just past the value, or None when it is malformed.
fn valid_any(data: &[u8], i: usize) -> Option<usize> {
    let i = skip_ws(data, i);
    match *data.get(i)? {
        b'{' => valid_object(data, i + 1),
        b'[' => valid_array(data, i + 1),
        b'"' => valid_string(data, i + 1),
        b'-' | b'0'..=b'9' => valid_number(data, i),
        b't' => data[i..].starts_with(b"true").then_some(i + 4),
        b'f' => data[i..].starts_with(b"false").then_some(i + 5),
        b'n' => data[i..].starts_with(b"null").then_some(i + 4),
        _ => None,
    }
}

fn valid_object(data: &[u8], i: usize) -> Option<usize> {
    let mut i = skip_ws(data, i);
    if *data.get(i)? == b'}' {
        return Some(i + 1);
    }
    loop {
        if *data.get(i)? != b'"' {
            return None;
        }
        i = skip_ws(data, valid_string(data, i + 1)?);
        if *data.get(i)? != b':' {
            return None;
        }
        i = skip_ws(data, valid_any(data, i + 1)?);
        match *data.get(i)? {
            b'}' => return Some(i + 1),
            b',' => i = skip_ws(data, i + 1),
            _ => return None,
        }
    }
}

fn valid_array(data: &[u8], i: usize) -> Option<usize> {
    let mut i = skip_ws(data, i);
    if *data.get(i)? == b']' {
        return Some(i + 1);
    }
    loop {
        i = skip_ws(data, valid_any(data, i)?);
        match *data.get(i)? {
            b']' => return Some(i + 1),
            b',' => i += 1,
            _ => return None,
        }
    }
}

fn valid_string(data: &[u8], mut i: usize) -> Option<usize> {
    while i < data.len() {
        match data[i] {
            c if c < b' ' => return None,
            b'\\' => {
                i += 1;
                match *data.get(i)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                    b'u' => {
                        for _ in 0..4 {
                            i += 1;
                            if !data.get(i)?.is_ascii_hexdigit() {
                                return None;
                            }
                        }
                    }
                    _ => return None,
                }
            }
            b'"' => return Some(i + 1),
            _ => {}
        }
        i += 1;
    }
    None
}

fn valid_number(data: &[u8], mut i: usize) -> Option<usize> {
    let digits = |mut i: usize| {
        while i < data.len() && data[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    if data[i] == b'-' {
        i += 1;
        if !data.get(i)?.is_ascii_digit() {
            return None;
        }
    }
    if data[i] == b'0' {
        i += 1;
    } else {
        i = digits(i);
    }
    if data.get(i) == Some(&b'.') {
        i += 1;
        if !data.get(i)?.is_ascii_digit() {
            return None;
        }
        i = digits(i);
    }
    if matches!(data.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(data.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !data.get(i)?.is_ascii_digit() {
            return None;
        }
        i = digits(i);
    }
    Some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates() {
        for ok in [
            "1",
            " -0.5e+3 ",
            "{}",
            "[]",
            "\t{\"a\":[1,{\"b\":null}],\"c\":\"\\u00e9\\n\"}\n",
            "true",
            "\"x\"",
            "[1 ,2]",
            "0",
            "-0",
            "[[],{}]",
            "\"\u{7f}\"",
        ] {
            assert!(valid(ok.as_bytes()), "{ok:?}");
        }
        for bad in [
            "",
            " ",
            "nope",
            "{\"a\":01}",
            "[1,]",
            "{\"a\":1,}",
            "{a:1}",
            "1 2",
            "\"a\nb\"",
            "\"\\x\"",
            "\"\\u12g4\"",
            "-",
            "1.",
            "1e",
            "tru",
            "[",
            "{\"a\"}",
            "+1",
            ".5",
            "[1 2]",
            "'a'",
            "\"abc",
            "{\"a\":}",
            "nul",
            "[,1]",
            "{,}",
        ] {
            assert!(!valid(bad.as_bytes()), "{bad:?}");
        }
    }

    #[test]
    fn walks_values_as_written() {
        let doc = " {\"a\" : [1, \"x\\\"y\", {\"k\":[ ]}] , \"b\\u00e9\": \"\\ud83d\\ude00 \\ud83d z\", \"n\": null,\"t\":true }\n";
        let root = parse(doc).unwrap();
        assert!(root.is_object());
        assert_eq!(root.raw, doc[1..]);
        let mut seen = Vec::new();
        root.for_each(|k, v| {
            seen.push((
                k.unwrap().into_owned(),
                v.raw.to_string(),
                v.str.to_string(),
                v.kind,
            ));
            true
        });
        assert_eq!(
            seen,
            vec![
                (
                    "a".to_string(),
                    "[1, \"x\\\"y\", {\"k\":[ ]}]".to_string(),
                    String::new(),
                    Type::Json
                ),
                (
                    "bé".to_string(),
                    "\"\\ud83d\\ude00 \\ud83d z\"".to_string(),
                    "😀 \u{fffd} z".to_string(),
                    Type::String
                ),
                (
                    "n".to_string(),
                    "null".to_string(),
                    String::new(),
                    Type::Null
                ),
                (
                    "t".to_string(),
                    "true".to_string(),
                    String::new(),
                    Type::True
                ),
            ]
        );
        let mut items = Vec::new();
        parse("[1, \"x\\\"y\", {\"k\":[ ]}]")
            .unwrap()
            .for_each(|k, v| {
                assert!(k.is_none());
                items.push(v.raw.to_string());
                items.len() < 2
            });
        assert_eq!(items, vec!["1", "\"x\\\"y\""]);
        assert_eq!(parse(" 12 ").unwrap().raw, "12");
        assert_eq!(parse("\"a\" ").unwrap().str, "a");
        assert_eq!(parse("false").unwrap().kind, Type::False);
    }
}
