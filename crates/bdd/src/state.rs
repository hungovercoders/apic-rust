//! What a run shares between its scenarios (the options, the first typed
//! errors, the values to mask) and what each scenario carries from step to
//! step (its runner and the last response).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apic_httpc::CancelToken;
use apic_httpfile::Request;
use apic_project::Project;
use apic_runner::{
    redact_expr, shared_session, usage, Code, Error, Options as RunnerOptions, Runner,
    SharedSession, TransportError, MASKED,
};
use apic_session::Jar;
use regex::Regex;

use crate::gherkin::Table;

/// Where a report or the runner's notes are written: a writer several
/// owners can hold.
pub type Output = Arc<Mutex<dyn Write + Send>>;

/// Shared by every scenario in a run.
#[derive(Clone, Default)]
pub struct Config {
    pub project: Arc<Project>,
    pub env: String,
    pub vars: BTreeMap<String, String>,
    /// Read and write `.apic/session.json` instead of an isolated
    /// in-memory session.
    pub use_session: bool,
    pub timeout: Duration,
    pub insecure: bool,
    pub redact: bool,
    /// Keep a cookie jar (in memory per scenario, shared on disk under
    /// `use_session`).
    pub cookies: bool,
    pub stderr: Option<Output>,
    /// `--cacert`, `--cert` and `--key`.
    pub ca_cert: String,
    pub cert: String,
    pub key: String,
    /// `--proxy` and `--no-proxy`.
    pub proxy: String,
    pub no_proxy: bool,
}

impl Config {
    /// The configuration for a project, everything else at its default.
    pub fn new(project: Arc<Project>) -> Config {
        Config {
            project,
            ..Config::default()
        }
    }
}

/// The shortest secret value masked literally in report text. Shorter
/// values (a captured id of "1", say) cannot be masked by substitution
/// without corrupting line numbers, counts and JSON in the report; they
/// are protected instead by the structural masking of error messages, URLs
/// and request values, which never print secret-sourced values under
/// `--redact`.
const MIN_MASK_LEN: usize = 3;

/// One run's state.
pub(crate) struct Run<'a> {
    pub(crate) cfg: &'a Config,
    /// The first usage error raised by a step (unknown environment,
    /// request or variable).
    pub(crate) usage_err: Option<Error>,
    /// The first transport error raised by a step.
    pub(crate) transport_err: Option<Error>,
    /// Values that must never appear in reports when redacting.
    secrets: BTreeSet<String>,
    pub(crate) stop_on_failure: bool,
    /// A scenario failed and `--stop-on-failure` is on.
    pub(crate) halted: bool,
}

/// Cuts `s` to at most `n` bytes and marks the cut. A character the cut
/// falls inside is replaced, byte for byte, by the replacement character.
pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    for _ in end..n {
        out.push('\u{FFFD}');
    }
    out.push('…');
    out
}

pub(crate) fn excerpt(s: &str, n: usize) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&joined, n)
}

impl<'a> Run<'a> {
    pub(crate) fn new(cfg: &'a Config, stop_on_failure: bool) -> Run<'a> {
        Run {
            cfg,
            usage_err: None,
            transport_err: None,
            secrets: BTreeSet::new(),
            stop_on_failure,
            halted: false,
        }
    }

    /// Records the first usage and transport errors so the run can map
    /// them to exit codes 2 and 3 after the suite finishes.
    fn note_error(&mut self, err: &Error) {
        if err.usage().is_some() {
            if self.usage_err.is_none() {
                self.usage_err = Some(err.clone());
            }
        } else if err.transport().is_some() && self.transport_err.is_none() {
            self.transport_err = Some(err.clone());
        }
    }

    /// Masks a step error's text under `--redact` (error messages may echo
    /// rendered values), records typed errors for the exit code, and
    /// returns the error to report for the step.
    pub(crate) fn fail(&mut self, err: impl Into<Error>) -> Error {
        let mut err: Error = err.into();
        if self.cfg.redact {
            err = if let Some(ue) = err.usage() {
                usage(ue.code, self.mask_error(&ue.msg)).into()
            } else if let Some(te) = err.transport() {
                // The masked text replaces the cause, so the code is kept.
                TransportError::with_code(te.error_code(), self.mask_error(&te.message)).into()
            } else {
                Error::Other(self.mask_error(&err.to_string()))
            };
        }
        self.note_error(&err);
        err
    }

    /// Registers values to mask in redacted reports.
    pub(crate) fn note_secrets<'v>(&mut self, vals: impl IntoIterator<Item = &'v String>) {
        if !self.cfg.redact {
            return;
        }
        for v in vals {
            if !v.is_empty() {
                self.secrets.insert(v.clone());
            }
        }
    }

