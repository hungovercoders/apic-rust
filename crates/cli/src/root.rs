//! The root command: the flags every command shares, running a command
//! line and reporting what went wrong.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use apic_httpc::CancelToken;
use apic_output::style;
use apic_project::Project;
use apic_runner::{usage, Code, Runner};
use serde::Serialize;

use crate::io::{wln, Input, Output};
use crate::parse::{self, Flags, Parsed};
use crate::shared::theme;
use crate::spec::{command_path, CommandSpec, FlagSpec};

/// The version, stamped at build time through the `APIC_VERSION`
/// environment variable.
pub const VERSION: &str = match option_env!("APIC_VERSION") {
    Some(v) => v,
    None => "dev",
};

/// The version this binary reports.
pub fn version() -> &'static str {
    if VERSION.is_empty() {
        "dev"
    } else {
        VERSION
    }
}

/// The global flags of one invocation.
#[derive(Clone, Debug, Default)]
pub(crate) struct Globals {
    pub dir: String,
    pub env: String,
    pub vars: Vec<String>,
    pub json: bool,
    pub no_color: bool,
    pub timeout: Duration,
    pub no_sess: bool,
    pub insecure: bool,
    pub redact: bool,
    pub cookies: bool,
    pub proxy: String,
    pub no_proxy: bool,
    pub cacert: String,
    pub cert: String,
    pub key: String,
}

/// How a command ends when it does not end well.
#[derive(Debug)]
pub(crate) enum CliError {
    /// An explicit exit code (failed assertions, say), with a line for
    /// stderr unless the message is empty.
    Exit { code: i32, msg: String },
    /// An error to report with its catalogue entry.
    Run(apic_runner::Error),
}

impl CliError {
    pub fn exit(code: i32) -> CliError {
        CliError::Exit {
            code,
            msg: String::new(),
        }
    }

    /// Any other failure, as its message.
    pub fn other(msg: impl std::fmt::Display) -> CliError {
        CliError::Run(apic_runner::Error::Other(msg.to_string()))
    }

    /// What a report-failure message says the run came to.
    pub fn text(&self) -> String {
        match self {
            CliError::Exit { msg, .. } => msg.clone(),
            CliError::Run(err) => err.to_string(),
        }
    }
}

impl From<apic_runner::Error> for CliError {
    fn from(err: apic_runner::Error) -> CliError {
        CliError::Run(err)
    }
}

impl From<apic_runner::UsageError> for CliError {
    fn from(err: apic_runner::UsageError) -> CliError {
        CliError::Run(err.into())
    }
}

pub(crate) type CliResult = Result<(), CliError>;

/// A usage error with its code, as a command's failure.
pub(crate) fn fail(code: Code, msg: impl Into<String>) -> CliError {
    usage(code, msg).into()
}

/// One command being run: what cancels it, its arguments and its flags.
pub(crate) struct Invocation {
    pub cancel: CancelToken,
    pub args: Vec<String>,
    pub flags: Flags,
}

/// Holds the IO streams of the commands (swappable for tests) and runs
/// command lines against them.
pub struct App {
    pub stdout: Output,
    pub stderr: Output,
    pub stdin: Input,
    pub(crate) g: Globals,
}

impl Default for App {
    fn default() -> App {
        App::new()
    }
}

static SPEC: LazyLock<CommandSpec> = LazyLock::new(root_spec);

