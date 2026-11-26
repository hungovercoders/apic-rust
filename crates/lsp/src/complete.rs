//! What to offer where in a request file: directives after `# @`,
//! variables inside `{{`, selectors after `# @assert` and `# @capture x =`,
//! operators, auth types and request names. The same table the VS Code
//! extension's completion was built from, now in one place.

use std::collections::HashSet;
use std::sync::LazyLock;

use apic_gocompat::{format_float_json, json_string};
use apic_httpfile::KNOWN_DIRECTIVES;
use apic_runner::{Body, MASKED};
use regex::Regex;
use serde_json::Value;

use crate::protocol::{
    lines, uri_to_path, CompletionItem, CompletionList, MarkupContent, Position, Range,
    TextDocumentPositionParams, TextEdit, FORMAT_PLAIN, FORMAT_SNIPPET, KIND_FIELD, KIND_KEYWORD,
    KIND_MODULE, KIND_OPERATOR, KIND_VARIABLE,
};
use crate::server::{file_of, request_at, Handled, ProjectState, Reply, Server, State};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextKind {
    Directive,
    Variable,
    Selector,
    Operator,
    Auth,
    Ref,
}

/// What the cursor is completing: the text typed so far (`prefix`) and
/// the byte column it starts at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompletionContext {
    pub kind: ContextKind,
    pub prefix: String,
    pub start: usize,
    /// A variable whose `}}` already follows the cursor.
    pub closed: bool,
}

// Word characters, white space and its opposite, as ASCII has them.
const W: &str = "[0-9A-Za-z_]";
const S: &str = r"[\t\n\f\r ]";
const NS: &str = r"[^\t\n\f\r ]";

fn re(pattern: &str) -> Regex {
    let pattern = pattern
        .replace(r"\w", W)
        .replace(r"\s", S)
        .replace(r"\S", NS)
        .replace(r"\d", "[0-9]");
    Regex::new(&pattern).expect("a valid pattern")
}

fn comment_re(rest: &str) -> Regex {
    re(&format!(r"^\s*(?:#|//)\s*{rest}"))
}

static DIRECTIVE_RE: LazyLock<Regex> = LazyLock::new(|| comment_re(r"@((?:\w|-)*)$"));
static SELECTOR_RE: LazyLock<Regex> =
    LazyLock::new(|| comment_re(r"@(assert\s+|capture\s+(?:\w|[.-])+\s*=\s*)(\S*)$"));
