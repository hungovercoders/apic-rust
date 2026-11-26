//! The generator behind `apic import` for a Postman collection: folders
//! become files, requests become named requests, variables become
//! `http-client.env.json`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::LazyLock;

use apic_gocompat::{check_json, compact_json, json_string, mkdir_all, path_error, quote};
use apic_httpfile::quote_param;
use apic_project::path;
use regex::Regex;
use serde::{Serialize, Serializer};

use crate::model::{
    looks_like_collection, param, Auth, Body, BodyFile, Collection, Environment, Event, FormField,
    GraphQL, Header, Item, Url, Variable,
};
use crate::scripts::translate_tests;

/// Why a collection could not be imported. `Display` is the message to
/// show.
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

/// Controls generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The directory to write into (default `.`).
    pub out_dir: String,
    /// Postman environment exports, one environment each.
    pub env_files: Vec<String>,
    /// Overwrite existing files.
    pub force: bool,
}

/// A list that is `null` in JSON while it is empty.
fn null_when_empty<S: Serializer>(list: &[String], s: S) -> Result<S::Ok, S::Error> {
    if list.is_empty() {
        s.serialize_none()
    } else {
        list.serialize(s)
    }
}

/// Summarises what was generated. It has the shape of the OpenAPI
/// importer's result plus what could not be translated.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ImportResult {
    /// The files written, in the order they were.
    #[serde(serialize_with = "null_when_empty")]
    pub files: Vec<String>,
    /// How many requests the collection's `.http` files hold, kept files
    /// included.
    pub requests: usize,
    /// The shared variable most likely to be the base URL.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub env_file: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub private_env_file: String,
    /// Existing files kept (use `force`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unsupported: Vec<Unsupported>,
}

/// One thing the collection had that the `.http` files cannot carry, with
/// the request it belongs to and a one-line reason.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Unsupported {
    /// The request id, or `collection`.
    pub request: String,
    /// The script line, auth type or body mode.
    pub what: String,
    pub reason: String,
}

fn read_file(path: &str) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|e| Error(path_error("open", Path::new(path), &e)))
}

/// Writes a file; a new one is readable by its owner alone when `private`.
fn write_file(path: &str, content: &[u8], private: bool) -> Result<(), Error> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(if private { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = private;
    options
        .open(path)
        .and_then(|mut f| f.write_all(content))
        .map_err(|e| Error(path_error("open", Path::new(path), &e)))
}

fn exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

/// Reads a Postman collection and writes `.http` files, an env file per
/// Postman environment given, and `apic.yaml` when the collection has a
/// default auth.
pub fn import(collection_path: &str, opts: Options) -> Result<ImportResult, Error> {
    let mut opts = opts;
    let data = read_file(collection_path)?;
    let c = parse_collection(&data).map_err(|e| Error(format!("{collection_path}: {e}")))?;
    if opts.out_dir.is_empty() {
        opts.out_dir = ".".to_string();
    }
    mkdir_all(Path::new(&opts.out_dir)).map_err(Error)?;
    let collection_auth = c.auth.as_ref();
    let mut g = Generator {
        res: ImportResult::default(),
        files: HashMap::new(),
        order: Vec::new(),
        names: HashMap::new(),
        collection_auth,
    };

    // Requests grouped by the file they land in, in collection order.
    g.walk(
        c.item.as_deref().unwrap_or(&[]),
        &[],
        &kebab(&c.info.name),
        collection_auth,
    );
    let default_auth = g.default_auth(collection_auth);
    for name in g.order.clone() {
        g.write_file(&name, collection_path, !default_auth.is_empty(), &opts)?;
    }
    let env_names = g.write_envs(&c.variable, &opts)?;
    g.write_config(&default_auth, &env_names, &opts)?;
    Ok(g.res)
}

/// Decodes a collection and refuses the v1 format.
fn parse_collection(data: &[u8]) -> Result<Collection, String> {
    if !looks_like_collection(data) {
        return Err("not a Postman collection (no info.schema and no item list)".to_string());
    }
    let c = Collection::from_json(data)?;
    let schema = c.info.schema.to_lowercase();
    if schema.contains("/v1.") || (schema.is_empty() && c.item.is_none()) {
        return Err(
            "a Postman v1 collection is not supported: export it again as Collection v2.1"
                .to_string(),
        );
    }
    if !schema.is_empty() && !schema.contains("/v2.") {
        return Err(format!(
            "unsupported collection schema {}: export it as Collection v2.1",
            quote(&c.info.schema)
        ));
    }
    Ok(c)
}

/// The variables of the public or the private env file, by environment.
type Envs = BTreeMap<String, BTreeMap<String, String>>;

/// Sorts variables into the public and private env files by environment.
#[derive(Default)]
struct EnvVars {
    public: Envs,
    private: Envs,
}

impl EnvVars {
    fn add(&mut self, env: &str, vars: &[Variable]) {
        for v in vars {
            if !v.active() || v.key.is_empty() {
                continue;
            }
            let target = if v.r#type == "secret" {
                &mut self.private
            } else {
                &mut self.public
            };
            target
                .entry(env.to_string())
                .or_default()
                .insert(v.key.clone(), v.value.clone());
        }
    }
}

