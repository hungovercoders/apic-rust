//! The generator behind `apic import` for an OpenAPI document: one file per
//! tag, one named request per operation, path parameters as `{{vars}}` and
//! an example body derived from the schema.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::LazyLock;

use apic_gocompat::{json_string, mkdir_all, path_error, quote};
use apic_httpfile::quote_param;
use apic_project::path;
use regex::{Captures, Regex};
use serde::{Serialize, Serializer};

use crate::mock::{clean_path, media_type_name};
use crate::model::{Document, Ref};
use crate::value::{Object, Value, ISO_TIMESTAMP, UUID};
use crate::yaml::NodeId;

/// Why a document could not be read, imported or mocked. `Display` is the
/// message to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub(crate) String);

impl Error {
    /// The message, as `Display` writes it.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// A list that is `null` in JSON while it is empty.
fn null_when_empty<S: Serializer>(list: &[String], s: S) -> Result<S::Ok, S::Error> {
    if list.is_empty() {
        s.serialize_none()
    } else {
        list.serialize(s)
    }
}

/// Summarises what was generated.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ImportResult {
    /// The files written, in the order they were.
    #[serde(serialize_with = "null_when_empty")]
    pub files: Vec<String>,
    /// How many requests the written `.http` files hold.
    pub requests: usize,
    /// The first server's URL, its variables filled in.
    pub base_url: String,
    /// The `http-client.env.json` written; empty when one was kept.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub env_file: String,
    /// The files that exist and were left alone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// Controls generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The directory to write into (default `.`).
    pub out_dir: String,
    /// The environment name for the generated `http-client.env.json`
    /// (default `dev`).
    pub env_name: String,
    /// Overwrite existing files.
    pub force: bool,
}

static SERVER_VAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([^{}]+)\}").expect("a valid pattern"));
static NON_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^A-Za-z0-9]+").expect("a valid pattern"));
static VAR_JUNK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^A-Za-z0-9_. -]").expect("a valid pattern"));
/// Matches anything that is not an RFC 7230 token character.
static HEADER_JUNK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("[^!#$%&'*+.^_`|~0-9A-Za-z-]").expect("a valid pattern"));
static CAMEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").expect("a valid pattern"));
/// The OpenAPI responses key grammar: a status code or a class pattern
/// such as 2XX.
pub(crate) static STATUS_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[1-5]([0-9][0-9]|XX)$").expect("a valid pattern"));

impl Document {
    fn resolve_server_url(&self, srv: Ref) -> Result<String, String> {
        let raw_url = self.str(self.get(srv, "url")).trim().to_string();
        if raw_url.is_empty() {
            return Ok(String::new());
        }
        let mut defaults: HashMap<String, String> = HashMap::new();
        for (key, value) in self.entries(self.get(srv, "variables")) {
            // An absent default stays unresolved.
            if let Some(def) = self.get(Some(value), "default") {
                defaults.insert(key, self.str(Some(def)));
            }
        }
        let out = SERVER_VAR.replace_all(&raw_url, |m: &Captures<'_>| {
            // An empty default is a valid value.
            match defaults.get(&m[1]) {
                Some(v) => v.clone(),
                None => m[0].to_string(),
            }
        });
        if let Some(unresolved) = SERVER_VAR.captures(&out) {
            return Err(format!(
                "server URL {} has unresolved variable {{{}}}",
                quote(&raw_url),
                &unresolved[1]
            ));
        }
        Ok(out.trim_end_matches('/').to_string())
    }

    /// Resolves the first server that declares a URL; without one it is
    /// `https://example.com`.
    pub(crate) fn base_url(&self) -> Result<String, String> {
        for srv in self.items(self.get(Some(self.root), "servers")) {
            if self.str(self.get(srv, "url")).trim().is_empty() {
                continue;
            }
            return self.resolve_server_url(srv);
        }
        Ok("https://example.com".to_string())
    }

    /// Reads every operation in path order, named uniquely across the
    /// document, and groups them by first tag (`api` when untagged). The
    /// tags come back sorted.
    fn operations_by_tag(&self) -> (HashMap<String, Vec<Operation>>, Vec<String>) {
        let mut by_tag: HashMap<String, Vec<Operation>> = HashMap::new();
        let mut tag_order = Vec::new();
        let mut seen_names: HashMap<String, usize> = HashMap::new();
        for (path, value) in self.entries(self.get(Some(self.root), "paths")) {
            let item = self.resolve(Some(value));
            let shared = self.parameters(self.get(item, "parameters"));
            for (key, value) in self.entries(item) {
                if !is_http_method(&key) {
                    continue;
                }
                let op = self.resolve(Some(value));
                let mut o = self.operation(&path, &key.to_ascii_uppercase(), op, &shared);
                o.name = unique_name(request_name(&o, &o.method, &path), &mut seen_names);
                let tag = match self.items(self.get(op, "tags")).first() {
                    Some(first) => self.str(*first),
                    None => "api".to_string(),
                };
                if !by_tag.contains_key(&tag) {
                    tag_order.push(tag.clone());
                }
                by_tag.entry(tag).or_default().push(o);
            }
        }
        tag_order.sort();
        (by_tag, tag_order)
    }
}

