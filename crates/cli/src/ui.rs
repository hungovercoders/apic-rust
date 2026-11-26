//! apic ui: the terminal UI.

use std::sync::Arc;

use apic_httpd::{Listener, Server};
use apic_runner::{Code, Runner};

use crate::io::Input;
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::theme;
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "ui",
        "Browse and run the project's requests in a terminal UI",
    )
    .long(
        "ui opens an interactive terminal UI: the requests on the left, and a
preview, the response, the checks, the session and the history on the
right. Run a request with enter, a whole file with f, switch environment with e, and
press ? for every key.

This is the one apic command that needs a terminal. It refuses to start when
stdout is not a TTY or --json is given; use apic run / apic list there.

Like every apic command, ui resolves .http files relative to -C (default:
the current directory) — it has no project registry, so it won't find a
project written elsewhere unless you cd into it or pass -C.

--demo needs nothing set up: it serves the built-in fake API in-process and
opens the UI on the example project that targets it.",
    )
    .example(
        "  apic ui
  apic ui -C api --env staging
  apic ui --demo",
    )
    .args(ArgRule::None)
    .flag(FlagSpec::bool(
        "demo",
        "serve the built-in demo API in-process and open the UI on its example project",
    ))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let demo = inv.flags.bool("demo");
    if a.g.json {
        return Err(fail(
            Code::Terminal,
            "apic ui is interactive and does not support --json; use apic run or apic list --json",
        ));
    }
    if !a.stdout.is_terminal() {
        return Err(fail(
            Code::Terminal,
            "apic ui needs an interactive terminal (stdout is not a TTY); use apic run or apic list",
        ));
    }
    // Stopped, and its project removed, when this returns.
    let mut _served = None;
    if demo {
        let started = start_demo()?;
        a.g.dir = started.root().to_string();
        _served = Some(started);
    }
    // The UI owns the screen, so a browser sign-in prompt has nowhere to
    // go: its runners are never interactive.
    let r = ui_runner(a)?;
    let mut cfg = apic_ui::Config::new(r.clone());
    cfg.root = r.project.root.clone();
    cfg.env = r.opts.env.clone();
    let mut again = App {
        stdout: a.stdout.clone(),
        stderr: a.stderr.clone(),
        stdin: Input::Taken,
        g: a.g.clone(),
    };
    cfg.new_runner = Some(Box::new(move |env: &str| {
        again.g.env = env.to_string();
        ui_runner(&again).map_err(|e| match e {
            CliError::Run(err) => err,
            other => apic_runner::Error::Other(other.text()),
        })
    }));
    cfg.editor = editor_command();
    cfg.redact = a.g.redact;
    cfg.demo = demo;
    cfg.theme = theme();
    if !a.stdin.is_stdin() {
        return Err(fail(
            Code::Terminal,
            "apic ui needs an interactive terminal",
        ));
    }
    let stdin = apic_ui::stdin_file().map_err(|e| {
        fail(
            Code::Terminal,
            format!("ui: {}", apic_gocompat::io_error_text(&e)),
        )
    })?;
    let mut model = apic_ui::Model::new(cfg);
    apic_ui::run(&inv.cancel, &mut model, stdin, &mut a.stdout.clone())
        .map_err(|e| fail(Code::Terminal, format!("ui: {e}")))
}

fn ui_runner(a: &App) -> Result<Arc<Runner>, CliError> {
    let mut r = a.new_runner()?;
    r.interactive = false;
    Ok(Arc::new(r))
}

/// Picks the editor for the o key: $VISUAL, then $EDITOR, then a platform
/// default.
fn editor_command() -> String {
    for name in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(name) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    if cfg!(windows) { "notepad" } else { "vi" }.to_string()
}

/// The demo API being served in-process, and the project written for it.
pub(crate) struct Demo {
    root: String,
    server: Option<Server>,
}

impl Demo {
    /// The directory the example project was written to.
    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    /// Stops the server and removes the project.
    #[cfg(test)]
    pub(crate) fn stop(mut self) {
        self.end();
    }

    fn end(&mut self) {
        if let Some(server) = self.server.take() {
            server.shutdown();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

impl Drop for Demo {
    fn drop(&mut self) {
        self.end();
    }
}

/// Writes the example project to a temporary directory and serves the
/// fake API on a free localhost port.
pub(crate) fn start_demo() -> Result<Demo, CliError> {
    let dir = temp_dir("apic-ui-demo-").map_err(CliError::other)?;
    let listener = match Listener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(CliError::other(e));
        }
    };
    let port = listener.addr().port();
    if let Err(e) = apic_demoapi::write_project(&dir, port, true) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(CliError::other(e));
    }
    let server = Server::start(listener, apic_demoapi::new());
    Ok(Demo {
        root: dir,
        server: Some(server),
    })
}

/// Makes a new directory of its own under the system's temporary one.
fn temp_dir(prefix: &str) -> Result<String, String> {
    let base = std::env::temp_dir();
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
        ^ (u128::from(std::process::id()) << 64);
    for _ in 0..10_000 {
        let path = base.join(format!("{prefix}{}", seed % 10_000_000_000));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return Ok(path.to_string_lossy().into_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
            }
            Err(e) => return Err(apic_gocompat::path_error("mkdir", &path, &e)),
        }
    }
    Err(format!(
        "mkdir {}: file exists",
        base.join(prefix).display()
    ))
}
