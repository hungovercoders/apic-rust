use std::sync::LazyLock;

use apic_gocompat::{check_json, indent_json};
use regex::Regex;

use crate::parse::{
    group, is_save_line, RE_COMMENT, RE_DIRECTIVE, RE_FILE_VAR, RE_HEADER, RE_REQUEST_LINE,
    RE_SEPARATOR,
};

/// Rewrites `.http` content in its canonical form. It works on the lines
/// rather than the AST so that everything the parser skips (comments,
/// unknown directives, editor script blocks) is kept verbatim:
///
///   - one blank line between blocks, `### Title` on its own line;
///   - directives in a fixed order ([`directive_order`]: name,
///     description, disabled, step, auth, ref, forceRef, sleep, retry,
///     timeout, no-redirect, no-session, no-cookies, assert, capture, then
///     unknown ones as written), each `# @key value`; comments keep their
///     place among the directives;
///   - `@name = value` file variables with single spaces;
///   - the request line with single spaces, query continuations indented
///     four spaces, header names in canonical case;
///   - a JSON body pretty-printed with two spaces when it parses and holds
///     no `{{placeholders}}`; every other body byte for byte as written,
///     since the body is what the request sends;
///   - trailing whitespace removed outside bodies, blank lines after a
///     body dropped, one final newline.
///
/// Formatting is idempotent: `format(&format(s)) == format(s)`.
pub fn format(src: &str) -> String {
    let src = src.replace("\r\n", "\n");
    let mut out: Vec<String> = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let mut title = String::new();
    let mut implicit = true; // the text before the first separator has no title line
    let mut flush = |title: &str, block: &[&str], implicit: bool| {
        let formatted = format_block(title, block, implicit);
        if !formatted.is_empty() {
            if !out.is_empty() {
                out.push(String::new());
            }
            out.extend(formatted);
        }
    };
    for line in src.split('\n') {
        if let Some(m) = RE_SEPARATOR.captures(line) {
            flush(&title, &block, implicit);
            implicit = false;
            title = m[1].trim().to_string();
            block.clear();
            continue;
        }
        block.push(line);
    }
    flush(&title, &block, implicit);
    // One final newline, none before it.
    trim_trailing_blank(&mut out);
    if out.is_empty() {
        return String::new();
    }
    out.join("\n") + "\n"
}

/// Orders the known directives; unknown ones come after, in the order
/// they were written.
const DIRECTIVE_RANK: &[&str] = &[
    "name",
    "description",
    "disabled",
    "step",
    "auth",
    "ref",
    "forceRef",
    "sleep",
    "retry",
    "timeout",
    "no-redirect",
    "no-session",
    "no-cookies",
    "assert",
    "capture",
];

/// The order [`format()`] writes the known directives in, so the help and
/// the docs that list it can be checked against the formatter.
pub fn directive_order() -> Vec<&'static str> {
    DIRECTIVE_RANK.to_vec()
}

static RE_BODY_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^<@?[\t\n\f\r ]+").expect("a valid pattern"));

fn trim_blanks_end(s: &str) -> &str {
    s.trim_end_matches([' ', '\t'])
}

/// Formats one `###` block; `implicit` is the text before the first
/// separator, which has no title line.
fn format_block(title: &str, lines: &[&str], implicit: bool) -> Vec<String> {
    let mut out = Vec::new();
    if !implicit {
        if title.is_empty() {
            out.push("###".to_string());
        } else {
            out.push(format!("### {title}"));
        }
    }
    let (preamble, i) = format_preamble(lines);
    out.extend(preamble);
    if i >= lines.len() {
        trim_trailing_blank(&mut out);
        return out;
    }
    let (req_lines, i) = format_request_line(lines, i);
    out.extend(req_lines);
    let (headers, i, saw_blank) = format_headers(lines, i);
    out.extend(headers.iter().cloned());
    if !saw_blank {
        trim_trailing_blank(&mut out);
        return out;
    }
    let body = trim_blank_lines(&lines[i..]);
    if body.is_empty() {
        trim_trailing_blank(&mut out);
        return out;
    }
    out.extend(format_body(body, &headers));
    out
}

/// A line of the preamble, or a directive with the comment lines written
/// just above it.
struct PreambleItem {
    /// The comments above, then the directive itself.
    lines: Vec<String>,
    directive: bool,
    rank: usize,
}

impl PreambleItem {
    fn line(line: String) -> Self {
        PreambleItem {
            lines: vec![line],
            directive: false,
            rank: 0,
        }
    }
}

