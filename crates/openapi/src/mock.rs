//! Serving an OpenAPI 3 document as a stateless mock, for `apic mock`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use apic_gocompat::{json_string, quote};
use apic_httpd::{Handler, Lookup, Mux, Request, Response};
use regex::Regex;
use serde::Serialize;

use crate::import::{is_http_method, is_json, one_line, Error, STATUS_KEY};
use crate::model::{Document, Ref};
use crate::value::Value;

/// One operation the mock serves, as the startup report lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Route {
    pub method: String,
    pub path: String,
    /// Served when no Prefer header asks otherwise.
    pub status: u16,
    /// The operationId.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// Answers every operation of an OpenAPI 3 document with the response the
/// document declares: the first 2xx (else default, else the first), a body
/// from its example or derived from its schema, and its headers. It keeps
/// no state and validates nothing: it is the document read back, so a
/// request file can run before the API it describes exists.
pub struct Mock {
    pub routes: Vec<Route>,
    /// Operations that could not be registered, with the reason.
    pub skipped: Vec<String>,
    /// Routes each pattern to its entry in `dispatchers`.
    mux: Mux<usize>,
    dispatchers: Vec<Dispatch>,
    /// What is registered on the mux, by its `METHOD /path` pattern.
    patterns: HashMap<String, usize>,
}

impl Mock {
    /// Builds the mock for an OpenAPI 3 document (YAML or JSON). The paths
    /// are registered as written and, when the first server URL has a path
    /// such as /v1, under that prefix too.
    pub fn new(data: &[u8]) -> Result<Mock, Error> {
        let d = Document::parse(data).map_err(Error)?;
        let prefix = d.base_path();
        let mut m = Mock {
            routes: Vec::new(),
            skipped: Vec::new(),
            mux: Mux::new(),
            dispatchers: Vec::new(),
            patterns: HashMap::new(),
        };
        for (path, item) in d.entries(d.get(Some(d.root), "paths")) {
            if !path.trim().starts_with('/') {
                continue; // an extension key, not a path
            }
            for (key, op) in d.entries(Some(item)) {
                if is_http_method(&key) {
                    let rt = d.mock_route(&key.to_ascii_uppercase(), &path, d.resolve(Some(op)));
                    m.add(rt, &prefix);
                }
            }
        }
        if let Some(err) = d.err() {
            return Err(Error(err));
        }
        if m.routes.is_empty() && m.skipped.is_empty() {
            return Err(Error(
                "the document declares no operations under paths".to_string(),
            ));
        }
        Ok(m)
    }

    /// Registers a route under its own path, or records why it could not
    /// be, then under the server prefix as well. The prefixed alias is a
    /// convenience: when the document also declares that path outright,
    /// the declared operation keeps it and nothing is reported.
    fn add(&mut self, mut rt: MockRoute, prefix: &str) {
        let (pat, partial) = mux_pattern(&rt.path);
        if partial {
            rt.exact = Some(template_regexp(prefix, &rt.path));
        }
        let rt = Arc::new(rt);
        if let Err(err) = self.attach(&format!("{} {pat}", rt.method), &rt) {
            self.skipped
                .push(format!("{} {}: {err}", rt.method, rt.path));
            return;
        }
        if !prefix.is_empty() {
            let (alias, _) = mux_pattern(&format!("{prefix}{}", rt.path));
            let _ = self.attach(&format!("{} {alias}", rt.method), &rt);
        }
        self.routes.push(Route {
            method: rt.method.clone(),
            path: rt.path.clone(),
            status: rt.def.status,
            name: rt.name.clone(),
        });
    }

    /// Adds a route to the dispatcher for a pattern, registering the
    /// pattern on the mux the first time it is seen.
    fn attach(&mut self, pattern: &str, rt: &Arc<MockRoute>) -> Result<(), String> {
        let i = match self.patterns.get(pattern) {
            Some(&i) => i,
            None => {
                let i = self.dispatchers.len();
                self.register(pattern, i)?;
                self.dispatchers.push(Dispatch::default());
                self.patterns.insert(pattern.to_string(), i);
                i
            }
        };
        let d = &mut self.dispatchers[i];
        if rt.exact.is_some() {
            d.exact.push(rt.clone());
        } else if d.plain.is_none() {
            d.plain = Some(rt.clone());
        } else {
            return Err("its path is the same as an earlier operation's".to_string());
        }
        Ok(())
    }

