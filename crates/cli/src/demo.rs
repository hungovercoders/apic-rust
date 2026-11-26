//! apic demo: scaffold and serve a fake API.

use serde::Serialize;

use crate::io::wln;
use crate::root::{App, CliError, CliResult, Invocation};
use crate::serve::{listen_local, serve_local};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "demo",
        "Scaffold and serve a fake API, so apic can be tried with no setup",
    )
    .long(
        "demo writes a small example .http project (login, whoami, and a todos
CRUD resource) into --out, then serves the fake API those requests target.
No network access or git clone needed. Existing files are left alone
unless --force is given.

Run it, then use the project from another terminal (the examples below;
substitute your --out if you set one).

Every apic command, including ui, resolves .http files relative to -C
(default: the current directory) — apic ui on its own won't find the
project this command just wrote unless you cd into it or pass -C.",
    )
    .example(
        "  apic demo                                  # terminal 1: writes ./apic-demo, serves the API
  apic run login whoami -C apic-demo         # terminal 2
  apic ui -C apic-demo --env local           # or the UI, same directory",
    )
    .args(ArgRule::None)
    .flag(FlagSpec::string("out", "apic-demo", "directory to write the example project into").short('o'))
    .flag(FlagSpec::int("port", 8089, "port to serve the demo API on"))
    .flag(FlagSpec::bool("force", "overwrite existing files"))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let out = inv.flags.string("out");
    let port = inv.flags.int("port");
    // Bind first: a port already in use fails here, before anything is
    // written or "listening" is printed.
    let listener = listen_local(port)?;
    let port = u16::try_from(port).unwrap_or(0);
    let (written, skipped) =
        apic_demoapi::write_project(&out, port, inv.flags.bool("force")).map_err(CliError::other)?;
    let url = format!("http://localhost:{port}");
    if a.g.json {
        #[derive(Serialize)]
        struct Out<'a> {
            out: &'a str,
            url: &'a str,
            #[serde(serialize_with = "crate::shared::null_when_empty")]
            written: &'a [String],
            #[serde(skip_serializing_if = "<[String]>::is_empty")]
            skipped: &'a [String],
            listening: bool,
        }
        a.write_json(&Out { out: &out, url: &url, written: &written, skipped: &skipped, listening: true })?;
    } else {
        for f in &written {
            wln!(a.stdout, "wrote {f}");
        }
        for f in &skipped {
            wln!(a.stdout, "kept  {f} (use --force to overwrite)");
        }
        wln!(a.stdout, "demo api listening on {url}");
        wln!(a.stdout, "try: apic run login whoami -C {out} --env local");
    }
    serve_local(&inv.cancel, listener, apic_demoapi::new());
    Ok(())
}
