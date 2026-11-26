use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use apic_gocompat::quote;
use apic_httpfile::{parse_retry, parse_sleep, Assert, Diagnostic, Ref, Request, RequestKey};
use apic_phrase::Phrase;

use crate::config::CONFIG_FILE;
use crate::confine::confine;
use crate::path;
use crate::project::{within, Error, Project};

/// Builds a diagnostic spanning `[col, end)` on `line`; zero columns mean
/// the whole line.
fn diag(
    path: &str,
    severity: &str,
    code: &str,
    line: usize,
    col: usize,
    end: usize,
    msg: String,
) -> Diagnostic {
    let mut d = Diagnostic {
        path: path.to_string(),
        line,
        severity: severity.to_string(),
        code: code.to_string(),
        message: msg,
        ..Diagnostic::default()
    };
    if col > 0 {
        d.column = col;
        d.end_line = line;
        d.end_column = end;
    }
    d
}

impl Project {
    /// Returns parse diagnostics plus project-level checks.
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut v = Validator {
            p: self,
            diags: self.diagnostics.clone(),
            phrases: Vec::new(),
        };
        v.duplicate_names();
        v.diags.extend(validate_auth_configs(&self.root));
        v.config();
        for r in self.requests() {
            v.steps(&r);
            v.refs(&r);
            v.timing_directives(&r);
            v.auth_directives(&r);
            v.asserts(&r);
            v.captures(&r);
            v.bodies(&r);
        }
        // The sort is stable: ties keep the order the checks ran in.
        v.diags
            .sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
        v.diags
    }

    /// Resolves a `# @ref` target to the one request it names. A request
    /// naming itself resolves, and is reported as a cycle by `ref_path`.
    fn ref_target(&self, r: &Ref) -> Result<Arc<Request>, Error> {
        if r.id.is_empty() {
            return Err(Error::msg("needs a request name"));
        }
        let mut targets = self.resolve(&r.id)?;
        if targets.len() != 1 {
            return Err(Error::msg(format!(
                "names {} requests; refer to one request by name or file#name",
                targets.len()
            )));
        }
        Ok(targets.remove(0))
    }

    /// Follows `# @ref` directives from `from` and returns the requests on
    /// the way to `to` (ending with it), or `None` when `to` is not
    /// reachable. Targets that do not resolve are skipped: bad-ref reports
    /// those.
    fn ref_path(
        &self,
        from: &Arc<Request>,
        to: &Arc<Request>,
        seen: &mut HashSet<RequestKey>,
    ) -> Option<Vec<Arc<Request>>> {
        if Arc::ptr_eq(from, to) {
            return Some(vec![Arc::clone(to)]);
        }
        if !seen.insert(RequestKey::new(from)) {
            return None;
        }
        for r in from.refs() {
            let Ok(target) = self.ref_target(&r) else {
                continue;
            };
            if let Some(rest) = self.ref_path(&target, to, seen) {
                let mut chain = vec![Arc::clone(from)];
                chain.extend(rest);
                return Some(chain);
            }
        }
        None
    }
}

/// Collects `validate`'s diagnostics, one check per concern. The order the
/// checks run in is the order ties keep after the stable sort.
struct Validator<'a> {
    p: &'a Project,
    diags: Vec<Diagnostic>,
    /// Every `# @step` so far, to find ambiguous pairs.
    phrases: Vec<DeclaredStep>,
}

struct DeclaredStep {
    req: Arc<Request>,
    ph: Phrase,
    /// Line of the `# @step` directive.
    line: usize,
}

