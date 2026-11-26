//! The body path language, the part after `body.$`. It is JSONPath as
//! Hurl, Postman and Bruno users write it, evaluated over the document's
//! own text so a selected object or array keeps its text exactly as the
//! server sent it:
//!
//! ```text
//! .name ["name"] ['name']   a key; a bare key runs to the next . or [
//! [2] [-1]                  an index, negative from the end
//! [1:3] [:2] [-2:]          a slice, end exclusive
//! .* [*]                    every element or value
//! ..name ..* ..[0]          recursive descent: at any depth
//! [?(@.done == true)]       a filter: == != < <= > >= and =~ /re/i,
//!                           @.x alone for presence, ! for absence,
//!                           && and || between terms
//! .# .length                at the end: how many (an array's elements,
//!                           an object's keys, a string's characters, or
//!                           the matches of a wildcard, slice, filter or
//!                           descent); a number, boolean or null has no
//!                           count; in the middle, # maps over an array,
//!                           so items.#.tags.# is each item's count
//! .0                        a numeric key on an array is an index
//! ```
//!
//! A path that goes through a wildcard, slice, filter or descent selects
//! every match; the value is then a JSON array of them, and no matches
//! means nothing is there.

use std::cmp::Ordering;

use apic_gocompat::quote;
use regex::Regex;

use crate::json::{Node, Type};
use crate::number::Number;
use crate::regexp::compile_regex;

#[derive(Clone, Debug)]
enum Segment {
    /// A key.
    Child { name: String },
    /// `[n]`
    Index(i64),
    /// `[a:b]`
    Slice { lo: Option<i64>, hi: Option<i64> },
    /// `*`: every element or value.
    Wild,
    /// `#`: count at the end, map in the middle.
    Hash,
    /// `.length`: a key when the object has one, else count.
    Length,
    /// `..` followed by an inner segment.
    Descend(Box<Segment>),
    /// `[?(...)]`
    Filter(OrExpr),
}

/// A parsed body path.
#[derive(Clone, Debug)]
pub struct Path {
    segs: Vec<Segment>,
    /// A filter's `@` path, where `.length` on an object is only its own
    /// key: `[?(@.length)]` asks whether the key is there.
    relative: bool,
}

/// What a path selected in a document.
pub(crate) struct Selection<'a> {
    pub nodes: Vec<Node<'a>>,
    /// Set when the path ended in a count.
    pub count: Option<usize>,
    /// The path went through a wildcard, slice, filter or descent, so the
    /// matches are a set.
    pub many: bool,
}

/// Parses the tail of a `body.$` selector: `.items[0].id`, `..id`,
/// `["key"]`. An error names the construct it could not read.
pub fn parse_path(tail: &str) -> Result<Path, String> {
    let mut segs = Vec::new();
    let b = tail.as_bytes();
    let mut i = 0;
    while i < tail.len() {
        if tail[i..].starts_with("..") {
            i += 2;
            let (inner, n) = parse_step(&tail[i..])?;
            if matches!(inner, Segment::Hash | Segment::Descend(_)) {
                return Err(format!(
                    "unsupported selector part {}: .. must be followed by a key, *, an index or a filter",
                    quote(&format!("..{}", &tail[i..i + n]))
                ));
            }
            segs.push(Segment::Descend(Box::new(inner)));
            i += n;
        } else if b[i] == b'.' {
            i += 1;
            let (seg, n) = parse_step(&tail[i..])?;
            segs.push(seg);
            i += n;
        } else if b[i] == b'[' {
            let (seg, n) = parse_bracket(&tail[i..])?;
            segs.push(seg);
            i += n;
        } else {
            return Err(format!(
                "unsupported selector part {}: expected . or [ before it",
                quote(&tail[i..])
            ));
        }
    }
    Ok(Path {
        segs,
        relative: false,
    })
}