    /// Adds a pattern to the mux, wording the reason for one it cannot
    /// hold.
    fn register(&mut self, pattern: &str, dispatcher: usize) -> Result<(), String> {
        self.mux.handle(pattern, dispatcher).map_err(|msg| {
            if msg.contains("conflicts with") {
                return "its path conflicts with another operation's".to_string();
            }
            let first = msg.split('\n').next().unwrap_or("");
            format!(
                "ServeMux rejects its path: {}",
                first.strip_prefix("http: ").unwrap_or(first)
            )
        })
    }

    /// Routes to the operation, or answers 405 (with Allow) when the path
    /// is known under another method and 404 when it is not. Both carry a
    /// JSON message rather than plain text, so `apic run` shows why. The
    /// mux's own answer for a near miss (a redirect from /pets to a
    /// declared /pets/) is a miss too: a path the document does not
    /// declare is a 404.
    pub fn serve(&self, r: &Request) -> Response {
        if let Some(d) = self.lookup(r) {
            return d.serve(r);
        }
        let allow = self.allowed(r);
        if !allow.is_empty() {
            let allow = allow.join(", ");
            return mock_error(
                405,
                &format!(
                    "{} {} is not in the document; the path has {allow}",
                    r.method, r.path
                ),
            )
            .with_header("Allow", &allow);
        }
        mock_error(
            404,
            &format!("{} {} is not in the document", r.method, r.path),
        )
    }

    /// Turns the mock into a handler a server can run.
    pub fn into_handler(self) -> Handler {
        Box::new(move |r| self.serve(r))
    }

    /// Lists the methods under which the request's path is an operation.
    fn allowed(&self, r: &Request) -> Vec<String> {
        let mut allow = Vec::new();
        let mut tried: HashSet<&str> = HashSet::from([r.method.as_str()]);
        for rt in &self.routes {
            if !tried.insert(&rt.method) {
                continue;
            }
            let mut probe = r.clone();
            probe.method.clone_from(&rt.method);
            if self.lookup(&probe).is_some() {
                allow.push(rt.method.clone());
            }
        }
        allow
    }

    /// The dispatcher the mux routes a request to, or nothing when the mux
    /// would answer itself: not found, method not allowed, or a redirect.
    fn lookup(&self, r: &Request) -> Option<&Dispatch> {
        match self.mux.lookup(r) {
            Lookup::Found { handler, .. } => self.dispatchers.get(*handler),
            _ => None,
        }
    }
}

impl Document {
    /// The path of the first server URL (`/v1` for
    /// `https://api.example.com/v1/`), so the mock answers at /v1/pets as
    /// well as /pets; empty when the server has none.
    fn base_path(&self) -> String {
        let Ok(base) = self.base_url() else {
            return String::new();
        };
        url_path(&base)
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string()
    }
}

/// The decoded path of a URL; `None` for one that does not parse.
fn url_path(raw: &str) -> Option<String> {
    if raw.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    let rest = raw.split('#').next().unwrap_or("");
    // A scheme is a letter followed by letters, digits, `+`, `-` and `.`,
    // up to a colon.
    let mut scheme = "";
    let mut after = rest;
    for (i, c) in rest.char_indices() {
        if c.is_ascii_alphabetic() {
            continue;
        }
        if i > 0 && (c.is_ascii_digit() || matches!(c, '+' | '-' | '.')) {
            continue;
        }
        if c == ':' {
            if i == 0 {
                return None; // missing protocol scheme
            }
            scheme = &rest[..i];
            after = &rest[i + 1..];
        }
        break;
    }
    let after = after.split('?').next().unwrap_or("");
    if !after.starts_with('/') {
        if !scheme.is_empty() {
            return Some(String::new()); // opaque: mailto:someone
        }
        // A first segment with a colon would read as a scheme.
        let first = after.split('/').next().unwrap_or("");
        if first.contains(':') {
            return None;
        }
    }
    let path = if (!scheme.is_empty() || !after.starts_with("///")) && after.starts_with("//") {
        let authority_and_path = &after[2..];
        let (authority, path) = match authority_and_path.find('/') {
            Some(i) => authority_and_path.split_at(i),
            None => (authority_and_path, ""),
        };
        if !valid_authority(authority) {
            return None;
        }
        path
    } else {
        after
    };
    crate::model::path_unescape(path)
}

