//! apic curl: a request as the curl command that sends the same thing.
//! It is `apic snippet --lang curl`, kept for its own --json shape.

use serde::Serialize;

use crate::io::wln;
use crate::root::{App, CliResult, Invocation};
use crate::spec::{ArgRule, CommandSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "curl <request>",
        "Print the equivalent curl command (with variables resolved)",
    )
    .example(
        "  apic curl login
  apic curl get-user --env staging | sh",
    )
    .long(
        "Print the curl command that sends what apic would, variables resolved.
It is apic snippet --lang curl; see apic snippet for other languages.",
    )
    .args(ArgRule::Exact(1))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let (id, command) = a.snippet(&inv.args[0], "curl")?;
    if a.g.json {
        #[derive(Serialize)]
        struct Out {
            id: String,
            command: String,
        }
        return a.write_json(&Out { id, command });
    }
    wln!(a.stdout, "{command}");
    Ok(())
}
