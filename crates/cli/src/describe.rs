//! apic describe: what a request needs and where each value comes from.

use apic_runner::Description;

use crate::root::{App, CliResult, Invocation};
use crate::shared::theme;
use crate::spec::{ArgRule, CommandSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "describe <request>",
        "Show a request's variables, where each comes from, captures and asserts",
    )
    .example(
        "  apic describe get-user
  apic describe get-user --env staging --json",
    )
    .args(ArgRule::Exact(1))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let (r, req) = a.single(&inv.args[0])?;
    let d = r.describe(&req);
    if a.g.json {
        return a.write_json(&mask_description(&d));
    }
    a.stdout.print(&apic_output::describe(&theme(), &d, &req.headers));
    Ok(())
}

fn mask_description(d: &Description) -> Description {
    let mut out = d.clone();
    for v in &mut out.variables {
        if v.secret {
            v.value = "***".to_string();
        }
    }
    out
}
