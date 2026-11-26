//! The runner itself: its options, how one is built, and the state a run
//! reads and leaves behind.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use apic_auth::DigestState;
use apic_env::Environments;
use apic_gocompat::{parse_duration, path_error, quote, Time};
use apic_httpc::{CancelToken, Client, RoundTrip, TlsConfig};
use apic_httpfile::{Assert, Capture, Request, RequestKey};
use apic_project::{confine, path, Project};
use apic_session::Jar;

use crate::errors::{uerr, uwrap, Code, Error, SleepError, UsageError};
use crate::proxy::{parse_proxy, proxy_source, ProxyConfig, ProxySettings};
use crate::result::Result;
use crate::tls::ProtoWant;

static VERSION: RwLock<String> = RwLock::new(String::new());

/// Sets the version the User-Agent header carries; the CLI stamps it.
pub fn set_version(version: &str) {
    *VERSION.write().unwrap_or_else(|e| e.into_inner()) = version.to_string();
}

/// The version the User-Agent header carries: `dev` until it is set.
pub fn version() -> String {
    let v = VERSION.read().unwrap_or_else(|e| e.into_inner());
    if v.is_empty() {
        "dev".to_string()
    } else {
        v.clone()
    }
}

/// The session store as the runner and its callers share it.
pub type SharedSession = Arc<Mutex<apic_session::Store>>;

/// Wraps a session store for sharing with a runner.
pub fn shared_session(store: apic_session::Store) -> SharedSession {
    Arc::new(Mutex::new(store))
}

/// Controls a [`Runner`].
#[derive(Clone, Default)]
pub struct Options {
    /// The environment name from http-client.env.json.
    pub env: String,
    /// --var overrides.
    pub vars: BTreeMap<String, String>,
    /// Do not read or write .apic/session.json.
    pub no_session: bool,
    /// The default per-request timeout; zero means apic.yaml's, then
    /// thirty seconds.
    pub timeout: Duration,
    /// Skip TLS verification.
    pub insecure: bool,
    /// In a flow, continue after a failure.
    pub keep_going: bool,
    /// Mask every request value and capture in output (for CI logs).
    pub redact: bool,
    /// `asserts` and `captures` are added to each request `run` is called
    /// for, not to the dependencies it pulls in through `# @ref`: `apic
    /// run --assert` and `--capture`, which the caller has checked.
    pub asserts: Vec<Assert>,
    pub captures: Vec<Capture>,
    /// Saves the response body of the request `run` is called for to this
    /// path (relative to the working directory, overwriting), as a `>>!
    /// file` in the request would; `apic run --output`. Dependencies the
    /// request pulls in through `# @ref` keep their own `>>` lines.
    pub output: String,
    /// Caps the response body read into memory; zero means apic.yaml's
    /// maxBodyBytes, then [`crate::DEFAULT_MAX_BODY_BYTES`].
    pub max_body_bytes: i64,
    /// Use this store instead of opening .apic/session.json (tests use a
    /// memory store).
    pub session: Option<SharedSession>,
    /// Switches the cookie jar on (--cookies); apic.yaml's `cookies: true`
    /// does the same. `cookie_jar` is the jar to use instead of opening
    /// .apic/cookies.json (tests and `apic test` scenarios pass their own).
    pub cookies: bool,
    pub cookie_jar: Option<Jar>,
    /// The default retry policy ("<n> [interval]") for requests without
    /// `# @retry`; empty means apic.yaml's retry, then none. `no_retry`
    /// switches every retry off.
    pub retry: String,
    pub no_retry: bool,
    /// The --cacert, --cert and --key flags: a PEM bundle to trust and a
    /// client certificate to present. They override apic.yaml's tls
    /// section and the environment's SSLConfiguration.
    pub ca_cert: String,
    pub cert: String,
    pub key: String,
    /// The --proxy flag, an http, https or socks5 URL that beats
    /// apic.yaml's proxy and the environment; `no_proxy` (--no-proxy)
    /// sends every request directly, whatever is configured.
    pub proxy: String,
    pub no_proxy: bool,
    /// Records no responses whatever apic.yaml's history says: `apic
    /// test` and the iterations of `apic run --data` set it, so a test
    /// suite or a data file does not push a request's history out.
    pub no_history: bool,
}

