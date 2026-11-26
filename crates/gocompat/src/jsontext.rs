//! JSON text as apic's files and output were first written: strings with
//! `<`, `>` and `&` escaped, compact and indented layouts that leave an
//! empty object as `{}`, and the syntax errors a reader of a broken file
//! is shown. These work on JSON text, not on a parsed tree, so a caller
//! can keep a document's numbers and key order exactly as they were.

use crate::quote::quote;

/// Returns `s` as a JSON string literal. `"` and `\` are escaped, control
/// characters are `\b`, `\f`, `\n`, `\r`, `\t` or `\u00XX`, U+2028 and
/// U+2029 are always `\u2028` and `\u2029`, and with `escape_html` the
/// characters `<`, `>` and `&` are `\u003c`, `\u003e` and `\u0026`.
pub fn json_string(s: &str, escape_html: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            '<' | '>' | '&' if escape_html => out.push_str(&format!("\\u{:04x}", c as u32)),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Removes the insignificant white space from valid JSON text. U+2028 and
/// U+2029 inside strings become `\u2028` and `\u2029`, and with
/// `escape_html` so do `<`, `>` and `&` (`\u003c`, `\u003e`, `\u0026`).
pub fn compact_json(src: &str, escape_html: bool) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in src.chars() {
        if in_string {
            match c {
                '<' | '>' | '&' if escape_html => out.push_str(&format!("\\u{:04x}", c as u32)),
                '\u{2028}' => out.push_str("\\u2028"),
                '\u{2029}' => out.push_str("\\u2029"),
                c => out.push(c),
            }
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            ' ' | '\t' | '\r' | '\n' => {}
            '"' => {
                in_string = true;
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// Lays valid JSON text out one member or element to a line, each line
/// starting with `prefix` and one `indent` per level of nesting. An empty
/// object or array stays `{}` or `[]`, and nothing follows the last
/// bracket: add the trailing newline yourself.
pub fn indent_json(src: &str, prefix: &str, indent: &str) -> String {
    fn newline(out: &mut String, prefix: &str, indent: &str, depth: usize) {
        out.push('\n');
        out.push_str(prefix);
        for _ in 0..depth {
            out.push_str(indent);
        }
    }
    let mut out = String::with_capacity(src.len() * 2);
    let mut need_indent = false;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for c in src.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if matches!(c, ' ' | '\t' | '\r' | '\n') {
            continue;
        }
        if need_indent && c != ']' && c != '}' {
            need_indent = false;
            depth += 1;
            newline(&mut out, prefix, indent, depth);
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                // The newline waits for the next character, so that an
                // empty object or array stays on one line.
                need_indent = true;
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, prefix, indent, depth);
            }
            ':' => out.push_str(": "),
            '}' | ']' => {
                if need_indent {
                    need_indent = false;
                } else {
                    depth = depth.saturating_sub(1);
                    newline(&mut out, prefix, indent, depth);
                }
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// What a JSON document is inside of, innermost last.
#[derive(Clone, Copy, PartialEq)]
enum Parse {
    ObjectKey,
    ObjectValue,
    ArrayValue,
}

#[derive(Clone, Copy)]
enum State {
    BeginValueOrEmpty,
    BeginValue,
    BeginStringOrEmpty,
    BeginString,
    EndValue,
    EndTop,
    InString,
    InStringEsc,
    InStringEscU(u8),
    Neg,
    Int,
    Zero,
    Dot,
    Frac,
    Exp,
    ExpSign,
    ExpDigits,
    Literal(&'static str, usize),
}

/// The deepest nesting a document may have.
const MAX_DEPTH: usize = 10000;

struct Scanner {
    state: State,
    stack: Vec<Parse>,
}

fn quote_char(c: u8) -> String {
    match c {
        b'\'' => "'\\''".to_string(),
        b'"' => "'\"'".to_string(),
        _ => {
            let q = quote(&char::from(c).to_string());
            format!("'{}'", &q[1..q.len() - 1])
        }
    }
}

fn is_space(c: u8) -> bool {
    c <= b' ' && matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}

impl Scanner {
    fn error(c: u8, context: &str) -> String {
        format!("invalid character {} {}", quote_char(c), context)
    }

    fn step(&mut self, c: u8) -> Result<(), String> {
        match self.state {
            State::BeginValueOrEmpty => {
                if is_space(c) {
                    Ok(())
                } else if c == b']' {
                    self.end_value(c)
                } else {
                    self.begin_value(c)
                }
            }
            State::BeginValue => self.begin_value(c),
            State::BeginStringOrEmpty => {
                if is_space(c) {
                    Ok(())
                } else if c == b'}' {
                    if let Some(top) = self.stack.last_mut() {
                        *top = Parse::ObjectValue;
                    }
                    self.end_value(c)
                } else {
                    self.begin_string(c)
                }
            }
            State::BeginString => self.begin_string(c),
            State::EndValue => self.end_value(c),
            State::EndTop => {
                if is_space(c) {
                    Ok(())
                } else {
                    Err(Self::error(c, "after top-level value"))
                }
            }
            State::InString | State::InStringEsc | State::InStringEscU(_) => self.step_string(c),
            State::Neg
            | State::Int
            | State::Zero
            | State::Dot
            | State::Frac
            | State::Exp
            | State::ExpSign
            | State::ExpDigits => self.step_number(c),
            State::Literal(word, pos) => {
                let want = word.as_bytes()[pos];
                if c == want {
                    self.state = if pos + 1 == word.len() {
                        State::EndValue
                    } else {
                        State::Literal(word, pos + 1)
                    };
                    Ok(())
                } else {
                    Err(Self::error(
                        c,
                        &format!("in literal {} (expecting {})", word, quote_char(want)),
                    ))
                }
            }
        }
    }

    /// A step inside a string.
    fn step_string(&mut self, c: u8) -> Result<(), String> {
        match self.state {
            State::InString => {
                match c {
                    b'"' => self.state = State::EndValue,
                    b'\\' => self.state = State::InStringEsc,
                    c if c < 0x20 => return Err(Self::error(c, "in string literal")),
                    _ => {}
                }
                Ok(())
            }
            State::InStringEsc => match c {
                b'b' | b'f' | b'n' | b'r' | b't' | b'\\' | b'/' | b'"' => {
                    self.state = State::InString;
                    Ok(())
                }
                b'u' => {
                    self.state = State::InStringEscU(0);
                    Ok(())
                }
                _ => Err(Self::error(c, "in string escape code")),
            },
            State::InStringEscU(n) => {
                if c.is_ascii_hexdigit() {
                    self.state = if n == 3 {
                        State::InString
                    } else {
                        State::InStringEscU(n + 1)
                    };
                    Ok(())
                } else {
                    Err(Self::error(c, "in \\u hexadecimal character escape"))
                }
            }
            _ => Ok(()),
        }
    }

    /// A step inside a number.
    fn step_number(&mut self, c: u8) -> Result<(), String> {
        match self.state {
            State::Neg => match c {
                b'0' => {
                    self.state = State::Zero;
                    Ok(())
                }
                b'1'..=b'9' => {
                    self.state = State::Int;
                    Ok(())
                }
                _ => Err(Self::error(c, "in numeric literal")),
            },
            State::Int => {
                if c.is_ascii_digit() {
                    Ok(())
                } else {
                    self.after_int(c)
                }
            }
            State::Zero => self.after_int(c),
            State::Dot => {
                if c.is_ascii_digit() {
                    self.state = State::Frac;
                    Ok(())
                } else {
                    Err(Self::error(c, "after decimal point in numeric literal"))
                }
            }
            State::Frac => {
                if c.is_ascii_digit() {
                    Ok(())
                } else if c == b'e' || c == b'E' {
                    self.state = State::Exp;
                    Ok(())
                } else {
                    self.end_value(c)
                }
            }
            State::Exp => {
                if c == b'+' || c == b'-' {
                    self.state = State::ExpSign;
                    Ok(())
                } else {
                    self.exp_sign(c)
                }
            }
            State::ExpSign => self.exp_sign(c),
            State::ExpDigits => {
                if c.is_ascii_digit() {
                    Ok(())
                } else {
                    self.end_value(c)
                }
            }
            _ => Ok(()),
        }
    }

    fn push(&mut self, p: Parse) -> Result<(), String> {
        self.stack.push(p);
        if self.stack.len() <= MAX_DEPTH {
            Ok(())
        } else {
            Err("exceeded max depth".to_string())
        }
    }

    fn begin_value(&mut self, c: u8) -> Result<(), String> {
        if is_space(c) {
            return Ok(());
        }
        match c {
            b'{' => {
                self.state = State::BeginStringOrEmpty;
                self.push(Parse::ObjectKey)
            }
            b'[' => {
                self.state = State::BeginValueOrEmpty;
                self.push(Parse::ArrayValue)
            }
            b'"' => {
                self.state = State::InString;
                Ok(())
            }
            b'-' => {
                self.state = State::Neg;
                Ok(())
            }
            b'0' => {
                self.state = State::Zero;
                Ok(())
            }
            b'1'..=b'9' => {
                self.state = State::Int;
                Ok(())
            }
            b't' => {
                self.state = State::Literal("true", 1);
                Ok(())
            }
            b'f' => {
                self.state = State::Literal("false", 1);
                Ok(())
            }
            b'n' => {
                self.state = State::Literal("null", 1);
                Ok(())
            }
            _ => Err(Self::error(c, "looking for beginning of value")),
        }
    }

    fn begin_string(&mut self, c: u8) -> Result<(), String> {
        if is_space(c) {
            return Ok(());
        }
        if c == b'"' {
            self.state = State::InString;
            return Ok(());
        }
        Err(Self::error(c, "looking for beginning of object key string"))
    }

    fn after_int(&mut self, c: u8) -> Result<(), String> {
        match c {
            b'.' => {
                self.state = State::Dot;
                Ok(())
            }
            b'e' | b'E' => {
                self.state = State::Exp;
                Ok(())
            }
            _ => self.end_value(c),
        }
    }

    fn exp_sign(&mut self, c: u8) -> Result<(), String> {
        if c.is_ascii_digit() {
            self.state = State::ExpDigits;
            Ok(())
        } else {
            Err(Self::error(c, "in exponent of numeric literal"))
        }
    }

    fn pop(&mut self) {
        self.stack.pop();
        self.state = if self.stack.is_empty() {
            State::EndTop
        } else {
            State::EndValue
        };
    }

    fn end_value(&mut self, c: u8) -> Result<(), String> {
        let Some(&top) = self.stack.last() else {
            self.state = State::EndTop;
            return self.step(c);
        };
        if is_space(c) {
            self.state = State::EndValue;
            return Ok(());
        }
        let n = self.stack.len() - 1;
        match top {
            Parse::ObjectKey => {
                if c == b':' {
                    self.stack[n] = Parse::ObjectValue;
                    self.state = State::BeginValue;
                    return Ok(());
                }
                Err(Self::error(c, "after object key"))
            }
            Parse::ObjectValue => {
                if c == b',' {
                    self.stack[n] = Parse::ObjectKey;
                    self.state = State::BeginString;
                    return Ok(());
                }
                if c == b'}' {
                    self.pop();
                    return Ok(());
                }
                Err(Self::error(c, "after object key:value pair"))
            }
            Parse::ArrayValue => {
                if c == b',' {
                    self.state = State::BeginValue;
                    return Ok(());
                }
                if c == b']' {
                    self.pop();
                    return Ok(());
                }
                Err(Self::error(c, "after array element"))
            }
        }
    }
}

/// Checks that `data` is one JSON value and nothing else. The error is the
/// message a reader of a broken file is shown, such as
/// `invalid character '}' looking for beginning of value` or
/// `unexpected end of JSON input`.
pub fn check_json(data: &[u8]) -> Result<(), String> {
    let mut s = Scanner {
        state: State::BeginValue,
        stack: Vec::new(),
    };
    for &c in data {
        s.step(c)?;
    }
    if matches!(s.state, State::EndTop) {
        return Ok(());
    }
    // A number or a literal still open ends at the end of the input, as
    // it would at a space.
    s.step(b' ')?;
    if matches!(s.state, State::EndTop) {
        return Ok(());
    }
    Err("unexpected end of JSON input".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings() {
        assert_eq!(
            json_string("a<b>&c\u{2028}\u{7f}\u{01}é\"\\\n", true),
            "\"a\\u003cb\\u003e\\u0026c\\u2028\u{7f}\\u0001é\\\"\\\\\\n\""
        );
        assert_eq!(json_string("<\u{08}\u{0c}>", false), "\"<\\b\\f>\"");
    }

    #[test]
    fn compact_and_indent() {
        let src = "{ \"a\" : [1, 2, {}], \"b\": \"<x> y\", \"e\": [ ] ,\"n\":null}";
        let compact = compact_json(src, true);
        assert_eq!(
            compact,
            "{\"a\":[1,2,{}],\"b\":\"\\u003cx\\u003e y\",\"e\":[],\"n\":null}"
        );
        assert_eq!(compact_json("\"a\\\" <\"", false), "\"a\\\" <\"");
        assert_eq!(
            indent_json(&compact, "", "  "),
            "{\n  \"a\": [\n    1,\n    2,\n    {}\n  ],\n  \"b\": \"\\u003cx\\u003e y\",\n  \"e\": [],\n  \"n\": null\n}"
        );
        assert_eq!(indent_json("{}", "", "  "), "{}");
        assert_eq!(
            indent_json("[{\"a\":1}]", ">", "\t"),
            "[\n>\t{\n>\t\t\"a\": 1\n>\t}\n>]"
        );
    }

    #[test]
    fn syntax_errors() {
        let cases: &[(&str, &str)] = &[
            (
                "{not json",
                "invalid character 'n' looking for beginning of object key string",
            ),
            (
                "{\"dev\": {\"baseUrl\": }}",
                "invalid character '}' looking for beginning of value",
            ),
            (
                "not json at all",
                "invalid character 'o' in literal null (expecting 'u')",
            ),
            ("", "unexpected end of JSON input"),
            ("{\"a\":", "unexpected end of JSON input"),
            ("{\"a\" 1}", "invalid character '1' after object key"),
            (
                "{\"a\":1 2}",
                "invalid character '2' after object key:value pair",
            ),
            ("[1 2]", "invalid character '2' after array element"),
            ("1 2", "invalid character '2' after top-level value"),
            ("\"a\nb\"", "invalid character '\\n' in string literal"),
            ("\"\\x\"", "invalid character 'x' in string escape code"),
            (
                "\"\\u12g4\"",
                "invalid character 'g' in \\u hexadecimal character escape",
            ),
            ("-", "invalid character ' ' in numeric literal"),
            ("-x", "invalid character 'x' in numeric literal"),
            (
                "1.",
                "invalid character ' ' after decimal point in numeric literal",
            ),
            (
                "1e+",
                "invalid character ' ' in exponent of numeric literal",
            ),
            (
                "tru",
                "invalid character ' ' in literal true (expecting 'e')",
            ),
            (
                "[\"a\",]",
                "invalid character ']' looking for beginning of value",
            ),
            (
                "{\"a\":1,}",
                "invalid character '}' looking for beginning of object key string",
            ),
            (
                "'a'",
                "invalid character '\\'' looking for beginning of value",
            ),
            ("01", "invalid character '1' after top-level value"),
            ("\"abc", "unexpected end of JSON input"),
            (
                "[\u{1}]",
                "invalid character '\\x01' looking for beginning of value",
            ),
        ];
        for (src, want) in cases {
            assert_eq!(check_json(src.as_bytes()).unwrap_err(), *want, "{src:?}");
        }
        for ok in [
            "1",
            " 12 ",
            "-0.5e+3",
            "{}",
            "[]",
            "{\"a\":[1,{\"b\":null}],\"c\":\"\\u00e9\"}",
            "true",
            "\"x\"",
        ] {
            assert!(check_json(ok.as_bytes()).is_ok(), "{ok:?}");
        }
    }
}