/// Picks the shared variable most likely to be the base URL, passing over
/// one that is set but empty.
fn base_url(shared: Option<&BTreeMap<String, String>>) -> String {
    let Some(shared) = shared else {
        return String::new();
    };
    ["baseUrl", "base_url", "baseURL", "url", "host"]
        .iter()
        .filter_map(|key| shared.get(*key))
        .find(|v| !v.is_empty())
        .cloned()
        .unwrap_or_default()
}

/// Writes an env file, keeping an existing one unless `force`; either way
/// the path comes back.
fn write_env(
    p: String,
    envs: &Envs,
    private: bool,
    force: bool,
    res: &mut ImportResult,
) -> Result<String, Error> {
    if exists(&p) && !force {
        res.skipped.push(p.clone());
        return Ok(p);
    }
    let mut data = String::from("{");
    for (i, (env, vars)) in envs.iter().enumerate() {
        data.push_str(if i > 0 { ",\n  " } else { "\n  " });
        data.push_str(&json_string(env, true));
        data.push_str(": {");
        for (j, (key, value)) in vars.iter().enumerate() {
            data.push_str(if j > 0 { ",\n    " } else { "\n    " });
            let _ = write!(
                data,
                "{}: {}",
                json_string(key, true),
                json_string(value, true)
            );
        }
        data.push_str(if vars.is_empty() { "}" } else { "\n  }" });
    }
    data.push_str(if envs.is_empty() { "}\n" } else { "\n}\n" });
    write_file(&p, data.as_bytes(), private)?;
    res.files.push(p.clone());
    Ok(p)
}

/// Accumulates requests by file.
struct Generator<'a> {
    res: ImportResult,
    files: HashMap<String, Vec<Request>>,
    order: Vec<String>,
    names: HashMap<String, usize>,
    /// Becomes `auth.default`; requests under it carry no `# @auth` of
    /// their own.
    collection_auth: Option<&'a Auth>,
}

/// One generated request block.
#[derive(Default)]
struct Request {
    name: String,
    title: String,
    description: String,
    method: String,
    url: String,
    /// Commented continuation lines (disabled parameters).
    query: Vec<String>,
    /// Header lines, comments included.
    headers: Vec<String>,
    /// The `# @auth ...` value; `none` when the request opts out.
    auth: String,
    /// No auth of its own: the collection default applies.
    inherits: bool,
    /// Set when the body decides it and no header does.
    content_type: String,
    body: String,
    /// `< file`.
    body_file: String,
    /// Body text that must go into a side file.
    side_file: String,
    side_ext: String,
    file_vars: Vec<FileVar>,
    asserts: Vec<String>,
    captures: Vec<String>,
}

struct FileVar {
    name: String,
    value: String,
}

/// What an auth block maps onto: a `# @auth` value, or header and query
/// lines for an API key.
#[derive(Default)]
struct AuthSpec {
    line: String,
    headers: Vec<String>,
    query: Vec<String>,
}

