//! apic lsp: the language server for editors other than VS Code.

use std::time::Duration;

use apic_output::style;
use apic_runner::Code;

use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::spec::{ArgRule, CommandSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "lsp",
        "Serve diagnostics, completion, hover, code lenses and formatting to an editor over LSP (stdio)",
    )
    .long(
        "Start a Language Server Protocol server on stdin/stdout, for Neovim,
Helix, Zed, JetBrains IDEs or any editor with an LSP client.

It publishes apic validate's problems as you type, completes directives,
{{variables}}, selectors, operators and auth types, shows a variable's value
and source on hover (secrets masked), formats with apic fmt, and puts Run,
Describe and curl lenses above each request.

The project is found from each file: the nearest directory holding
apic.yaml or an env file, as apic itself looks. --env picks the
environment, as does initializationOptions.env from the editor.

The server speaks JSON-RPC on stdout, so --json changes nothing here. It
exits 0 after the client's shutdown request and exit notification, and 1
when the client goes without one.",
    )
    .example(
        "  apic lsp
  apic lsp --env staging",
    )
    .args(ArgRule::None)
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    // The protocol owns stdout; reports written to the client's log carry
    // no colour codes.
    style::set_color_profile(style::Profile::Ascii);
    apic_runner::set_version(crate::version());
    let opts = apic_lsp::Options {
        root: a.g.dir.clone(),
        env: a.g.env.clone(),
        version: crate::version().to_string(),
        debounce: Duration::from_millis(150),
    };
    match apic_lsp::serve(&inv.cancel, a.stdin.take(), a.stdout.clone(), opts) {
        // Ctrl-C or SIGTERM: the server was told to stop.
        Ok(()) | Err(apic_lsp::Error::Cancelled) => Ok(()),
        // Not a usage error: the protocol asks for exit 1 when the client
        // goes without a shutdown request.
        Err(err @ apic_lsp::Error::NoShutdown) => Err(CliError::Exit { code: 1, msg: err.to_string() }),
        Err(err) => Err(fail(Code::Server, err.to_string())),
    }
}
