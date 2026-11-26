//! apic test: run Gherkin feature files against the project's requests.

use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use apic_gocompat::Time;
use apic_httpc::CancelToken;
use apic_project::{path as fp, Project};
use apic_runner::Code;
use serde::Serialize;

use crate::io::{w, wln};
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::theme;
use crate::spec::{CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "test [path|file.feature]...",
        "Run Gherkin feature files against the project's requests",
    )
    .long(
        r#"Run .feature files with a built-in step vocabulary, no Cucumber runtime
needed. Requests are referenced by id ("When I run \"get-user\"") or through
phrases declared on them with "# @step" in the .http file.

Each scenario starts with a fresh, in-memory session so tests never touch
.apic/session.json (use --use-session to change that). Undefined steps fail
the run.

Exit codes: 0 all scenarios passed · 1 assertion failures or undefined
steps · 2 definition problems (no features, feature path outside the
project, unknown environment, unknown request, missing variable, bad
phrase) · 3 a server could not be reached."#,
    )
    .example(
        "  apic test
  apic test features/users.feature --env staging --tags @smoke
  apic test --format junit --output report.xml
  apic test --format html --output report.html
  apic test --json | jq '.[].elements[].steps[].result.status'
  apic test --steps",
    )
    .flag(
        FlagSpec::string(
            "format",
            "pretty",
            &format!("report format: {}, html", apic_bdd::FORMATS.join(", ")),
        )
        .short('f'),
    )
    .flag(FlagSpec::string("output", "", "write the report to a file instead of stdout").short('o'))
    .flag(FlagSpec::string("tags", "", r#"tag expression, e.g. "@smoke && ~@slow""#).short('t'))
    .flag(FlagSpec::bool("stop-on-failure", "stop after the first failed scenario"))
    .flag(FlagSpec::bool(
        "use-session",
        "read and write .apic/session.json instead of an isolated session per scenario",
    ))
    .flag(FlagSpec::bool(
        "steps",
        "print the built-in step vocabulary and declared phrases, then exit",
    ))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let f = &inv.flags;
    let (format, output, tags) = (f.string("format"), f.string("output"), f.string("tags"));
    let use_session = f.bool("use-session");
    if f.bool("steps") {
        return a.print_steps();
    }
    let p = a.load_project()?;
    let vars = a.var_map()?;
    apic_runner::set_version(crate::version());
    if use_session && a.g.no_sess {
        return Err(fail(
            Code::Flag,
            "--use-session and --no-session cannot be combined",
        ));
    }
    let mut opts = apic_bdd::Options {
        paths: inv.args.clone(),
        format: format.clone(),
        tags,
        stop_on_failure: f.bool("stop-on-failure"),
        no_colors: a.g.no_color
            || a.g.json
            || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
            || !output.is_empty()
            || !a.stdout.is_terminal(),
        output: Some(a.stdout.shared()),
        ..apic_bdd::Options::new(apic_bdd::Config {
            env: a.g.env.clone(),
            vars,
            use_session,
            timeout: a.g.timeout,
            insecure: a.g.insecure,
            redact: a.g.redact,
            cookies: a.g.cookies,
            stderr: Some(a.stderr.shared()),
            ca_cert: a.g.cacert.clone(),
            cert: a.g.cert.clone(),
            key: a.g.key.clone(),
            proxy: a.g.proxy.clone(),
            no_proxy: a.g.no_proxy,
            ..apic_bdd::Config::new(p.clone())
        })
    };
    if opts.config.env.is_empty() {
        opts.config.env = p.config.env.clone();
    }
    if a.g.json {
        opts.format = "cucumber".to_string();
    }
    let mut lazy: Option<Arc<Mutex<LazyFile>>> = None;
    if !output.is_empty() {
        let features = apic_bdd::feature_files(&opts).map_err(|e| fail(Code::Features, e))?;
        output_overlaps_sources(&output, &p, &features)?;
        // Created on first write, which happens only after the features
        // have been read, so a report path can never truncate its input.
        let lf = Arc::new(Mutex::new(LazyFile { path: output.clone(), f: None, err: None }));
        opts.output = Some(lf.clone());
        lazy = Some(lf);
    }
    let result = if format == "html" && !a.g.json {
        test_html(&inv.cancel, &opts, &output)
    } else {
        match apic_bdd::run(&inv.cancel, &opts) {
            (_, Some(err)) => Err(test_err(err)),
            (0, None) => Ok(()),
            (code, None) => Err(CliError::exit(code)),
        }
    };
    if let Some(lf) = lazy {
        // A report that was not fully written fails the command whatever
        // the scenarios did: CI must not lose it silently.
        let closed = lf.lock().unwrap_or_else(PoisonError::into_inner).close();
        if let Err(cerr) = closed {
            return Err(fail(
                Code::File,
                format!(
                    "write report {output}: {cerr} (run result: {})",
                    describe_outcome(result.as_ref().err())
                ),
            ));
        }
    }
    result
}

/// Runs the features with the cucumber formatter into memory and renders
/// the self-contained HTML report from it, to --output or stdout. The
/// cucumber report is already masked under --redact.
fn test_html(cancel: &CancelToken, opts: &apic_bdd::Options, output: &str) -> CliResult {
    let started = Time::now();
    let (_, raw, code, run_err) = apic_bdd::run_summary(cancel, opts);
    if let Some(err) = &run_err {
        if raw.is_empty() {
            return Err(test_err(err.clone()));
        }
    }
    let meta = apic_report::Meta {
        version: crate::version().to_string(),
        env: opts.config.env.clone(),
        time: Some(started),
        redacted: opts.config.redact,
        project: opts.config.project.root.clone(),
        ..apic_report::Meta::default()
    };
    if let Some(dest) = &opts.output {
        let mut dest = dest.lock().unwrap_or_else(PoisonError::into_inner);
        apic_report::features(&mut *dest, &meta, &raw)
            .map_err(|e| fail(Code::File, format!("write report {output}: {e}")))?;
    }
    if let Some(err) = run_err {
        return Err(test_err(err));
    }
    if code != 0 {
        return Err(CliError::exit(code));
    }
    Ok(())
}

/// Maps a suite error onto the exit codes: transport and usage errors
/// keep their 3 and 2, anything else is a definition problem (2).
fn test_err(err: apic_runner::Error) -> CliError {
    if err.transport().is_some() || err.usage().is_some() {
        return err.into();
    }
    fail(Code::Features, err.to_string())
}

/// Words the run result for the report-failure message.
pub(crate) fn describe_outcome(err: Option<&CliError>) -> String {
    match err {
        None => "scenarios passed".to_string(),
        Some(e) => e.text(),
    }
}

/// Returns path in absolute, symlink-free, canonical form even when it
/// (or some of its parent directories) does not exist yet: the nearest
/// existing ancestor is resolved and the remainder appended.
fn real_path(path: &str) -> String {
    match fp::abs(path) {
        Ok(abs) => apic_project::real_prefix(&abs),
        Err(_) => path.to_string(),
    }
}

/// Reports whether two paths name the same file, links followed.
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// Refuses a report path that is, or aliases through a symlink or hard
/// link, a file the test command reads or writes: request, feature and
/// body files, the project config, environment files and the session.
/// `features` are the resolved feature files selected for this run.
pub(crate) fn output_overlaps_sources(output: &str, p: &Project, features: &[String]) -> CliResult {
    let refuse = || {
        Err(fail(
            Code::Flag,
            format!("--output {output} would overwrite a project file; write it elsewhere"),
        ))
    };
    let mut body_files: HashSet<String> = HashSet::new();
    for req in p.requests() {
        for r in req.body_files() {
            let path = fp::join(&fp::join(&p.root, &fp::dir(&req.file.path)), &r.path);
            if let Ok(abs) = fp::abs(&path) {
                body_files.insert(fp::clean(&abs));
            }
        }
    }
    let root_real = real_path(&p.root);
    // Reports whether path (existing or not) lies in the project.
    let under_root = |path: &str| apic_project::within(&root_real, &real_path(path)).is_some();
    // Recognises project inputs by name only inside the project; files
    // elsewhere are matched by identity below.
    let is_input = |path: &str| {
        if fp::abs(path).is_ok_and(|abs| body_files.contains(&fp::clean(&abs))) {
            return true;
        }
        if !under_root(path) {
            return false;
        }
        if matches!(fp::ext(path).to_lowercase().as_str(), ".feature" | ".http" | ".rest") {
            return true;
        }
        let base = fp::base(path);
        if [
            apic_project::CONFIG_FILE,
            apic_env::PUBLIC_FILE,
            apic_env::PRIVATE_FILE,
            apic_env::DOT_ENV_FILE,
        ]
        .contains(&base.as_str())
        {
            return true;
        }
        fp::base(&fp::dir(path)) == apic_session::DIR
    };
    if is_input(output) {
        return refuse();
    }
    let Ok(real) = fp::eval_symlinks(output) else {
        return Ok(()); // does not exist yet: nothing to alias
    };
    if is_input(&real) {
        return refuse();
    }
    let Ok(target) = std::fs::metadata(&real) else {
        return Ok(());
    };
    let same_as = |path: &str| std::fs::metadata(path).is_ok_and(|info| same_file(&info, &target));
    // Explicitly selected inputs are checked directly, wherever they live.
    if body_files.iter().any(|bf| same_as(bf)) || features.iter().any(|f| same_as(f)) {
        return refuse();
    }
    // Request files can live outside the root (`dir: ../api` in apic.yaml).
    if p.files.iter().any(|f| same_as(&fp::join(&p.root, &f.path))) {
        return refuse();
    }
    // Fixed project files, for an output that aliases them from elsewhere.
    let session_file = fp::join(apic_session::DIR, apic_session::FILE);
    for name in [
        apic_project::CONFIG_FILE,
        apic_env::PUBLIC_FILE,
        apic_env::PRIVATE_FILE,
        apic_env::DOT_ENV_FILE,
        session_file.as_str(),
    ] {
        if same_as(&fp::join(&p.root, name)) {
            return refuse();
        }
    }
    if walk_finds(&p.root, true, &|path| is_input(path) && same_as(path)) {
        return refuse();
    }
    Ok(())
}

/// Walks a directory the way the project is scanned, hidden directories
/// (but for the session's), node_modules and vendor left out, and reports
/// whether `hit` is true of any file.
fn walk_finds(dir: &str, top: bool, hit: &dyn Fn(&str) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = fp::join(dir, &name);
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir {
            let skip = name != apic_session::DIR
                && (name.starts_with('.') || name == "node_modules" || name == "vendor");
            if !skip && walk_finds(&path, false, hit) {
                return true;
            }
        } else if hit(&path) {
            return true;
        }
    }
    let _ = top;
    false
}