    /// Masks an error message: every registered secret regardless of
    /// length, since a message is short and never parsed, with short
    /// values replaced only where they stand alone (quoted or between
    /// separators).
    pub(crate) fn mask_error(&self, s: &str) -> String {
        let mut s = self.mask(s);
        for k in &self.secrets {
            if k.len() >= MIN_MASK_LEN {
                continue;
            }
            let pattern = format!(r"(^|[^\pL\pN]){}($|[^\pL\pN])", regex::escape(k));
            if let Ok(re) = Regex::new(&pattern) {
                s = re
                    .replace_all(&s, format!("${{1}}{MASKED}${{2}}"))
                    .into_owned();
            }
        }
        s
    }

    /// Replaces every registered secret value in `s`, longest first.
    pub(crate) fn mask(&self, s: &str) -> String {
        let mut keys: Vec<&String> = self
            .secrets
            .iter()
            .filter(|k| k.len() >= MIN_MASK_LEN)
            .collect();
        if keys.is_empty() {
            return s.to_string();
        }
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        let mut s = s.to_string();
        for k in keys {
            s = s.replace(k.as_str(), MASKED);
        }
        s
    }

    pub(crate) fn new_scenario(&mut self, env: &str) -> Result<Scenario, Error> {
        self.scenario_with(env, None, None)
    }

    /// Builds a scenario for `env`, reusing `store` and `jar` (the
    /// previous scenario's session and cookies) when switching
    /// environments mid-scenario.
    pub(crate) fn scenario_with(
        &mut self,
        env: &str,
        store: Option<SharedSession>,
        jar: Option<Jar>,
    ) -> Result<Scenario, Error> {
        let c = self.cfg;
        let mut opts = RunnerOptions {
            env: env.to_string(),
            vars: c.vars.clone(),
            timeout: c.timeout,
            insecure: c.insecure,
            redact: c.redact,
            cookies: c.cookies,
            ca_cert: c.ca_cert.clone(),
            cert: c.cert.clone(),
            key: c.key.clone(),
            proxy: c.proxy.clone(),
            no_proxy: c.no_proxy,
            no_history: true,
            ..RunnerOptions::default()
        };
        if store.is_some() {
            opts.session = store;
        } else if !c.use_session {
            opts.session = Some(shared_session(apic_session::Store::new_memory()));
        }
        // Each scenario gets its own jar, like its own session;
        // --use-session shares .apic/cookies.json instead.
        if jar.is_some() {
            opts.cookie_jar = jar;
        } else if !c.use_session {
            opts.cookie_jar = Some(Jar::new_memory());
        }
        let mut r = Runner::new(c.project.clone(), opts)?;
        if let Some(stderr) = &c.stderr {
            r.stderr = Some(stderr.clone());
        }
        if c.redact {
            // r.opts.env is the effective environment (apic.yaml may
            // supply the default). --var values are operator input and may
            // be secrets.
            self.note_secrets(c.vars.values());
            self.note_secrets(r.envs.private_vars(&r.opts.env).values());
            self.note_secrets(r.envs.dot_env.values());
            if let Some(session) = &r.session {
                let vars = lock(session).vars(&r.opts.env);
                self.note_secrets(vars.values());
            }
            // APIC_VAR_* is the documented way to pass CI secrets.
            let shell: Vec<String> = std::env::vars_os()
                .filter(|(k, _)| k.to_string_lossy().starts_with("APIC_VAR_"))
                .map(|(_, v)| v.to_string_lossy().into_owned())
                .collect();
            self.note_secrets(shell.iter());
        }
        Ok(Scenario {
            r: Arc::new(r),
            last: None,
        })
    }