static PATH_VAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(/|^):([A-Za-z_][0-9A-Za-z_-]*)").expect("a valid pattern"));
static NON_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^A-Za-z0-9]+").expect("a valid pattern"));

/// Postman's names for a random UUID become apic's.
fn dynamic_vars(s: &str) -> String {
    s.replace("{{$guid}}", "{{$uuid}}")
        .replace("{{$randomUUID}}", "{{$uuid}}")
}

/// Whether an auth block says nothing of its own.
fn unset(auth: Option<&Auth>) -> bool {
    auth.is_none_or(|a| a.r#type.is_empty() || a.r#type == "inherit")
}

impl<'a> Generator<'a> {
    /// Turns collection-level auth into the project default. An API key
    /// on the collection applies to every request as a header or a query
    /// value, so it is written into each request that inherits it instead.
    fn default_auth(&mut self, a: Option<&Auth>) -> String {
        let Some(a) = a else {
            return String::new();
        };
        if a.r#type.is_empty() || a.r#type == "noauth" {
            return String::new();
        }
        let Some(spec) = self.auth_spec(a, "collection") else {
            return String::new();
        };
        if !spec.line.is_empty() {
            return spec.line;
        }
        for requests in self.files.values_mut() {
            for r in requests.iter_mut().filter(|r| r.inherits) {
                r.headers.extend(spec.headers.iter().cloned());
                r.query.extend(spec.query.iter().cloned());
            }
        }
        String::new()
    }

    /// Writes one `.http` file and the body side files its requests need,
    /// keeping an existing file unless `force`.
    fn write_file(
        &mut self,
        name: &str,
        collection_path: &str,
        has_default_auth: bool,
        opts: &Options,
    ) -> Result<(), Error> {
        let file = path::join(&opts.out_dir, &format!("{name}.http"));
        let mut requests = self.files.remove(name).unwrap_or_default();
        if exists(&file) && !opts.force {
            self.res.skipped.push(file);
            self.res.requests += requests.len();
            return Ok(());
        }
        let label = if name.trim().is_empty() {
            "requests"
        } else {
            name
        };
        let mut b = format!(
            "# {} — generated by `apic import` from {}. Edit freely.\n",
            one_line(label),
            quote(&one_line(&path::base(collection_path)))
        );
        let mut seen_vars = HashSet::new();
        for v in requests.iter().flat_map(|r| &r.file_vars) {
            if seen_vars.insert(v.name.as_str()) {
                let _ = writeln!(b, "@{} = {}", v.name, v.value);
            }
        }
        for r in &mut requests {
            if !r.side_file.is_empty() {
                self.write_side_file(name, r, opts)?;
            }
            b.push_str(&r.render(has_default_auth));
            self.res.requests += 1;
        }
        write_file(&file, b.as_bytes(), false)?;
        self.res.files.push(file);
        Ok(())
    }

    /// Writes a body that cannot sit inline next to its `.http` file and
    /// points the request at it.
    fn write_side_file(
        &mut self,
        name: &str,
        r: &mut Request,
        opts: &Options,
    ) -> Result<(), Error> {
        let side = format!("{name}.{}.body{}", r.name, r.side_ext);
        let side_path = path::join(&opts.out_dir, &side);
        r.body_file = format!("./{side}");
        if exists(&side_path) && !opts.force {
            self.res.skipped.push(side_path);
            return Ok(());
        }
        write_file(&side_path, r.side_file.as_bytes(), false)?;
        self.res.files.push(side_path);
        Ok(())
    }

    /// Writes the variables: the collection's into `$shared`, each
    /// environment file into an environment of its own, secrets into the
    /// private file. It returns the environments' names in the order
    /// given.
    fn write_envs(
        &mut self,
        collection_vars: &[Variable],
        opts: &Options,
    ) -> Result<Vec<String>, Error> {
        let mut e = EnvVars::default();
        e.add("$shared", collection_vars);
        let mut env_names = Vec::new();
        for env_path in &opts.env_files {
            let env_data = read_file(env_path)?;
            let not_one = || Error(format!("{env_path}: not a Postman environment export"));
            let env = Environment::from_json(&env_data).map_err(|_| not_one())?;
            let Some(values) = &env.values else {
                return Err(not_one());
            };
            let base = path::base(env_path);
            let stem = base.strip_suffix(path::ext(&base)).unwrap_or(&base);
            let env_name = kebab(non_empty(&env.name, stem));
            env_names.push(env_name.clone());
            // An environment exists even when every value is a secret.
            e.public.entry(env_name.clone()).or_default();
            e.add(&env_name, values);
        }
        self.res.base_url = base_url(e.public.get("$shared"));
        if !e.public.is_empty() {
            let p = path::join(&opts.out_dir, "http-client.env.json");
            self.res.env_file = write_env(p, &e.public, false, opts.force, &mut self.res)?;
        }
        if !e.private.is_empty() {
            let p = path::join(&opts.out_dir, "http-client.private.env.json");
            self.res.private_env_file = write_env(p, &e.private, true, opts.force, &mut self.res)?;
        }
        Ok(env_names)
    }

    /// Writes `apic.yaml` when there is a default environment or a default
    /// auth to put in it.
    fn write_config(
        &mut self,
        default_auth: &str,
        env_names: &[String],
        opts: &Options,
    ) -> Result<(), Error> {
        if default_auth.is_empty() && env_names.is_empty() {
            return Ok(());
        }
        let mut y = String::from(
            "# yaml-language-server: $schema=https://apic.sh/schemas/apic.schema.json\n",
        );
        if let Some(first) = env_names.first() {
            let _ = writeln!(y, "env: {first}");
        }
        if !default_auth.is_empty() {
            // A JSON string is a valid YAML double-quoted scalar.
            let _ = writeln!(y, "auth:\n  default: {}", json_string(default_auth, true));
        }
        let cfg = path::join(&opts.out_dir, "apic.yaml");
        if exists(&cfg) && !opts.force {
            self.res.skipped.push(cfg);
            return Ok(());
        }
        write_file(&cfg, y.as_bytes(), false)?;
        self.res.files.push(cfg);
        Ok(())
    }

    fn walk(
        &mut self,
        items: &'a [Item],
        folders: &[&str],
        root_file: &str,
        inherited: Option<&'a Auth>,
    ) {
        for it in items {
            let auth = match &it.auth {
                Some(own) if !own.r#type.is_empty() && own.r#type != "inherit" => Some(own),
                _ => inherited,
            };
            let Some(request) = &it.request else {
                let mut inner = folders.to_vec();
                inner.push(&it.name);
                self.walk(&it.item, &inner, root_file, auth);
                continue;
            };
            let file = if folders.is_empty() {
                root_file.to_string()
            } else {
                folders
                    .iter()
                    .map(|f| kebab(f))
                    .collect::<Vec<_>>()
                    .join("-")
            };
            let r = self.request(it, request, auth);
            if !self.files.contains_key(&file) {
                self.order.push(file.clone());
            }
            self.files.entry(file).or_default().push(r);
        }
    }

    fn note(&mut self, req: &str, what: &str, reason: &str) {
        self.res.unsupported.push(Unsupported {
            request: req.to_string(),
            what: what.to_string(),
            reason: reason.to_string(),
        });
    }

    fn request(
        &mut self,
        it: &Item,
        req: &crate::model::Request,
        folder_auth: Option<&'a Auth>,
    ) -> Request {
        let name = unique_name(kebab(&it.name), &mut self.names);
        let mut r = Request {
            name,
            title: one_line(&it.name),
            method: non_empty(&req.method, "GET").to_uppercase(),
            ..Request::default()
        };
        let description = if req.description.trim().is_empty() {
            &it.description
        } else {
            &req.description
        };
        let d = description.split('\n').next().unwrap_or("").trim();
        if !d.is_empty() && d != r.title {
            r.description = one_line(d);
        }
        r.url(&req.url);
        let has_content_type = r.headers(&req.header);
        self.auth(&mut r, req.auth.as_ref(), folder_auth);
        if let Some(body) = req.body.as_ref().filter(|b| !b.disabled) {
            self.body(&mut r, body, has_content_type);
        }
        if r.content_type.eq_ignore_ascii_case("multipart/form-data")
            || r.content_type.starts_with("multipart/form-data;")
        {
            // The boundary in the body must be the one the header names.
            r.headers
                .retain(|h| !h.to_lowercase().starts_with("content-type:"));
        }
        self.events(&mut r, &it.event);
        r
    }

    /// Applies the request's own auth, else the folder's, else the
    /// collection's, which the project default covers; any of them may be
    /// `noauth`.
    fn auth(&mut self, r: &mut Request, own: Option<&Auth>, folder_auth: Option<&'a Auth>) {
        let auth = if unset(own) { folder_auth } else { own };
        if unset(auth) {
            return;
        }
        let Some(auth) = auth else { return };
        if self.collection_auth.is_some_and(|c| std::ptr::eq(c, auth)) {
            r.inherits = true;
        } else if auth.r#type == "noauth" {
            r.auth = "none".to_string();
        } else if let Some(spec) = self.auth_spec(auth, &r.name) {
            r.auth = spec.line;
            r.headers.extend(spec.headers);
            r.query.extend(spec.query);
        }
    }

    /// Translates test scripts into assertions and captures, and reports
    /// what it cannot translate.
    fn events(&mut self, r: &mut Request, evs: &[Event]) {
        for ev in evs {
            match ev.listen.as_str() {
                "test" => {
                    let t = translate_tests(&ev.script.exec);
                    r.asserts.extend(t.asserts);
                    r.captures.extend(t.captures);
                    for line in t.unsupported {
                        self.note(
                            &r.name,
                            &one_line(&line),
                            "test script line not translated; apic has no scripting",
                        );
                    }
                }
                "prerequest" if !ev.script.exec.concat().trim().is_empty() => {
                    self.note(
                        &r.name,
                        "pre-request script",
                        "not translated; compute values with --var or a # @ref request instead",
                    );
                }
                _ => {}
            }
        }
    }

    /// Maps a Postman body onto the request.
    fn body(&mut self, r: &mut Request, b: &Body, has_content_type: bool) {
        match b.mode.as_str() {
            "raw" => r.raw_body(b, has_content_type),
            "urlencoded" => r.urlencoded_body(&b.urlencoded, has_content_type),
            "formdata" => self.formdata_body(r, &b.formdata),
            "file" => self.file_body(r, b.file.as_ref()),
            "graphql" => r.graphql_body(b.graphql.as_ref(), has_content_type),
            "" => {}
            mode => self.note(&r.name, &format!("body mode {mode}"), "not translated"),
        }
    }

    fn formdata_body(&mut self, r: &mut Request, fields: &[FormField]) {
        let mut parts = String::new();
        for f in fields {
            if f.disabled || f.key.is_empty() {
                continue;
            }
            parts.push_str("--WebAppBoundary\n");
            if f.r#type == "file" {
                self.file_part(&mut parts, r, f);
                continue;
            }
            let _ = writeln!(
                parts,
                "Content-Disposition: form-data; name={}",
                quote_param(&one_line(&f.key))
            );
            if !f.content_type.is_empty() {
                let _ = writeln!(parts, "Content-Type: {}", one_line(&f.content_type));
            }
            let _ = write!(parts, "\n{}\n", one_line(&dynamic_vars(&f.value)));
        }
        if parts.is_empty() {
            return;
        }
        parts.push_str("--WebAppBoundary--");
        r.content_type = "multipart/form-data; boundary=WebAppBoundary".to_string();
        r.body = parts;
    }

    /// Writes a multipart file part that reads the file from next to the
    /// `.http` file.
    fn file_part(&mut self, parts: &mut String, r: &Request, f: &FormField) {
        let src = f.src.trim();
        let base = base_name(src);
        let base = non_empty(&base, &f.key);
        let _ = writeln!(
            parts,
            "Content-Disposition: form-data; name={}; filename={}",
            quote_param(&one_line(&f.key)),
            quote_param(&one_line(base))
        );
        if !f.content_type.is_empty() {
            let _ = writeln!(parts, "Content-Type: {}", one_line(&f.content_type));
        }
        let _ = write!(parts, "\n< ./{}\n", one_line(base));
        if src != base {
            self.note(
                &r.name,
                &format!("file part {}", one_line(&f.key)),
                &format!(
                    "Postman pointed at {}; put {} next to the .http file",
                    one_line(src),
                    one_line(base)
                ),
            );
        }
    }

    fn file_body(&mut self, r: &mut Request, f: Option<&BodyFile>) {
        let Some(f) = f.filter(|f| !f.src.is_empty()) else {
            return;
        };
        let base = base_name(&f.src);
        r.body_file = format!("./{}", one_line(&base));
        if f.src != base {
            self.note(
                &r.name,
                "body file",
                &format!(
                    "Postman pointed at {}; put {} next to the .http file",
                    one_line(&f.src),
                    one_line(&base)
                ),
            );
        }
    }

    /// Maps a Postman auth block onto a `# @auth` value, or onto header
    /// and query lines for an API key. `None` when apic has no equivalent,
    /// which is then reported.
    fn auth_spec(&mut self, a: &Auth, req: &str) -> Option<AuthSpec> {
        let line = |line: String| {
            Some(AuthSpec {
                line,
                ..AuthSpec::default()
            })
        };
        match a.r#type.as_str() {
            "bearer" => line(format!(
                "bearer {}",
                auth_arg(non_empty(param(&a.bearer, "token"), "{{token}}"))
            )),
            "basic" => line(format!(
                "basic {} {}",
                auth_arg(non_empty(param(&a.basic, "username"), "{{user}}")),
                auth_arg(non_empty(param(&a.basic, "password"), "{{password}}"))
            )),
            "digest" => line(format!(
                "digest {} {}",
                auth_arg(non_empty(param(&a.digest, "username"), "{{user}}")),
                auth_arg(non_empty(param(&a.digest, "password"), "{{password}}"))
            )),
            "apikey" => {
                let key = one_line(non_empty(param(&a.apikey, "key"), "X-Api-Key"));
                let value = one_line(non_empty(param(&a.apikey, "value"), "{{apiKey}}"));
                let mut spec = AuthSpec::default();
                if param(&a.apikey, "in") == "query" {
                    spec.query.push(format!("&{key}={value}"));
                } else {
                    spec.headers.push(format!("{key}: {value}"));
                }
                Some(spec)
            }
            "awsv4" => {
                let mut s = "aws".to_string();
                let region = param(&a.awsv4, "region");
                if !region.is_empty() {
                    let _ = write!(s, " region={}", auth_arg(region));
                }
                let service = param(&a.awsv4, "service");
                if !service.is_empty() {
                    let _ = write!(s, " service={}", auth_arg(service));
                }
                line(s)
            }
            "oauth2" => {
                let p = |key: &str| param(&a.oauth2, key);
                let grant = p("grant_type");
                let mut s = format!(
                    "oauth2 tokenUrl={} clientId={}",
                    auth_arg(non_empty(p("accessTokenUrl"), "{{tokenUrl}}")),
                    auth_arg(non_empty(p("clientId"), "{{clientId}}"))
                );
                if !p("clientSecret").is_empty() {
                    let _ = write!(s, " clientSecret={}", auth_arg(p("clientSecret")));
                }
                if !p("scope").is_empty() {
                    let _ = write!(s, " scope={}", auth_arg(p("scope")));
                }
                match grant {
                    "" | "client_credentials" => {}
                    "password_credentials" => {
                        let _ = write!(
                            s,
                            " grant=password username={} password={}",
                            auth_arg(non_empty(p("username"), "{{user}}")),
                            auth_arg(non_empty(p("password"), "{{password}}"))
                        );
                    }
                    _ => {
                        self.note(
                            req,
                            &format!("oauth2 grant {grant}"),
                            "only client credentials and password grants map onto # @auth oauth2; browser flows need a token from elsewhere",
                        );
                        return None;
                    }
                }
                if p("client_authentication") == "header" {
                    s.push_str(" clientAuth=basic");
                }
                line(s)
            }
            "noauth" | "inherit" | "" => None,
            other => {
                self.note(
                    req,
                    &format!("auth {other}"),
                    "apic has no equivalent; write the header yourself or use # @auth exec",
                );
                None
            }
        }
    }
}