impl Validator<'_> {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        path: &str,
        severity: &str,
        code: &str,
        line: usize,
        col: usize,
        end: usize,
        msg: String,
    ) {
        self.diags
            .push(diag(path, severity, code, line, col, end, msg));
    }

    /// The directory of a request's file, as an absolute path.
    fn dir_of(&self, r: &Request) -> String {
        path::join(&self.p.root, &path::dir(&r.file.path))
    }

    fn duplicate_names(&mut self) {
        let p = self.p;
        for (name, rs) in &p.by_name {
            if rs.len() < 2 {
                continue;
            }
            for r in rs {
                let (line, col, end) = r.directive_span("name");
                self.add(
                    &r.file.path,
                    "warning",
                    "duplicate-name",
                    line,
                    col,
                    end,
                    format!(
                        "request name {} is also used elsewhere; `apic run {}` will be ambiguous",
                        quote(name),
                        name
                    ),
                );
            }
        }
    }

    fn config(&mut self) {
        let cfg = &self.p.config;
        if !cfg.auth.default.is_empty() {
            match apic_auth::parse(&cfg.auth.default) {
                Err(err) => self.add(
                    CONFIG_FILE,
                    "error",
                    "bad-config-auth",
                    0,
                    0,
                    0,
                    format!("auth.default: {err}"),
                ),
                Ok(spec) if spec.r#type == "exec" && !cfg.auth.allow_exec => self.add(
                    CONFIG_FILE,
                    "warning",
                    "exec-disabled",
                    0,
                    0,
                    0,
                    "auth.default: @auth exec will be refused until apic.yaml sets auth.allowExec: true"
                        .to_string(),
                ),
                Ok(_) => {}
            }
        }
        if !cfg.retry.is_empty() {
            if let Err(err) = parse_retry(&cfg.retry) {
                self.add(
                    CONFIG_FILE,
                    "error",
                    "bad-retry",
                    0,
                    0,
                    0,
                    format!("retry {}: {err}", quote(&cfg.retry)),
                );
            }
        }
    }

    /// Checks each `# @step` phrase on its own and against every phrase
    /// declared before it, here and in earlier requests.
    fn steps(&mut self, r: &Arc<Request>) {
        let file = &r.file.path;
        for d in &r.directives {
            if d.key != "step" {
                continue;
            }
            let (col, end) = (d.column, d.column + d.value.len());
            let ph = match Phrase::parse(&d.value) {
                Ok(ph) => ph,
                Err(err) => {
                    self.add(file, "error", "bad-step", d.line, col, end, err);
                    continue;
                }
            };
            if let Err(err) = ph.conflicts_with_builtin() {
                self.add(file, "error", "ambiguous-step", d.line, col, end, err);
            }
            let clashes: Vec<String> = self
                .phrases
                .iter()
                .filter(|other| ph.conflicts_with(&other.ph))
                .map(|other| {
                    format!(
                        "@step {} matches the same text as @step {} on {} ({}:{})",
                        quote(&ph.text),
                        quote(&other.ph.text),
                        other.req.id(),
                        other.req.file.path,
                        other.line
                    )
                })
                .collect();
            for msg in clashes {
                self.add(file, "error", "ambiguous-step", d.line, col, end, msg);
            }
            self.phrases.push(DeclaredStep {
                req: Arc::clone(r),
                ph,
                line: d.line,
            });
        }
    }

    fn refs(&mut self, r: &Arc<Request>) {
        let file = &r.file.path;
        for rf in r.refs() {
            let (col, end) = (rf.column, rf.column + rf.id.len());
            let key = if rf.force { "@forceRef" } else { "@ref" };
            let target = match self.p.ref_target(&rf) {
                Ok(target) => target,
                Err(err) => {
                    let msg = format!("{key} {}: {err}", rf.id);
                    self.add(file, "error", "bad-ref", rf.line, col, end, msg);
                    continue;
                }
            };
            let Some(chain) = self.p.ref_path(&target, r, &mut HashSet::new()) else {
                continue;
            };
            let mut ids = vec![r.id()];
            ids.extend(chain.iter().map(|c| c.id()));
            let msg = format!("{key} {} is a cycle: {}", rf.id, ids.join(" -> "));
            self.add(file, "error", "ref-cycle", rf.line, col, end, msg);
        }
    }

    /// Checks `# @retry` and `# @sleep`.
    fn timing_directives(&mut self, r: &Request) {
        let file = &r.file.path;
        for d in &r.directives {
            let (col, end) = (d.column, d.column + d.value.len());
            match d.key.as_str() {
                "retry" => {
                    if let Err(err) = parse_retry(&d.value) {
                        let msg = format!("@retry {}: {err}", quote(&d.value));
                        self.add(file, "error", "bad-retry", d.line, col, end, msg);
                    }
                }
                "sleep" => {
                    if let Err(err) = parse_sleep(&d.value) {
                        let msg = format!("@sleep: {err}");
                        self.add(file, "error", "bad-sleep", d.line, col, end, msg);
                    }
                }
                _ => {}
            }
        }
    }

    fn auth_directives(&mut self, r: &Request) {
        let file = &r.file.path;
        for d in &r.directives {
            if d.key != "auth" {
                continue;
            }
            let (col, end) = (d.column, d.column + d.value.len());
            match apic_auth::parse(&d.value) {
                Err(err) => {
                    self.add(file, "error", "bad-auth", d.line, col, end, err.to_string());
                }
                Ok(spec) if spec.r#type == "exec" && !self.p.config.auth.allow_exec => self.add(
                    file,
                    "warning",
                    "exec-disabled",
                    d.line,
                    col,
                    end,
                    "@auth exec will be refused until apic.yaml sets auth.allowExec: true"
                        .to_string(),
                ),
                Ok(_) => {}
            }
        }
    }

    fn asserts(&mut self, r: &Request) {
        let file = &r.file.path;
        for a in &r.asserts {
            let expr = match apic_assert::parse(&a.expr) {
                Ok(expr) => expr,
                Err(err) => {
                    let end = a.column + a.expr.len();
                    self.add(file, "error", "bad-assert", a.line, a.column, end, err);
                    continue;
                }
            };
            if let Err(err) = apic_selector::check(&expr.selector) {
                // The selector opens the expression, so its span starts
                // where the expression does.
                let msg = format!("assert {}: {}", quote(&a.expr), selector_problem(&err));
                let end = a.column + expr.selector.len();
                self.add(
                    file,
                    "error",
                    "unknown-selector",
                    a.line,
                    a.column,
                    end,
                    msg,
                );
            }
            if expr.op == "matchesSchema" && !expr.value.contains("{{") {
                self.schema_file(r, a, &expr.value);
            }
        }
    }

    /// Checks the file a matchesSchema assertion names, resolved as the
    /// runner reads it: relative to the file or absolute, through
    /// symlinks, confined to the root.
    fn schema_file(&mut self, r: &Request, a: &Assert, schema: &str) {
        let why = match confine(&self.p.root, &self.dir_of(r), schema) {
            Ok(real) if fs::metadata(&real).is_ok() => return,
            Err(err) if err.is_outside_root() => "resolves outside the project root",
            _ => "not found",
        };
        // Where the file name starts within the expression; one column
        // before the expression when it cannot be found in it.
        let col = match a.expr.rfind(schema) {
            Some(at) => a.column + at,
            None => a.column.saturating_sub(1),
        };
        self.add(
            &r.file.path,
            "error",
            "missing-schema-file",
            a.line,
            col,
            col + schema.len(),
            format!("schema file {schema} {why}"),
        );
    }

    fn captures(&mut self, r: &Request) {
        for c in &r.captures {
            if let Err(err) = apic_selector::check(&c.selector) {
                let msg = format!("capture {}: {}", quote(&c.name), selector_problem(&err));
                self.add(
                    &r.file.path,
                    "error",
                    "unknown-selector",
                    c.line,
                    c.column,
                    c.column + c.selector.len(),
                    msg,
                );
            }
        }
    }

    /// Checks what a request sends and where its response goes: the
    /// multipart body, the `>>` save path, a GraphQL body and `<` body
    /// files.
    fn bodies(&mut self, r: &Request) {
        let file = &r.file.path;
        let body_line = if r.body_line == 0 {
            r.line
        } else {
            r.body_line
        };
        if let Err(err) = r.multipart() {
            self.add(file, "error", "bad-multipart", body_line, 0, 0, err);
        }
        if let Some(sv) = &r.save_to {
            let target = if path::is_abs(&sv.path) {
                sv.path.clone()
            } else {
                path::join(&self.dir_of(r), &sv.path)
            };
            if within(&self.p.root, &target).is_none() {
                let msg = format!(
                    ">> {} resolves outside the project root; the response body is only written inside it",
                    sv.path
                );
                let end = sv.column + sv.path.len();
                self.add(file, "error", "bad-save-path", sv.line, sv.column, end, msg);
            }
        }
        if r.is_graphql() {
            if let Err(err) = r.graphql() {
                self.add(file, "error", "bad-graphql", body_line, 0, 0, err);
            }
        }
        for rf in r.body_files() {
            let target = path::join(&self.dir_of(r), &rf.path);
            match fs::metadata(&target) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                _ => continue,
            }
            let line = if rf.line == 0 { r.line } else { rf.line };
            let end = rf.column + rf.path.len();
            let msg = format!("body file {} not found", rf.path);
            self.add(
                file,
                "error",
                "missing-body-file",
                line,
                rf.column,
                end,
                msg,
            );
        }
    }
}