/// Checks an authority the way a URL parser does: the host holds only
/// characters a host may, and what follows its last colon is a port.
fn valid_authority(authority: &str) -> bool {
    let host = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let allowed = |b: u8| {
        !b.is_ascii() || b.is_ascii_alphanumeric() || b"-_.~!$&'()*+,;=:[]<>\"%".contains(&b)
    };
    if !host.bytes().all(allowed) || crate::model::path_unescape(host).is_none() {
        return false;
    }
    let port = if host.starts_with('[') {
        match host.rfind(']') {
            Some(end) => &host[end + 1..],
            None => return false,
        }
    } else {
        match host.rfind(':') {
            Some(i) => &host[i..],
            None => "",
        }
    };
    match port.strip_prefix(':') {
        Some(digits) => digits.bytes().all(|b| b.is_ascii_digit()),
        None => port.is_empty(),
    }
}

/// Serves the operations that share one mux pattern. A template with
/// literal text around a parameter, such as /files/{name}.json, needs the
/// same wildcard segment as /files/{id}, so the mux pattern is /files/{p0}
/// for both; the routes with literal text are tried first, each against
/// its own expression, and the plain {parameter} route takes what is left.
#[derive(Default)]
struct Dispatch {
    exact: Vec<Arc<MockRoute>>,
    plain: Option<Arc<MockRoute>>,
}

impl Dispatch {
    fn serve(&self, r: &Request) -> Response {
        for rt in &self.exact {
            if rt.exact.as_ref().is_some_and(|re| re.is_match(&r.path)) {
                return rt.serve(r);
            }
        }
        if let Some(plain) = &self.plain {
            return plain.serve(r);
        }
        mock_error(
            404,
            &format!("{} {} is not in the document", r.method, r.path),
        )
    }
}

/// A path template with its whitespace removed and a leading slash, as the
/// importer and the mock both read one.
pub(crate) fn clean_path(path: &str) -> String {
    let path: String = path.split_whitespace().collect();
    if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    }
}

/// Turns an OpenAPI path template into a mux pattern. Each segment holding
/// a {parameter} becomes a wildcard segment (the mux wants a whole segment
/// named by an identifier, where OpenAPI allows {pet-id} and {name}.json),
/// and a trailing slash becomes an exact match rather than a prefix. The
/// flag reports a segment with literal text around its parameter, which
/// the wildcard over-matches and `template_regexp` narrows.
fn mux_pattern(path: &str) -> (String, bool) {
    let cleaned = clean_path(path);
    let mut partial = false;
    let mut n = 0;
    let segs: Vec<String> = cleaned
        .split('/')
        .map(|s| {
            if !s.contains(['{', '}']) {
                return s.to_string();
            }
            let whole = s.starts_with('{') && s.ends_with('}') && s.matches('{').count() == 1;
            if !whole {
                partial = true;
            }
            n += 1;
            format!("{{p{}}}", n - 1)
        })
        .collect();
    let mut pattern = segs.join("/");
    if pattern.ends_with('/') {
        pattern.push_str("{$}");
    }
    (pattern, partial)
}

/// Matches exactly the paths a template describes, with the server prefix
/// optional: literal text as written, each {parameter} as one non-empty
/// segment or part of one.
fn template_regexp(prefix: &str, path: &str) -> Regex {
    let mut b = String::from("^");
    if !prefix.is_empty() {
        b.push_str(&format!("(?:{})?", regex::escape(prefix)));
    }
    let cleaned = clean_path(path);
    let mut rest = cleaned.as_str();
    while !rest.is_empty() {
        let Some(open) = rest.find('{') else {
            b.push_str(&regex::escape(rest));
            break;
        };
        b.push_str(&regex::escape(&rest[..open]));
        let Some(size) = rest[open..].find('}') else {
            b.push_str(&regex::escape(&rest[open..]));
            break;
        };
        b.push_str("[^/]+");
        rest = &rest[open + size + 1..];
    }
    b.push('$');
    Regex::new(&b).expect("an escaped template is a valid pattern")
}

/// One operation: its documented responses and the one served by default.
struct MockRoute {
    method: String,
    path: String,
    name: String,
    responses: Vec<Arc<MockResponse>>,
    def: Arc<MockResponse>,
    /// Set when the template has literal text around a parameter.
    exact: Option<Regex>,
}

/// One entry of an operation's responses, rendered once.
#[derive(Default)]
struct MockResponse {
    /// `200`, `2XX` or `default`.
    key: String,
    status: u16,
    headers: Vec<(String, String)>,
    media: Vec<MockMedia>,
}

