//! Sending: one request with its `# @ref` dependencies, its retries and
//! what it leaves behind; and a list of requests as a flow.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apic_auth::{digest_host, DigestTransport};
use apic_gocompat::{check_json, parse_duration, path_error, quote};
use apic_httpc::{
    follow_redirects, CancelToken, CookieJar, RedirectAction, RedirectPolicy, RoundTrip,
};
use apic_httpfile::{Assert, Capture, Ref, Request, RequestKey};
use apic_project::{path, Project};
use apic_session::{EnvJar, RequestUrl};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::body::body_too_large;
use crate::errors::{info, uerr, Code, Error, TransportError};
use crate::gourl::UrlParts;
use crate::result::{is_sensitive_header, Body, Resolved, Response, Result, Timings, BASE64};
use crate::runner::{dedupe, lock, uctx, Progress};
use crate::tls::{want_for, ProtoWant};
use crate::vars::render_error;
use crate::Runner;

/// What `run` gives back: the result, when the request got far enough to
/// have one, and the error that stopped it, if any. A request whose `#
/// @ref` dependency could not run has both: the result keeps what the
/// dependencies produced.
pub type Outcome = (Option<Result>, Option<Error>);

fn ref_key(r: &Ref) -> &'static str {
    if r.force {
        "@forceRef"
    } else {
        "@ref"
    }
}

/// The result of a request that never went out: the request as written.
fn unsent(req: &Arc<Request>) -> Result {
    Result {
        request: Resolved {
            name: req.name.clone(),
            file: req.file.path.clone(),
            line: req.line,
            method: req.method.clone(),
            url: req.url.clone(),
            ..Resolved::default()
        },
        ..Result::for_request(req)
    }
}

/// How many times a request may be sent and the wait between attempts.
#[derive(Clone, Copy)]
struct RetryPolicy {
    n: usize,
    interval: Duration,
}

struct PreparedAssert {
    expr: apic_assert::Expr,
    expected: String,
}

/// Pulls in the requests one request's `# @ref`s name: the forced ones
/// always, the rest only when the request is missing a variable.
struct DepRun<'a> {
    r: &'a Runner,
    cancel: &'a CancelToken,
    req: &'a Arc<Request>,
    chain: &'a [Arc<Request>],
    ran: &'a mut HashSet<RequestKey>,
    deps: Vec<Result>,
}

impl DepRun<'_> {
    /// Runs the dependencies the request needs and resolves it. An `Err`
    /// is how the request ends without going out: with its own result,
    /// failed because a dependency did, with an error, or with both.
    fn resolve(&mut self) -> std::result::Result<Resolved, Box<Outcome>> {
        let refs = self.req.refs();
        for r in refs.iter().filter(|r| r.force) {
            if let Some(out) = self.run(r) {
                return Err(Box::new(out));
            }
        }
        let mut resolved = self
            .r
            .resolve(self.req)
            .map_err(|e| Box::new((None, Some(e))))?;
        if !resolved.missing.is_empty() && self.pull(&refs)? {
            resolved = self
                .r
                .resolve(self.req)
                .map_err(|e| Box::new((None, Some(e))))?;
        }
        if !resolved.missing.is_empty() {
            let err = self.r.missing_error(self.req, &resolved.missing);
            return Err(Box::new((None, Some(err))));
        }
        Ok(resolved)
    }

    /// Runs the plain `# @ref`s this invocation has not run yet.
    fn pull(&mut self, refs: &[Ref]) -> std::result::Result<bool, Box<Outcome>> {
        let mut pulled = false;
        for r in refs.iter().filter(|r| !r.force) {
            if let Ok(target) = self.r.ref_target(self.req, r) {
                if self.ran.contains(&RequestKey::new(&target)) {
                    continue;
                }
            }
            if let Some(out) = self.run(r) {
                return Err(Box::new(out));
            }
            pulled = true;
        }
        Ok(pulled)
    }

    /// Runs one dependency. `Some` means it failed and the request will
    /// not go out.
    fn run(&mut self, r: &Ref) -> Option<Outcome> {
        let target = match self.r.ref_target(self.req, r) {
            Ok(t) => t,
            Err(err) => return Some((Some(self.failed(err.to_string(), Some(&err))), Some(err))),
        };
        let mut next = self.chain.to_vec();
        next.push(self.req.clone());
        if next.iter().any(|c| Arc::ptr_eq(c, &target)) {
            return Some((None, Some(cycle_error(self.req, r, &next, &target))));
        }
        self.ran.insert(RequestKey::new(&target));
        let (dep, err) = self.r.run_chain(self.cancel, &target, &next, self.ran);
        let dep_ok = dep.as_ref().is_some_and(|d| d.ok);
        self.deps.extend(dep);
        if let Some(err) = err {
            let msg = format!("{} {}: {err}", ref_key(r), r.id);
            return Some((Some(self.failed(msg, Some(&err))), Some(err)));
        }
        if !dep_ok {
            let msg = format!("{} {} failed", ref_key(r), r.id);
            return Some((Some(self.failed(msg, None)), None));
        }
        None
    }

    /// Builds the result of a request that never went out because a
    /// dependency failed, keeping what the dependency produced. `err` is
    /// the error behind `msg`, if any; a dependency that failed its
    /// assertions is not an error and has none.
    fn failed(&mut self, msg: String, err: Option<&Error>) -> Result {
        let mut res = unsent(self.req);
        res.deps = std::mem::take(&mut self.deps);
        res.redact = self.r.opts.redact;
        if let Some(err) = err {
            let mut e = info(err);
            e.message = msg.clone();
            res.error = Some(e);
        }
        res.errors = vec![msg];
        res
    }
}