    /// Summarises a result for a step error message.
    pub(crate) fn describe_failure(&self, res: &apic_runner::Result) -> String {
        let mut b = String::new();
        let mut url = res.request.display_url(res.redact);
        if res.redact {
            // The query values are masked already; secrets may sit in the
            // path too.
            url = self.mask_error(&url);
        }
        b.push_str(&format!("  {} {url}\n", res.request.method));
        if let Some(resp) = &res.response {
            b.push_str(&format!(
                "  {} {} ({} ms)\n",
                resp.status, resp.status_text, resp.duration_ms
            ));
        }
        for a in &res.asserts {
            if res.redact && (!a.error.is_empty() || !a.pass) {
                b.push_str(&format!("  ✗ {}\n", redact_expr(&a.expr)));
            } else if !a.error.is_empty() {
                b.push_str(&format!("  ✗ {} ({})\n", a.expr, a.error));
            } else if !a.pass {
                b.push_str(&format!(
                    "  ✗ {} (actual: {})\n",
                    a.expr,
                    excerpt(&a.actual, 120)
                ));
            }
        }
        if res.redact {
            if !res.errors.is_empty() {
                b.push_str(&format!(
                    "  ✗ {} error(s) (details hidden by --redact)\n",
                    res.errors.len()
                ));
            }
            return b;
        }
        for e in &res.errors {
            b.push_str(&format!("  ✗ {e}\n"));
        }
        if let Some(raw) = res.raw() {
            if !raw.body.is_empty() {
                b.push_str(&format!(
                    "  body: {}",
                    excerpt(&String::from_utf8_lossy(&raw.body), 300)
                ));
            }
        }
        b
    }
}

pub(crate) fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The per-scenario state.
pub(crate) struct Scenario {
    pub(crate) r: Arc<Runner>,
    pub(crate) last: Option<Arc<apic_runner::Result>>,
}

impl Scenario {
    /// Substitutes {{variables}} in a step argument.
    pub(crate) fn render(&self, run: &mut Run<'_>, text: &str) -> Result<String, Error> {
        self.r.render(text).map_err(|err| {
            if run.cfg.redact {
                run.fail(usage(
                    Code::Features,
                    format!("step value (hidden by --redact): {err}"),
                ))
            } else {
                run.fail(usage(
                    Code::Features,
                    format!("{}: {err}", apic_gocompat::quote(text)),
                ))
            }
        })
    }

    /// Executes a target (request id or file) and records the last
    /// result. `vars` apply to this invocation only: phrase parameters and
    /// `with:` tables do not leak into later steps.
    pub(crate) fn run(
        &mut self,
        run: &mut Run<'_>,
        cancel: &CancelToken,
        target: &str,
        vars: &BTreeMap<String, String>,
    ) -> Result<(), Error> {
        let reqs = match self.r.target(target) {
            Ok(reqs) => reqs,
            Err(err) => {
                let coded =
                    apic_runner::UsageError::wrapping(Code::UnknownRequest, err.to_string(), &err);
                return Err(run.fail(coded));
            }
        };
        self.run_requests(run, cancel, &reqs, vars)
    }

    /// Runs `reqs` as a flow with `vars` scoped to this invocation.
    pub(crate) fn run_requests(
        &mut self,
        run: &mut Run<'_>,
        cancel: &CancelToken,
        reqs: &[Arc<Request>],
        vars: &BTreeMap<String, String>,
    ) -> Result<(), Error> {
        // Whatever happens next, the previous response is no longer "the
        // response": an assertion after an empty or failed run must not
        // read it.
        self.last = None;
        if reqs.is_empty() {
            return Err(run.fail(usage(
                Code::Features,
                "nothing to run: the target has no requests",
            )));
        }
        let restore = self.set_scoped(vars);
        let (results, err) = self.r.run_all(cancel, reqs);
        restore.undo(&self.r);
        // The last response is the last request that went out: a
        // # @disabled request the flow skipped has none.
        self.last = results
            .iter()
            .rev()
            .find(|res| res.skipped.is_empty())
            .map(|res| Arc::new(res.clone()));
        for res in &results {
            run.note_secrets(res.captures.values());
        }
        if let Some(mut err) = err {
            if let (true, Some(te), Some(last)) = (run.cfg.redact, err.transport(), &self.last) {
                // A transport error quotes the full URL; keep the masked
                // form only, and record that form so the original is
                // never printed.
                err = TransportError::with_code(
                    te.error_code(),
                    format!(
                        "could not reach {} {} (details hidden by --redact)",
                        last.request.method,
                        run.mask_error(&last.request.display_url(true))
                    ),
                )
                .into();
            }
            return Err(run.fail(err));
        }
        for res in &results {
            if !res.ok {
                let id = if res.request.name.is_empty() {
                    format!("{}:{}", res.request.file, res.request.line)
                } else {
                    res.request.name.clone()
                };
                let text = format!("{id} failed:\n{}", run.describe_failure(res));
                return Err(run.fail(Error::Other(text)));
            }
        }
        Ok(())
    }