static OPERATOR_RE: LazyLock<Regex> = LazyLock::new(|| comment_re(r"@assert\s+\S+\s+(\S*)$"));
static AUTH_RE: LazyLock<Regex> = LazyLock::new(|| comment_re(r"@auth\s+(\S*)$"));
static REF_RE: LazyLock<Regex> = LazyLock::new(|| comment_re(r"@(?:ref|forceRef)\s+(\S*)$"));
static VARIABLE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\{\{\s*([^{}]*)$"));

/// Reads the context at byte column `col` of a line.
pub(crate) fn context_at(line: &str, col: usize) -> Option<CompletionContext> {
    let (before, after) = line.split_at(col);
    let ctx = |kind, prefix: &str, start| CompletionContext {
        kind,
        prefix: prefix.to_string(),
        start,
        closed: false,
    };
    if let Some(m) = DIRECTIVE_RE.captures(before) {
        return Some(ctx(ContextKind::Directive, &m[1], col - m[1].len() - 1));
    }
    if let Some(m) = VARIABLE_RE.captures(before) {
        return Some(CompletionContext {
            closed: after.starts_with("}}"),
            ..ctx(ContextKind::Variable, &m[1], col - m[1].len())
        });
    }
    if let Some(m) = SELECTOR_RE.captures(before) {
        return Some(ctx(ContextKind::Selector, &m[2], col - m[2].len()));
    }
    if let Some(m) = OPERATOR_RE.captures(before) {
        return Some(ctx(ContextKind::Operator, &m[1], col - m[1].len()));
    }
    if let Some(m) = AUTH_RE.captures(before) {
        return Some(ctx(ContextKind::Auth, &m[1], col - m[1].len()));
    }
    if let Some(m) = REF_RE.captures(before) {
        return Some(ctx(ContextKind::Ref, &m[1], col - m[1].len()));
    }
    None
}

/// A suggestion before it is placed on the line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Item {
    pub label: String,
    pub insert: String,
    pub detail: String,
    pub doc: String,
    pub sort: String,
    pub snippet: bool,
    pub kind: i64,
}

/// The assertion operators, in the order the docs list them.
pub const OPERATORS: &[&str] = &[
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "contains",
    "startsWith",
    "endsWith",
    "matches",
    "exists",
    "not exists",
    "isString",
    "isNumber",
    "isInteger",
    "isBoolean",
    "isArray",
    "isObject",
    "isNull",
    "isEmpty",
    "not isEmpty",
    "length ==",
    "matchesSchema",
];

/// What `# @auth` takes, with the options each starts with.
const AUTH_TYPES: &[(&str, &str)] = &[
    ("bearer", "bearer {{${1:token}}}"),
    ("basic", "basic {{${1:user}}} {{${2:password}}}"),
    ("apikey", "apikey {{${1:apiKey}}} header=${2:X-Api-Key}"),
    ("digest", "digest {{${1:user}}} {{${2:password}}}"),
    ("aws", "aws region=${1:eu-west-2}"),
    (
        "oauth2",
        "oauth2 tokenUrl={{${1:tokenUrl}}} clientId={{${2:clientId}}} clientSecret={{${3:clientSecret}}}",
    ),
    ("exec", "exec ${1:command}"),
    ("none", "none"),
];

/// A directive's snippet body and one-line description, from the format
/// page of the docs. They are in the order directives are offered in: the
/// common ones first, as that page lists them.
const DIRECTIVE_INFO: &[(&str, &str, &str)] = &[
    ("name", "name ${1:request-name}", "Name used on the command line and by MCP."),
    ("description", "description ${1:text}", "One line shown by list and describe; defaults to the ### title."),
    ("capture", "capture ${1:name} = ${2:body.$.}", "After the response arrives, store the selected value as a variable for later requests and the session."),
    ("assert", "assert ${1:status} ${2|==,!=,<,<=,>,>=,contains,startsWith,endsWith,matches,exists,not exists|} ${3:200}", "Check the response. A failure sets ok: false and exit code 1."),
    ("auth", "auth ${1|bearer,basic,apikey,digest,aws,oauth2,exec,none|} ", "Attach credentials: none, bearer, basic, apikey, digest, aws, oauth2 or exec."),
    ("ref", "ref ${1:login}", "Run the named request first when this one is missing a variable, once per invocation."),
    ("forceRef", "forceRef ${1:login}", "Run the named request first every time this one runs."),
    ("step", "step ${1:phrase}", "A Gherkin phrase that runs this request from a .feature file; {name} becomes a variable."),
    ("retry", "retry ${1:5} ${2:1s}", "Re-send until every assertion passes, up to N times, this long apart."),
    ("timeout", "timeout ${1:10s}", "Per-request timeout."),
    ("sleep", "sleep ${1:1s}", "Wait this long before sending, e.g. for a rate-limited API."),
    ("disabled", "disabled", "Skip this request when its file runs as a flow; running it by name still sends it."),
    ("no-redirect", "no-redirect", "Do not follow 3xx redirects."),
    ("no-session", "no-session", "Do not persist this request's captures."),
    ("no-cookies", "no-cookies", "Send no cookies with this request and keep none it sets."),
    ("note", "note ${1:text}", "Free text, accepted and ignored (REST Client compatibility)."),
    ("prompt", "prompt ${1:name}", "Accepted and ignored: apic never prompts. Pass the value with --var or an env file."),
];

fn directive_info(name: &str) -> Option<(&'static str, &'static str)> {
    DIRECTIVE_INFO
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, body, doc)| (*body, *doc))
}

/// Every directive the parser knows, in the order of `DIRECTIVE_INFO`,
/// then any newer one by name.
fn directive_names() -> Vec<&'static str> {
    let known = |n: &str| KNOWN_DIRECTIVES.iter().any(|(k, _)| *k == n);
    let mut out: Vec<&'static str> = DIRECTIVE_INFO
        .iter()
        .map(|(n, _, _)| *n)
        .filter(|n| known(n))
        .collect();
    let mut rest: Vec<&'static str> = KNOWN_DIRECTIVES
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| !out.contains(k))
        .collect();
    rest.sort_unstable();
    rest.dedup();
    out.extend(rest);
    out
}