/// Names a `# @ref` chain that leads back to a request already on it, as
/// "a -> b -> a".
fn cycle_error(req: &Request, r: &Ref, chain: &[Arc<Request>], target: &Arc<Request>) -> Error {
    let mut ids = Vec::new();
    for c in chain {
        if Arc::ptr_eq(c, target) || !ids.is_empty() {
            ids.push(c.id());
        }
    }
    ids.push(target.id());
    uerr(
        Code::Ref,
        format!(
            "{}:{}: {} {} is a cycle: {}",
            req.file.path,
            r.line,
            ref_key(r),
            r.id,
            ids.join(" -> ")
        ),
    )
}

/// What a request's history is kept under: its name, or file#name when
/// the name is used in more than one file, so two requests that share a
/// name never share a history.
pub fn history_key(p: &Project, req: &Request) -> String {
    if p.lookup(&req.name).is_err() {
        return format!("{}#{}", req.file.path, req.name);
    }
    req.name.clone()
}

/// The cookie jar of one environment, as the redirect helper asks for it.
struct JarAdapter(EnvJar);

impl CookieJar for JarAdapter {
    fn cookies(&self, url: &str) -> Vec<(String, String)> {
        let Some(u) = RequestUrl::parse(url) else {
            return Vec::new();
        };
        let Some(header) = self.0.cookie_header(&u, None) else {
            return Vec::new();
        };
        header
            .split("; ")
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (name.to_string(), value.to_string())
            })
            .collect()
    }

    fn set_cookies(&self, url: &str, set_cookie: &[String]) {
        if let Some(u) = RequestUrl::parse(url) {
            self.0.set_cookie_headers(&u, set_cookie);
        }
    }
}

/// Says how digest auth went for the result.
fn digest_note(digest: &DigestTransport) -> String {
    if digest.challenged() {
        format!(
            "digest: 401 challenge answered ({} requests)",
            digest.rounds()
        )
    } else if digest.answered() {
        "digest: answered from an earlier challenge".to_string()
    } else {
        "digest: the server sent no challenge".to_string()
    }
}

/// The JSON view of a response body: the JSON itself when it is JSON, the
/// text when it is text, and base64 (with the encoding named) when it is
/// neither, so a binary body survives `--json` intact.
pub(crate) fn json_or_string(data: &[u8]) -> (Body, String) {
    let Ok(text) = std::str::from_utf8(data) else {
        return (Body::text(STANDARD.encode(data)), BASE64.to_string());
    };
    let t = text.trim();
    if !t.is_empty() && check_json(t.as_bytes()).is_ok() {
        return (Body::json(t), String::new());
    }
    (Body::text(text), String::new())
}

fn host_of(url: &str) -> String {
    UrlParts::parse(url).map(|u| u.host).unwrap_or_default()
}