/// Reads what follows a `.` or `..`: a bracket, `*`, `#`, or a bare key up
/// to the next `.` or `[`.
fn parse_step(s: &str) -> Result<(Segment, usize), String> {
    if s.is_empty() {
        return Err(format!(
            "unsupported selector part {}: a path cannot end with a dot",
            quote(".")
        ));
    }
    if s.starts_with('[') {
        return parse_bracket(s);
    }
    let end = s.find(['.', '[']).unwrap_or(s.len());
    let name = &s[..end];
    match name {
        "" => {
            return Err(format!(
                "unsupported selector part {}: empty key",
                quote(&format!(".{s}"))
            ))
        }
        "*" => return Ok((Segment::Wild, end)),
        "#" => return Ok((Segment::Hash, end)),
        "length" => return Ok((Segment::Length, end)),
        _ => {}
    }
    if name.starts_with("#(") {
        return Err(format!(
            "unsupported selector part {}: gjson queries are not supported; use a filter such as [?(@.id == 2)]",
            quote(s)
        ));
    }
    Ok((
        Segment::Child {
            name: name.to_string(),
        },
        end,
    ))
}

fn atoi(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Reads one `[...]`, respecting quotes and regex literals inside it, and
/// returns the segment and the bytes consumed.
fn parse_bracket(s: &str) -> Result<(Segment, usize), String> {
    let Some(end) = closing_bracket(s) else {
        return Err(format!(
            "unsupported selector part {}: no closing ]",
            quote(s)
        ));
    };
    let text = &s[..=end];
    let inner = s[1..end].trim();
    let ib = inner.as_bytes();
    if inner == "*" {
        return Ok((Segment::Wild, end + 1));
    }
    if inner == "#" {
        return Ok((Segment::Hash, end + 1));
    }
    if let Some(expr) = inner.strip_prefix('?') {
        let mut expr = expr.trim();
        if expr.starts_with('(') && expr.ends_with(')') {
            expr = &expr[1..expr.len() - 1];
        }
        let f =
            parse_filter(expr).map_err(|e| format!("unsupported filter {}: {}", quote(text), e))?;
        return Ok((Segment::Filter(f), end + 1));
    }
    if ib.len() >= 2 && (ib[0] == b'"' || ib[0] == b'\'') {
        let Some(name) = unquote(inner) else {
            return Err(format!(
                "unsupported selector part {}: a quoted key must be one string",
                quote(text)
            ));
        };
        return Ok((Segment::Child { name }, end + 1));
    }
    if inner.contains(',') {
        return Err(format!(
            "unsupported selector part {}: unions are not supported; select each value on its own",
            quote(text)
        ));
    }
    if inner.contains(':') {
        let parts: Vec<&str> = inner.split(':').collect();
        if parts.len() > 2 {
            return Err(format!(
                "unsupported selector part {}: slice steps are not supported",
                quote(text)
            ));
        }
        let (mut lo, mut hi) = (None, None);
        for (i, part) in parts.iter().enumerate() {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let Some(n) = atoi(part) else {
                return Err(format!(
                    "unsupported selector part {}: slice bounds must be integers",
                    quote(text)
                ));
            };
            if i == 0 {
                lo = Some(n);
            } else {
                hi = Some(n);
            }
        }
        return Ok((Segment::Slice { lo, hi }, end + 1));
    }
    match atoi(inner) {
        Some(n) => Ok((Segment::Index(n), end + 1)),
        None => Err(format!(
            "unsupported selector part {}: expected an index, a slice, a quoted key, * or a filter",
            quote(text)
        )),
    }
}

/// Splits an expression such as `body.$.items[?(@.done == true)].id == 2`
/// into the selector at its start and the rest, so spaces inside a bracket
/// (a filter, a quoted key) stay part of the selector.
pub fn leading(s: &str) -> (&str, &str) {
    let s = s.trim_start_matches([' ', '\t']);
    let b = s.as_bytes();
    let mut out = (s, "");
    scan(s, false, |i, depth| {
        if depth == 0 && (b[i] == b' ' || b[i] == b'\t') {
            out = (&s[..i], s[i..].trim_start_matches([' ', '\t']));
            return false;
        }
        true
    });
    out
}

/// Finds the `]` closing the `[` at the start of `s`, skipping nested
/// brackets, quoted strings and the `/regex/` of a filter's `=~`.
fn closing_bracket(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut end = None;
    scan(s, true, |i, depth| {
        if b[i] == b']' && depth == 1 {
            end = Some(i);
            return false;
        }
        true
    });
    end
}

/// Calls `visit` with each byte of `s` that is not inside a quoted string
/// or the `/regex/` of an `=~`, and the bracket depth before it; `visit`
/// returns false to stop. Quotes and regexes count only inside a bracket
/// unless `top` is set (a filter's text, whose brackets were stripped), so
/// a bare key such as it's stays a key.
fn scan(s: &str, top: bool, mut visit: impl FnMut(usize, i64) -> bool) {
    let b = s.as_bytes();
    let mut quote = 0u8;
    let mut in_regex = false;
    let mut depth = 0i64;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == b'\\' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
            i += 1;
            continue;
        }
        if in_regex {
            if c == b'\\' {
                i += 1;
            } else if c == b'/' {
                in_regex = false;
            }
            i += 1;
            continue;
        }
        if (depth > 0 || top) && (c == b'"' || c == b'\'') {
            quote = c;
            i += 1;
            continue;
        }
        if (depth > 0 || top) && c == b'/' && s[..i].trim_end_matches(' ').ends_with("=~") {
            in_regex = true;
            i += 1;
            continue;
        }
        if !visit(i, depth) {
            return;
        }
        match c {
            b'[' => depth += 1,
            b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
}

/// The character a one-letter escape stands for.
fn simple_escape(e: char) -> Option<char> {
    match e {
        'a' => Some('\u{07}'),
        'b' => Some('\u{08}'),
        'f' => Some('\u{0c}'),
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        'v' => Some('\u{0b}'),
        '\\' => Some('\\'),
        '"' => Some('"'),
        _ => None,
    }
}

/// Reads a double-quoted string with the escapes of a string literal:
/// `\n`, `\t`, `\\`, `\"`, `\xNN`, `\uNNNN`, `\UNNNNNNNN` and octal `\NNN`.
fn unquote_double(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut bytes: Vec<u8> = Vec::new();
    let flush = |bytes: &mut Vec<u8>, out: &mut String| {
        if !bytes.is_empty() {
            out.push_str(&String::from_utf8_lossy(bytes));
            bytes.clear();
        }
    };
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\n' => return None,
            '\\' => {}
            c => {
                flush(&mut bytes, &mut out);
                out.push(c);
                continue;
            }
        }
        let e = chars.next()?;
        if let Some(ch) = simple_escape(e) {
            flush(&mut bytes, &mut out);
            out.push(ch);
            continue;
        }
        let mut take = |n: usize, radix: u32| -> Option<u32> {
            let mut v = 0u32;
            for _ in 0..n {
                v = v
                    .checked_mul(radix)?
                    .checked_add(chars.next()?.to_digit(radix)?)?;
            }
            Some(v)
        };
        match e {
            'x' => bytes.push(take(2, 16)? as u8),
            'u' | 'U' => {
                let v = take(if e == 'u' { 4 } else { 8 }, 16)?;
                flush(&mut bytes, &mut out);
                out.push(char::from_u32(v)?);
            }
            '0'..='7' => {
                let mut v = e.to_digit(8)?;
                for _ in 0..2 {
                    v = v * 8 + chars.next()?.to_digit(8)?;
                }
                if v > 255 {
                    return None;
                }
                bytes.push(v as u8);
            }
            _ => return None,
        }
    }
    flush(&mut bytes, &mut out);
    Some(out)
}