/// Reads an OpenAPI document (JSON or YAML) and writes `.http` files.
pub fn import(spec_path: &str, opts: Options) -> Result<ImportResult, Error> {
    let mut opts = opts;
    let data = std::fs::read(spec_path)
        .map_err(|e| Error(path_error("open", Path::new(spec_path), &e)))?;
    let doc = Document::parse(&data).map_err(|e| Error(format!("{spec_path}: {e}")))?;
    if opts.env_name.is_empty() {
        opts.env_name = "dev".to_string();
    }
    if opts.out_dir.is_empty() {
        opts.out_dir = ".".to_string();
    }
    mkdir_all(Path::new(&opts.out_dir)).map_err(Error)?;

    let base_url = doc.base_url().map_err(Error)?;
    let (mut by_tag, tag_order) = doc.operations_by_tag();
    // Everything that resolves references has run by now; stop before
    // writing any file if the document was structurally broken.
    if let Some(err) = doc.err() {
        return Err(Error(format!("{spec_path}: {err}")));
    }

    let mut w = Writer {
        opts,
        spec_path: spec_path.to_string(),
        res: ImportResult {
            base_url,
            ..ImportResult::default()
        },
    };
    let mut used_files = HashSet::new();
    for tag in &tag_order {
        let ops = by_tag.remove(tag).unwrap_or_default();
        w.write_tag(&file_stem(tag, &mut used_files), tag, ops)?;
    }
    w.write_env()?;
    Ok(w.res)
}

/// Names a tag's `.http` file: the tag in kebab case, numbered when an
/// earlier tag already took that name (foo/bar and foo-bar both kebab to
/// foo-bar).
fn file_stem(tag: &str, used: &mut HashSet<String>) -> String {
    let base = kebab(tag);
    let mut name = base.clone();
    let mut i = 2;
    while used.contains(&name) {
        name = format!("{base}-{i}");
        i += 1;
    }
    used.insert(name.clone());
    name
}

/// Writes the generated files into the output directory and records each
/// one, written or kept, on the result.
struct Writer {
    opts: Options,
    spec_path: String,
    res: ImportResult,
}

fn write_file(path: &str, content: &[u8]) -> Result<(), Error> {
    std::fs::write(path, content).map_err(|e| Error(path_error("open", Path::new(path), &e)))
}

impl Writer {
    /// Reports whether `path` already exists and must be left alone
    /// because `force` is off; a kept path is recorded as skipped.
    fn kept(&mut self, path: &str) -> bool {
        if std::fs::metadata(path).is_ok() && !self.opts.force {
            self.res.skipped.push(path.to_string());
            return true;
        }
        false
    }

    /// Writes one tag's requests to `name.http`, with the upload
    /// placeholders and body files they refer to.
    fn write_tag(&mut self, name: &str, tag: &str, ops: Vec<Operation>) -> Result<(), Error> {
        let file = path::join(&self.opts.out_dir, &format!("{name}.http"));
        if self.kept(&file) {
            return Ok(());
        }
        // A label first, so a tag spelled like a directive (`@auth ...`)
        // stays a comment; the file name comes from the document too.
        let mut b = format!(
            "# Tag {} — generated by `apic import` from {}. Edit freely.\n",
            quote(&one_line(tag)),
            quote(&one_line(&path::base(&self.spec_path)))
        );
        for mut o in ops {
            self.write_uploads(name, &mut o)?;
            self.write_body_file(name, &mut o)?;
            b.push_str(&o.render());
            self.res.requests += 1;
        }
        write_file(&file, b.as_bytes())?;
        self.res.files.push(file);
        Ok(())
    }

    /// Writes a placeholder file for each file part of the operation's
    /// multipart body and points the part at it.
    fn write_uploads(&mut self, name: &str, o: &mut Operation) -> Result<(), Error> {
        for p in o.parts.iter_mut().filter(|p| p.file) {
            // A placeholder the user replaces with the real upload, so
            // the generated request validates and runs as written.
            let side = format!("{name}.{}.{}.bin", o.name, kebab(&p.name));
            let side_path = path::join(&self.opts.out_dir, &side);
            p.path = format!("./{side}");
            if self.kept(&side_path) {
                continue;
            }
            let content = format!("replace this file with the {} to upload\n", p.name);
            write_file(&side_path, content.as_bytes())?;
            self.res.files.push(side_path);
        }
        Ok(())
    }