/// Reports one failed attempt of a request that is being retried, before
/// the runner waits and sends it again.
#[derive(Clone, Debug)]
pub struct Progress {
    pub req: Arc<Request>,
    /// The attempt that just failed, from 1.
    pub attempt: usize,
    /// Attempts the policy allows.
    pub max: usize,
    /// Why it failed: the first failed assertion, or the error.
    pub failure: String,
}

/// Called after each failed attempt of a request that will be retried.
pub type ProgressFn = Box<dyn Fn(&Progress) + Send + Sync>;
/// Called by `run_all` as each request finishes, with its result and the
/// error, if any, that stopped it.
pub type OnResultFn = Box<dyn Fn(&Result, Option<&Error>) + Send + Sync>;
/// Waits between attempts and for `# @sleep`, unless cancelled first.
pub type SleepFn =
    Box<dyn Fn(&CancelToken, Duration) -> std::result::Result<(), SleepError> + Send + Sync>;
/// The clock the time built-ins read.
pub type NowFn = Box<dyn Fn() -> Time + Send + Sync>;

/// Executes requests for one project.
pub struct Runner {
    pub project: Arc<Project>,
    pub envs: Environments,
    pub session: Option<SharedSession>,
    /// The cookie jar, `None` unless cookies are switched on. It is
    /// in-memory under --no-session and persisted to .apic/cookies.json
    /// otherwise.
    pub jar: Option<Jar>,
    /// Records each named request's response when apic.yaml sets
    /// `history: N`; `None` when it does not, under --no-session, or with
    /// `Options::no_history`.
    pub history: Option<apic_history::Store>,
    pub opts: Options,
    /// Interactive prompts such as device-code sign-in; the process's
    /// standard error unless replaced.
    pub stderr: Option<apic_auth::Stderr>,
    /// Says a person is at the terminal: an OAuth2 grant that needs a
    /// browser may open one. The CLI sets it when stdin and stderr are
    /// terminals and --json is off; MCP and apic test leave it off.
    pub interactive: bool,
    /// When set, called after each failed attempt of a request that will
    /// be retried.
    pub progress: Option<ProgressFn>,
    /// When set, called by `run_all` as each request finishes, with its
    /// result and the error, if any, that stopped it.
    pub on_result: Option<OnResultFn>,
    /// Waits between attempts and for `# @sleep`; tests replace it.
    pub sleep: SleepFn,
    /// The clock the time built-ins read; `None` means the system's, in
    /// the machine's zone. Tests set it to pin `$timestamp` and friends.
    pub now: Option<NowFn>,
    /// The abbreviation of the zone `now` reports its times in, for a
    /// `$localDatetime` layout that prints one. Not read without `now`.
    pub zone: Option<String>,
    /// The requests a target named directly (get-user, users.http#3):
    /// `run_all` sends them even when they are `# @disabled`.
    pub(crate) named: Mutex<HashSet<RequestKey>>,
    /// Variables set at --var precedence after the runner was built.
    pub(crate) extra_vars: Mutex<BTreeMap<String, String>>,
    /// --proxy or apic.yaml's proxy; `None` means the environment.
    pub(crate) proxy: Option<ProxySettings>,
    /// Digest challenges seen this invocation, per server.
    pub(crate) digest: Arc<DigestState>,
    pub(crate) results: Mutex<HashMap<String, Arc<Result>>>,
    pub(crate) captured: Mutex<BTreeMap<String, String>>,
    pub(crate) tls_cache: Mutex<HashMap<String, TlsConfig>>,
    /// Clients are shared across the requests of one invocation, keyed by
    /// their TLS settings, so a flow reuses its connections.
    pub(crate) clients: Mutex<HashMap<String, Client>>,
}