/// Opens its path on the first write. The first create or write error is
/// kept and reported by close, since formatters may ignore it.
struct LazyFile {
    path: String,
    f: Option<std::fs::File>,
    err: Option<String>,
}

impl LazyFile {
    fn fail(&mut self, e: std::io::Error, what: &str) -> std::io::Error {
        self.err = Some(apic_gocompat::path_error(what, std::path::Path::new(&self.path), &e));
        e
    }

    fn close(&mut self) -> Result<(), String> {
        if let Some(f) = self.f.take() {
            if let Err(e) = f.sync_all().or_else(|e| match e.kind() {
                // Not every file can be synced (a pipe, /dev/null).
                std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported => Ok(()),
                _ => Err(e),
            }) {
                if self.err.is_none() {
                    self.fail(e, "close");
                }
            }
        }
        match self.err.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Write for LazyFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(e) = &self.err {
            return Err(std::io::Error::other(e.clone()));
        }
        if self.f.is_none() {
            match std::fs::File::create(&self.path) {
                Ok(f) => self.f = Some(f),
                Err(e) => return Err(self.fail(e, "open")),
            }
        }
        let Some(f) = self.f.as_mut() else {
            return Ok(0);
        };
        match f.write(buf) {
            Ok(n) => Ok(n),
            Err(e) => Err(self.fail(e, "write")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl App {
    fn print_steps(&self) -> CliResult {
        if self.g.json {
            #[derive(Serialize)]
            struct Entry {
                pattern: String,
                purpose: String,
                #[serde(skip_serializing_if = "String::is_empty")]
                request: String,
            }
            let mut out: Vec<Entry> = apic_bdd::VOCABULARY
                .iter()
                .map(|v| Entry {
                    pattern: v.pattern.to_string(),
                    purpose: v.purpose.to_string(),
                    request: String::new(),
                })
                .collect();
            // Project phrases are appended when a project loads; the
            // built-in vocabulary is still useful outside one.
            if let Ok(p) = self.load_project() {
                for r in p.requests() {
                    for st in r.steps() {
                        out.push(Entry { pattern: st, purpose: format!("runs {}", r.id()), request: r.id() });
                    }
                }
            }
            return self.write_json(&out);
        }
        let t = theme();
        wln!(self.stdout, "{}", t.bold.render("built-in steps"));
        for v in apic_bdd::VOCABULARY {
            w!(self.stdout, "  {}\n      {}\n", v.pattern, t.dim.render(v.purpose));
        }
        let Ok(p) = self.load_project() else {
            return Ok(()); // vocabulary alone is still useful outside a project
        };
        let mut printed = false;
        for r in p.requests() {
            for st in r.steps() {
                if !printed {
                    wln!(self.stdout, "{}", t.bold.render("\nphrases declared in this project"));
                    printed = true;
                }
                w!(
                    self.stdout,
                    "  {st}\n      {}\n",
                    t.dim.render(&format!("runs {} ({})", r.id(), r.file.path))
                );
            }
        }
        Ok(())
    }
}