/// One content type of a response: its body and named examples.
struct MockMedia {
    content_type: String,
    /// The media type alone, lower-case, for Accept matching.
    name: String,
    /// `None` when the media type declares nothing the mock can serve.
    body: Option<Vec<u8>>,
    examples: HashMap<String, Vec<u8>>,
}

impl Document {
    fn mock_route(&self, method: &str, path: &str, op: Ref) -> MockRoute {
        let mut responses = Vec::new();
        for (key, value) in self.entries(self.get(op, "responses")) {
            let Some(status) = status_for(&key) else {
                continue;
            };
            let media = self
                .entries(self.get(Some(value), "content"))
                .into_iter()
                .map(|(ct, media)| self.mock_media(&ct, Some(media)))
                .collect();
            responses.push(Arc::new(MockResponse {
                key,
                status,
                headers: self.mock_headers(self.get(Some(value), "headers")),
                media,
            }));
        }
        MockRoute {
            method: method.to_string(),
            path: path.to_string(),
            name: self.str(self.get(op, "operationId")),
            def: default_response(&responses),
            responses,
            exact: None,
        }
    }

    fn mock_media(&self, content_type: &str, media: Ref) -> MockMedia {
        let media = self.resolve(media);
        let mut mm = MockMedia {
            content_type: content_type.to_string(),
            name: media_type_name(content_type),
            body: None,
            examples: HashMap::new(),
        };
        if let Some(v) = self.media_example(media) {
            mm.body = render_body(content_type, v);
        }
        for (key, ex) in self.entries(self.get(media, "examples")) {
            if let Some(v) = self.get_raw(Some(ex), "value") {
                if let Some(body) = render_body(content_type, self.decode(Some(v))) {
                    mm.examples.insert(key, body);
                }
            }
        }
        mm
    }

    /// Renders a response's declared headers from their examples or
    /// schemas; Content-Type comes from the media type instead.
    fn mock_headers(&self, hs: Ref) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (key, value) in self.entries(hs) {
            if is_framing_header(&key) {
                continue;
            }
            if let Some(v) = self.header_example(self.resolve(Some(value))) {
                out.push((key, one_line(&header_text(v))));
            }
        }
        out
    }

    fn header_example(&self, h: Ref) -> Option<Value> {
        if let Some(n) = self.get_raw(h, "example") {
            return Some(self.decode(Some(n)));
        }
        if let Some(ex) = self.first_example_value(self.get(h, "examples")) {
            return Some(self.decode(Some(ex)));
        }
        let s = self.get(h, "schema")?;
        self.example_from_schema(Some(s), 0)
    }
}

/// Reads a responses key: a status code, a class such as 4XX (its first
/// code), or default (200). Anything else is not a response.
fn status_for(key: &str) -> Option<u16> {
    if key == "default" {
        return Some(200);
    }
    if !STATUS_KEY.is_match(key) {
        return None;
    }
    if key.ends_with("XX") {
        return Some(u16::from(key.as_bytes()[0] - b'0') * 100);
    }
    key.parse().ok()
}

/// The response served without a Prefer header: the first 2xx, else
/// default, else the first declared; an operation that declares none
/// answers 200 with no body.
fn default_response(rs: &[Arc<MockResponse>]) -> Arc<MockResponse> {
    rs.iter()
        .find(|r| r.key.starts_with('2'))
        .or_else(|| rs.iter().find(|r| r.key == "default"))
        .or_else(|| rs.first())
        .cloned()
        .unwrap_or_else(|| {
            Arc::new(MockResponse {
                key: "200".to_string(),
                status: 200,
                ..MockResponse::default()
            })
        })
}

/// Serialises an example for the wire. A JSON media type gets JSON; any
/// other gets a string as it is and a scalar as text, and nothing for an
/// object or a list, since JSON under an XML or CSV content type would
/// mislead (the importer refuses the same). The importer's placeholders
/// become the fixed values it writes into a verbatim body, so a mocked
/// response is the same on every call.
fn render_body(content_type: &str, v: Value) -> Option<Vec<u8>> {
    let v = v.concretize();
    if !is_json(content_type) {
        return match v {
            Value::Str(s) => Some(s.into_bytes()),
            Value::Object(_) | Value::Array(_) | Value::Null => None,
            scalar => scalar.scalar_text().map(String::into_bytes),
        };
    }
    let mut data = v.to_json_indent();
    data.push('\n');
    Some(data.into_bytes())
}