/// Maps a raw body's language onto its content type.
fn raw_content_type(language: &str) -> Option<&'static str> {
    match language {
        "json" => Some("application/json"),
        "xml" => Some("application/xml"),
        "html" => Some("text/html"),
        "javascript" => Some("application/javascript"),
        "text" => Some("text/plain"),
        _ => None,
    }
}

impl Request {
    /// Maps the request URL: Postman's `{{var}}` is apic's, `:param`
    /// becomes `{{param}}` with its value as a file variable, disabled
    /// query parameters become comments.
    fn url(&mut self, u: &Url) {
        let raw = u.raw.trim();
        let (path_part, query) = match raw.split_once('?') {
            Some((path_part, query)) => (path_part, Some(query)),
            None => (raw, None),
        };
        let mut raw = PATH_VAR.replace_all(path_part, "${1}{{${2}}}").into_owned();
        if let Some(query) = query {
            raw.push('?');
            raw.push_str(query);
        }
        self.url = dynamic_vars(&one_line(&raw));
        for v in &u.variable {
            if !v.key.is_empty() && !v.value.is_empty() {
                self.file_vars.push(FileVar {
                    name: v.key.clone(),
                    value: one_line(&v.value),
                });
            }
        }
        for q in u.query.iter().filter(|q| q.disabled) {
            self.query.push(format!(
                "# &{}={}  (disabled)",
                one_line(&q.key),
                one_line(&q.value)
            ));
        }
    }

