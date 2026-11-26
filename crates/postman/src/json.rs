//! A JSON reader that keeps what decoding a collection depends on: the
//! members of an object in the order they were written (the first value
//! of the wrong type is the one reported) and numbers as they were spelt.

use apic_gocompat::{check_json, format_float_json, json_string};

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    /// A number, as written.
    Number(String),
    Str(String),
    Array(Vec<Json>),
    /// The members in document order, a repeated key included.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Reads one JSON value; `None` when `data` is not valid JSON.
    pub(crate) fn parse(data: &[u8]) -> Option<Json> {
        check_json(data).ok()?;
        let text = String::from_utf8_lossy(data);
        let mut p = Parser {
            src: text.as_bytes(),
            pos: 0,
        };
        Some(p.value())
    }

    /// What messages call the value: `string`, `number`, `object`...
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "bool",
            Json::Number(_) => "number",
            Json::Str(_) => "string",
            Json::Array(_) => "array",
            Json::Object(_) => "object",
        }
    }

    /// The value of a member, by its exact name; the last one when the
    /// key is repeated.
    pub(crate) fn member(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value as compact JSON with its keys sorted, numbers in their
    /// shortest form and `<`, `>` and `&` escaped. `Err` holds a number
    /// too large to be one.
    pub(crate) fn marshal(&self) -> Result<String, String> {
        let mut out = String::new();
        self.write(&mut out)?;
        Ok(out)
    }

    fn write(&self, out: &mut String) -> Result<(), String> {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Number(n) => out.push_str(&number_text(n)?),
            Json::Str(s) => out.push_str(&json_string(s, true)),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out)?;
                }
                out.push(']');
            }
            Json::Object(members) => {
                // A repeated key keeps its last value.
                let mut sorted: std::collections::BTreeMap<&str, &Json> =
                    std::collections::BTreeMap::new();
                for (k, v) in members {
                    sorted.insert(k, v);
                }
                out.push('{');
                for (i, (k, v)) in sorted.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&json_string(k, true));
                    out.push(':');
                    v.write(out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }
}

/// A number as a 64-bit float prints: `20`, `1.5`, `1e+21`. `Err` holds
/// the number as written when it is too large for one.
pub(crate) fn number_text(n: &str) -> Result<String, String> {
    match n.parse::<f64>() {
        Ok(f) if f.is_finite() => Ok(format_float_json(f)),
        _ => Err(n.to_string()),
    }
}

/// Reads JSON already known to be valid.
struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip_space(&mut self) {
        while matches!(self.src.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Json {
        self.skip_space();
        match self.src.get(self.pos) {
            Some(b'{') => {
                self.pos += 1;
                let mut members = Vec::new();
                loop {
                    self.skip_space();
                    match self.src.get(self.pos) {
                        Some(b'"') => {
                            let key = self.string();
                            self.skip_space();
                            self.pos += 1; // the colon
                            members.push((key, self.value()));
                        }
                        Some(b',') => self.pos += 1,
                        _ => {
                            self.pos += 1; // the closing brace
                            return Json::Object(members);
                        }
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.src.get(self.pos) {
                        Some(b']') | None => {
                            self.pos += 1;
                            return Json::Array(items);
                        }
                        Some(b',') => self.pos += 1,
                        _ => items.push(self.value()),
                    }
                }
            }
            Some(b'"') => Json::Str(self.string()),
            Some(b't') => {
                self.pos += 4;
                Json::Bool(true)
            }
            Some(b'f') => {
                self.pos += 5;
                Json::Bool(false)
            }
            Some(b'n') => {
                self.pos += 4;
                Json::Null
            }
            _ => {
                let start = self.pos;
                while matches!(
                    self.src.get(self.pos),
                    Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                ) {
                    self.pos += 1;
                }
                if self.pos == start {
                    self.pos += 1; // never on valid input; keeps the walk moving
                }
                Json::Number(String::from_utf8_lossy(&self.src[start..self.pos]).into_owned())
            }
        }
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let digits = self.src.get(at..at + 4)?;
        u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
    }

    /// Reads the string that starts at the current quote. An escape for
    /// half of a surrogate pair on its own becomes U+FFFD.
    fn string(&mut self) -> String {
        self.pos += 1;
        let mut out = Vec::new();
        while let Some(&b) = self.src.get(self.pos) {
            self.pos += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let Some(&esc) = self.src.get(self.pos) else {
                        break;
                    };
                    self.pos += 1;
                    let c = match esc {
                        b'b' => '\u{08}',
                        b'f' => '\u{0c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let first = self.hex4(self.pos).unwrap_or(0xFFFD);
                            self.pos += 4;
                            let low = (self.src.get(self.pos..self.pos + 2) == Some(b"\\u"))
                                .then(|| self.hex4(self.pos + 2))
                                .flatten();
                            match (first, low) {
                                (0xD800..=0xDBFF, Some(low @ 0xDC00..=0xDFFF)) => {
                                    self.pos += 6;
                                    let code = 0x10000 + ((first - 0xD800) << 10) + (low - 0xDC00);
                                    char::from_u32(code).unwrap_or('\u{FFFD}')
                                }
                                _ => char::from_u32(first).unwrap_or('\u{FFFD}'),
                            }
                        }
                        other => other as char, // `"`, `\` and `/`
                    };
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                b => out.push(b),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_in_order_and_marshals_sorted() {
        let v = Json::parse(
            br#" {"b": [1, 2.50, 1e2, true, null], "a": {"z": "x\n\u00e9\ud83d\ude00\ud800<", "y": {}}, "b": []} "#,
        )
        .unwrap();
        let Json::Object(members) = &v else {
            panic!("not an object")
        };
        let keys: Vec<&str> = members.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["b", "a", "b"]);
        assert_eq!(v.member("b"), Some(&Json::Array(vec![])));
        assert_eq!(members[0].1.marshal().unwrap(), "[1,2.5,100,true,null]");
        assert_eq!(
            v.marshal().unwrap(),
            "{\"a\":{\"y\":{},\"z\":\"x\\n\u{e9}\u{1f600}\u{fffd}\\u003c\"},\"b\":[]}"
        );
        assert!(Json::parse(b"{\"a\": }").is_none());
        assert!(Json::parse(b"{} x").is_none());
        assert_eq!(
            Json::parse(b"1e999").unwrap().marshal(),
            Err("1e999".to_string())
        );
    }
}