    /// Writes the operation's body to a file of its own when it must be
    /// sent verbatim, and points the operation at it.
    fn write_body_file(&mut self, name: &str, o: &mut Operation) -> Result<(), Error> {
        if !o.body_raw {
            return Ok(());
        }
        // The runner renders inline bodies as templates; a literal
        // {{...}} in an example must stay literal, so it goes into a
        // body file read verbatim (`< file`, not `<@ file`).
        let ext = if is_json(&o.content_type) {
            ".json"
        } else {
            ".txt"
        };
        let side = format!("{name}.{}.body{ext}", o.name);
        let side_path = path::join(&self.opts.out_dir, &side);
        o.body_file = format!("./{side}");
        if self.kept(&side_path) {
            return Ok(()); // kept, like an existing .http file
        }
        write_file(&side_path, o.body.as_bytes())?; // verbatim: no added newline
        self.res.files.push(side_path);
        Ok(())
    }

    /// Writes `http-client.env.json` with the base URL, unless one exists
    /// and `force` is off.
    fn write_env(&mut self) -> Result<(), Error> {
        let env_file = path::join(&self.opts.out_dir, "http-client.env.json");
        if std::fs::metadata(&env_file).is_err() || self.opts.force {
            // The public env file: base URLs only. Secrets belong in
            // http-client.private.env.json.
            let data = format!(
                "{{\n  {}: {{\n    \"baseUrl\": {}\n  }}\n}}\n",
                json_string(&self.opts.env_name, true),
                json_string(&self.res.base_url, true)
            );
            write_file(&env_file, data.as_bytes())?;
            self.res.env_file = env_file;
        }
        Ok(())
    }
}

/// Whether a key of a path item names an operation.
pub(crate) fn is_http_method(key: &str) -> bool {
    matches!(
        key,
        "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
    )
}

/// The subset of an OpenAPI operation the generator needs.
#[derive(Default)]
struct Operation {
    path: String,
    method: String,
    name: String,
    operation_id: String,
    summary: String,
    description: String,
    params: Vec<Parameter>,
    /// The example request body, empty when none.
    body: String,
    content_type: String,
    /// The first 2xx response code; empty when none is declared.
    success: String,
    /// The body holds a literal `{{` from the document and must not be
    /// rendered as a template.
    body_raw: bool,
    /// The `< file` reference used instead of the body when it is raw.
    body_file: String,
    /// A response advertises a JSON content type.
    wants_json: bool,
    /// A multipart/form-data body, one part per property; the body is
    /// then empty and the content type carries the boundary.
    parts: Vec<FormPart>,
}

/// One part of a generated multipart/form-data body.
#[derive(Default)]
struct FormPart {
    name: String,
    /// The text content; empty for a file part.
    value: String,
    /// The part's Content-Type, when it has one.
    content_type: String,
    /// A binary property: the part reads a placeholder file.
    file: bool,
    /// The `< path` of a file part, set once the file is named.
    path: String,
}

/// The boundary generated multipart bodies declare; it is what the REST
/// Client and JetBrains examples use, so it reads familiar.
const MULTIPART_BOUNDARY: &str = "WebAppBoundary";

#[derive(Clone)]
struct Parameter {
    name: String,
    location: String,
    required: bool,
}

impl Document {
    /// Reads a parameter list (`$ref`-able entries), in order.
    fn parameters(&self, n: Ref) -> Vec<Parameter> {
        let mut out = Vec::new();
        for p in self.items(n) {
            let (name, location) = (self.str(self.get(p, "name")), self.str(self.get(p, "in")));
            if name.is_empty() || location.is_empty() {
                continue;
            }
            let required = location == "path" || self.boolean(self.get(p, "required"));
            out.push(Parameter {
                name,
                location,
                required,
            });
        }
        out
    }

    fn operation(&self, path: &str, method: &str, op: Ref, shared: &[Parameter]) -> Operation {
        // The path lands on the request line, which the runner renders as
        // a template: no line breaks, no template markers, and always
        // rooted so a key such as `@evil.example` cannot rewrite the host.
        let path = no_template(&clean_path(path), "%7B", "%7D");
        let mut o = Operation {
            path,
            method: method.to_string(),
            operation_id: self.str(self.get(op, "operationId")),
            summary: self.str(self.get(op, "summary")),
            description: self.str(self.get(op, "description")),
            ..Operation::default()
        };
        o.params = merge_params(shared, self.parameters(self.get(op, "parameters")));
        (o.body, o.content_type, o.body_raw) = self.example_body(self.get(op, "requestBody"));
        if media_type_name(&o.content_type) == "multipart/form-data" {
            o.parts = self.multipart_parts(self.get(op, "requestBody"));
            if !o.parts.is_empty() {
                o.body = String::new();
                o.body_raw = false;
                o.content_type = format!("multipart/form-data; boundary={MULTIPART_BOUNDARY}");
            }
        }
        let responses = self.entries(self.get(op, "responses"));
        // Only a well-formed status key may become a directive value.
        if let Some((key, _)) = responses
            .iter()
            .find(|(key, _)| key.starts_with('2') && STATUS_KEY.is_match(key))
        {
            o.success.clone_from(key);
        }
        for (_, r) in &responses {
            for (ct, _) in self.entries(self.get(Some(*r), "content")) {
                if is_json(&ct) {
                    o.wants_json = true;
                }
            }
        }
        o
    }
}