fn unquote(s: &str) -> Option<String> {
    let b = s.as_bytes();
    if b.len() < 2 || b[b.len() - 1] != b[0] {
        return None;
    }
    let body = &s[1..s.len() - 1];
    if b[0] == b'"' {
        return unquote_double(body);
    }
    if body.replace("\\'", "").contains('\'') {
        return None;
    }
    Some(body.replace("\\'", "'").replace("\\\\", "\\"))
}

impl Path {
    /// Evaluates the path on a JSON document.
    pub(crate) fn eval<'a>(&self, doc: Node<'a>) -> Selection<'a> {
        let mut nodes = vec![doc];
        let mut many = false;
        let mut mapped = false; // through a # in the middle: items.#.id
        for (i, seg) in self.segs.iter().enumerate() {
            let last = i == self.segs.len() - 1;
            match seg {
                Segment::Hash => {
                    if last {
                        return self.count(nodes, many, mapped);
                    }
                    nodes = flat_map(nodes, |r| elements(&r));
                    many = true;
                    mapped = true;
                }
                Segment::Length => {
                    // An object's own length key wins, and in a filter an
                    // object's .length is only ever that key.
                    let key = nodes.len() == 1
                        && !many
                        && (child(&nodes[0], "length").len() == 1
                            || self.relative && nodes[0].is_object());
                    if last && !key {
                        return self.count(nodes, many, mapped);
                    }
                    nodes = flat_map(nodes, |r| child(&r, "length"));
                }
                Segment::Child { name } => nodes = flat_map(nodes, |r| child(&r, name)),
                Segment::Index(n) => nodes = flat_map(nodes, |r| index(&r, *n)),
                Segment::Slice { lo, hi } => {
                    nodes = flat_map(nodes, |r| slice(&r, *lo, *hi));
                    many = true;
                }
                Segment::Wild => {
                    nodes = flat_map(nodes, |r| elements(&r));
                    many = true;
                }
                Segment::Filter(f) => {
                    nodes = flat_map(nodes, |r| filter(&r, f));
                    many = true;
                }
                Segment::Descend(inner) => {
                    nodes = flat_map(nodes, |r| {
                        let mut out = Vec::new();
                        descend(r, &mut |d| out.extend(step(d, inner)));
                        out
                    });
                    many = true;
                }
            }
            if nodes.is_empty() {
                let counting = self.segs.last().is_some_and(is_count);
                if i + 1 < self.segs.len() && counting && many && !mapped {
                    // Counting matches that turned out empty is 0, not absent.
                    return Selection {
                        nodes: Vec::new(),
                        count: Some(0),
                        many,
                    };
                }
                return Selection {
                    nodes: Vec::new(),
                    count: None,
                    many,
                };
            }
        }
        Selection {
            nodes,
            count: None,
            many,
        }
    }

    /// Ends a path with `#` or `.length`: the number of matches of a set,
    /// each node's own count after a mapping `#`, or the one node's count.
    fn count<'a>(&self, nodes: Vec<Node<'a>>, many: bool, mapped: bool) -> Selection<'a> {
        if mapped {
            let out: Vec<Node<'a>> = nodes
                .iter()
                .filter_map(|n| self.count_one(n))
                .map(Node::number)
                .collect();
            return Selection {
                nodes: out,
                count: None,
                many: true,
            };
        }
        if many {
            return Selection {
                count: Some(nodes.len()),
                nodes: Vec::new(),
                many: true,
            };
        }
        Selection {
            nodes: Vec::new(),
            count: self.count_one(&nodes[0]),
            many: false,
        }
    }

    /// An array's elements, an object's keys or a string's characters; a
    /// number, boolean or null has no count. In a filter an object's keys
    /// are not counted, so `@.length` is the key alone.
    fn count_one(&self, r: &Node<'_>) -> Option<usize> {
        if r.is_array() || r.is_object() && !self.relative {
            return Some(size(r));
        }
        if r.kind == Type::String {
            return Some(r.str.chars().count());
        }
        None
    }
}

