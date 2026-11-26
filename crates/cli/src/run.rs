//! apic run: send one request, or every request in a file as a flow.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use apic_datafile::Row;
use apic_gocompat::{quote, Time};
use apic_httpfile::{Assert, Capture, Request};
use apic_runner::{Code, Iteration, Runner};

use crate::io::{wln, Output};
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::{theme, unknown_target};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};
use crate::test::{describe_outcome, output_overlaps_sources};

/// apic run's flags.
#[derive(Clone, Default)]
struct RunFlags {
    verbose: bool,
    body_only: bool,
    keep_going: bool,
    no_retry: bool,
    dry_run: bool,
    share_session: bool,
    asserts: Vec<String>,
    captures: Vec<String>,
    retry: String,
    report: String,
    output: String,
    data: String,
    body_limit: String,
}

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "run <request|file.http|file.http#name>...",
        "Send one request, or every request in a file as a flow",
    )
    .long(
        r#"Send requests and report status, timing, body, captures and assertions.

Targets:
  get-user             a request with "# @name get-user"
  users.http           every request in the file, in order (a flow)
  users.http#get-user  a named request in a specific file
  users.http#3         the third request in the file

Captured values (# @capture) are stored per environment in .apic/session.json
so a later invocation can use them. Use --no-session to disable. A request
that declares "# @ref login" runs login first when a value it needs is
missing; "# @forceRef login" runs it first every time. One that declares
"# @retry 10 2s" is re-sent until its assertions pass, up to 10 times, two
seconds apart; each failed attempt prints a line as it happens.

--assert and --capture add a check or a capture to every target for this
run only, the same expressions "# @assert" and "# @capture" take, so a
selector can be tried before it is written into the file. --dry-run
resolves each target as a run would and prints the request instead of
sending it: no dependency runs, no auth is applied and nothing is captured."#,
    )
    .example(
        r#"  apic run login
  apic run get-user --env staging --var userId=42
  apic run smoke.http --json | jq .response.status
  apic run get-user --body-only | jq .email
  apic run smoke.http --keep-going --report report.html
  apic run daily-report --output reports/daily.csv
  apic run list-todos --assert 'body.$[?(@.done != true)].# == 0'
  apic run create-todo --capture id=body.$.id --json
  apic run delete-todo --var todoId=3 --dry-run"#,
    )
    .args(ArgRule::Min(1))
    .flag(FlagSpec::bool("verbose", "show request and response headers").short('v'))
    .flag(FlagSpec::bool("body-only", "print only the response body (for piping)"))
    .flag(FlagSpec::bool("keep-going", "in a flow, continue after a failure"))
    .flag(FlagSpec::string("retry", "", "re-send until the assertions pass: \"<attempts> [interval]\", e.g. \"10 2s\" (requests with # @retry keep their own)"))
    .flag(FlagSpec::bool("no-retry", "send every request once, ignoring # @retry, --retry and apic.yaml"))
    .flag(FlagSpec::string("report", "", "also write a self-contained HTML report of the run to this file"))
    .flag(FlagSpec::string("output", "", "save the response body to this file (one request only; like a \">>! file\" line in the request)"))
    .flag(FlagSpec::string("data", "", "run the targets once per row of a CSV file (header row names the variables) or JSON array of objects; - reads stdin"))
    .flag(FlagSpec::bool("data-share-session", "with --data, let captures from one iteration reach the next and the session file"))
    .flag(FlagSpec::string_array("assert", "check the response with an expression, as # @assert would, for this run only (repeatable)"))
    .flag(FlagSpec::string_array("capture", "capture a value, name=selector, as # @capture would, for this run only (repeatable)"))
    .flag(FlagSpec::bool("dry-run", "resolve the targets and print the requests that would be sent, without sending them"))
    .flag(FlagSpec::string("body-limit", "", "show at most this much of each response body, e.g. 4k or 64k; the result says body_truncated and the history keeps it all"))
    .runs(run)
}