/// Formats the comments, directives and file variables up to the request
/// line, with blank lines dropped, and returns the index of the line
/// after them. A directive carries the comment lines written just above
/// it, so the two move together when directives are reordered; a file
/// variable, or a comment with no directive after it, stays where it is.
fn format_preamble(lines: &[&str]) -> (Vec<String>, usize) {
    let mut preamble: Vec<PreambleItem> = Vec::new();
    let mut comments: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() {
            i += 1;
            continue;
        }
        if let Some(m) = RE_FILE_VAR.captures(t) {
            preamble.extend(comments.drain(..).map(PreambleItem::line));
            preamble.push(PreambleItem::line(format!("@{} = {}", &m[1], m[2].trim())));
            i += 1;
            continue;
        }
        let Some(m) = RE_COMMENT.captures(t) else {
            break;
        };
        if let Some(d) = RE_DIRECTIVE.captures(m[1].trim()) {
            let item = directive_item(std::mem::take(&mut comments), &d[1], group(&d, 2).trim());
            preamble.push(item);
            i += 1;
            continue;
        }
        comments.push(trim_blanks_end(lines[i]).to_string());
        i += 1;
    }
    preamble.extend(comments.drain(..).map(PreambleItem::line));
    (sort_directives(preamble), i)
}

/// A directive in its canonical spelling, carrying the comments above it.
fn directive_item(mut comments: Vec<String>, key: &str, value: &str) -> PreambleItem {
    let mut line = format!("# @{key}");
    if !value.is_empty() {
        line.push(' ');
        line.push_str(value);
    }
    let rank = DIRECTIVE_RANK
        .iter()
        .position(|k| *k == key)
        .unwrap_or(DIRECTIVE_RANK.len());
    comments.push(line);
    PreambleItem {
        lines: comments,
        directive: true,
        rank,
    }
}

/// Sorts the directives among themselves and puts them back into the
/// slots they occupied; the other items keep their places.
fn sort_directives(preamble: Vec<PreambleItem>) -> Vec<String> {
    let slots: Vec<bool> = preamble.iter().map(|it| it.directive).collect();
    let (mut directives, others): (Vec<_>, Vec<_>) =
        preamble.into_iter().partition(|it| it.directive);
    directives.sort_by_key(|it| it.rank); // stable: equal ranks keep their order
    let mut directives = directives.into_iter();
    let mut others = others.into_iter();
    let mut out = Vec::new();
    for directive in slots {
        let next = if directive {
            directives.next()
        } else {
            others.next()
        };
        if let Some(item) = next {
            out.extend(item.lines);
        }
    }
    out
}

/// Whether the line at `lines[i]` continues the query of the request line
/// above it: indented, and starting with `?` or `&`.
fn query_continuation(line: &str) -> Option<&str> {
    let t = line.trim();
    ((t.starts_with('?') || t.starts_with('&')) && line != t).then_some(t)
}

/// Formats the request line at `lines[i]` and the indented query
/// continuation lines after it.
fn format_request_line(lines: &[&str], mut i: usize) -> (Vec<String>, usize) {
    let line = lines[i].trim();
    let req_line = match RE_REQUEST_LINE.captures(line) {
        Some(m) => {
            let mut s = format!("{} {}", &m[1], &m[2]);
            let version = group(&m, 3);
            if !version.is_empty() {
                s.push(' ');
                s.push_str(version);
            }
            s
        }
        None => line.split_whitespace().collect::<Vec<_>>().join(" "),
    };
    let mut out = vec![req_line];
    i += 1;
    while i < lines.len() {
        let Some(t) = query_continuation(lines[i]) else {
            break;
        };
        out.push(format!("    {t}"));
        i += 1;
    }
    (out, i)
}

/// Writes a header name in its canonical case: the first letter and every
/// letter after a hyphen upper-cased, the rest lower-cased.
fn canonical_header_name(name: &str) -> String {
    let mut upper = true;
    name.chars()
        .map(|c| {
            let out = if upper {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            };
            upper = c == '-';
            out
        })
        .collect()
}

/// Formats the headers until the first blank line, keeping the comments
/// among them, and returns the index after that blank line and whether
/// there was one.
fn format_headers(lines: &[&str], mut i: usize) -> (Vec<String>, usize, bool) {
    let mut headers = Vec::new();
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() {
            return (headers, i + 1, true);
        }
        if RE_COMMENT.is_match(t) {
            headers.push(t.to_string());
        } else if let Some(m) = RE_HEADER.captures(t) {
            headers.push(format!("{}: {}", canonical_header_name(&m[1]), m[2].trim()));
        } else {
            headers.push(trim_blanks_end(lines[i]).to_string()); // the parser will report it
        }
        i += 1;
    }
    (headers, i, false)
}

/// Drops the blank lines around a body (the parser drops those too).
fn trim_blank_lines<'a, 'b>(mut body: &'b [&'a str]) -> &'b [&'a str] {
    while let Some((last, rest)) = body.split_last() {
        if !last.trim().is_empty() {
            break;
        }
        body = rest;
    }
    while let Some((first, rest)) = body.split_first() {
        if !first.trim().is_empty() {
            break;
        }
        body = rest;
    }
    body
}

