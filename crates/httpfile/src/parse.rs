use std::fmt;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use apic_gocompat::quote;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::multipart::file_reference;
use crate::{Assert, Capture, Directive, File, FileInfo, Header, Request, SaveTo, Var};

/// A non-fatal problem found while parsing or validating.
///
/// `line` and `column` are 1-based; `column` counts bytes from the start
/// of the line. `end_line` and `end_column` mark the end of the span,
/// exclusive, so a diagnostic on the word "nope" at column 12 has
/// `end_column` 16. The columns are zero when the problem has no useful
/// span (a whole file, or apic.yaml). `code` is a stable identifier from
/// [`CODES`] that tools can key fixes on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub path: String,
    pub line: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub column: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub end_line: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub end_column: usize,
    /// `"error"` or `"warning"`.
    pub severity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    pub message: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.column > 0 {
            write!(
                f,
                "{}:{}:{}: {}: {}",
                self.path, self.line, self.column, self.severity, self.message
            )
        } else {
            write!(
                f,
                "{}:{}: {}: {}",
                self.path, self.line, self.severity, self.message
            )
        }
    }
}

/// The diagnostic codes apic reports, with a one-line description each.
/// They are part of the `validate --json` contract: a code never changes
/// meaning, and new ones are only added. See [`code_description`] for a
/// lookup.
pub const CODES: &[(&str, &str)] = &[
    ("orphan-directives", "directives that are not followed by a request line"),
    ("bad-name", "`# @name` without a value"),
    ("bad-capture", "`# @capture` that is not `name = selector`"),
    ("bad-assert", "`# @assert` that is not `selector op value`"),
    ("bad-header", "a line in the header section that is not `Name: value`"),
    ("unknown-directive", "a `# @directive` apic does not know (ignored)"),
    ("editor-script", "an editor-only script or redirect block (skipped, not sent)"),
    ("duplicate-name", "a request name used more than once in the project"),
    ("bad-auth", "an `# @auth` spec that does not parse"),
    ("exec-disabled", "`# @auth exec` without auth.allowExec in apic.yaml"),
    ("bad-config-auth", "auth.default in apic.yaml does not parse"),
    ("bad-step", "a `# @step` phrase that does not parse"),
    ("ambiguous-step", "a `# @step` phrase that matches the same text as another step"),
    ("bad-ref", "a `# @ref` or `# @forceRef` whose target is not one request in the project"),
    ("ref-cycle", "a `# @ref` chain that leads back to the request it started from"),
    ("bad-retry", "a `# @retry` (or retry in apic.yaml) that is not `<attempts> [interval]`"),
    ("bad-sleep", "a `# @sleep` whose value is not a duration such as 500ms or 2s"),
    ("bad-http-version", "a request line whose HTTP version is not HTTP/1.1 or HTTP/2"),
    ("bad-auth-config", "a JetBrains `Security.Auth` configuration in an env file that apic cannot use (not OAuth2, the Implicit grant, a missing Token URL or Client ID)"),
    ("unknown-auth-key", "a field of a `Security.Auth` configuration that apic does not act on"),
    ("unknown-selector", "a selector that is not status, statusText, duration, header.*, cookie.*, body or body.$*, or a body path apic cannot read"),
    ("missing-body-file", "a `< file` body, or a `< file` part of a multipart body, whose file does not exist"),
    ("bad-multipart", "a multipart/form-data body without a boundary, or whose parts are not laid out between `--boundary` delimiters"),
    ("bad-graphql", "a GraphQL request (GRAPHQL method or X-REQUEST-TYPE: GraphQL) without a query, or whose variables block is not a JSON object"),
    ("bad-save-path", "a `>> file` after the body with no path, a path outside the project, or a second one on the same request"),
    ("missing-schema-file", "a `# @assert … matchesSchema <file>` whose schema file does not exist or lies outside the project"),
];