fn is_count(s: &Segment) -> bool {
    matches!(s, Segment::Hash | Segment::Length)
}

/// Applies one non-descent segment to one node, for `..`.
fn step<'a>(r: &Node<'a>, seg: &Segment) -> Vec<Node<'a>> {
    match seg {
        Segment::Child { name } => child(r, name),
        Segment::Length => child(r, "length"),
        Segment::Index(n) => index(r, *n),
        Segment::Slice { lo, hi } => slice(r, *lo, *hi),
        Segment::Wild => elements(r),
        Segment::Filter(f) => filter(r, f),
        Segment::Hash | Segment::Descend(_) => Vec::new(),
    }
}

fn flat_map<'a>(
    nodes: Vec<Node<'a>>,
    mut f: impl FnMut(Node<'a>) -> Vec<Node<'a>>,
) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    for n in nodes {
        out.extend(f(n));
    }
    out
}

/// The value under `key` in an object, compared exactly, so dots and stars
/// in keys are literal. On an array a key of digits is an index, as
/// `items.0` is.
fn child<'a>(r: &Node<'a>, key: &str) -> Vec<Node<'a>> {
    if r.is_array() {
        if !key.starts_with('+') {
            if let Some(i) = atoi(key).filter(|&i| i >= 0) {
                return index(r, i);
            }
        }
        return Vec::new();
    }
    if !r.is_object() {
        return Vec::new();
    }
    let mut out = Vec::new();
    r.for_each(|k, v| {
        if k.as_deref() == Some(key) {
            out.push(v);
            return false;
        }
        true
    });
    out
}

/// How many elements an array has, without keeping them.
fn size(r: &Node<'_>) -> usize {
    let mut n = 0;
    r.for_each(|_, _| {
        n += 1;
        true
    });
    n
}