/// Keeps the body byte for byte, except that a plain JSON document is
/// pretty-printed. `>> file` lines at the end say where the response
/// goes; they are not body text, so the JSON before them is still
/// pretty-printed and they follow it after one blank line.
fn format_body(mut body: &[&str], headers: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut saves: Vec<String> = Vec::new();
    while let Some((last, rest)) = body.split_last() {
        let t = last.trim();
        if t.is_empty() {
            body = rest;
            continue;
        }
        if !is_save_line(t) {
            break;
        }
        saves.insert(0, t.to_string());
        body = rest;
    }
    if !body.is_empty() {
        out.push(String::new());
        match pretty_json(&body.join("\n"), headers) {
            Some(pretty) => out.extend(pretty.split('\n').map(str::to_string)),
            None => out.extend(body.iter().map(|l| l.to_string())),
        }
    }
    if !saves.is_empty() {
        out.push(String::new());
        out.extend(saves);
    }
    out
}

/// Re-indents a body that is one JSON document without placeholders or
/// handler blocks, under a JSON content type or none.
fn pretty_json(body: &str, headers: &[String]) -> Option<String> {
    let t = body.trim();
    if t.is_empty() || RE_BODY_FILE.is_match(t) || t.contains("{{") || t.contains("{%") {
        return None;
    }
    if !t.starts_with('{') && !t.starts_with('[') {
        return None;
    }
    for h in headers {
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("Content-Type")
                && !v.trim().to_lowercase().contains("json")
            {
                return None;
            }
        }
    }
    check_json(t.as_bytes())
        .is_ok()
        .then(|| indent_json(t, "", "  "))
}

fn trim_trailing_blank(lines: &mut Vec<String>) {
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
}

/// Formats one request of a file and leaves every other byte as written:
/// the request whose `# @name` is `name`, or the n-th request of the file
/// when `name` is a number, counted as the parser and so `apic run
/// file.http#3` count them (the text before the first `###` is a request
/// when it holds a request line; a `###` block without one is not). It
/// returns `None` when nothing matches. It is for an agent (or an editor)
/// that added or changed one request in a file it does not own, so the
/// change stays that request's.
pub fn format_request(src: &str, name: &str) -> Option<String> {
    // The source is split as it is, so the lines around the block keep
    // their bytes, CRLF included; the formatted block is given the file's
    // line ending so the result stays one kind.
    let lines: Vec<&str> = src.split('\n').collect();
    let crlf = src.contains("\r\n");
    // Blocks as the parser sees them: the implicit one before the first
    // separator, then one per `###` line, each running to the line before
    // the next separator, trailing blank lines included.
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for (i, line) in lines.iter().enumerate() {
        if RE_SEPARATOR.is_match(line) {
            blocks.push((start, i));
            start = i;
        }
    }
    blocks.push((start, lines.len()));
    let want = name.parse::<i64>().ok();
    let mut n = 0i64;
    for (start, end) in blocks {
        let block = &lines[start..end];
        if !has_request_line(block) {
            continue;
        }
        n += 1;
        let matches = match want {
            Some(want) => n == want,
            None => block_named(block, name),
        };
        if !matches {
            continue;
        }
        let whole = format(&block.join("\n"));
        let mut formatted: Vec<String> = whole
            .strip_suffix('\n')
            .unwrap_or(&whole)
            .split('\n')
            .map(|l| {
                if crlf {
                    format!("{l}\r")
                } else {
                    l.to_string()
                }
            })
            .collect();
        // One blank line before the next block (a bare "\r" under CRLF, so
        // the join gives "\r\n"), or the file's final newline, the empty
        // last element of the split, when this block is last.
        if end < lines.len() && crlf {
            formatted.push("\r".to_string());
        } else if end < lines.len() || src.ends_with('\n') {
            formatted.push(String::new());
        }
        let mut out: Vec<&str> = lines[..start].to_vec();
        out.extend(formatted.iter().map(String::as_str));
        out.extend(&lines[end..]);
        return Some(out.join("\n"));
    }
    None
}

/// Reports whether a line is one the parser passes over before a request
/// line: blank, a file variable, or a comment (a `###` line among them).
fn preamble_line(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || RE_FILE_VAR.is_match(t) || RE_COMMENT.is_match(t)
}

/// Reports whether a block holds a request, as the parser decides it: a
/// line after the preamble.
fn has_request_line(lines: &[&str]) -> bool {
    lines.iter().any(|line| !preamble_line(line))
}

/// Reports whether a block's preamble carries `# @name name`.
fn block_named(lines: &[&str], name: &str) -> bool {
    for line in lines {
        if !preamble_line(line) {
            return false; // the request line: no more directives
        }
        let Some(m) = RE_COMMENT.captures(line.trim()) else {
            continue;
        };
        if let Some(d) = RE_DIRECTIVE.captures(m[1].trim()) {
            if &d[1] == "name" && group(&d, 2).trim() == name {
                return true;
            }
        }
    }
    false
}