/// Headers the server sets from what it sends, never from the document: a
/// documented Content-Length would truncate the body.
fn is_framing_header(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "content-type" | "content-length" | "transfer-encoding" | "connection"
    )
}

fn header_text(v: Value) -> String {
    match v.concretize() {
        Value::Str(s) => s,
        Value::Null => String::new(),
        v @ (Value::Object(_) | Value::Array(_)) => v.to_json(),
        scalar => scalar.scalar_text().unwrap_or_default(),
    }
}

/// A concrete status code a Prefer header may ask for.
static STATUS_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[1-5][0-9][0-9]$").expect("a valid pattern"));

impl MockRoute {
    /// Writes the operation's default response, or the one a Prefer header
    /// asks for: code=<status> picks another documented response (an exact
    /// key, its class, or default served with that status) and
    /// example=<name> one of the media type's named examples. A choice the
    /// document does not offer is a 406 with a JSON message.
    fn serve(&self, r: &Request) -> Response {
        let pref = parse_prefer(&r.headers.values("Prefer").join(","));
        let mut res = &self.def;
        let mut status = self.def.status;
        if let Some(code) = pref.get("code").filter(|c| !c.is_empty()) {
            if !STATUS_CODE.is_match(code) {
                return mock_error(406, &format!("Prefer: code={code} is not a status code"));
            }
            let Some(found) = self.by_code(code) else {
                return mock_error(
                    406,
                    &format!(
                        "the document has no {code} response for {} {}",
                        self.method, self.path
                    ),
                );
            };
            res = found;
            status = code.parse().unwrap_or(status);
        }
        let media = res.pick(&r.headers.values("Accept").join(","));
        let mut body = media.and_then(|m| m.body.as_deref());
        if let Some(name) = pref.get("example").filter(|n| !n.is_empty()) {
            let Some(b) = media.and_then(|m| m.examples.get(name)) else {
                return mock_error(
                    406,
                    &format!(
                        "the {} response of {} {} has no example named {}",
                        res.key,
                        self.method,
                        self.path,
                        quote(name)
                    ),
                );
            };
            body = Some(b);
        }
        let mut out = Response::new(status);
        for (name, value) in &res.headers {
            out = out.add_header(name, value);
        }
        if let (Some(media), Some(body)) = (media, body) {
            out = out
                .with_header("Content-Type", &media.content_type)
                .with_body(body);
        }
        out
    }

    /// Finds the response a Prefer: code asks for: the exact key, its
    /// class (4XX for 404), or default.
    fn by_code(&self, code: &str) -> Option<&Arc<MockResponse>> {
        let class = format!("{}XX", &code[..1]);
        let wanted = [code, class.as_str(), "default"];
        wanted
            .iter()
            .find_map(|want| self.responses.iter().find(|r| r.key == *want))
    }
}

impl MockResponse {
    /// Chooses the media type the Accept header asks for, else the first.
    fn pick(&self, accept: &str) -> Option<&MockMedia> {
        let first = self.media.first()?;
        for a in accept.split(',') {
            let a = media_type_name(a);
            if a.is_empty() || a == "*/*" {
                break;
            }
            for m in &self.media {
                if m.name == a
                    || a.strip_suffix('*')
                        .is_some_and(|p| p.ends_with('/') && m.name.starts_with(p))
                {
                    return Some(m);
                }
            }
        }
        Some(first)
    }
}

/// The media type without its parameters, lower-case: `application/json`
/// for `Application/JSON; charset=utf-8`.
pub(crate) fn media_type_name(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

/// Reads a Prefer header (RFC 7240) into lower-case names and values;
/// preferences split on commas and semicolons, so
/// `code=404, example=missing` and `code=404; example=missing` read the
/// same.
fn parse_prefer(h: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for part in h.split([',', ';']).filter(|p| !p.is_empty()) {
        let part = part.trim();
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        out.insert(
            k.trim().to_lowercase(),
            v.trim().trim_matches('"').to_string(),
        );
    }
    out
}

fn mock_error(status: u16, msg: &str) -> Response {
    Response::json(
        status,
        format!("{{\"error\":{}}}\n", json_string(msg, true)),
    )
}

#[cfg(test)]
#[path = "mock_tests.rs"]
mod tests;