/// Lists the path-level parameters an operation does not override, then
/// its own: an operation parameter overrides a path-level one with the
/// same name and location.
fn merge_params(shared: &[Parameter], own: Vec<Parameter>) -> Vec<Parameter> {
    let mut out: Vec<Parameter> = shared
        .iter()
        .filter(|p| {
            !own.iter()
                .any(|q| q.name == p.name && q.location == p.location)
        })
        .cloned()
        .collect();
    out.extend(own);
    out
}

/// Where an operation's parameters land in the generated request: the path
/// with its variables filled in, query continuations (optional ones
/// prefixed `# `), header lines and cookies.
struct ParamLines {
    path: String,
    query: Vec<String>,
    headers: Vec<String>,
    cookies: Vec<String>,
    optional_cookies: Vec<String>,
}

impl Operation {
    /// Writes the operation as one named request block.
    fn render(&self) -> String {
        let mut b = String::new();
        self.write_heading(&mut b);
        let lines = self.param_lines();
        let _ = writeln!(b, "{} {{{{baseUrl}}}}{}", self.method, lines.path);
        write_query(&mut b, &lines.query);
        self.write_headers(&mut b, &lines);
        self.write_body(&mut b);
        b
    }

    /// Writes the `###` heading, the request's name and description, and
    /// the status it asserts.
    fn write_heading(&self, b: &mut String) {
        let mut title = one_line(&self.summary); // one line: it is the ### heading
        if title.is_empty() {
            title = format!("{} {}", self.method, self.path);
        }
        let _ = writeln!(b, "\n### {title}");
        let _ = writeln!(b, "# @name {}", self.name);
        let d = self.description.split('\n').next().unwrap_or("").trim();
        if !d.is_empty() && d != title {
            let _ = writeln!(b, "# @description {d}");
        }
        let s = &self.success;
        if s.is_empty() {
            // No 2xx declared (only `default` or error codes): nothing to assert.
        } else if s.ends_with("XX") && s.len() == 3 {
            // Patterned key such as 2XX: assert the range instead of a literal.
            let first = s.as_bytes()[0];
            let _ = write!(
                b,
                "# @assert status >= {}00\n# @assert status < {}00\n",
                first as char,
                (first + 1) as char
            );
        } else {
            let _ = writeln!(b, "# @assert status == {s}");
        }
    }

    /// Places each parameter as a template variable in the form its
    /// location allows.
    fn param_lines(&self) -> ParamLines {
        let mut l = ParamLines {
            path: self.path.clone(),
            query: Vec::new(),
            headers: Vec::new(),
            cookies: Vec::new(),
            optional_cookies: Vec::new(),
        };
        let names = param_var_names(&self.params);
        for p in &self.params {
            let v = format!("{{{{{}}}}}", names[&format!("{}:{}", p.location, p.name)]);
            // A document is untrusted input: a name is written in the form
            // its location allows, so it can neither add lines to the
            // generated file nor change the request's meaning (`a&b`, `x;y`).
            // No line breaks; spaces are encoded or dropped per location.
            let name = one_line(&p.name);
            match p.location.as_str() {
                "cookie" => {
                    // Cookie names are tokens (spaces included).
                    let name = HEADER_JUNK.replace_all(&name, "");
                    if name.is_empty() {
                        l.headers.push(format!(
                            "# skipped cookie parameter {}: not a valid cookie name",
                            quote(&p.name)
                        ));
                        continue;
                    }
                    if p.required {
                        l.cookies.push(format!("{name}={v}"));
                    } else {
                        l.optional_cookies.push(format!("{name}={v}"));
                    }
                }
                "path" => {
                    // The document's own spelling.
                    l.path = l.path.replace(&format!("{{{}}}", p.name), &v);
                }
                "query" => {
                    let name = escape_query_name(&name); // only what would change the query's shape
                    if p.required {
                        l.query.push(format!("{name}={v}"));
                    } else {
                        l.query.push(format!("# {name}={v}"));
                    }
                }
                "header" => {
                    // `#` is a token character but opens a comment in .http files.
                    let name = HEADER_JUNK.replace_all(&name, "");
                    let name = name.trim_start_matches('#');
                    if name.is_empty() {
                        l.headers.push(format!(
                            "# skipped header parameter {}: not a valid header name",
                            quote(&p.name)
                        ));
                        continue;
                    }
                    let line = format!("{name}: {v}");
                    l.headers.push(if p.required {
                        line
                    } else {
                        format!("# {line}")
                    });
                }
                _ => {}
            }
        }
        l
    }