    /// Adds the request's headers, disabled ones as comments, and reports
    /// whether one of them sets the content type.
    fn headers(&mut self, hs: &[Header]) -> bool {
        let mut has_content_type = false;
        for h in hs {
            let key = one_line(&h.key);
            if key.is_empty() {
                continue;
            }
            if h.disabled {
                self.headers
                    .push(format!("# {key}: {}  (disabled)", one_line(&h.value)));
                continue;
            }
            if key.eq_ignore_ascii_case("Content-Type") {
                has_content_type = true;
            }
            self.headers.push(format!("{key}: {}", one_line(&h.value)));
        }
        has_content_type
    }

    fn raw_body(&mut self, b: &Body, has_content_type: bool) {
        let replaced = dynamic_vars(&b.raw);
        let text = replaced.trim_end_matches('\n');
        if text.is_empty() {
            return;
        }
        if !has_content_type {
            if let Some(ct) = b
                .options
                .as_ref()
                .and_then(|o| raw_content_type(&o.raw.language))
            {
                self.content_type = ct.to_string();
            }
        }
        if splits_block(text) {
            self.side_file = text.to_string();
            self.side_ext = if self.content_type == "application/json" {
                ".json"
            } else {
                ".txt"
            }
            .to_string();
            return;
        }
        self.body = text.to_string();
    }