/// apic run: checks the flags, runs the targets (once, or once per --data
/// row), prints each result as it lands and writes the report.
fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let f = RunFlags {
        verbose: inv.flags.bool("verbose"),
        body_only: inv.flags.bool("body-only"),
        keep_going: inv.flags.bool("keep-going"),
        no_retry: inv.flags.bool("no-retry"),
        dry_run: inv.flags.bool("dry-run"),
        share_session: inv.flags.bool("data-share-session"),
        asserts: inv.flags.strings("assert"),
        captures: inv.flags.strings("capture"),
        retry: inv.flags.string("retry"),
        report: inv.flags.string("report"),
        output: inv.flags.string("output"),
        data: inv.flags.string("data"),
        body_limit: inv.flags.string("body-limit"),
    };
    let (extra_asserts, extra_captures) = ad_hoc(&f.asserts, &f.captures)?;
    let limit = parse_size(&f.body_limit)
        .map_err(|e| fail(Code::Flag, format!("--body-limit {}: {e}", quote(&f.body_limit))))?;
    let p = a.load_project()?;
    // A data run records no history: a response per row would push out
    // what the history is for.
    let data = !f.data.is_empty();
    let mut r = a.runner_for(p, |o| o.no_history = data)?;
    f.check_dry_run()?;
    let mut j = RunJob {
        stdout: a.stdout.clone(),
        json: a.g.json,
        redact: a.g.redact,
        f: f.clone(),
        limit,
        asserts: extra_asserts,
        captures: extra_captures,
        rows: Vec::new(),
        flow: false,
        state: Arc::new(Mutex::new(JobState::default())),
    };
    if data {
        j.rows = a.read_rows(&f.data)?;
    }
    j.configure(&mut r);
    let reqs = targets(&r, &inv.args)?;
    j.flow = reqs.len() > 1;
    if f.dry_run {
        return a.dry_run(&r, &reqs, f.verbose, f.body_only);
    }
    j.check_outputs(&mut r, reqs.len())?;
    let started = Time::now();
    let (results, run_err) = if j.rows.is_empty() {
        r.on_result = Some(j.on_result());
        let (mut results, run_err) = r.run_all(&inv.cancel, &reqs);
        j.mark(&mut results, None);
        if j.flow && j.human() {
            apic_output::summary(&mut a.stdout.clone(), &results);
        }
        (results, run_err.map(CliError::from))
    } else {
        j.run_rows(a, inv, &mut r)
    };
    if let Some(e) = j.lock().print_err.take() {
        return Err(CliError::other(e));
    }
    if !f.report.is_empty() {
        j.write_report(&r, started, &results, run_err.as_ref())?;
    }
    if let Some(err) = run_err {
        return Err(err);
    }
    if results.iter().any(|res| !res.ok) {
        return Err(CliError::exit(apic_runner::EXIT_ASSERT));
    }
    Ok(())
}

impl RunFlags {
    /// Refuses the flags --dry-run has nothing to do with.
    fn check_dry_run(&self) -> CliResult {
        if !self.dry_run {
            return Ok(());
        }
        if !self.data.is_empty() {
            return Err(fail(
                Code::Flag,
                "--dry-run shows one request per target; it does not take --data",
            ));
        }
        if !self.output.is_empty() || !self.report.is_empty() {
            return Err(fail(
                Code::Flag,
                "--dry-run sends nothing, so there is no response to save or report",
            ));
        }
        Ok(())
    }
}

/// What printing results as they land keeps track of.
#[derive(Default)]
struct JobState {
    print_err: Option<std::io::Error>,
    printed: usize,
    /// The row being run under --data.
    iteration: Option<Iteration>,
}

/// One apic run: its flags, its --data rows, and the state of printing
/// results as they land.
#[derive(Clone)]
struct RunJob {
    stdout: Output,
    json: bool,
    redact: bool,
    f: RunFlags,
    limit: usize,
    asserts: Vec<Assert>,
    captures: Vec<Capture>,
    rows: Vec<Row>,
    flow: bool,
    state: Arc<Mutex<JobState>>,
}

impl RunJob {
    fn lock(&self) -> std::sync::MutexGuard<'_, JobState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reports whether results are printed for a person to read.
    fn human(&self) -> bool {
        !self.json && !self.f.body_only
    }

    /// Applies the run's flags to a runner: the first one, and each
    /// iteration's own under --data.
    fn configure(&self, r: &mut Runner) {
        r.opts.keep_going = self.f.keep_going;
        r.opts.retry = self.f.retry.clone();
        r.opts.no_retry = self.f.no_retry;
        r.opts.asserts = self.asserts.clone();
        r.opts.captures = self.captures.clone();
        if self.human() {
            let out = self.stdout.clone();
            r.progress = Some(Box::new(move |p| {
                out.print(&apic_output::attempt(&theme(), p));
            }));
        }
    }

