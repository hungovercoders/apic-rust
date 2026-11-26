//! apic ui: the terminal UI.

use crate::root::{App, CliResult, Invocation};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new("ui", "Browse and run the project's requests in a terminal UI")
        .args(ArgRule::None)
        .flag(FlagSpec::bool(
            "demo",
            "serve the built-in demo API in-process and open the UI on its example project",
        ))
        .runs(run)
}

fn run(_a: &mut App, _inv: &Invocation) -> CliResult {
    Ok(())
}