impl Result {
    /// Runs the captures and the assertions against the response.
    fn evaluate(
        &mut self,
        captures: &[Capture],
        asserts: &[PreparedAssert],
        schemas: apic_assert::Options<'_>,
    ) {
        let Some(raw) = self.raw.clone() else {
            return;
        };
        for c in captures {
            match apic_selector::select(&raw, &c.selector) {
                Err(err) => self.fail(format!("capture {}: {err}", c.name)),
                Ok(None) => self.fail(format!("capture {}: nothing at {}", c.name, c.selector)),
                Ok(Some(v)) => {
                    self.captures.insert(c.name.clone(), v);
                }
            }
        }
        for a in asserts {
            let ar = apic_assert::eval_with(&a.expr, &a.expected, &raw, schemas);
            if !ar.pass {
                self.ok = false;
            }
            self.asserts.push(ar);
        }
    }
}

impl Runner {
    /// Resolves, sends and evaluates a single request. An error is a
    /// usage or transport problem; assertion failures are reported in
    /// the result's `ok`.
    ///
    /// Requests the request declares with `# @forceRef` run first every
    /// time; those declared with `# @ref` run first only when a variable
    /// is missing, and once per call. Their results ride in the result's
    /// `deps`. A dependency that fails stops the request: the result is
    /// not OK and names it.
    pub fn run(&self, cancel: &CancelToken, req: &Arc<Request>) -> Outcome {
        let mut ran = HashSet::new();
        let (mut res, err) = self.run_chain(cancel, req, &[], &mut ran);
        if err.is_none() {
            if let Some(res) = &mut res {
                if !self.opts.output.is_empty() && res.raw.is_some() {
                    // --output names a path from the working directory,
                    // not the file.
                    if let Err(e) = self.save_body(req, &self.opts.output, true, true, res) {
                        res.fail(e);
                    }
                }
                // Recorded once the result is final, --output included, so
                // the history holds what `apic run --json` prints.
                self.record(req, res);
            }
        }
        (res, err)
    }