    fn urlencoded_body(&mut self, fields: &[FormField], has_content_type: bool) {
        let pairs: Vec<String> = fields
            .iter()
            .filter(|f| !f.disabled && !f.key.is_empty())
            .map(|f| {
                format!(
                    "{}={}",
                    form_escape(&f.key),
                    form_escape(&dynamic_vars(&f.value))
                )
            })
            .collect();
        if !has_content_type {
            self.content_type = "application/x-www-form-urlencoded".to_string();
        }
        self.body = pairs.join("&");
    }

    fn graphql_body(&mut self, q: Option<&GraphQL>, has_content_type: bool) {
        let Some(q) = q else { return };
        let mut data = format!("{{\"query\":{}", json_string(&dynamic_vars(&q.query), true));
        let vars = q.variables.trim();
        if !vars.is_empty() && check_json(vars.as_bytes()).is_ok() {
            data.push_str(",\"variables\":");
            data.push_str(&compact_json(vars, true));
        }
        data.push('}');
        if !has_content_type {
            self.content_type = "application/json".to_string();
        }
        self.body = data;
    }

    fn render(&self, has_default_auth: bool) -> String {
        let mut b = String::new();
        if self.title.is_empty() {
            let _ = writeln!(b, "\n### {} {}", self.method, self.url);
        } else {
            let _ = writeln!(b, "\n### {}", self.title);
        }
        let _ = writeln!(b, "# @name {}", self.name);
        if !self.description.is_empty() {
            let _ = writeln!(b, "# @description {}", self.description);
        }
        if self.auth == "none" {
            if has_default_auth {
                b.push_str("# @auth none\n");
            }
        } else if !self.auth.is_empty() {
            let _ = writeln!(b, "# @auth {}", self.auth);
        }
        for c in &self.captures {
            let _ = writeln!(b, "# @capture {c}");
        }
        for a in &self.asserts {
            let _ = writeln!(b, "# @assert {a}");
        }
        let _ = writeln!(b, "{} {}", self.method, self.url);
        let mut has_query = self.url.contains('?');
        for q in &self.query {
            if !q.starts_with('#') && !has_query {
                // The parser appends continuation lines verbatim, so the
                // first active parameter opens the query string.
                has_query = true;
                let _ = writeln!(b, "    ?{}", q.strip_prefix('&').unwrap_or(q));
            } else {
                let _ = writeln!(b, "    {q}");
            }
        }
        if !self.content_type.is_empty() {
            let _ = writeln!(b, "Content-Type: {}", self.content_type);
        }
        for h in &self.headers {
            b.push_str(h);
            b.push('\n');
        }
        if !self.body_file.is_empty() {
            let _ = write!(b, "\n< {}\n", self.body_file);
        } else if !self.body.is_empty() {
            let _ = write!(b, "\n{}\n", self.body);
        }
        b
    }
}