/// Waits for `d` unless `cancel` is cancelled first.
pub fn sleep_cancel(cancel: &CancelToken, d: Duration) -> std::result::Result<(), SleepError> {
    if d.is_zero() {
        return if cancel.is_cancelled() {
            Err(SleepError::Cancelled)
        } else {
            Ok(())
        };
    }
    if cancel.wait_timeout(d) {
        return Err(SleepError::Cancelled);
    }
    Ok(())
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The error for an environment the project's files do not have, with
/// the closest names it does have.
fn unknown_environment(env: &str, envs: &apic_env::Environments, root: &str) -> Error {
    let names = envs.names();
    let file = apic_env::PUBLIC_FILE;
    if names.is_empty() {
        return uerr(
            Code::Environment,
            format!(
                "environment {} requested but no {file} found in {}",
                quote(env),
                root
            ),
        );
    }
    let closest = apic_suggest::closest(env, &names);
    if !closest.is_empty() {
        let mut ue = UsageError::new(
            Code::Environment,
            format!(
                "environment {} not found in {file}; {} (have: {})",
                quote(env),
                apic_suggest::phrase(&closest),
                names.join(", ")
            ),
        );
        ue.suggestions = closest;
        return ue.into();
    }
    uerr(
        Code::Environment,
        format!(
            "environment {} not found in {file} (have: {})",
            quote(env),
            names.join(", ")
        ),
    )
}

/// The proxy requests go through, from the flag or else the project's
/// configuration.
fn proxy_settings(
    p: &Project,
    opts: &Options,
) -> std::result::Result<Option<ProxySettings>, Error> {
    if opts.proxy.is_empty() && p.config.proxy.is_empty() {
        return Ok(None);
    }
    let from_flag = !opts.proxy.is_empty();
    let raw = if from_flag {
        &opts.proxy
    } else {
        &p.config.proxy
    };
    let source = proxy_source(from_flag);
    Ok(Some(ProxySettings {
        url: parse_proxy(raw, source)?,
        source: source.to_string(),
        no_proxy: p.config.no_proxy.clone(),
    }))
}

impl Runner {
    /// Builds a runner, loading env files and the session.
    pub fn new(p: Arc<Project>, mut opts: Options) -> std::result::Result<Runner, Error> {
        for d in &p.diagnostics {
            if d.severity == "error" {
                return Err(uerr(
                    Code::InvalidFile,
                    format!("{}:{}: {} (run `apic validate`)", d.path, d.line, d.message),
                ));
            }
        }
        if opts.env.is_empty() {
            opts.env = p.config.env.clone();
        }
        if opts.timeout.is_zero() {
            opts.timeout = Duration::from_secs(30);
            if !p.config.timeout.is_empty() {
                opts.timeout = parse_duration(&p.config.timeout).map_err(|_| {
                    uerr(
                        Code::Project,
                        format!("apic.yaml: bad timeout {}", quote(&p.config.timeout)),
                    )
                })?;
            }
        }
        let envs =
            apic_env::load(Path::new(&p.root)).map_err(|e| uerr(Code::Project, e.to_string()))?;
        if !opts.env.is_empty() && !envs.has(&opts.env) {
            return Err(unknown_environment(&opts.env, &envs, &p.root));
        }
        // Flag paths are the user's own, relative to where they typed
        // them, which absolute paths tell apart from the project's
        // confined ones.
        for flag in [&mut opts.ca_cert, &mut opts.cert, &mut opts.key] {
            if !flag.is_empty() {
                if let Ok(abs) = path::abs(flag) {
                    *flag = abs;
                }
            }
        }
        let proxy = proxy_settings(&p, &opts)?;
        let root = Path::new(&p.root);
        let session = match &opts.session {
            Some(s) => Some(s.clone()),
            None if !opts.no_session => Some(shared_session(
                apic_session::open(root)
                    .map_err(|e| uerr(Code::Session, format!("session: {e}")))?,
            )),
            None => None,
        };
        let history = (p.config.history > 0 && !opts.no_session && !opts.no_history)
            .then(|| apic_history::Store::new(root, p.config.history));
        let mut jar = None;
        if opts.cookies || p.config.cookies {
            jar = Some(match &opts.cookie_jar {
                Some(j) => j.clone(),
                None if opts.no_session => apic_session::new_memory_jar(),
                None => apic_session::open_jar(root)
                    .map_err(|e| uerr(Code::Session, format!("cookies: {e}")))?,
            });
        }
        Ok(Runner {
            project: p,
            envs,
            session,
            jar,
            history,
            opts,
            stderr: Some(Arc::new(Mutex::new(std::io::stderr()))),
            interactive: false,
            progress: None,
            on_result: None,
            sleep: Box::new(sleep_cancel),
            now: None,
            zone: None,
            named: Mutex::default(),
            extra_vars: Mutex::default(),
            proxy,
            digest: Arc::new(DigestState::new()),
            results: Mutex::default(),
            captured: Mutex::default(),
            tls_cache: Mutex::default(),
            clients: Mutex::default(),
        })
    }

    /// One value of the session, for the current environment.
    pub(crate) fn session_get(&self, name: &str) -> Option<String> {
        let session = self.session.as_ref()?;
        let store = lock(session);
        store.get(&self.opts.env, name).map(str::to_string)
    }

    /// A variable at --var precedence: one set with `set_var`, else one
    /// of the options.
    pub(crate) fn var(&self, name: &str) -> Option<String> {
        if let Some(v) = lock(&self.extra_vars).get(name) {
            return Some(v.clone());
        }
        self.opts.vars.get(name).cloned()
    }

    /// The variables at --var precedence: the options' with those set
    /// since over them.
    pub fn vars(&self) -> BTreeMap<String, String> {
        let mut out = self.opts.vars.clone();
        out.extend(lock(&self.extra_vars).clone());
        out
    }

    /// Adds or overrides a variable at --var precedence.
    pub fn set_var(&self, name: &str, value: &str) {
        lock(&self.extra_vars).insert(name.to_string(), value.to_string());
    }

    /// Takes back a variable set with [`Runner::set_var`]; one of the
    /// options shows through again.
    pub fn unset_var(&self, name: &str) {
        lock(&self.extra_vars).remove(name);
    }

    /// Substitutes {{placeholders}} in arbitrary text using the runner's
    /// variables (no file-level @vars, since no request is in scope).
    pub fn render(&self, s: &str) -> std::result::Result<String, Error> {
        apic_template::render(s, |e| self.resolve_expr(None, e)).map_err(crate::vars::render_error)
    }

    /// Stores a value in the capture layer, below --var and shell
    /// overrides and above the environment files, exactly like `#
    /// @capture`.
    pub fn capture(&self, name: &str, value: &str) {
        lock(&self.captured).insert(name.to_string(), value.to_string());
    }

    /// The named responses of this run, for `{{name.response...}}`
    /// references. Each is the result as it stood when its request
    /// finished.
    pub fn results(&self) -> HashMap<String, Arc<Result>> {
        lock(&self.results).clone()
    }

    /// Registers a named response, e.g. one carried over from another
    /// runner.
    pub fn set_result(&self, name: &str, res: Arc<Result>) {
        lock(&self.results).insert(name.to_string(), res);
    }

    /// A copy of the values captured during this run.
    pub fn captured(&self) -> BTreeMap<String, String> {
        lock(&self.captured).clone()
    }

    /// The time the runner's clock reads now.
    pub fn current_time(&self) -> Time {
        self.clock().time
    }

    pub(crate) fn report(&self, p: Progress) {
        if let Some(progress) = &self.progress {
            progress(&p);
        }
    }

    /// What applying auth needs from the runner: the token cache, the
    /// client token endpoints are reached with, and who is watching.
    /// `deadline` bounds the requests it sends, when the caller has one.
    pub(crate) fn auth_env(
        &self,
        deadline: Option<Instant>,
    ) -> std::result::Result<apic_auth::Env, Error> {
        // Token endpoints get the project-wide settings, not a host
        // override.
        let client = self.client_for("", ProtoWant::Any)?;
        let cache = self.session.as_ref().map(|s| {
            Arc::new(SessionCache {
                session: s.clone(),
                env: self.opts.env.clone(),
            }) as Arc<dyn apic_auth::Cache>
        });
        Ok(apic_auth::Env {
            allow_exec: self.project.config.auth.allow_exec,
            stderr: self.stderr.clone(),
            interactive: self.interactive,
            client: Some(Arc::new(TokenClient {
                client,
                timeout: self.opts.timeout,
                deadline,
                proxy: self.proxy_config(),
            })),
            cache,
            ..apic_auth::Env::default()
        })
    }

    /// Reads a file named relative to the project root, confined to it:
    /// a schema a feature step names, which has no .http file to be
    /// relative to.
    pub fn project_file(&self, rel: &str) -> std::result::Result<Vec<u8>, Error> {
        let root = &self.project.root;
        let real = confine(root, root, rel).map_err(|err| {
            if err.is_outside_root() {
                uerr(
                    Code::BodyFile,
                    format!("{} resolves outside project root", quote(rel)),
                )
            } else {
                uerr(Code::BodyFile, format!("{rel}: {err}"))
            }
        })?;
        std::fs::read(&real).map_err(|e| {
            uerr(
                Code::BodyFile,
                format!("{rel}: {}", path_error("open", Path::new(&real), &e)),
            )
        })
    }

    /// Resolves a `< file` reference relative to the request's file and
    /// confines it to the project root.
    pub(crate) fn file_path(
        &self,
        req: &Request,
        rel: &str,
        what: &str,
    ) -> std::result::Result<String, Error> {
        let root = &self.project.root;
        let dir = path::join(root, &path::dir(&req.file.path));
        confine(root, &dir, rel).map_err(|err| {
            if err.is_outside_root() {
                uerr(
                    Code::BodyFile,
                    format!(
                        "{}:{}: {what} {} resolves outside project root",
                        req.file.path,
                        req.line,
                        quote(rel)
                    ),
                )
            } else {
                uerr(
                    Code::BodyFile,
                    format!("{}:{}: {what}: {err}", req.file.path, req.line),
                )
            }
        })
    }

    /// Reads a `< file` the request refers to (the whole body, or one
    /// part of a multipart body); `what` names it in errors.
    pub(crate) fn read_body_file(
        &self,
        req: &Request,
        rel: &str,
        what: &str,
    ) -> std::result::Result<Vec<u8>, Error> {
        let real = self.file_path(req, rel, what)?;
        std::fs::read(&real).map_err(|e| {
            uerr(
                Code::BodyFile,
                format!(
                    "{}:{}: {what}: {}",
                    req.file.path,
                    req.line,
                    path_error("open", Path::new(&real), &e)
                ),
            )
        })
    }

    /// Explains unresolved variables with a hint on how to provide them.
    pub fn missing_error(&self, req: &Arc<Request>, missing: &[String]) -> Error {
        let mut parts = Vec::new();
        for m in missing {
            let (info, _, _) = self.lookup(Some(req), m, 0);
            let mut hint = format!("pass --var {m}=... or add it to {}", apic_env::PUBLIC_FILE);
            if !info.captured_by.is_empty() {
                let by = &info.captured_by;
                hint = format!(
                    "it is captured by request {}; run `apic run {by}` first, or pass --var {m}=...",
                    quote(by)
                );
            } else if let Some((name, sel)) = m.split_once(".response.") {
                let ran = lock(&self.results)
                    .get(name)
                    .is_some_and(|res| res.raw.is_some());
                hint = if ran {
                    format!("the response of {} has nothing at {sel}", quote(name))
                } else {
                    format!("request {} has not run in this invocation; add `# @ref {name}`, run the whole file as a flow, or capture the value with # @capture", quote(name))
                };
            } else if m.starts_with('$') {
                hint = "built-in could not be resolved".to_string();
            }
            parts.push(format!("{{{{{m}}}}}: {hint}"));
        }
        uerr(
            Code::MissingVariable,
            format!(
                "{}:{}: missing variable{}\n  {}",
                req.file.path,
                req.line,
                plural(missing.len()),
                parts.join("\n  ")
            ),
        )
    }
}

pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

pub(crate) fn dedupe(input: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in input {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// A usage error whose message ends with another error, whose code it
/// keeps.
pub(crate) fn uctx(code: Code, prefix: String, err: &Error) -> Error {
    uwrap(code, format!("{prefix}: {err}"), err)
}

/// Adapts the session store to the token cache, scoped to the current
/// environment.
struct SessionCache {
    session: SharedSession,
    env: String,
}

impl apic_auth::Cache for SessionCache {
    fn get(&self, key: &str) -> Option<String> {
        lock(&self.session).get(&self.env, key).map(str::to_string)
    }

    fn set(&self, key: &str, value: &str) -> std::result::Result<(), String> {
        let mut store = lock(&self.session);
        store.set(&self.env, [(key, value)]);
        store.save().map_err(|e| e.to_string())
    }
}

/// The client token endpoints are reached with: the project's TLS
/// settings, the default timeout and the proxy in effect.
struct TokenClient {
    client: Client,
    timeout: Duration,
    deadline: Option<Instant>,
    proxy: ProxyConfig,
}

impl RoundTrip for TokenClient {
    fn round_trip(
        &self,
        req: &apic_httpc::Request,
    ) -> std::result::Result<apic_httpc::Response, apic_httpc::Error> {
        let mut req = req.clone();
        if req.timeout.is_none() {
            req.timeout = Some(self.timeout);
        }
        req.deadline = match (req.deadline, self.deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        match self.proxy.dial(&req.url) {
            Ok(proxy) => req.proxy = proxy,
            Err(message) => {
                let mut op = req.method.to_lowercase();
                if let Some(first) = op.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                return Err(apic_httpc::Error {
                    kind: apic_httpc::ErrorKind::Other,
                    op,
                    url: req.url.clone(),
                    message,
                    reading_body: false,
                });
            }
        }
        self.client.round_trip(&req)
    }
}