    /// Writes the Content-Type and Accept headers, then the parameters'
    /// header and cookie lines.
    fn write_headers(&self, b: &mut String, l: &ParamLines) {
        if !self.content_type.is_empty() {
            let _ = writeln!(b, "Content-Type: {}", self.content_type);
        }
        if self.wants_json {
            b.push_str("Accept: application/json\n");
        }
        for h in &l.headers {
            b.push_str(h);
            b.push('\n');
        }
        if !l.cookies.is_empty() {
            let _ = writeln!(b, "Cookie: {}", l.cookies.join("; "));
        }
        for c in &l.optional_cookies {
            let _ = writeln!(b, "# Cookie: {c}  (optional)");
        }
    }

    /// Writes the request body: multipart parts, a body file reference or
    /// the inline example, whichever the operation has.
    fn write_body(&self, b: &mut String) {
        if !self.parts.is_empty() {
            b.push('\n');
            for p in &self.parts {
                p.write(b);
            }
            let _ = writeln!(b, "--{MULTIPART_BOUNDARY}--");
        } else if !self.body_file.is_empty() {
            let _ = write!(b, "\n< {}\n", self.body_file);
        } else if !self.body.is_empty() {
            let _ = write!(b, "\n{}\n", self.body);
        }
    }
}

/// Writes the query continuation lines, required before optional.
fn write_query(b: &mut String, query: &[String]) {
    // Active continuation lines first: the parser stops reading query
    // continuations at the first comment, so optional parameters follow.
    let mut active = 0;
    for q in query.iter().filter(|q| !q.starts_with("# ")) {
        let sep = if active == 0 { "?" } else { "&" };
        active += 1;
        let _ = writeln!(b, "    {sep}{q}");
    }
    let mut shown = active;
    for q in query {
        let Some(q) = q.strip_prefix("# ") else {
            continue;
        };
        // Commented parameters are not sent; show the separator a caller
        // would add when uncommenting them in order.
        let sep = if shown == 0 { "?" } else { "&" };
        shown += 1;
        let _ = writeln!(b, "    # {sep}{q}  (optional)");
    }
}

impl FormPart {
    /// Writes the part with its boundary line: a file part reads its
    /// placeholder, a text part carries its value.
    fn write(&self, b: &mut String) {
        let _ = writeln!(b, "--{MULTIPART_BOUNDARY}");
        if self.file {
            let _ = writeln!(
                b,
                "Content-Disposition: form-data; name={}; filename={}",
                quote_param(&self.name),
                quote_param(&path::base(&self.path))
            );
        } else {
            let _ = writeln!(
                b,
                "Content-Disposition: form-data; name={}",
                quote_param(&self.name)
            );
        }
        if !self.content_type.is_empty() {
            let _ = writeln!(b, "Content-Type: {}", self.content_type);
        }
        b.push('\n');
        if self.file {
            let _ = writeln!(b, "< {}", self.path);
        } else {
            b.push_str(&self.value);
            b.push('\n');
        }
    }
}

impl Document {
    /// Builds a request body from the first media type that can provide
    /// one: its example, first named example, or a value derived from the
    /// schema. Without any, the first media type's content type is kept.
    /// The last value reports that the body contains a literal `{{` from
    /// the document and must be sent verbatim rather than rendered.
    fn example_body(&self, rb: Ref) -> (String, String, bool) {
        let mts = self.entries(self.get(rb, "content"));
        let Some((first, _)) = mts.first() else {
            return (String::new(), String::new(), false);
        };
        for (key, value) in &mts {
            let ct = no_template(&one_line(key), "", ""); // a header value the runner renders
            let Some(mut v) = self.media_example(self.resolve(Some(*value))) else {
                continue;
            };
            let raw = v.has_literal_braces();
            if raw {
                v = v.concretize(); // a verbatim body cannot carry placeholders
            }
            if is_json(&ct) {
                let (data, verbatim) = json_body(v, raw);
                return (data, ct, verbatim);
            }
            match v {
                Value::Str(s) => {
                    let raw = raw || splits_block(&s);
                    return (s, ct, raw);
                }
                Value::Placeholder(p) => return (p.to_string(), ct, false),
                Value::Null => return ("null".to_string(), ct, false), // an explicit null example is a body
                Value::Object(obj) => {
                    if media_type_name(&ct) == "application/x-www-form-urlencoded" {
                        // Percent-encoding would turn a {{$uuid}} placeholder
                        // into literal text the runner no longer recognises,
                        // so form bodies use fixed sample values instead.
                        return (form_encode(obj), ct, raw);
                    }
                    if media_type_name(&ct) == "multipart/form-data" {
                        // The operation builds the parts from the schema.
                        return (String::new(), ct, false);
                    }
                }
                Value::Array(_) => {}
                // A scalar serialises the same under any media type.
                scalar => return (scalar.scalar_text().unwrap_or_default(), ct, false),
            }
            // XML and other structured non-JSON bodies cannot be rendered
            // faithfully from a schema: try the next media type rather
            // than emit JSON under a misleading content type.
        }
        (String::new(), no_template(&one_line(first), "", ""), false)
    }