/// A `{{$…}}` placeholder and what it produces.
pub(crate) struct Builtin {
    pub name: &'static str,
    pub insert: &'static str,
    pub doc: &'static str,
}

const fn builtin(name: &'static str, insert: &'static str, doc: &'static str) -> Builtin {
    Builtin { name, insert, doc }
}

pub(crate) const BUILTINS: &[Builtin] = &[
    builtin("$uuid", "$uuid", "random UUID v4"),
    builtin("$guid", "$guid", "random UUID v4"),
    builtin("$timestamp", "$timestamp", "Unix seconds"),
    builtin("$isoTimestamp", "$isoTimestamp", "RFC 3339 UTC"),
    builtin(
        "$datetime",
        r#"$datetime ${1|rfc1123,iso8601,"2006-01-02"|}"#,
        "formatted UTC time (Go layout for custom formats); an offset like `-1 d` may follow",
    ),
    builtin(
        "$localDatetime",
        r#"$localDatetime ${1|rfc1123,iso8601,"2006-01-02"|}"#,
        "formatted time in the local zone; an offset like `1 h` may follow",
    ),
    builtin(
        "$randomInt",
        "$randomInt ${1:1} ${2:100}",
        "random integer in [min, max)",
    ),
    builtin(
        "$random.integer",
        "$random.integer(${1:1}, ${2:100})",
        "random integer in [min, max) (JetBrains)",
    ),
    builtin(
        "$random.float",
        "$random.float(${1:0}, ${2:1})",
        "random float with three decimals (JetBrains)",
    ),
    builtin(
        "$random.alphabetic",
        "$random.alphabetic(${1:10})",
        "random letters (JetBrains)",
    ),
    builtin(
        "$random.alphanumeric",
        "$random.alphanumeric(${1:10})",
        "random letters and digits (JetBrains)",
    ),
    builtin(
        "$random.hexadecimal",
        "$random.hexadecimal(${1:10})",
        "random hex digits (JetBrains)",
    ),
    builtin(
        "$random.email",
        "$random.email",
        "<8 letters>@example.com (JetBrains)",
    ),
    builtin("$random.uuid", "$random.uuid", "random UUID v4 (JetBrains)"),
    builtin("$projectRoot", "$projectRoot", "the project root, absolute"),
    builtin(
        "$processEnv",
        "$processEnv ${1:NAME}",
        "shell environment variable",
    ),
    builtin("$env", "$env.${1:NAME}", "shell environment variable"),
    builtin("$dotenv", "$dotenv ${1:NAME}", "value from .env"),
    builtin(
        "$auth.token",
        r#"$auth.token("${1:name}")"#,
        "access token of a Security.Auth configuration in the env files (JetBrains)",
    ),
    builtin(
        "$auth.idToken",
        r#"$auth.idToken("${1:name}")"#,
        "ID token of a Security.Auth configuration in the env files (JetBrains)",
    ),
];

/// The selectors: label, what is inserted, description, whether the
/// insert is a snippet.
const SELECTORS: &[(&str, &str, &str, bool)] = &[
    ("status", "status", "status code, e.g. 200", false),
    ("statusText", "statusText", "e.g. OK", false),
    (
        "header.",
        "header.${1:content-type}",
        "first value of a response header, case-insensitive",
        true,
    ),
    (
        "cookie.",
        "cookie.${1:name}",
        "value of a cookie the response set",
        true,
    ),
    ("body", "body", "raw body", false),
    ("body.$", "body.$", "whole body (must be JSON)", false),
    (
        "body.$.",
        "body.$.${1:path}",
        "JSON path such as body.$.items[0].id or body.$.items.# (count)",
        true,
    ),
    (
        "duration",
        "duration",
        "round-trip time in milliseconds",
        false,
    ),
];