/// How validate words a selector it cannot read: the short
/// `unknown selector "x"` for a form apic does not know, and the parser's
/// own message (naming the part) for a body path.
fn selector_problem(err: &apic_selector::Error) -> String {
    match err {
        apic_selector::Error::Unknown { selector } => {
            format!("unknown selector {}", quote(selector))
        }
        other => other.to_string(),
    }
}

/// Checks the env files' JetBrains Security.Auth configurations as each
/// environment sees them (the private file's fields merged over the public
/// file's): one that cannot be used is an error, a field apic does not act
/// on a warning. Each is reported once.
fn validate_auth_configs(root: &str) -> Vec<Diagnostic> {
    let Ok(envs) = apic_env::load(Path::new(root)) else {
        // The env files' own errors are reported when they are loaded.
        return Vec::new();
    };
    let mut diags = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut names = vec![String::new()];
    names.extend(envs.names());
    for env in &names {
        // The configurations come sorted by name.
        for (name, config) in envs.auth(env) {
            let file = config.files.last().map_or("", String::as_str);
            let render = |s: &str| Ok::<String, String>(s.to_string());
            match apic_auth::from_jetbrains(&config.fields, render) {
                Err(err) => {
                    let msg = format!("Security.Auth {}: {err}", quote(&name));
                    if seen.insert(msg.clone()) {
                        diags.push(diag(file, "error", "bad-auth-config", 0, 0, 0, msg));
                    }
                }
                Ok(jb) => {
                    for field in &jb.ignored {
                        let msg = format!(
                            "Security.Auth {}: {} is not used by apic (ignored)",
                            quote(&name),
                            quote(field)
                        );
                        if seen.insert(msg.clone()) {
                            diags.push(diag(file, "warning", "unknown-auth-key", 0, 0, 0, msg));
                        }
                    }
                }
            }
        }
    }
    diags
}