    /// Takes a media type's example, else its first named example, else a
    /// value derived from its schema; `None` when none gives one.
    pub(crate) fn media_example(&self, media: Ref) -> Option<Value> {
        if let Some(example) = self.get_raw(media, "example") {
            return Some(self.decode(Some(example)));
        }
        if let Some(example) = self.first_example_value(self.get(media, "examples")) {
            return Some(self.decode(Some(example)));
        }
        let schema = self.get(media, "schema")?;
        self.example_from_schema(Some(schema), 0)
    }

    /// Builds one part per property of a multipart/form-data request body:
    /// a string property with a binary format (or a content media type)
    /// becomes a file part, anything else a text part with its example.
    fn multipart_parts(&self, rb: Ref) -> Vec<FormPart> {
        let placeholder = || {
            vec![FormPart {
                name: "field".to_string(),
                value: "value".to_string(),
                ..FormPart::default()
            }]
        };
        for (key, value) in self.entries(self.get(rb, "content")) {
            let name = key.split(';').next().unwrap_or("").trim();
            if !name.eq_ignore_ascii_case("multipart/form-data") {
                continue;
            }
            let media = self.resolve(Some(value));
            let schema = self.resolve(self.get(media, "schema"));
            let encoding = self.get(media, "encoding");
            let mut parts: Vec<FormPart> = self
                .entries(self.get(schema, "properties"))
                .into_iter()
                .map(|(key, value)| self.property_part(&key, self.resolve(Some(value)), encoding))
                .collect();
            if parts.is_empty() {
                // No property list: an example object names the fields,
                // and with nothing at all one placeholder keeps the body
                // valid.
                parts = example_parts(self.decode(self.get_raw(media, "example")));
                if parts.is_empty() {
                    parts = placeholder();
                }
            }
            return parts;
        }
        placeholder()
    }

    /// Builds the part for one schema property: a file part for a binary
    /// string, otherwise a text part carrying its example. The property's
    /// encoding entry sets the part's content type.
    fn property_part(&self, key: &str, ps: Ref, encoding: Ref) -> FormPart {
        let mut part = FormPart {
            name: one_line(key),
            ..FormPart::default()
        };
        let format = self.str(self.get(ps, "format"));
        if let Some(enc) = self.get(encoding, key) {
            part.content_type = one_line(&self.str(self.get(Some(enc), "contentType")));
        }
        if self.schema_type(ps) == "string"
            && (format == "binary"
                || format == "byte"
                || self.get(ps, "contentMediaType").is_some())
        {
            part.file = true;
            if part.content_type.is_empty() {
                part.content_type = one_line(&self.str(self.get(ps, "contentMediaType")));
            }
            if part.content_type.is_empty() {
                part.content_type = "application/octet-stream".to_string();
            }
            return part;
        }
        match self.example_from_schema(ps, 1).unwrap_or(Value::Null) {
            Value::Placeholder(p) => part.value = p.to_string(),
            Value::Str(s) => part.value = one_line(&no_template(&s, "", "")),
            Value::Null => {}
            v => {
                part.value = v.concretize().to_json();
                if part.content_type.is_empty() {
                    part.content_type = "application/json".to_string();
                }
            }
        }
        part
    }
}

/// Indents `v` as a JSON body. A body the `.http` parser would misread is
/// written with fixed sample values instead and reported raw, so it goes
/// to a body file.
fn json_body(v: Value, raw: bool) -> (String, bool) {
    let data = v.to_json_indent();
    if !raw && splits_block(&data) {
        // Verbatim for the parser's sake: placeholders cannot render there.
        return (v.concretize().to_json_indent(), true);
    }
    (data, raw)
}

/// Builds one text part per field of an example object, with fixed sample
/// values; anything but an object gives none.
fn example_parts(example: Value) -> Vec<FormPart> {
    let Value::Object(obj) = example else {
        return Vec::new();
    };
    obj.into_entries()
        .into_iter()
        .map(|(k, v)| {
            let mut part = FormPart {
                name: one_line(&k),
                ..FormPart::default()
            };
            match v.concretize() {
                Value::Str(s) => part.value = one_line(&no_template(&s, "", "")),
                Value::Null => {}
                v => {
                    part.value = v.to_json();
                    part.content_type = "application/json".to_string();
                }
            }
            part
        })
        .collect()
}

/// Encodes the characters that would let a parameter name change the
/// structure of the generated query string.
fn escape_query_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '%' => out.push_str("%25"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '#' => out.push_str("%23"),
            '+' => out.push_str("%2B"),
            ';' => out.push_str("%3B"),
            ' ' => out.push_str("%20"),
            '{' => out.push_str("%7B"),
            '}' => out.push_str("%7D"),
            c => out.push(c),
        }
    }
    out
}

/// Neutralises `{{` and `}}` in text from the document that ends up where
/// the runner renders templates, so a document cannot make a generated
/// request read local variables. Braces become `open`/`close`, or vanish
/// when those are empty.
fn no_template(s: &str, open: &str, close: &str) -> String {
    s.replace("{{", &format!("{open}{open}"))
        .replace("}}", &format!("{close}{close}"))
}