impl Server {
    pub(crate) fn completion(&self, p: &TextDocumentPositionParams) -> Handled {
        let Some(path) = uri_to_path(&p.uri) else {
            return Ok(Reply::Null);
        };
        let mut st = self.lock();
        let text = st.text(&path);
        let ls = lines(&text);
        if p.position.line < 0 || p.position.line >= ls.len() as i64 {
            return Ok(Reply::Null);
        }
        let line = ls[p.position.line as usize];
        let col = st.units.to_byte(line, p.position.character);
        let Some(c) = context_at(line, col) else {
            return Ok(Reply::Null);
        };
        let ps = st.project_for(&path);
        let mut items: Vec<Item> = Vec::new();
        match c.kind {
            ContextKind::Directive => {
                for (i, name) in directive_names().into_iter().enumerate() {
                    let (body, doc) = match directive_info(name) {
                        Some((body, doc)) => (body.to_string(), doc),
                        None => (
                            format!("{name} "),
                            apic_httpfile::known_directive(name).unwrap_or(""),
                        ),
                    };
                    items.push(Item {
                        label: format!("@{name}"),
                        insert: format!("@{body}"),
                        snippet: true,
                        detail: "apic directive".to_string(),
                        doc: doc.to_string(),
                        sort: format!("{i:02}"),
                        kind: KIND_KEYWORD,
                    });
                }
            }
            ContextKind::Variable => items = st.variable_items(&ps, &path, c.closed),
            ContextKind::Selector => {
                items = st.selector_items(&ps, &path, &text, p.position.line, &c.prefix)
            }
            ContextKind::Operator => {
                for (i, op) in OPERATORS.iter().enumerate() {
                    items.push(Item {
                        label: op.to_string(),
                        insert: op.to_string(),
                        detail: "operator".to_string(),
                        sort: format!("{i:02}"),
                        kind: KIND_OPERATOR,
                        ..Item::default()
                    });
                }
            }
            ContextKind::Auth => {
                for (i, (name, insert)) in AUTH_TYPES.iter().enumerate() {
                    items.push(Item {
                        label: name.to_string(),
                        insert: insert.to_string(),
                        snippet: true,
                        detail: "# @auth type".to_string(),
                        sort: format!("{i:02}"),
                        kind: KIND_KEYWORD,
                        ..Item::default()
                    });
                }
            }
            ContextKind::Ref => {
                if let Some(project) = &ps.p {
                    let mut seen = HashSet::new();
                    for r in project.requests() {
                        if !r.name.is_empty() && seen.insert(r.name.clone()) {
                            items.push(Item {
                                label: r.name.clone(),
                                insert: r.name.clone(),
                                detail: format!("request {}", r.file.path),
                                kind: KIND_MODULE,
                                ..Item::default()
                            });
                        }
                    }
                }
            }
        }
        let edit = Range {
            start: Position {
                line: p.position.line,
                character: st.units.to_client(line, c.start),
            },
            end: p.position,
        };
        let mut out = CompletionList {
            is_incomplete: false,
            items: Vec::with_capacity(items.len()),
        };
        for it in items {
            let mut ci = CompletionItem {
                label: it.label.clone(),
                kind: it.kind,
                detail: it.detail,
                documentation: None,
                sort_text: it.sort,
                filter_text: it.label,
                insert_text_format: FORMAT_PLAIN,
                text_edit: None,
            };
            if !it.doc.is_empty() {
                ci.documentation = Some(MarkupContent {
                    kind: "markdown".to_string(),
                    value: it.doc,
                });
            }
            let mut new_text = it.insert;
            if it.snippet {
                if st.snippets {
                    ci.insert_text_format = FORMAT_SNIPPET;
                    new_text = escape_dollars(&new_text);
                } else {
                    new_text = plain_snippet(&new_text);
                }
            }
            ci.text_edit = Some(TextEdit {
                range: edit,
                new_text,
            });
            out.items.push(ci);
        }
        Ok(Reply::of(&out))
    }
}