    /// Checks that --report and --output stay out of the project's
    /// sources, and that --output names one response.
    fn check_outputs(&self, r: &mut Runner, targets: usize) -> CliResult {
        if !self.f.report.is_empty() {
            output_overlaps_sources(&self.f.report, &r.project, &[])?;
        }
        if self.f.output.is_empty() {
            return Ok(());
        }
        if self.flow {
            return Err(fail(Code::Flag, format!("--output saves one response; the targets name {targets} requests (add `>> file` lines to the requests instead)")));
        }
        if !self.rows.is_empty() {
            return Err(fail(Code::Flag, format!("--output saves one response; --data runs the request {} times (add a `>> file` line with a {{{{placeholder}}}} in the path instead)", self.rows.len())));
        }
        output_overlaps_sources(&self.f.output, &r.project, &[])?;
        r.opts.output = self.f.output.clone();
        Ok(())
    }

    /// Marks results the way they are shown: the iteration they belong
    /// to and the limit on their bodies.
    fn mark(&self, results: &mut [apic_runner::Result], iteration: Option<&Iteration>) {
        for res in results {
            if let Some(it) = iteration {
                tag_iteration(res, it);
            }
            if self.limit > 0 {
                limit_bodies(res, self.limit);
            }
        }
    }

    /// Prints each result as it lands, so a flow shows progress (and the
    /// attempt lines of a retried request sit under the right request)
    /// instead of everything at the end.
    fn on_result(&self) -> apic_runner::OnResultFn {
        let j = self.clone();
        Box::new(move |res, err| {
            let mut res = res.clone();
            let iteration = j.lock().iteration.clone();
            j.mark(std::slice::from_mut(&mut res), iteration.as_ref());
            let mut out = j.stdout.clone();
            let printed = j.lock().printed;
            if j.json {
                if let Err(e) = apic_output::json(&mut out, &res) {
                    let mut state = j.lock();
                    if state.print_err.is_none() {
                        state.print_err = Some(e);
                    }
                }
            } else if j.f.body_only {
                apic_output::body(&mut out, &res);
            } else {
                if j.flow && printed > 0 {
                    wln!(out);
                }
                if res.response.is_none() && err.is_some() {
                    // The error is printed once the command ends; show
                    // what ran first and the request line for context.
                    apic_output::deps(&mut out, &res, j.f.verbose);
                    wln!(out, "{} {}", res.request.method, res.request.display_url(res.redact));
                } else {
                    apic_output::human(&mut out, &res, j.f.verbose);
                }
            }
            j.lock().printed += 1;
        })
    }