/// The root command with everything under it.
fn root_spec() -> CommandSpec {
    let mut root = CommandSpec::new("apic", "Run .http request files from the terminal, CI, or an AI agent")
        .long(
            "apic is epic: it runs the plain .http files VS Code, JetBrains and Neovim
can send with one click, from any terminal, CI job or AI agent, with one
static binary. It adds environments, captured variables that persist between
runs, assertions, AWS and OAuth2 auth, Gherkin features, JSON output, curl
export, an MCP server and a terminal UI (apic ui).

Try it with nothing set up:  apic ui --demo

For AI agents: apic skill prints the briefing an agent should read before
working with a project (apic skill api-design and api-security brief it on
the API itself), apic skill install writes it into the project (--all the
three), and apic mcp serves the project as tools.

Exit codes: 0 ok · 1 assertion or capture failed · 2 usage/parse/missing variable · 3 network error",
        )
        .example(
            "  apic ui --demo                     # a fake API and a UI to poke it with, nothing to set up
  apic init                          # scaffold a project in the current directory
  apic run login                     # send one request, keep what it captures
  apic run smoke.http --json | jq    # a whole file as a flow, one JSON line per request
  apic test                          # run the .feature files against the same requests",
        );
    let flags = [
        FlagSpec::string("dir", ".", "project root holding .http files and env files").short('C'),
        FlagSpec::string("env", "", "environment from http-client.env.json (default from apic.yaml)").short('e'),
        FlagSpec::string_array("var", "override a variable, name=value (repeatable)"),
        FlagSpec::bool("json", "machine-readable JSON output"),
        FlagSpec::bool("no-color", "disable colour (also honours NO_COLOR)"),
        FlagSpec::duration("timeout", "request timeout (default 30s or apic.yaml)"),
        FlagSpec::bool("no-session", "do not read or write captured values in .apic/session.json"),
        FlagSpec::bool("insecure", "skip TLS certificate verification"),
        FlagSpec::string("cacert", "", "PEM file with certificates to trust in addition to the system roots"),
        FlagSpec::string("cert", "", "PEM client certificate to present (mTLS)"),
        FlagSpec::string("key", "", "PEM private key for --cert (default: the --cert file)"),
        FlagSpec::string("proxy", "", "send requests through this proxy (http, https or socks5 URL); beats proxy: in apic.yaml and HTTP(S)_PROXY"),
        FlagSpec::bool("no-proxy", "send requests directly, ignoring --proxy, apic.yaml and the environment"),
        FlagSpec::bool("cookies", "keep a cookie jar per environment in .apic/cookies.json (or set cookies: true in apic.yaml)"),
        FlagSpec::bool("redact", "mask all request headers, bodies, query values and captures in output (for CI logs)"),
    ];
    for f in flags {
        root = root.flag(f.persistent());
    }
    for sub in [
        crate::run::spec(),
        crate::ui::spec(),
        crate::test::spec(),
        crate::list::spec(),
        crate::describe::spec(),
        crate::env::spec(),
        crate::session::spec(),
        crate::history::spec(),
        crate::select::spec(),
        crate::curl::spec(),
        crate::snippet::spec(),
        crate::validate::spec(),
        crate::fmt::spec(),
        crate::import::spec(),
        crate::init::spec(),
        crate::skill::spec(),
        crate::mcp::spec(),
        crate::lsp::spec(),
        crate::demo::spec(),
        crate::mock::spec(),
        version_spec(),
        crate::completion::spec(),
        crate::help::spec(),
    ] {
        root = root.subcommand(sub);
    }
    sort_commands(&mut root);
    root
}

/// Puts every level of the tree in name order, the order the help and the
/// reference list commands in.
fn sort_commands(cmd: &mut CommandSpec) {
    cmd.subcommands.sort_by_key(|c| c.name);
    for sub in &mut cmd.subcommands {
        sort_commands(sub);
    }
}

impl App {
    /// An app on the process's own streams.
    pub fn new() -> App {
        apic_demoapi::set_version(version()); // GET /health on the demo API reports it
        App {
            stdout: Output::stdout(),
            stderr: Output::stderr(),
            stdin: Input::Stdin,
            g: Globals {
                dir: ".".to_string(),
                ..Globals::default()
            },
        }
    }