    /// Applies `vars` at --var precedence and returns what restores the
    /// previous values.
    pub(crate) fn set_scoped(&self, vars: &BTreeMap<String, String>) -> Scoped {
        let current = self.r.vars();
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((k.clone(), current.get(k).cloned()));
            self.r.set_var(k, v);
        }
        Scoped { saved }
    }

    pub(crate) fn require_last(&self) -> Result<Arc<apic_runner::Result>, Error> {
        match &self.last {
            Some(last) if last.raw().is_some() => Ok(last.clone()),
            _ => Err(Error::Other(
                "no response yet: run a request first (e.g. `When I run \"get-user\"`)".to_string(),
            )),
        }
    }

    /// Renders rows top to bottom, each one seeing the rows above it at
    /// --var precedence, and returns the values plus what undoes that
    /// scoping (in reverse order, so a repeated name restores correctly).
    pub(crate) fn render_table(
        &self,
        run: &mut Run<'_>,
        rows: &[TableRow],
    ) -> Result<(BTreeMap<String, String>, Vec<Scoped>), Error> {
        let mut vars = BTreeMap::new();
        let mut undo: Vec<Scoped> = Vec::new();
        for row in rows {
            let v = match self.render(run, &row.value) {
                Ok(v) => v,
                Err(err) => {
                    undo_all(&self.r, undo);
                    return Err(err);
                }
            };
            vars.insert(row.name.clone(), v.clone());
            undo.push(self.set_scoped(&BTreeMap::from([(row.name.clone(), v)])));
        }
        Ok((vars, undo))
    }
}

/// The values variables had before a scoped assignment.
pub(crate) struct Scoped {
    saved: Vec<(String, Option<String>)>,
}

impl Scoped {
    pub(crate) fn undo(self, r: &Runner) {
        for (k, old) in self.saved {
            match old {
                Some(v) => r.set_var(&k, &v),
                None => r.unset_var(&k),
            }
        }
    }
}

/// Undoes scoped assignments, the latest first.
pub(crate) fn undo_all(r: &Runner, undo: Vec<Scoped>) {
    for scoped in undo.into_iter().rev() {
        scoped.undo(r);
    }
}

/// One `name | value` line of a step table, kept in file order so a value
/// may refer to the rows above it.
pub(crate) struct TableRow {
    pub(crate) name: String,
    pub(crate) value: String,
}

/// Reads a two-column table (with or without a header row) in row order;
/// a later row may reference an earlier one with {{name}}.
pub(crate) fn table_vars(t: &Table) -> Result<Vec<TableRow>, Error> {
    let mut out = Vec::new();
    for (i, row) in t.rows.iter().enumerate() {
        let [k, v] = row.as_slice() else {
            return Err(usage(
                Code::Features,
                format!("table row {} must have two cells: name | value", i + 1),
            )
            .into());
        };
        if i == 0 && k.eq_ignore_ascii_case("name") && v.eq_ignore_ascii_case("value") {
            continue;
        }
        if let Err(err) = check_var_name(k) {
            return Err(Error::context(format!("table row {}", i + 1), err));
        }
        out.push(TableRow {
            name: k.clone(),
            value: v.clone(),
        });
    }
    Ok(out)
}

/// Applies the same rule as `# @capture` names so every value set from a
/// feature can be referenced as {{name}}.
pub(crate) fn check_var_name(name: &str) -> Result<(), Error> {
    let mut chars = name.chars();
    let valid = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        return Err(usage(
            Code::Features,
            format!(
                "invalid variable name {}: letters, digits, underscore, dot and dash, starting with a letter or underscore",
                apic_gocompat::quote(name)
            ),
        )
        .into());
    }
    if name.contains(".response.") {
        // {{name.response.…}} always means a named response, so such a
        // variable could never be read back.
        return Err(usage(
            Code::Features,
            format!(
                "invalid variable name {}: \".response.\" is reserved for response references",
                apic_gocompat::quote(name)
            ),
        )
        .into());
    }
    Ok(())
}