/// Walks to element `i` and stops there; a negative `i` counts first.
fn index<'a>(r: &Node<'a>, i: i64) -> Vec<Node<'a>> {
    if !r.is_array() {
        return Vec::new();
    }
    let mut i = i;
    if i < 0 {
        i += size(r) as i64;
        if i < 0 {
            return Vec::new();
        }
    }
    let mut out = Vec::new();
    let mut n = 0;
    r.for_each(|_, v| {
        if n == i {
            out.push(v);
            return false;
        }
        n += 1;
        true
    });
    out
}

/// Walks elements a to b and stops at b; only negative or omitted bounds
/// that need the length count first.
fn slice<'a>(r: &Node<'a>, lo: Option<i64>, hi: Option<i64>) -> Vec<Node<'a>> {
    if !r.is_array() {
        return Vec::new();
    }
    let mut cached: Option<i64> = None;
    let mut length = || *cached.get_or_insert_with(|| size(r) as i64);
    let a = match lo {
        None => 0,
        Some(v) if v < 0 => (v + length()).max(0),
        Some(v) => v,
    };
    let b = match hi {
        None => length(),
        Some(v) if v < 0 => (v + length()).max(0),
        Some(v) => v,
    };
    if a >= b {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    r.for_each(|_, v| {
        if i >= a {
            out.push(v);
        }
        i += 1;
        i < b
    });
    out
}

fn elements<'a>(r: &Node<'a>) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    r.for_each(|_, v| {
        out.push(v);
        true
    });
    out
}

/// Visits `r` and everything under it, in document order, without
/// collecting the whole tree first.
fn descend<'a>(r: Node<'a>, visit: &mut dyn FnMut(&Node<'a>)) {
    visit(&r);
    if r.is_array() || r.is_object() {
        r.for_each(|_, v| {
            descend(v, visit);
            true
        });
    }
}

fn filter<'a>(r: &Node<'a>, f: &OrExpr) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    r.for_each(|_, e| {
        if f.matches(&e) {
            out.push(e);
        }
        true
    });
    out
}

// Filters: terms joined by && (binding tighter) and ||.

#[derive(Clone, Debug)]
struct OrExpr(Vec<Vec<Term>>);

#[derive(Clone, Debug)]
struct Term {
    not: bool,
    /// Relative to `@`.
    path: Path,
    /// Empty for a presence test.
    op: &'static str,
    value: Literal,
}

#[derive(Clone, Debug)]
enum Literal {
    None,
    String(String),
    Number(Number),
    True,
    False,
    Null,
    Regex(Regex),
}

impl OrExpr {
    fn matches(&self, r: &Node<'_>) -> bool {
        self.0.iter().any(|and| and.iter().all(|t| t.matches(r)))
    }
}

impl Term {
    fn matches(&self, r: &Node<'_>) -> bool {
        let sel = self.path.eval(r.clone());
        let nodes = match sel.count {
            Some(n) => vec![Node::number(n)],
            None => sel.nodes,
        };
        let ok = if self.op.is_empty() {
            !nodes.is_empty()
        } else if nodes.is_empty() {
            // Nothing there is not equal to anything (RFC 9535), and no
            // other comparison holds for it.
            self.op == "!="
        } else {
            nodes
                .iter()
                .any(|n| compare_literal(n, self.op, &self.value))
        };
        ok != self.not
    }
}

fn compare_literal(n: &Node<'_>, op: &str, lit: &Literal) -> bool {
    if op == "=~" {
        return match lit {
            Literal::Regex(re) => n.kind == Type::String && re.is_match(&n.str),
            _ => false,
        };
    }
    // None when the two cannot be compared at all.
    let cmp: Option<Ordering> = match (lit, n.kind) {
        (Literal::Number(want), Type::Number) => Number::parse(&n.raw).map(|v| v.cmp(want)),
        (Literal::String(want), Type::String) => Some(n.str.as_bytes().cmp(want.as_bytes())),
        (Literal::True, Type::True)
        | (Literal::False, Type::False)
        | (Literal::Null, Type::Null) => Some(Ordering::Equal),
        _ => None,
    };
    // Only numbers and strings have an order.
    let ordered = matches!(lit, Literal::Number(_) | Literal::String(_));
    match op {
        "==" => cmp == Some(Ordering::Equal),
        "!=" => cmp != Some(Ordering::Equal),
        "<" => ordered && cmp == Some(Ordering::Less),
        "<=" => ordered && matches!(cmp, Some(Ordering::Less | Ordering::Equal)),
        ">" => ordered && cmp == Some(Ordering::Greater),
        ">=" => ordered && matches!(cmp, Some(Ordering::Greater | Ordering::Equal)),
        _ => false,
    }
}