    /// Runs the targets once per --data row, with a heading for each
    /// iteration and a tally at the end.
    fn run_rows(
        &self,
        a: &App,
        inv: &Invocation,
        r: &mut Runner,
    ) -> (Vec<apic_runner::Result>, Option<CliError>) {
        let (mut passed, mut failed) = (0, 0);
        let base = r.opts.vars.clone();
        let session = r.session.clone();
        let mut all = Vec::new();
        let mut first_err: Option<apic_runner::Error> = None;
        let mut stopped: Option<CliError> = None;
        for (i, row) in self.rows.iter().enumerate() {
            let mut vars: BTreeMap<String, String> = base.clone();
            vars.extend(row.values.iter().map(|(k, v)| (k.clone(), v.clone())));
            let mut own;
            let ir: &mut Runner = if self.f.share_session {
                r.opts.vars = vars;
                &mut *r
            } else {
                let made = a.runner_for(r.project.clone(), |o| {
                    o.vars = vars;
                    o.no_history = true;
                    if let Some(s) = &session {
                        let snapshot = s.lock().unwrap_or_else(PoisonError::into_inner).snapshot();
                        o.session = Some(apic_runner::shared_session(snapshot));
                    }
                });
                match made {
                    Ok(made) => {
                        own = made;
                        &mut own
                    }
                    Err(e) => {
                        stopped = Some(e);
                        break;
                    }
                }
            };
            let reqs = match targets(ir, &inv.args) {
                Ok(reqs) => reqs,
                Err(e) => {
                    stopped = Some(e);
                    break;
                }
            };
            let it = Iteration { index: i + 1, total: self.rows.len(), row: row.values.clone() };
            {
                let mut state = self.lock();
                state.iteration = Some(it.clone());
                state.printed = 0;
            }
            self.configure(ir);
            ir.on_result = Some(self.on_result());
            if self.human() {
                self.iteration_heading(&it);
            }
            let (mut results, run_err) = ir.run_all(&inv.cancel, &reqs);
            self.mark(&mut results, Some(&it));
            if self.flow && self.human() {
                apic_output::summary(&mut self.stdout.clone(), &results);
            }
            let ok = run_err.is_none() && results.iter().all(|res| res.ok);
            if all_ok(&results) {
                passed += 1;
            } else {
                failed += 1;
            }
            all.extend(results);
            if first_err.is_none() {
                first_err = run_err;
            }
            // --keep-going carries on past a failed row, not past Ctrl-C.
            if (!ok && !self.f.keep_going) || inv.cancel.is_cancelled() {
                break;
            }
        }
        if self.human() {
            let t = theme();
            let mut line = t.ok.render(&format!("{passed} passed"));
            if failed > 0 {
                line = t.fail.render(&format!("{failed} failed")) + &format!(", {passed} passed");
            }
            wln!(
                self.stdout,
                "\n{} {line}",
                t.bold.render(&format!("{} of {} iterations:", passed + failed, self.rows.len()))
            );
        }
        (all, stopped.or(first_err.map(CliError::from)))
    }

    /// Prints the heading above one --data iteration, with its row unless
    /// the run is redacted.
    fn iteration_heading(&self, it: &Iteration) {
        if it.index > 1 {
            wln!(self.stdout);
        }
        let t = theme();
        let mut heading = format!("iteration {}/{}", it.index, it.total);
        let row = self.rows[it.index - 1].to_string();
        if !row.is_empty() && !self.redact {
            heading.push(' ');
            heading.push_str(&t.dim.render(&format!("· {row}")));
        }
        wln!(self.stdout, "{}", t.bold.render(&heading));
    }

    /// Writes the --report HTML for the run.
    fn write_report(
        &self,
        r: &Runner,
        started: Time,
        results: &[apic_runner::Result],
        run_err: Option<&CliError>,
    ) -> CliResult {
        let mut buf = Vec::new();
        let meta = apic_report::Meta {
            version: crate::version().to_string(),
            env: r.opts.env.clone(),
            time: Some(started),
            redacted: self.redact,
            project: r.project.root.clone(),
            ..apic_report::Meta::default()
        };
        apic_report::run(&mut buf, &meta, results)
            .map_err(|e| fail(Code::File, format!("write report {}: {e}", self.f.report)))?;
        crate::shared::write_file(&self.f.report, &buf, 0o644).map_err(|e| {
            let outcome = describe_outcome(run_err);
            fail(
                Code::File,
                format!("write report {}: {e} (run result: {outcome})", self.f.report),
            )
        })
    }
}

/// Reports whether an iteration ran and every result passed.
fn all_ok(results: &[apic_runner::Result]) -> bool {
    !results.is_empty() && results.iter().all(|res| res.ok)
}

/// Checks the --assert expressions and --capture definitions once, with
/// the checks `apic validate` gives the directives (the expression
/// parses, the selector is one apic knows, the name is one a directive
/// could have), so a typo is a flag error naming the flag before anything
/// is sent, not a failure after a request that may have changed something.
fn ad_hoc(asserts: &[String], captures: &[String]) -> Result<(Vec<Assert>, Vec<Capture>), CliError> {
    let mut extra_asserts = Vec::new();
    for raw in asserts {
        let checked = apic_assert::parse(raw)
            .and_then(|expr| apic_selector::check(&expr.selector).map_err(|e| e.to_string()));
        if let Err(e) = checked {
            return Err(fail(Code::Flag, format!("--assert {}: {e}", quote(raw))));
        }
        extra_asserts.push(Assert { expr: raw.clone(), ..Assert::default() });
    }
    let mut extra_captures = Vec::new();
    for def in captures {
        let (name, sel, ok) = match def.split_once('=') {
            Some((name, sel)) => (name.trim(), sel.trim(), true),
            None => (def.trim(), "", false),
        };
        if !ok || !apic_httpfile::valid_capture_name(name) || sel.is_empty() {
            return Err(fail(
                Code::Flag,
                format!(
                    "--capture {}: expected name=selector, e.g. token=body.$.access_token",
                    quote(def)
                ),
            ));
        }
        if let Err(e) = apic_selector::check(sel) {
            return Err(fail(Code::Flag, format!("--capture {}: {e}", quote(def))));
        }
        extra_captures.push(Capture {
            name: name.to_string(),
            selector: sel.to_string(),
            ..Capture::default()
        });
    }
    Ok((extra_asserts, extra_captures))
}