impl State {
    /// Offers the variables (the source as detail, secrets masked: the
    /// environment's variables include the session's captures, masked as
    /// the secrets they may be), the built-ins and the response
    /// references of the file's named requests.
    fn variable_items(&mut self, ps: &ProjectState, path: &str, closed: bool) -> Vec<Item> {
        let end = if closed { "" } else { "}}" };
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        if let Ok(r) = self.runner_for(ps) {
            for v in r.env_vars() {
                if !seen.insert(v.name.clone()) {
                    continue;
                }
                let doc = if v.missing {
                    "not set".to_string()
                } else if v.secret {
                    format!("= {MASKED}")
                } else {
                    format!("= {}", v.value)
                };
                items.push(Item {
                    label: v.name.clone(),
                    insert: format!("{}{end}", v.name),
                    detail: v.source,
                    doc,
                    sort: format!("0{}", v.name),
                    kind: KIND_VARIABLE,
                    snippet: false,
                });
            }
        }
        for b in BUILTINS {
            items.push(Item {
                label: b.name.to_string(),
                insert: format!("{}{end}", b.insert),
                snippet: true,
                detail: "built-in".to_string(),
                doc: b.doc.to_string(),
                sort: format!("1{}", b.name),
                kind: KIND_KEYWORD,
            });
        }
        if let Some(f) = file_of(ps, path) {
            for r in &f.requests {
                if r.name.is_empty() {
                    continue;
                }
                let name = &r.name;
                items.push(Item {
                    label: format!("{name}.response.body.$"),
                    insert: format!("{name}.response.body.$.${{1:path}}{end}"),
                    snippet: true,
                    detail: "response reference".to_string(),
                    doc: format!("The JSON body {name} received earlier in this run."),
                    sort: format!("2{name}"),
                    kind: KIND_FIELD,
                });
                items.push(Item {
                    label: format!("{name}.response.headers"),
                    insert: format!("{name}.response.headers.${{1:name}}{end}"),
                    snippet: true,
                    detail: "response reference".to_string(),
                    doc: format!("A header {name} received earlier in this run."),
                    sort: format!("2{name}"),
                    kind: KIND_FIELD,
                });
            }
        }
        items
    }

    /// Offers the selectors, or after `body.$` the keys of the last
    /// response the request got: from a run in this session, else the
    /// newest entry of its response history.
    fn selector_items(
        &mut self,
        ps: &ProjectState,
        path: &str,
        text: &str,
        line: i64,
        prefix: &str,
    ) -> Vec<Item> {
        if prefix.starts_with("body.$") {
            if let Some(body) = self.last_body(ps, path, text, line) {
                let keys = body_path_items(prefix, &body);
                if !keys.is_empty() {
                    return keys;
                }
            }
        }
        SELECTORS
            .iter()
            .enumerate()
            .map(|(i, (label, insert, doc, snippet))| Item {
                label: label.to_string(),
                insert: insert.to_string(),
                doc: doc.to_string(),
                snippet: *snippet,
                detail: "selector".to_string(),
                sort: format!("{i:02}"),
                kind: KIND_FIELD,
            })
            .collect()
    }

    fn last_body(&mut self, ps: &ProjectState, path: &str, text: &str, line: i64) -> Option<Value> {
        let project = ps.p.as_ref()?;
        let req = request_at(file_of(ps, path), text, line)?;
        if req.name.is_empty() {
            return None;
        }
        let r = self.runner_for(ps).ok()?;
        // The environment in effect, apic.yaml's when nobody picked one:
        // the one runs record their history under.
        let env = &r.opts.env;
        let key = apic_runner::history_key(project, &req);
        if let Some(b) = self.bodies.get(&body_key(&ps.root, env, &key)) {
            return Some(b.clone());
        }
        let store =
            apic_history::Store::new(std::path::Path::new(&ps.root), project.config.history);
        let e = store.get(env, &key, 1).ok()?;
        let res = apic_runner::parse_result(e.result.as_deref()?).ok()?;
        json_body(&res.response?.body)
    }
}

/// Where a run's JSON body is kept for body.$ completion.
pub(crate) fn body_key(root: &str, env: &str, key: &str) -> String {
    format!("{root}\0{env}\0{key}")
}

/// A response body as a JSON object or array; `None` for anything else.
pub(crate) fn json_body(body: &Body) -> Option<Value> {
    let v: Value = serde_json::from_str(body.as_json()?).ok()?;
    (v.is_object() || v.is_array()).then_some(v)
}