    /// Writes the response body of result to path: a `>> file` path
    /// relative to the request's file and confined to the project, or an
    /// --output path relative to the working directory. The file is
    /// created 0600 when a secret went into the request (its response may
    /// carry one back), else 0644; without overwrite an existing file is
    /// refused.
    fn save_body(
        &self,
        req: &Request,
        to: &str,
        overwrite: bool,
        from_cwd: bool,
        result: &mut Result,
    ) -> std::result::Result<(), String> {
        let io = |op: &str, p: &str, e: &std::io::Error| {
            format!("save to {to}: {}", path_error(op, Path::new(p), e))
        };
        let target = if from_cwd {
            path::abs(to).map_err(|e| io("getwd", to, &e))?
        } else {
            self.file_path(req, to, ">> file")
                .map_err(|e| e.to_string())?
        };
        let dir = path::dir(&target);
        std::fs::create_dir_all(&dir).map_err(|e| io("mkdir", &dir, &e))?;
        let secret = result.request.secret;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true);
        if overwrite {
            opts.create(true).truncate(true);
        } else {
            opts.create_new(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(if secret { 0o600 } else { 0o644 });
        }
        let mut f = opts.open(&target).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("save to {to}: the file exists; use >>! to overwrite it")
            } else {
                io("open", &target, &e)
            }
        })?;
        let body = result
            .raw
            .as_ref()
            .map(|r| r.body.as_slice())
            .unwrap_or(&[]);
        std::io::Write::write_all(&mut f, body).map_err(|e| io("write", &target, &e))?;
        drop(f);
        #[cfg(unix)]
        if secret {
            // An overwritten file keeps the mode it had; a response to a
            // request that carried a secret is tightened whatever it was.
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
        }
        // Reported relative to the project when inside it; the root may
        // be reached through a symlink while target is the real path.
        let root = &self.project.root;
        result.saved_to = [root.clone(), apic_project::real_prefix(root)]
            .iter()
            .find_map(|root| apic_project::within(root, &target))
            .map_or(target, |rel| path::to_slash(&rel));
        Ok(())
    }

    /// Resolves the target of a `# @ref` to exactly one request.
    fn ref_target(&self, req: &Request, r: &Ref) -> std::result::Result<Arc<Request>, Error> {
        let at = format!("{}:{}: {} {}", req.file.path, r.line, ref_key(r), r.id);
        let mut targets = self
            .project
            .resolve(&r.id)
            .map_err(|e| uctx(Code::Ref, at.clone(), &Error::Project(e)))?;
        if targets.len() != 1 {
            return Err(uerr(
                Code::Ref,
                format!(
                    "{at} names {} requests; refer to one request by name or file#name",
                    targets.len()
                ),
            ));
        }
        Ok(targets.remove(0))
    }

    /// `run` with the chain of requests whose refs led here, for cycle
    /// detection and the error that names the cycle, and the `# @ref`
    /// targets this call has already run, so a dependency reached twice
    /// runs once. Both are per call: the next `run` starts afresh.
    fn run_chain(
        &self,
        cancel: &CancelToken,
        req: &Arc<Request>,
        chain: &[Arc<Request>],
        ran: &mut HashSet<RequestKey>,
    ) -> Outcome {
        let mut d = DepRun {
            r: self,
            cancel,
            req,
            chain,
            ran,
            deps: Vec::new(),
        };
        let resolved = match d.resolve() {
            Ok(r) => r,
            Err(out) => return *out,
        };
        let top = chain.is_empty();
        let mut result = Result {
            request: resolved,
            ok: true,
            redact: self.opts.redact,
            deps: d.deps,
            ..Result::for_request(req)
        };
        match self.send(cancel, req, &mut result, top) {
            Ok(()) => (Some(result), None),
            Err(err) => (None, Some(err)),
        }
    }

    /// Sends a resolved request, with its retries, and keeps what it
    /// produced.
    fn send(
        &self,
        cancel: &CancelToken,
        req: &Arc<Request>,
        result: &mut Result,
        top: bool,
    ) -> std::result::Result<(), Error> {
        let mut asserts = req.asserts.clone();
        let mut captures = req.captures.clone();
        if top {
            // The ad hoc checks apply to the request that was asked for,
            // not to what it pulled in.
            asserts.extend(self.opts.asserts.iter().cloned());
            captures.extend(self.opts.captures.iter().cloned());
        }
        let prepared = self.prepare_asserts(req, &asserts)?;
        let (timeout, policy) = self.pacing(cancel, req)?;
        // Each schema is read and resolved once for all the attempts.
        let loader = apic_assert::schemas(|p: &str| {
            self.read_body_file(req, p, "schema")
                .map_err(|e| e.to_string())
        });
        let schemas = apic_assert::Options {
            schema: Some(&loader),
        };
        let resolved = result.request.clone();
        self.retry(cancel, req, policy, result, || {
            self.attempt(
                cancel, req, &resolved, &prepared, &captures, schemas, timeout,
            )
        })?;
        self.keep(req, result, top);
        Ok(())
    }

    /// Parses each assertion and renders its expected value once, before
    /// the first attempt.
    fn prepare_asserts(
        &self,
        req: &Arc<Request>,
        asserts: &[Assert],
    ) -> std::result::Result<Vec<PreparedAssert>, Error> {
        let mut prepared = Vec::with_capacity(asserts.len());
        for a in asserts {
            let at = format!("{}:{}", req.file.path, a.line);
            let expr = apic_assert::parse(&a.expr)
                .map_err(|e| uerr(Code::Directive, format!("{at}: {e}")))?;
            let mut expected = expr.value.clone();
            if !expr.unary() {
                expected = apic_template::render(&expr.value, |e| self.resolve_expr(Some(req), e))
                    .map_err(|err| {
                        let err = render_error(err);
                        match err.missing() {
                            Some(me) => self.missing_error(req, &dedupe(me.exprs.clone())),
                            None => uctx(
                                Code::Directive,
                                format!("{at}: assert {}", quote(&a.expr)),
                                &err,
                            ),
                        }
                    })?;
            }
            prepared.push(PreparedAssert { expr, expected });
        }
        Ok(prepared)
    }

    /// Reads the request's @timeout and @retry, and waits out its @sleep.
    fn pacing(
        &self,
        cancel: &CancelToken,
        req: &Request,
    ) -> std::result::Result<(Duration, RetryPolicy), Error> {
        let at = format!("{}:{}", req.file.path, req.line);
        let mut timeout = self.opts.timeout;
        if let Some(t) = req.directive("timeout") {
            timeout = parse_duration(t)
                .map_err(|_| uerr(Code::Directive, format!("{at}: bad @timeout {}", quote(t))))?;
        }
        let policy = self.retry_policy(req)?;
        let pause = req
            .sleep()
            .map_err(|e| uerr(Code::Directive, format!("{at}: @sleep: {e}")))?;
        if !pause.is_zero() {
            (self.sleep)(cancel, pause).map_err(TransportError::from)?;
        }
        Ok((timeout, policy))
    }

    /// Makes attempts until one passes or the policy runs out, and folds
    /// the attempt that counts into result.
    fn retry(
        &self,
        cancel: &CancelToken,
        req: &Arc<Request>,
        policy: RetryPolicy,
        result: &mut Result,
        attempt: impl Fn() -> std::result::Result<Result, Error>,
    ) -> std::result::Result<(), Error> {
        let mut n = 1;
        loop {
            let progress = |failure: String| Progress {
                req: req.clone(),
                attempt: n,
                max: policy.n,
                failure,
            };
            match attempt() {
                Err(err) => {
                    if err.usage().is_some() || n >= policy.n {
                        return Err(err);
                    }
                    self.report(progress(err.to_string()));
                }
                Ok(mut res) => {
                    if policy.n > 1 {
                        res.attempts = n;
                    }
                    if res.ok || n >= policy.n {
                        result.response = res.response;
                        result.raw = res.raw;
                        result.captures = res.captures;
                        result.asserts = res.asserts;
                        result.attempts = res.attempts;
                        result.auth_note = res.auth_note;
                        result.errors.extend(res.errors);
                        result.ok = result.ok && res.ok;
                        return Ok(());
                    }
                    self.report(progress(res.problem()));
                }
            }
            (self.sleep)(cancel, policy.interval).map_err(TransportError::from)?;
            n += 1;
        }
    }

    /// Stores what the attempt that counts produced: its body, its
    /// captures (for this run and later ones), the cookie jar and, for a
    /// dependency, its history. `top` is the request `run` was called
    /// for.
    fn keep(&self, req: &Arc<Request>, result: &mut Result, top: bool) {
        // --output replaces the request's own `>>` line for the request
        // it was given (the top of the chain), as `>>!` would.
        let replaced = !self.opts.output.is_empty() && top;
        if let (Some(save), true, false) = (&req.save_to, result.raw.is_some(), replaced) {
            if let Err(e) = self.save_body(req, &save.path, save.overwrite, false, result) {
                result.fail(e);
            }
        }
        lock(&self.captured).extend(result.captures.clone());
        let off = req.directive("no-session").is_some();
        if let (false, Some(session), false) = (result.captures.is_empty(), &self.session, off) {
            let saved = {
                let mut store = lock(session);
                store.set(&self.opts.env, result.captures.clone());
                store.save()
            };
            if let Err(e) = saved {
                result.fail(format!("session: {e}"));
            }
        }
        if let Some(jar) = &self.jar {
            if let Err(e) = jar.save() {
                result.fail(format!("cookies: {e}"));
            }
        }
        if !top {
            // A dependency is final here; the request `run` was called
            // for is recorded by `run`.
            self.record(req, result);
        }
        if !req.name.is_empty() {
            lock(&self.results).insert(req.name.clone(), Arc::new(result.clone()));
        }
    }

    /// Adds a named request's response to its history, in the form `apic
    /// run --json` prints: sensitive headers masked, and every value
    /// under --redact. The requests it ran first have histories of their
    /// own. History is a convenience: a write that fails (a read-only
    /// checkout, a full disk) is a warning on the result, never a failed
    /// request.
    fn record(&self, req: &Request, result: &mut Result) {
        if result.response.is_none() {
            return;
        }
        let why = if req.name.is_empty() {
            "the request has no # @name, and the history keeps named requests only"
        } else if self.history.is_some() {
            ""
        } else if self.opts.no_history {
            "a data run keeps no history"
        } else if self.opts.no_session {
            "--no-session keeps no history"
        } else {
            "set history: N in apic.yaml before the run for apic select to read it"
        };
        let Some(history) = self.history.as_ref().filter(|_| why.is_empty()) else {
            result.unrecorded = why.to_string();
            return;
        };
        let mut entry = result.clone();
        entry.deps = Vec::new();
        entry.iteration = None;
        entry.warnings = Vec::new();
        let key = history_key(&self.project, req);
        match history.record(&self.opts.env, &key, self.current_time(), &entry.to_json()) {
            Ok(()) => {
                result.recorded = true;
                result.record_key = key;
            }
            Err(e) => {
                result.warnings.push(format!("history: {e}"));
                result.unrecorded = "the history could not be written".to_string();
            }
        }
    }

    /// Sends a resolved request once and evaluates its captures and
    /// assertions into a fresh result, without committing anything to
    /// the runner or the session. It has its own timeout, so a retry
    /// loop gives every attempt the full time.
    #[allow(clippy::too_many_arguments)]
    fn attempt(
        &self,
        cancel: &CancelToken,
        req: &Arc<Request>,
        resolved: &Resolved,
        asserts: &[PreparedAssert],
        captures: &[Capture],
        schemas: apic_assert::Options<'_>,
        timeout: Duration,
    ) -> std::result::Result<Result, Error> {
        let at = format!("{}:{}", req.file.path, req.line);
        let deadline = Instant::now() + timeout;
        let mut http_req = self.build_request(cancel, req, resolved, deadline)?;
        let url = UrlParts::parse(&resolved.url).unwrap_or_default();
        let want = want_for(req).map_err(|e| uerr(Code::Directive, format!("{at}: {e}")))?;
        if want == ProtoWant::Http2 && url.scheme != "https" {
            return Err(uerr(Code::Directive, format!("{at}: HTTP/2 needs an https:// URL; apic does not send cleartext HTTP/2 (h2c). Write HTTP/1.1 or leave the version out")));
        }
        let client = self.client_for(&url.host, want)?;
        http_req.proxy = self
            .proxy_config()
            .dial(&http_req.url)
            .map_err(|e| uerr(Code::Proxy, format!("{at}: {e}")))?;
        http_req.deadline = Some(deadline);
        http_req.cancel = Some(cancel.clone());
        let max = self.max_body_bytes();
        http_req.max_body = Some(max as u64 + 1);

        let no_redirect = req.directive("no-redirect").is_some();
        // Drops apic's own credential headers when a redirect leaves the
        // host that was originally addressed. The client already does
        // this for Authorization, Cookie and Proxy-Authorization, but its
        // list is fixed and ours is not: the SigV4 signer sets
        // X-Amz-Security-Token, "# @auth exec header=..." sets whatever
        // the project asks for, and a file can write X-Api-Key:
        // {{secret}} by hand. Those would otherwise follow a redirect to
        // any host that answers.
        let check = move |next: &mut apic_httpc::Request,
                          via: &[apic_httpc::Request]|
              -> std::result::Result<RedirectAction, String> {
            if no_redirect {
                return Ok(RedirectAction::Stop);
            }
            if via.len() >= 10 {
                return Err("stopped after 10 redirects".to_string());
            }
            let Some(first) = via.first() else {
                return Ok(RedirectAction::Follow);
            };
            if host_of(&next.url).eq_ignore_ascii_case(&host_of(&first.url)) {
                return Ok(RedirectAction::Follow);
            }
            let names: Vec<String> = next.headers.names().iter().map(|n| n.to_string()).collect();
            for name in names {
                if is_sensitive_header(&name) {
                    next.headers.del(&name);
                }
            }
            Ok(RedirectAction::Follow)
        };
        let jar = match &self.jar {
            Some(jar) if req.directive("no-cookies").is_none() => {
                Some(JarAdapter(jar.http(&self.opts.env)))
            }
            _ => None,
        };
        let policy = RedirectPolicy {
            check: Some(&check),
            jar: jar.as_ref().map(|j| j as &dyn CookieJar),
        };
        let digest = match &resolved.auth_spec {
            Some(s) if s.r#type == "digest" => Some(
                DigestTransport::new(
                    Box::new(client.clone()),
                    s.args.first().map_or("", String::as_str),
                    s.args.get(1).map_or("", String::as_str),
                )
                .with_host(&digest_host(&http_req.url))
                .with_state(self.digest.clone()),
            ),
            _ => None,
        };
        let transport: &dyn RoundTrip = match &digest {
            Some(d) => d,
            None => &client,
        };
        let resp = follow_redirects(transport, &http_req, &policy)
            .map_err(|e| self.send_error(&e, want))?;
        if want == ProtoWant::Http2 && !resp.proto.starts_with("HTTP/2") {
            return Err(TransportError::with_code(
                Code::Protocol,
                format!(
                    "the request line asks for HTTP/2, but {} answered with {}",
                    url.host, resp.proto
                ),
            )
            .into());
        }
        // Bounded: an unbounded read lets one hostile or oversized
        // response take the process down, and the body is held more than
        // once while it is parsed and rendered.
        if resp.body.len() as u64 > max as u64 {
            return Err(TransportError::with_code(Code::Protocol, body_too_large(max)).into());
        }
        let dur = resp.timings.total;
        let status_text = resp.status_text().to_string();
        let mut headers = std::collections::BTreeMap::<String, String>::new();
        for name in resp.headers.names() {
            headers.insert(name.to_lowercase(), resp.headers.values(name).join(", "));
        }
        let (view, encoding) = json_or_string(&resp.body);
        let size = resp.body.len();
        let raw = apic_selector::Response {
            status: i64::from(resp.status_code),
            status_text: status_text.clone(),
            headers: resp.headers.iter().collect(),
            body: resp.body,
            duration: dur,
        };
        let mut result = Result {
            ok: true,
            redact: self.opts.redact,
            raw: Some(Arc::new(raw)),
            ..Result::for_request(req)
        };
        if let Some(d) = &digest {
            result.auth_note = digest_note(d);
        }
        result.response = Some(Response {
            status: i64::from(resp.status_code),
            status_text,
            headers,
            body: view,
            body_encoding: encoding,
            body_truncated: false,
            duration_ms: dur.as_millis() as i64,
            size,
            proto: resp.proto,
            timings: Some(Timings::from_http(&resp.timings)),
        });
        result.evaluate(captures, asserts, schemas);
        Ok(result)
    }

    /// Builds the outgoing request from the resolved one, with its
    /// headers and its auth applied.
    fn build_request(
        &self,
        cancel: &CancelToken,
        req: &Request,
        resolved: &Resolved,
        deadline: Instant,
    ) -> std::result::Result<apic_httpc::Request, Error> {
        let at = format!("{}:{}", req.file.path, req.line);
        let body = resolved.body_bytes();
        let mut http_req = apic_httpc::Request::new(&resolved.method, &resolved.url);
        http_req.body = body.clone();
        // A method or a URL that cannot be read is the request's own
        // fault; a URL that reads but cannot be sent fails when sending.
        if let Err(e) = UrlParts::parse(&resolved.url) {
            return Err(uerr(
                Code::Build,
                format!("{at}: parse {}: {e}", quote(&resolved.url)),
            ));
        }
        if let Err(e) = http_req.check() {
            if e.starts_with("net/http: invalid method") || e.starts_with("parse ") {
                return Err(uerr(Code::Build, format!("{at}: {e}")));
            }
        }
        http_req
            .headers
            .set("User-Agent", &format!("apic/{}", crate::version()));
        for h in &resolved.headers {
            if h.name.eq_ignore_ascii_case("Host") {
                http_req.host = Some(h.value.clone());
                continue;
            }
            http_req.headers.add(&h.name, &h.value);
        }
        let Some(spec) = &resolved.auth_spec else {
            return Ok(http_req);
        };
        let env = self.auth_env(Some(deadline))?;
        apic_auth::apply(cancel, spec, &mut http_req, &body, &env)
            .map_err(|e| uerr(Code::Auth, format!("{at}: auth: {e}")))?;
        Ok(http_req)
    }

    /// The error for a request that got no response.
    fn send_error(&self, err: &apic_httpc::Error, want: ProtoWant) -> Error {
        let mut te = self.transport_error(err);
        if want == ProtoWant::Http2 {
            if te.error_code() == Code::Transport {
                // Refused ALPN reads as a plain failure; say what it was.
                te.code = Some(Code::Protocol);
            }
            let message = format!(
                "{} (the request line asks for HTTP/2; the server may not offer it)",
                te.message
            );
            te = te.with_message(message);
        }
        te.into()
    }

    /// Wraps a network failure. The client's errors quote the full URL,
    /// query values included, so under redact the URL is masked the way
    /// the rest of the output masks it.
    fn transport_error(&self, err: &apic_httpc::Error) -> TransportError {
        let te = TransportError::from_http(err);
        if self.opts.redact && !err.reading_body {
            return te.with_message(format!("{} {}: {}", err.op, crate::MASKED, err.message));
        }
        te
    }

    /// Picks the policy for a request: `# @retry` on the request, else
    /// the options' retry (--retry), else retry in apic.yaml, else one
    /// attempt; `no_retry` makes it one attempt regardless.
    fn retry_policy(&self, req: &Request) -> std::result::Result<RetryPolicy, Error> {
        let policy = |(n, interval)| RetryPolicy { n, interval };
        if self.opts.no_retry {
            return Ok(policy((1, Duration::ZERO)));
        }
        if let Some(v) = req.directive("retry") {
            return apic_httpfile::parse_retry(v).map(policy).map_err(|e| {
                uerr(
                    Code::Directive,
                    format!(
                        "{}:{}: bad @retry {}: {e}",
                        req.file.path,
                        req.line,
                        quote(v)
                    ),
                )
            });
        }
        if !self.opts.retry.is_empty() {
            return apic_httpfile::parse_retry(&self.opts.retry)
                .map(policy)
                .map_err(|e| {
                    uerr(
                        Code::Flag,
                        format!("--retry {}: {e}", quote(&self.opts.retry)),
                    )
                });
        }
        let v = &self.project.config.retry;
        if !v.is_empty() {
            return apic_httpfile::parse_retry(v).map(policy).map_err(|e| {
                uerr(
                    Code::Project,
                    format!("apic.yaml: bad retry {}: {e}", quote(v)),
                )
            });
        }
        Ok(policy((1, Duration::ZERO)))
    }

    /// Resolves a command-line target as the project does. A target that
    /// names one request (get-user, users.http#get-user, users.http#3)
    /// is remembered, so `run_all` sends it even when it is `#
    /// @disabled`: that directive only keeps a request out of a file's
    /// flow.
    pub fn target(&self, target: &str) -> std::result::Result<Vec<Arc<Request>>, Error> {
        let reqs = self.project.resolve(target)?;
        if self.project.file(target).is_none() {
            lock(&self.named).extend(reqs.iter().map(RequestKey::new));
        }
        Ok(reqs)
    }

    /// Runs requests in order as a flow, stopping at the first failure
    /// unless `keep_going` is set. Results for requests that ran are
    /// always returned, with the first error that stopped one. A `#
    /// @disabled` request is skipped, with a result that says so, unless
    /// `target` resolved it by name.
    pub fn run_all(
        &self,
        cancel: &CancelToken,
        reqs: &[Arc<Request>],
    ) -> (Vec<Result>, Option<Error>) {
        let mut out = Vec::new();
        let mut first_err = None;
        let notify = |res: &Result, err: Option<&Error>| {
            if let Some(on_result) = &self.on_result {
                on_result(res, err);
            }
        };
        for req in reqs {
            if req.disabled() && !lock(&self.named).contains(&RequestKey::new(req)) {
                let mut res = unsent(req);
                res.ok = true;
                res.skipped = "disabled".to_string();
                res.redact = self.opts.redact;
                notify(&res, None);
                out.push(res);
                continue;
            }
            let (res, err) = self.run(cancel, req);
            let res = res.unwrap_or_else(|| {
                let mut res = unsent(req);
                if let Some(err) = &err {
                    res.errors = vec![err.to_string()];
                    res.error = Some(info(err));
                }
                res
            });
            notify(&res, err.as_ref());
            let ok = res.ok;
            out.push(res);
            if let Some(err) = err {
                // keep_going means past a failure, not past Ctrl-C: once
                // the run is cancelled every remaining request would fail
                // the same way, so stop here with what completed.
                if !self.opts.keep_going || cancel.is_cancelled() {
                    return (out, Some(err));
                }
                first_err.get_or_insert(err);
                continue;
            }
            if !ok && !self.opts.keep_going {
                return (out, None);
            }
        }
        (out, first_err)
    }
}