impl App {
    /// Prints each target as it would be sent and sends nothing. Under
    /// --body-only it prints the request body, there being no response.
    fn dry_run(&self, r: &Runner, reqs: &[Arc<Request>], verbose: bool, body_only: bool) -> CliResult {
        let mut out = self.stdout.clone();
        for (i, req) in reqs.iter().enumerate() {
            let res = r.dry_run(req)?;
            if self.g.json {
                apic_output::json(&mut out, &res).map_err(CliError::other)?;
            } else if body_only {
                let body = res.request.display_body(res.redact);
                if !body.is_empty() {
                    wln!(out, "{}", body.trim_end_matches('\n'));
                }
            } else {
                if i > 0 {
                    wln!(out);
                }
                apic_output::human(&mut out, &res, verbose);
            }
        }
        Ok(())
    }

    /// Reads the rows of --data from a file, or stdin for -.
    fn read_rows(&mut self, path: &str) -> Result<Vec<Row>, CliError> {
        let data = if path == "-" {
            self.stdin
                .read_all()
                .map_err(|e| apic_gocompat::io_error_text(&e))
        } else {
            crate::shared::read_file(path)
        };
        let data = data.map_err(|e| fail(Code::File, format!("--data: {e}")))?;
        let rows = apic_datafile::read(&data, path)
            .map_err(|e| fail(Code::Data, format!("--data {path}: {e}")))?;
        if rows.is_empty() {
            return Err(fail(Code::Data, format!("--data {path} has no rows to run")));
        }
        Ok(rows)
    }
}

/// Resolves the command line's targets in order.
fn targets(r: &Runner, args: &[String]) -> Result<Vec<Arc<Request>>, CliError> {
    let mut reqs = Vec::new();
    for t in args {
        reqs.extend(r.target(t).map_err(unknown_target)?);
    }
    Ok(reqs)
}

/// Bounds the body shown for a result and for what it ran first; the
/// history, written before this, keeps the whole body.
fn limit_bodies(res: &mut apic_runner::Result, limit: usize) {
    res.body_limit = limit;
    for d in &mut res.deps {
        limit_bodies(d, limit);
    }
}

/// Reads a byte count with an optional k or m suffix (1024 based, as `4k`
/// on a command line means); empty means none.
pub(crate) fn parse_size(s: &str) -> Result<usize, String> {
    let lower = s.trim().to_lowercase();
    let mut s = lower.as_str();
    if s.is_empty() {
        return Ok(0);
    }
    let mut mult: i64 = 1;
    if s.ends_with("kb") || s.ends_with('k') {
        mult = 1024;
        s = s.strip_suffix('b').unwrap_or(s);
        s = s.strip_suffix('k').unwrap_or(s);
    } else if s.ends_with("mb") || s.ends_with('m') {
        mult = 1024 * 1024;
        s = s.strip_suffix('b').unwrap_or(s);
        s = s.strip_suffix('m').unwrap_or(s);
    }
    let bad = || "expected a positive size such as 4k, 64k or 1m".to_string();
    let n: i64 = s.parse().map_err(|_| bad())?;
    if n <= 0 || n > i64::MAX / mult {
        return Err(bad());
    }
    usize::try_from(n * mult).map_err(|_| bad())
}

/// Marks a result, and the results of what it ran first, as belonging to
/// an iteration.
fn tag_iteration(res: &mut apic_runner::Result, it: &Iteration) {
    res.iteration = Some(it.clone());
    for d in &mut res.deps {
        tag_iteration(d, it);
    }
}