static INDEX_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\[\d*$"));
static PLAIN_KEY_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z_](?:\w|-)*$"));
static STEP_RE: LazyLock<Regex> = LazyLock::new(|| re(r#"\.?([^.\[\]]+)|\[(\d+)\]|\["([^"]*)"\]"#));

/// The paths one level below what is typed, from a response body: an
/// object's keys, or `#` and `[0]` for an array. `prefix` is the whole
/// selector typed so far, `body.$.items.`.
fn body_path_items(prefix: &str, body: &Value) -> Vec<Item> {
    let typed = prefix.strip_prefix("body.$").unwrap_or(prefix);
    let indexing = INDEX_RE.is_match(typed);
    let rest = if indexing {
        INDEX_RE.replace_all(typed, "").into_owned()
    } else {
        typed.to_string()
    };
    let cut = rest.rfind('.').max(rest.rfind('['));
    let p = if indexing {
        rest.as_str()
    } else if let Some(cut) = cut {
        &rest[..cut + 1]
    } else {
        ""
    };
    let Some(node) = descend(body, p) else {
        return Vec::new();
    };
    let base = format!("body.${p}");
    let mut sep = base.clone();
    if !sep.ends_with('.') {
        sep.push('.');
    }
    let field = |label: String, detail: String, doc: String, sort: String| Item {
        insert: label.clone(),
        label,
        detail,
        doc,
        sort,
        kind: KIND_FIELD,
        snippet: false,
    };
    let mut items = Vec::new();
    match node {
        Value::Array(n) => {
            items.push(field(
                format!("{sep}#"),
                "count".to_string(),
                format!("{} elements", n.len()),
                "0".to_string(),
            ));
            if let Some(first) = n.first() {
                let at = format!("{}[0]", base.strip_suffix('.').unwrap_or(&base));
                items.push(field(at, describe(first), String::new(), "1".to_string()));
            }
        }
        Value::Object(n) => {
            // The keys come sorted.
            for (k, v) in n {
                let label = if PLAIN_KEY_RE.is_match(k) {
                    format!("{sep}{k}")
                } else {
                    format!("{}[\"{k}\"]", sep.strip_suffix('.').unwrap_or(&sep))
                };
                items.push(field(label, describe(v), String::new(), format!("2{k}")));
            }
        }
        _ => {}
    }
    items
}

/// Follows `.a.b[0].` through a JSON value.
fn descend<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut node = root;
    for m in STEP_RE.captures_iter(path) {
        let group = |i: usize| m.get(i).map_or("", |g| g.as_str());
        if !group(2).is_empty() {
            let i = group(2).parse::<usize>().unwrap_or(usize::MAX);
            node = node.as_array()?.get(i)?;
            continue;
        }
        let mut key = group(1);
        if !group(3).is_empty() || m[0].starts_with("[\"") {
            key = group(3);
        }
        if key.is_empty() {
            continue;
        }
        node = node.as_object()?.get(key)?;
    }
    Some(node)
}

/// A value in a few words: what a completion shows beside its path.
fn describe(v: &Value) -> String {
    let s = match v {
        Value::Null => return "null".to_string(),
        Value::Array(a) => return format!("array of {}", a.len()),
        Value::Object(_) => return "object".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => format_float_json(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => json_string(s, true),
    };
    if s.len() <= 40 {
        return s;
    }
    // Cut by bytes; what is left of a character cut in two shows as the
    // replacement character.
    let bytes = &s.as_bytes()[..37];
    let mut out = match std::str::from_utf8(bytes) {
        Ok(whole) => whole.to_string(),
        Err(e) => {
            let mut out = String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned();
            for _ in e.valid_up_to()..bytes.len() {
                out.push('\u{fffd}');
            }
            out
        }
    };
    out.push('…');
    out
}

static CHOICE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\$\{\d+\|([^,|}]*)[^}]*\|\}"));
static PLACEHOLDER_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\$\{\d+:([^}]*)\}"));
static TABSTOP_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\$\d+"));

/// Makes every `$` that is apic's own (`$uuid`, `body.$.`) literal in a
/// snippet, where `$name` would be a snippet variable: only a `$` before
/// `{` or a digit is the snippet's.
pub(crate) fn escape_dollars(s: &str) -> String {
    let mut b = String::with_capacity(s.len() + 4);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && !matches!(chars.peek(), Some('{' | '0'..='9')) {
            b.push_str("\\$");
            continue;
        }
        b.push(c);
    }
    b
}

/// Turns a snippet into the text a client without snippet support should
/// insert: each placeholder's default, a choice's first option.
pub(crate) fn plain_snippet(s: &str) -> String {
    let s = CHOICE_RE.replace_all(s, "${1}");
    let s = PLACEHOLDER_RE.replace_all(&s, "${1}");
    TABSTOP_RE.replace_all(&s, "").into_owned()
}