const FILTER_OPS: [&str; 7] = ["==", "!=", "<=", ">=", "=~", "<", ">"];

fn parse_filter(s: &str) -> Result<OrExpr, String> {
    if s.trim().is_empty() {
        return Err("the filter is empty".to_string());
    }
    let mut or = Vec::new();
    for alt in split_outside(s, "||") {
        let mut and = Vec::new();
        for part in split_outside(alt, "&&") {
            and.push(parse_term(part.trim())?);
        }
        or.push(and);
    }
    Ok(OrExpr(or))
}

/// Splits `s` on `sep` where `sep` is not inside a bracket, quotes or a
/// regex.
fn split_outside<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut start = 0;
    scan(s, true, |i, depth| {
        if depth == 0 && i >= start && s.as_bytes()[i..].starts_with(sep.as_bytes()) {
            parts.push(&s[start..i]);
            start = i + sep.len();
        }
        true
    });
    parts.push(&s[start..]);
    parts
}

fn parse_term(s: &str) -> Result<Term, String> {
    let mut not = false;
    let mut s = s;
    if let Some(rest) = s.strip_prefix('!') {
        not = true;
        s = rest.trim();
    }
    if s.starts_with('(') {
        return Err("parentheses inside a filter are not supported".to_string());
    }
    if !s.starts_with('@') {
        return Err(format!("a term must start with @, got {}", quote(s)));
    }
    // The path runs to the first operator outside a bracket.
    let b = s.as_bytes();
    let mut path_end = s.len();
    let mut op = "";
    let mut bad = false;
    scan(s, true, |i, depth| {
        if i == 0 || depth > 0 || !b" =!<>".contains(&b[i]) {
            return true;
        }
        let rest = s[i..].trim_start_matches(' ');
        for o in FILTER_OPS {
            if rest.starts_with(o) {
                path_end = i;
                op = o;
                return false;
            }
        }
        bad = b[i] != b' ';
        !bad
    });
    if bad {
        return Err(format!("unknown operator in {}", quote(s)));
    }
    let mut path = parse_path(s[1..path_end].trim())?;
    path.relative = true;
    if op.is_empty() {
        if s[1..].trim() != s[1..path_end].trim() {
            return Err(format!("expected an operator in {}", quote(s)));
        }
        return Ok(Term {
            not,
            path,
            op,
            value: Literal::None,
        });
    }
    if not {
        return Err("! only negates a presence test such as !@.done".to_string());
    }
    let raw = s[path_end..].trim_start_matches(' ')[op.len()..].trim();
    let value = parse_literal(raw, op == "=~")?;
    Ok(Term {
        not,
        path,
        op,
        value,
    })
}

fn parse_literal(s: &str, regex: bool) -> Result<Literal, String> {
    let b = s.as_bytes();
    if regex {
        if b.len() < 2 || b[0] != b'/' {
            return Err(format!("=~ needs a /regex/, got {}", quote(s)));
        }
        let end = s.rfind('/').unwrap_or(0);
        if end == 0 {
            return Err(format!("=~ needs a closing / in {}", quote(s)));
        }
        let (pattern, flags) = (&s[1..end], &s[end + 1..]);
        if !flags.is_empty() && flags != "i" {
            return Err(format!("the only regex flag is i, got {}", quote(flags)));
        }
        let pattern = if flags == "i" {
            format!("(?i){pattern}")
        } else {
            pattern.to_string()
        };
        let re = compile_regex(&pattern).map_err(|e| format!("bad regex {}: {}", quote(s), e))?;
        return Ok(Literal::Regex(re));
    }
    match s {
        "true" => return Ok(Literal::True),
        "false" => return Ok(Literal::False),
        "null" => return Ok(Literal::Null),
        _ => {}
    }
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') {
        return match unquote(s) {
            Some(v) => Ok(Literal::String(v)),
            None => Err(format!("bad string {s}")),
        };
    }
    if let Some(n) = Number::parse(s) {
        return Ok(Literal::Number(n));
    }
    Err(format!(
        "expected a number, a quoted string, true, false or null, got {}",
        quote(s)
    ))
}