/// Reports whether an inline body would be misread by the `.http` parser:
/// a line starting with `###` opens a new request block and a body
/// starting with `< ` refers to a file. Such bodies are written to a body
/// file instead.
fn splits_block(body: &str) -> bool {
    let t = body.trim();
    if t.starts_with("< ") || t.starts_with("<@ ") {
        return true;
    }
    body.split('\n').any(|line| line.trim().starts_with("###"))
}

/// Collapses text from the document into a single line for use in a
/// comment or heading, so a value containing line breaks cannot inject
/// request blocks or directives into the generated file.
pub(crate) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escapes text for a query string: letters, digits, `-`, `_`, `.` and `~`
/// stay, a space is `+`, anything else `%XX`.
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Renders an object as application/x-www-form-urlencoded, in property
/// order, with fixed sample values; nested values are JSON-encoded.
fn form_encode(obj: Object) -> String {
    obj.into_entries()
        .into_iter()
        .map(|(k, v)| {
            let val = match v.concretize() {
                Value::Str(s) => s,
                Value::Null => String::new(),
                v @ (Value::Object(_) | Value::Array(_)) => v.to_json(),
                v => v.scalar_text().unwrap_or_default(),
            };
            format!("{}={}", query_escape(&k), query_escape(&val))
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Reports whether a media type carries JSON; names are case-insensitive,
/// so application/JSON and application/problem+JSON count.
pub(crate) fn is_json(media_type: &str) -> bool {
    media_type.to_lowercase().contains("json")
}

impl Document {
    /// The `value` of the first named example that has one.
    pub(crate) fn first_example_value(&self, examples: Ref) -> Option<NodeId> {
        self.entries(examples)
            .into_iter()
            .find_map(|(_, ex)| self.get_raw(Some(ex), "value"))
    }

    /// Derives a value from a schema. `None` only when nothing could be
    /// derived; a JSON null (`type: null` or `example: null`) is a real
    /// value.
    pub(crate) fn example_from_schema(&self, s: Ref, depth: usize) -> Option<Value> {
        let s = self.resolve(s);
        if s.is_none() || depth > 6 {
            return None;
        }
        if let Some(v) = self.declared_example(s) {
            return Some(v);
        }
        let mut typ = self.schema_type(s);
        let props = self.entries(self.get(s, "properties"));
        let sub = self.items(self.get(s, "allOf"));
        if !sub.is_empty() {
            if let Some(v) = self.example_from_all_of(&sub, &typ, &props, depth) {
                return Some(v);
            }
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(first) = self.items(self.get(s, key)).first() {
                return self.example_from_schema(*first, depth + 1);
            }
        }
        if typ.is_empty() && !props.is_empty() {
            typ = "object".to_string();
        }
        match typ.as_str() {
            "object" => Some(Value::Object(self.object_example(
                Object::default(),
                &props,
                depth,
            ))),
            "array" => Some(Value::Array(match self.get(s, "items") {
                Some(items) => vec![self
                    .example_from_schema(Some(items), depth + 1)
                    .unwrap_or(Value::Null)],
                None => Vec::new(),
            })),
            "string" => Some(string_example(&self.str(self.get(s, "format")))),
            typ => scalar_example(typ),
        }
    }

    /// The value a schema states outright: its example, the first of its
    /// examples, its default, or the first enum member.
    fn declared_example(&self, s: Ref) -> Option<Value> {
        if let Some(n) = self.get_raw(s, "example") {
            return Some(self.decode(Some(n)));
        }
        if let Some(first) = self.items_raw(self.get_raw(s, "examples")).first() {
            return Some(self.decode(Some(*first)));
        }
        if let Some(n) = self.get_raw(s, "default") {
            return Some(self.decode(Some(n)));
        }
        if let Some(first) = self.items_raw(self.get_raw(s, "enum")).first() {
            return Some(self.decode(Some(*first)));
        }
        None
    }

    /// Merges the branches of an allOf: every branch contributes, so
    /// object branches are merged in order (later keys win) together with
    /// the properties declared alongside. A scalar branch wins only when
    /// nothing object-shaped is in play.
    fn example_from_all_of(
        &self,
        sub: &[Ref],
        typ: &str,
        props: &[(String, NodeId)],
        depth: usize,
    ) -> Option<Value> {
        let mut merged = Object::default();
        let mut scalar = None;
        let mut object = false;
        for branch in sub {
            let Some(v) = self.example_from_schema(*branch, depth + 1) else {
                continue;
            };
            match v {
                Value::Object(obj) => {
                    for (k, v) in obj.into_entries() {
                        merged.set(&k, v);
                    }
                    object = true;
                }
                v => {
                    if scalar.is_none() {
                        scalar = Some(v);
                    }
                }
            }
        }
        if object || typ == "object" || !props.is_empty() {
            return Some(Value::Object(self.object_example(merged, props, depth)));
        }
        scalar
    }

    /// Sets an example for each property on `obj` and returns it.
    fn object_example(&self, mut obj: Object, props: &[(String, NodeId)], depth: usize) -> Object {
        for (key, value) in props {
            let v = self
                .example_from_schema(Some(*value), depth + 1)
                .unwrap_or(Value::Null);
            obj.set(key, v);
        }
        obj
    }

    /// The schema type, taking the first non-null entry of an OpenAPI 3.1
    /// type array.
    fn schema_type(&self, s: Ref) -> String {
        let t = self.get(s, "type");
        if t.is_none() {
            return String::new();
        }
        if self.is_sequence(t) {
            let items = self.items(t);
            for n in &items {
                let v = self.str(*n);
                if !v.is_empty() && v != "null" {
                    return v;
                }
            }
            return if items.is_empty() { "" } else { "null" }.to_string();
        }
        self.str(t)
    }
}

/// The placeholder for a non-string primitive type.
fn scalar_example(typ: &str) -> Option<Value> {
    match typ {
        "integer" => Some(Value::Int(1)),
        "number" => Some(Value::Float(1.5)),
        "boolean" => Some(Value::Bool(true)),
        "null" => Some(Value::Null),
        _ => None,
    }
}

/// The placeholder for a string of the given format.
fn string_example(format: &str) -> Value {
    match format {
        "date-time" => Value::Placeholder(ISO_TIMESTAMP),
        "date" => Value::Str("2026-01-01".to_string()),
        "email" => Value::Str("user@example.com".to_string()),
        "uuid" => Value::Placeholder(UUID),
        "uri" | "url" => Value::Str("https://example.com".to_string()),
        _ => Value::Str("string".to_string()),
    }
}

fn kebab(s: &str) -> String {
    let s = NON_WORD.replace_all(s, "-").to_ascii_lowercase();
    let s = s.trim_matches('-');
    if s.is_empty() {
        "api".to_string()
    } else {
        s.to_string()
    }
}

/// Upper-cases the first character of ASCII text.
fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Turns a parameter name into a template variable name: camelCase letters
/// and digits only, never `$`-prefixed (that is the built-in namespace),
/// never starting with a digit and never the reserved baseUrl.
fn var_name(s: &str) -> String {
    let s = VAR_JUNK.replace_all(s, "");
    let mut name = String::new();
    for (i, part) in s
        .split(['-', '_', '.', ' '])
        .filter(|p| !p.is_empty())
        .enumerate()
    {
        if i == 0 {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                name.push(first.to_ascii_lowercase());
                name.push_str(chars.as_str());
            }
        } else {
            name.push_str(&upper_first(part));
        }
    }
    if name.is_empty() {
        return "value".to_string();
    }
    if name.as_bytes()[0].is_ascii_digit() {
        name.insert(0, 'p');
    }
    if name.eq_ignore_ascii_case("baseUrl") {
        name.push_str("Param");
    }
    name
}

fn request_name(op: &Operation, method: &str, path: &str) -> String {
    if !op.operation_id.is_empty() {
        return kebab(&split_camel(&op.operation_id));
    }
    let p = path.replace('{', "by-").replace('}', "");
    kebab(&format!("{} {p}", method.to_lowercase()))
}

/// Assigns a variable name to every parameter of an operation,
/// disambiguating names that would otherwise collide (user-id and user_id
/// both camel-case to userId) with the parameter location and, if needed,
/// a counter. Keys are `<in>:<name>`.
fn param_var_names(params: &[Parameter]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut used = HashSet::new();
    for p in params {
        let base = var_name(&p.name);
        let mut name = base.clone();
        if used.contains(&name) {
            name = base.clone() + &upper_first(&p.location);
        }
        let mut i = 2;
        while used.contains(&name) {
            name = format!("{base}{i}");
            i += 1;
        }
        used.insert(name.clone());
        out.insert(format!("{}:{}", p.location, p.name), name);
    }
    out
}

fn split_camel(s: &str) -> String {
    CAMEL.replace_all(s, "${1}-${2}").into_owned()
}

fn unique_name(name: String, seen: &mut HashMap<String, usize>) -> String {
    let count = seen.entry(name.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        name
    } else {
        format!("{name}-{count}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(kebab("Foo/Bar baz"), "foo-bar-baz");
        assert_eq!(kebab("//"), "api");
        assert_eq!(var_name("user-id"), "userId");
        assert_eq!(var_name("X.Correlation-ID"), "xCorrelationID");
        assert_eq!(var_name("$$"), "value");
        assert_eq!(var_name("1st"), "p1st");
        assert_eq!(var_name("BaseURL"), "baseURLParam");
        assert_eq!(split_camel("listPetsV2ById"), "list-Pets-V2-By-Id");
        assert_eq!(query_escape("a b&[\"x\"]é"), "a+b%26%5B%22x%22%5D%C3%A9");
        let mut seen = HashMap::new();
        assert_eq!(unique_name("a".to_string(), &mut seen), "a");
        assert_eq!(unique_name("a".to_string(), &mut seen), "a-2");
    }
}