/// Returns the description of a diagnostic code, or `None` when apic does
/// not report it.
pub fn code_description(code: &str) -> Option<&'static str> {
    CODES.iter().find(|(c, _)| *c == code).map(|(_, d)| *d)
}

/// Returns the 1-based byte columns `[col, end)` of `sub` within `line`,
/// or zeros when `sub` is empty or not found. `after` is the offset to
/// start searching from, so a value that also appears in the directive
/// key is found in the right place.
pub fn span(line: &str, sub: &str, after: usize) -> (usize, usize) {
    if sub.is_empty() {
        return (0, 0);
    }
    let Some(i) = line.get(after..).and_then(|rest| rest.find(sub)) else {
        return (0, 0);
    };
    let col = after + i + 1;
    (col, col + sub.len())
}

/// [`span`] for an offset that came from a search of its own: no offset,
/// no span.
pub(crate) fn span_from(line: &str, sub: &str, after: Option<usize>) -> (usize, usize) {
    after.map_or((0, 0), |after| span(line, sub, after))
}

/// The `# @key` directives apic understands, with what each is for.
/// Others are reported as warnings by validate but otherwise ignored. See
/// [`known_directive`] and [`is_known_directive`] for lookups.
pub const KNOWN_DIRECTIVES: &[(&str, &str)] = &[
    ("name", "request name used on the command line"),
    ("description", "one-line description shown by list/describe"),
    ("capture", "store a response value: `# @capture name = selector`"),
    ("assert", "assert on the response: `# @assert selector op value`"),
    ("auth", "authentication: `# @auth bearer|basic|apikey|digest|aws|oauth2|exec|none ...`"),
    ("step", "Gherkin phrase that runs this request: `# @step a user named {name} exists`"),
    ("ref", "run another request first when a variable is missing: `# @ref login`"),
    ("forceRef", "run another request first, every time: `# @forceRef login`"),
    ("no-redirect", "do not follow redirects"),
    ("no-session", "do not persist captures from this request"),
    ("no-cookies", "send no cookies with this request and keep none it sets"),
    ("timeout", "per-request timeout, e.g. `10s`"),
    ("retry", "re-send until the assertions pass: `# @retry <attempts> [interval]`, e.g. `# @retry 10 2s`"),
    ("sleep", "wait before sending, e.g. `# @sleep 2s`"),
    ("disabled", "skip the request when its file runs as a flow; `apic run <name>` still sends it"),
    ("note", "free text, ignored (REST Client compatibility)"),
    ("prompt", "REST Client prompt, ignored (pass with --var instead)"),
];

/// Returns what a known directive is for, or `None` for one apic does not
/// understand.
pub fn known_directive(key: &str) -> Option<&'static str> {
    KNOWN_DIRECTIVES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, d)| *d)
}

/// Reports whether `key` is a directive apic understands.
pub fn is_known_directive(key: &str) -> bool {
    known_directive(key).is_some()
}

/// The name class `# @capture name = selector` accepts; a name given
/// elsewhere (`apic run --capture`) is held to the same one.
const CAPTURE_NAME: &str = r"[A-Za-z_][0-9A-Za-z_.-]*";

/// Reports whether `name` is one `# @capture` would accept.
pub fn valid_capture_name(name: &str) -> bool {
    RE_CAPTURE_NAME.is_match(name)
}

// The classes are spelt out in ASCII: a name is letters, digits and a few
// marks, and only the plain blanks separate things.
fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid pattern")
}

pub(crate) static RE_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| re(r"^###(.*)$"));
pub(crate) static RE_FILE_VAR: LazyLock<Regex> =
    LazyLock::new(|| re(r"^@([A-Za-z_][0-9A-Za-z_.-]*)[\t\n\f\r ]*=[\t\n\f\r ]*(.*)$"));