    /// The command tree: every command with its usage, texts, argument
    /// rule and flags.
    pub fn spec() -> &'static CommandSpec {
        &SPEC
    }

    /// The same tree as a `clap` command, global flags marked global: what
    /// the completion scripts are generated from.
    pub fn command(&self) -> clap::Command {
        crate::completion::clap_tree(App::spec())
    }

    /// Runs the command tree with `args` (excluding the program name) and
    /// returns the process exit code.
    pub fn execute<S: AsRef<str>>(&mut self, cancel: &CancelToken, args: &[S]) -> i32 {
        let args: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
        match self.dispatch(cancel, &args) {
            Ok(()) => apic_runner::EXIT_OK,
            Err(CliError::Exit { code, msg }) => {
                if !msg.is_empty() {
                    wln!(self.stderr, "error: {msg}");
                }
                code
            }
            Err(CliError::Run(err)) => {
                let code = apic_runner::exit_code(Some(&err));
                self.print_error(&err, code);
                code
            }
        }
    }

    fn dispatch(&mut self, cancel: &CancelToken, args: &[String]) -> CliResult {
        let root = App::spec();
        let (chain, rest) = parse::find(root, args);
        let leaf = chain[chain.len() - 1];
        if chain.len() == 1 {
            let flags = crate::spec::effective_flags(&chain);
            if let Some(word) = parse::strip_flags(&rest, &flags).first() {
                // The flags are not read for a command that cannot be
                // found, so --json has to be read from the arguments.
                self.g.json = self.g.json || args.iter().any(|a| a == "--json");
                return Err(fail(Code::Flag, unknown_command(root, word, "apic")));
            }
        }
        let (flags, rest) = match parse::parse_flags(&chain, &rest) {
            Parsed::Ok(flags, rest) => (flags, rest),
            Parsed::Help => {
                self.stdout.print(&crate::help::help(&chain));
                return Ok(());
            }
            Parsed::Err(msg, json) => {
                self.g.json = json;
                return Err(fail(Code::Flag, msg));
            }
        };
        let own_env = chain.len() > 1 && leaf.flags.iter().any(|f| f.name == "env");
        self.read_globals(&flags, own_env);
        let Some(run) = leaf.run else {
            self.stdout.print(&crate::help::help(&chain));
            return Ok(());
        };
        if let Err(msg) = leaf.args.check(&command_path(&chain), &rest) {
            return Err(fail(Code::Flag, msg));
        }
        if self.g.no_color
            || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
            || self.g.json
            || !self.stdout.is_terminal()
        {
            style::set_color_profile(style::Profile::Ascii);
        }
        let inv = Invocation {
            cancel: cancel.clone(),
            args: rest,
            flags,
        };
        run(self, &inv)
    }

    fn read_globals(&mut self, f: &Flags, own_env: bool) {
        self.g = Globals {
            dir: f.string("dir"),
            // A command with an --env of its own (init) leaves this one unset.
            env: if own_env {
                String::new()
            } else {
                f.string("env")
            },
            vars: f.strings("var"),
            json: f.bool("json"),
            no_color: f.bool("no-color"),
            timeout: f.duration("timeout"),
            no_sess: f.bool("no-session"),
            insecure: f.bool("insecure"),
            redact: f.bool("redact"),
            cookies: f.bool("cookies"),
            proxy: f.string("proxy"),
            no_proxy: f.bool("no-proxy"),
            cacert: f.string("cacert"),
            cert: f.string("cert"),
            key: f.string("key"),
        };
    }

    /// Writes an error to stderr: under --json as an object an agent can
    /// branch on, otherwise as a line, followed by the catalogue's hint
    /// (unless the message already names an apic command to run) and by
    /// the code and where the catalogue explains it. The extra lines are
    /// dim on a terminal and plain in a log, where they are read most.
    fn print_error(&self, err: &apic_runner::Error, exit: i32) {
        let mut info = apic_runner::info(err);
        info.exit = exit;
        if self.g.json {
            #[derive(Serialize)]
            struct Wrapper<'a> {
                error: &'a apic_runner::ErrorInfo,
            }
            // Hints say <command>, so nothing is escaped for HTML.
            let text = serde_json::to_string(&Wrapper { error: &info }).unwrap_or_default();
            wln!(
                self.stderr,
                "{}",
                apic_gocompat::escape_line_separators(&text)
            );
            return;
        }
        let message = err.to_string();
        wln!(self.stderr, "error: {message}");
        let mut lines: Vec<String> = Vec::new();
        if !info.hint.is_empty() && !names_command(&message) {
            lines.push(format!("  hint: {}", info.hint));
        }
        lines.push(format!("  {} {} · see {}", info.code, info.title, info.url));
        let dim = self.stderr.is_terminal();
        if !dim && info.code != Code::Terminal {
            // A pipe or a log is more often read by a script or an agent
            // than by a person, and --json serves those better; a hint it
            // never sees on a terminal costs a person nothing. Not for the
            // one error --json cannot help with: a command that needs a
            // terminal (apic ui) refuses --json too.
            lines.push(
                r#"  --json prints this error as one {"error": {...}} object to branch on; see https://apic.sh/agents/#errors"#
                    .to_string(),
            );
        }
        let t = theme();
        for l in lines {
            let l = if dim { t.dim.render(&l) } else { l };
            wln!(self.stderr, "{l}");
        }
    }

    pub(crate) fn load_project(&self) -> Result<Arc<Project>, CliError> {
        apic_project::load(&self.g.dir).map_err(|e| fail(Code::Project, e.to_string()))
    }

    pub(crate) fn new_runner(&self) -> Result<Runner, CliError> {
        let p = self.load_project()?;
        self.runner_for(p, |_| {})
    }

    /// Builds a runner on an already loaded project from the global
    /// flags; `modify` adjusts the options first.
    pub(crate) fn runner_for(
        &self,
        p: Arc<Project>,
        modify: impl FnOnce(&mut apic_runner::Options),
    ) -> Result<Runner, CliError> {
        let vars = self.var_map()?;
        apic_runner::set_version(version());
        let mut opts = apic_runner::Options {
            env: self.g.env.clone(),
            vars,
            no_session: self.g.no_sess,
            timeout: self.g.timeout,
            insecure: self.g.insecure,
            redact: self.g.redact,
            cookies: self.g.cookies,
            ca_cert: self.g.cacert.clone(),
            cert: self.g.cert.clone(),
            key: self.g.key.clone(),
            proxy: self.g.proxy.clone(),
            no_proxy: self.g.no_proxy,
            ..apic_runner::Options::default()
        };
        modify(&mut opts);
        let mut r = Runner::new(p, opts)?;
        // A browser sign-in (oauth2 grant=authorization_code) may start
        // only with a person present: a terminal on both sides and no
        // --json.
        r.interactive = !self.g.json && self.stderr.is_terminal() && std::io::stdin().is_terminal();
        Ok(r)
    }

    /// Parses the repeatable --var flag.
    pub(crate) fn var_map(&self) -> Result<BTreeMap<String, String>, CliError> {
        let mut vars = BTreeMap::new();
        for kv in &self.g.vars {
            match kv.split_once('=') {
                Some((k, v)) if !k.is_empty() => {
                    vars.insert(k.to_string(), v.to_string());
                }
                _ => {
                    return Err(fail(
                        Code::Flag,
                        format!("--var {} must be name=value", apic_gocompat::quote(kv)),
                    ))
                }
            }
        }
        Ok(vars)
    }
}