/// Quotes a `# @auth` argument when it holds spaces.
fn auth_arg(s: &str) -> String {
    let s = one_line(s);
    if s.contains([' ', '\t', '"']) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s
    }
}

/// The file name of a path Postman recorded on any OS: exports carry the
/// author's machine's paths, Windows ones included.
fn base_name(p: &str) -> String {
    let p = p.replace('\\', "/");
    if p.is_empty() {
        return ".".to_string();
    }
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Reports whether an inline body would be misread by the `.http` parser:
/// a line starting with `###` opens a new block and a body starting with
/// `< ` refers to a file.
fn splits_block(body: &str) -> bool {
    let t = body.trim();
    if t.starts_with("< ") || t.starts_with("<@ ") {
        return true;
    }
    body.split('\n').any(|line| line.trim().starts_with("###"))
}

/// Percent-encodes a form field, leaving `{{placeholders}}` for the runner
/// to fill in.
fn form_escape(s: &str) -> String {
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
    replace_braces(&out)
}

/// Turns `%7B%7B` and `%7D%7D` back into `{{` and `}}`, reading left to
/// right.
fn replace_braces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("%7B%7B") {
            out.push_str("{{");
            rest = after;
        } else if let Some(after) = rest.strip_prefix("%7D%7D") {
            out.push_str("}}");
            rest = after;
        } else {
            let mut chars = rest.chars();
            if let Some(c) = chars.next() {
                out.push(c);
            }
            rest = chars.as_str();
        }
    }
    out
}

fn kebab(s: &str) -> String {
    let s = NON_WORD.replace_all(s, "-").to_ascii_lowercase();
    let s = s.trim_matches('-');
    if s.is_empty() {
        "request".to_string()
    } else {
        s.to_string()
    }
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

/// Collapses text into a single line, so a value from the collection
/// cannot inject request blocks or directives into the file.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn non_empty<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.trim().is_empty() {
        fallback
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(base_name("C:\\Users\\me\\report.pdf"), "report.pdf");
        assert_eq!(base_name("/tmp/a/"), "a");
        assert_eq!(base_name(""), ".");
        assert_eq!(base_name("//"), "/");
        assert_eq!(form_escape("a b&{{x}}{y}"), "a+b%26{{x}}%7By%7D");
        assert_eq!(auth_arg("a \"b\""), "\"a \\\"b\\\"\"");
        assert_eq!(kebab("Todo API!"), "todo-api");
        assert_eq!(kebab("?"), "request");
    }
}