pub(crate) static RE_COMMENT: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:#|//)[\t\n\f\r ]?(.*)$"));
pub(crate) static RE_DIRECTIVE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^@([A-Za-z][0-9A-Za-z_-]*)(?:[\t\n\f\r ]+(.*))?$"));
pub(crate) static RE_REQUEST_LINE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^([A-Z]+)[\t\n\f\r ]+([^\t\n\f\r ].*?)(?:[\t\n\f\r ]+(HTTP/[0-9.]+))?[\t\n\f\r ]*$")
});
// Header names are RFC 7230 tokens, so X.Correlation-ID is valid. A
// leading `#` is the one exception: that line is a comment in this
// dialect, so the pattern does not claim it either.
pub(crate) static RE_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^([!$%\&'*+.^_`|\~0-9A-Za-z\-][!\#$%\&'*+.^_`|\~0-9A-Za-z\-]*):[\t\n\f\r ]*(.*)$")
});
static RE_CAPTURE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"^({CAPTURE_NAME})[\t\n\f\r ]*=[\t\n\f\r ]*(.+)$")));
static RE_CAPTURE_NAME: LazyLock<Regex> = LazyLock::new(|| re(&format!("^{CAPTURE_NAME}$")));

/// The text of an optional capture group, `""` when it did not take part.
pub(crate) fn group<'t>(m: &regex::Captures<'t>, i: usize) -> &'t str {
    m.get(i).map_or("", |g| g.as_str())
}

/// Reads and parses a `.http` file from disk. The path, as given, is the
/// one diagnostics and ids carry.
pub fn parse_file(path: impl AsRef<Path>) -> std::io::Result<(Arc<File>, Vec<Diagnostic>)> {
    let path = path.as_ref();
    let data = std::fs::read(path)?;
    Ok(parse(
        &path.to_string_lossy(),
        &String::from_utf8_lossy(&data),
    ))
}

/// Parses `.http` content. `path` is used only for diagnostics and ids.
pub fn parse(path: &str, content: &str) -> (Arc<File>, Vec<Diagnostic>) {
    let mut p = Parser {
        path,
        vars: Vec::new(),
        diags: Vec::new(),
    };
    let content = content.replace("\r\n", "\n");
    let lines: Vec<&str> = content.split('\n').collect();
    let requests = p.parse(&lines);
    let info = Arc::new(FileInfo {
        path: path.to_string(),
        vars: p.vars,
    });
    let requests = requests
        .into_iter()
        .map(|mut req| {
            req.file = Arc::clone(&info);
            Arc::new(req)
        })
        .collect();
    (Arc::new(File { info, requests }), p.diags)
}

struct Parser<'a> {
    path: &'a str,
    vars: Vec<Var>,
    diags: Vec<Diagnostic>,
}

#[derive(Default)]
struct Block<'a> {
    title: String,
    /// Raw lines, with their 1-based numbers in `nums`.
    lines: Vec<&'a str>,
    nums: Vec<usize>,
}

impl Parser<'_> {
    fn parse(&mut self, lines: &[&str]) -> Vec<Request> {
        // Split into blocks on `###`. The first block is implicit.
        let mut blocks = Vec::new();
        let mut cur = Block::default();
        for (i, line) in lines.iter().enumerate() {
            if let Some(m) = RE_SEPARATOR.captures(line) {
                blocks.push(std::mem::take(&mut cur));
                cur.title = m[1].trim().to_string();
                continue;
            }
            cur.lines.push(line);
            cur.nums.push(i + 1);
        }
        blocks.push(cur);

        let mut requests = Vec::new();
        for b in &blocks {
            if let Some(mut req) = self.parse_block(b) {
                req.index = requests.len() + 1;
                requests.push(req);
            }
        }
        requests
    }

    /// Records a diagnostic spanning `[col, end)` on `line`; zero columns
    /// mean the whole line.
    fn diag(
        &mut self,
        severity: &str,
        code: &str,
        line: usize,
        (col, end): (usize, usize),
        message: String,
    ) {
        let mut d = Diagnostic {
            path: self.path.to_string(),
            line,
            severity: severity.to_string(),
            code: code.to_string(),
            message,
            ..Diagnostic::default()
        };
        if col > 0 {
            d.column = col;
            d.end_line = line;
            d.end_column = end;
        }
        self.diags.push(d);
    }

    fn warn(&mut self, code: &str, line: usize, span: (usize, usize), message: String) {
        self.diag("warning", code, line, span, message);
    }

    fn error(&mut self, code: &str, line: usize, span: (usize, usize), message: String) {
        self.diag("error", code, line, span, message);
    }

    /// Parses one `###` block. It returns `None` when the block holds no
    /// request (e.g. only file variables or comments).
    fn parse_block(&mut self, b: &Block) -> Option<Request> {
        let mut req = Request {
            title: b.title.clone(),
            ..Request::default()
        };
        let i = self.parse_preamble(b, &mut req);
        if i >= b.lines.len() {
            if let Some(first) = req.directives.first() {
                let at = span(raw_line(b, first.line), &format!("@{}", first.key), 0);
                self.error(
                    "orphan-directives",
                    first.line,
                    at,
                    "directives without a request line".to_string(),
                );
            }
            return None;
        }
        let i = self.parse_request_line(b, &mut req, i);
        let i = self.parse_headers(b, &mut req, i);
        if i < b.lines.len() {
            self.parse_body(b, &mut req, i);
        }
        if req.description.is_empty() {
            req.description = req.title.clone();
        }
        Some(req)
    }

    /// Reads the comments, directives, file variables and blank lines
    /// before the request line, and returns the index of the first line
    /// that is none of those.
    fn parse_preamble(&mut self, b: &Block, req: &mut Request) -> usize {
        let mut i = 0;
        while i < b.lines.len() {
            let line = b.lines[i];
            let n = b.nums[i];
            let trimmed = line.trim();
            if trimmed.is_empty() {
                i += 1;
                continue;
            }
            if let Some(m) = RE_FILE_VAR.captures(trimmed) {
                self.vars.push(Var {
                    name: m[1].to_string(),
                    value: m[2].trim().to_string(),
                    line: n,
                });
                i += 1;
                continue;
            }
            if let Some(m) = RE_COMMENT.captures(trimmed) {
                self.parse_comment(req, line, m[1].trim(), n);
                i += 1;
                continue;
            }
            break;
        }
        i
    }

    /// Reads the request line at `b.lines[i]` and the query continuation
    /// lines after it (indented lines starting with `?` or `&`), and
    /// returns the index after them.
    fn parse_request_line(&mut self, b: &Block, req: &mut Request, mut i: usize) -> usize {
        let line = b.lines[i].trim();
        req.line = b.nums[i];
        if let Some(m) = RE_REQUEST_LINE.captures(line) {
            req.method = m[1].to_string();
            req.url = m[2].to_string();
            req.http_version = group(&m, 3).to_string();
        } else {
            req.method = "GET".to_string();
            req.url = line.to_string();
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() == 2 && f[1].starts_with("HTTP/") {
                req.url = f[0].to_string();
                req.http_version = f[1].to_string();
            }
        }
        if let Err(err) = req.protocol() {
            // The version ends the line; the URL before it may hold the
            // same text.
            let raw = b.lines[i];
            let at = span_from(raw, &req.http_version, raw.rfind(&req.http_version));
            self.error("bad-http-version", req.line, at, err);
        }
        i += 1;
        while i < b.lines.len() {
            let t = b.lines[i].trim();
            if (!t.starts_with('?') && !t.starts_with('&')) || b.lines[i] == t {
                break;
            }
            req.url.push_str(t);
            i += 1;
        }
        i
    }

    /// Reads the headers until the first blank line, and returns the
    /// index after that line.
    fn parse_headers(&mut self, b: &Block, req: &mut Request, mut i: usize) -> usize {
        while i < b.lines.len() {
            let t = b.lines[i].trim();
            if t.is_empty() {
                return i + 1;
            }
            if RE_COMMENT.is_match(t) {
                i += 1;
                continue;
            }
            if let Some(m) = RE_HEADER.captures(t) {
                req.headers.push(Header {
                    name: m[1].to_string(),
                    value: m[2].to_string(),
                });
                i += 1;
                continue;
            }
            self.error(
                "bad-header",
                b.nums[i],
                span(b.lines[i], t, 0),
                format!(
                    "expected a header (`Name: value`) or a blank line before the body, got {}",
                    quote(t)
                ),
            );
            i += 1;
        }
        i
    }

    /// Reads the rest of the block from `b.lines[i]` as the body, trailing
    /// blank lines trimmed. Editor-only handler blocks are lifted out
    /// first so they are ignored rather than sent.
    fn parse_body(&mut self, b: &Block, req: &mut Request, i: usize) {
        let (body_lines, handlers, saves) = split_handler_blocks(&b.lines[i..], &b.nums[i..]);
        for h in &handlers {
            self.warn(
                "editor-script",
                h.line,
                span(raw_line(b, h.line), h.text, 0),
                format!(
                    "ignoring {} (apic has no scripting; see https://apic.sh/comparison/)",
                    h.what
                ),
            );
        }
        for sv in &saves {
            self.parse_save(b, req, sv);
        }
        let body = body_lines.join("\n");
        let body = body
            .trim_end_matches(['\n', '\t', ' '])
            .trim_start_matches('\n');
        // The line the body starts on, for diagnostics inside it.
        let first = (i..b.lines.len()).find(|&j| !b.lines[j].trim().is_empty());
        if let Some(j) = first {
            req.body_line = b.nums[j];
        }
        let Some((path, templated)) = file_reference(body.trim()) else {
            req.body = body.to_string();
            return;
        };
        req.body_file_templated = templated;
        req.body_file = path;
        if let Some(j) = first {
            req.body_file_line = req.body_line;
            req.body_file_column = span_from(b.lines[j], &req.body_file, b.lines[j].find('<')).0;
        }
    }

    /// Records a `>> file` line; one per request.
    fn parse_save(&mut self, b: &Block, req: &mut Request, sv: &SaveLine) {
        let raw = raw_line(b, sv.line);
        let marker = if sv.overwrite { ">>!" } else { ">>" };
        let at = span_from(raw, &sv.path, raw.find(marker).map(|i| i + marker.len()));
        if sv.path.is_empty() {
            self.error(
                "bad-save-path",
                sv.line,
                span(raw, marker, 0),
                format!("{marker} needs a file path to save the response body to"),
            );
        } else if let Some(prev) = &req.save_to {
            let message = format!(
                "the response body is already saved to {} on line {}; one {marker} per request",
                prev.path, prev.line
            );
            self.error("bad-save-path", sv.line, at, message);
        } else {
            req.save_to = Some(SaveTo {
                path: sv.path.clone(),
                overwrite: sv.overwrite,
                line: sv.line,
                column: at.0,
            });
        }
    }

    /// Handles one comment line. `raw` is the line as written, for column
    /// positions; `text` is the comment with its marker stripped.
    fn parse_comment(&mut self, req: &mut Request, raw: &str, text: &str, line: usize) {
        let Some(m) = RE_DIRECTIVE.captures(text) else {
            return; // plain comment
        };
        let key = &m[1];
        let value = group(&m, 2).trim();
        // The key's span, and the value's span searched for after the key
        // so a value that repeats the key (`# @name name`) is found in the
        // right place.
        let (key_col, key_end) = span(raw, &format!("@{key}"), 0);
        let (val_col, val_end) = span(raw, value, key_end);
        let col = if val_col == 0 { key_col } else { val_col };
        req.directives.push(Directive {
            key: key.to_string(),
            value: value.to_string(),
            line,
            column: col,
        });
        match key {
            "name" => {
                if value.is_empty() {
                    self.error(
                        "bad-name",
                        line,
                        (key_col, key_end),
                        "@name needs a value".to_string(),
                    );
                }
                req.name = value.to_string();
            }
            "description" => req.description = value.to_string(),
            "capture" => {
                let Some(cm) = RE_CAPTURE.captures(value) else {
                    self.error(
                        "bad-capture",
                        line,
                        (col, val_end),
                        format!(
                            "@capture must look like `name = selector`, got {}",
                            quote(value)
                        ),
                    );
                    return;
                };
                let sel = cm[2].trim();
                let after = (val_col + cm[1].len()).checked_sub(1);
                req.captures.push(Capture {
                    name: cm[1].to_string(),
                    selector: sel.to_string(),
                    line,
                    column: span_from(raw, sel, after).0,
                });
            }
            "assert" => {
                if value.is_empty() {
                    self.error(
                        "bad-assert",
                        line,
                        (key_col, key_end),
                        "@assert needs an expression".to_string(),
                    );
                    return;
                }
                req.asserts.push(Assert {
                    expr: value.to_string(),
                    line,
                    column: val_col,
                });
            }
            _ => {
                if !is_known_directive(key) {
                    self.warn(
                        "unknown-directive",
                        line,
                        (key_col, key_end),
                        format!("unknown directive @{key} (ignored)"),
                    );
                }
            }
        }
    }
}