/// The message for a word that names no command, with the commands it
/// may have been meant for.
pub(crate) fn unknown_command(cmd: &CommandSpec, word: &str, path: &str) -> String {
    let mut msg = format!(
        "unknown command {} for {}",
        apic_gocompat::quote(word),
        apic_gocompat::quote(path)
    );
    let close = parse::suggestions_for(cmd, word);
    if !close.is_empty() {
        msg.push_str("\n\nDid you mean this?\n");
        for s in close {
            msg.push_str(&format!("\t{s}\n"));
        }
    }
    msg
}

/// Reports whether a message already tells the user which apic command
/// to run ("run `apic list`"), as against merely mentioning apic ("apic
/// does not send cleartext HTTP/2").
fn names_command(msg: &str) -> bool {
    App::spec()
        .subcommands
        .iter()
        .any(|c| msg.contains(&format!("apic {}", c.name)))
}

/// Runs the CLI and returns the process exit code.
pub fn main<S: AsRef<str>>(args: &[S]) -> i32 {
    let guard = crate::signal::signal_context();
    let code = App::new().execute(guard.token(), args);
    guard.stop();
    code
}

fn version_spec() -> CommandSpec {
    CommandSpec::new("version", "Print the apic version")
        .example("  apic version\n  apic version --json | jq -r .version")
        .runs(|a, _| {
            let info = build_info();
            if a.g.json {
                return a.write_json(&info);
            }
            let t = theme();
            wln!(
                a.stdout,
                "apic {} {}",
                t.accent.render(&info.version),
                t.dim.render(&format!(
                    "· {} · {} {}/{}",
                    info.commit, info.go, info.os, info.arch
                ))
            );
            wln!(
                a.stdout,
                "{}",
                t.dim.render("apic is epic · https://apic.sh/")
            );
            Ok(())
        })
}

/// What `apic version --json` prints. The `go` key holds the toolchain
/// the binary was built with, `rustc 1.85.0`: the key is part of the
/// output contract and kept its name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub commit: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub date: String,
    pub go: String,
    pub os: String,
    pub arch: String,
}

/// The version, the commit the binary was built from and what built it.
pub fn build_info() -> VersionInfo {
    let revision = env!("APIC_BUILD_COMMIT");
    let mut commit = match revision.len() {
        0 => "unknown".to_string(),
        n if n > 7 => revision[..7].to_string(),
        _ => revision.to_string(),
    };
    if env!("APIC_BUILD_DIRTY") == "true" {
        commit.push_str("-dirty");
    }
    VersionInfo {
        version: version().to_string(),
        commit,
        date: env!("APIC_BUILD_DATE").to_string(),
        go: env!("APIC_BUILD_RUSTC").to_string(),
        os: match std::env::consts::OS {
            "macos" => "darwin",
            other => other,
        }
        .to_string(),
        arch: match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            "x86" => "386",
            other => other,
        }
        .to_string(),
    }
}