/// Returns the text of a 1-based line number within a block, or `""`.
fn raw_line<'a>(b: &Block<'a>, line: usize) -> &'a str {
    b.nums
        .iter()
        .position(|&n| n == line)
        .map_or("", |i| b.lines[i])
}

/// An editor-only script block found in a request body.
struct HandlerBlock<'a> {
    line: usize,
    what: &'static str,
    /// The trimmed first line of the block, for its span.
    text: &'a str,
}

/// A `>> file` or `>>! file` line found after a body.
struct SaveLine {
    path: String,
    overwrite: bool,
    line: usize,
}

/// Reports a `>> file` or `>>! file` line, trimmed.
pub(crate) fn is_save_line(t: &str) -> bool {
    t == ">>" || t == ">>!" || t.starts_with(">> ") || t.starts_with(">>! ")
}

/// Removes the response-handler and pre-request-script blocks that VS Code
/// REST Client and JetBrains allow after a body, returning the real body
/// lines, what was dropped, and the `>> file` lines that say where the
/// response body goes.
///
/// Without this they are not "ignored" as the comparison page promises: a
/// `> {% ... %}` handler is sent as part of the request body, and a
/// leading `< {%` is mistaken for the `< ./file` body-file syntax and
/// fails at run time looking for a file called `{%`. Note `< ./body.json`
/// is a real body file and must survive; only `< {%` is a script.
fn split_handler_blocks<'a>(
    lines: &[&'a str],
    nums: &[usize],
) -> (Vec<&'a str>, Vec<HandlerBlock<'a>>, Vec<SaveLine>) {
    let mut body = Vec::new();
    let mut found = Vec::new();
    let mut saves = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        let num = nums.get(i).copied().unwrap_or(0);
        if t.starts_with("> {%") || t.starts_with("< {%") {
            let what = if t.starts_with("< {%") {
                "pre-request script block"
            } else {
                "response handler block"
            };
            found.push(HandlerBlock {
                line: num,
                what,
                text: t,
            });
            // Consume to the closing %}, or to the end of the block if the
            // file never closes it.
            while i < lines.len() && !lines[i].contains("%}") {
                i += 1;
            }
        } else if is_save_line(t) {
            let overwrite = t.starts_with(">>!");
            let rest = t.strip_prefix(">>!").unwrap_or(t);
            let rest = rest.strip_prefix(">>").unwrap_or(rest);
            saves.push(SaveLine {
                path: rest.trim().to_string(),
                overwrite,
                line: num,
            });
        } else if t.starts_with("> ") && !t.starts_with(">> ") {
            found.push(HandlerBlock {
                line: num,
                what: "response handler file",
                text: t,
            });
        } else {
            body.push(lines[i]);
        }
        i += 1;
    }
    (body, found, saves)
}
