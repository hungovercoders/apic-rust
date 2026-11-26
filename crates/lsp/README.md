# apic-lsp

apic's language server (`apic_lsp`): `apic lsp` speaks the Language Server
Protocol over stdio, so any editor with an LSP client gets the diagnostics,
completion, hover, code lenses and formatting the VS Code extension has,
from the same code the CLI runs. The transport, JSON-RPC 2.0 with the
protocol's `Content-Length` framing, is written here rather than pulled in
as a library.

```rust
use std::time::Duration;
use apic_lsp::{serve, Error, Options};

apic_output::style::set_color_profile(apic_output::style::Profile::Ascii); // the log carries no colour
apic_runner::set_version(version);
let cancel = apic_httpc::CancelToken::new();          // cancel it on Ctrl-C or SIGTERM
let opts = Options { root: dir, env, version: version.into(), debounce: Duration::from_millis(150) };
match serve(&cancel, std::io::stdin(), std::io::stdout(), opts) {
    Ok(()) | Err(Error::Cancelled) => {}               // exit 0
    Err(Error::NoShutdown) => { /* exit 1, the message is ERR_NO_SHUTDOWN */ }
    Err(err @ Error::Io(_)) => { /* a usage error with code E214: usage(Code::Server, err.to_string()) */ }
}
```

## API

| Item | What it is |
| --- | --- |
| `serve(cancel: &CancelToken, r: impl Read + Send + 'static, w: impl Write + Send + 'static, opts: Options) -> Result<(), Error>` | Runs a server on `r` and `w` until the client sends `exit` or closes the stream. `Ok` after a `shutdown` request and an `exit` notification. It returns only once the runs and checks in flight have ended; the writer is flushed after every message |
| `Options { root, env, version: String, debounce: Duration }` | `root`: the project root when the client names no workspace (`apic lsp -C dir`). `env`: the environment hover, completion and the run command use, replaced by the client's `initializationOptions.env`. `version`: reported in `serverInfo`. `debounce`: how long after an edit the project is checked again; zero checks after every edit. `Default` is all empty and zero |
| `Error` | `NoShutdown` (the client went without a shutdown request: exit 1), `Cancelled` (the token was cancelled; displays as `context canceled`), `Io(String)` (a message that could not be read or written: `bad Content-Length "lots"`, `reading message body: unexpected EOF`). `Display`, `std::error::Error`, `Clone`, `PartialEq` |
| `ERR_NO_SHUTDOWN` | `&str`: what `Error::NoShutdown` displays, `the client exited without a shutdown request` |
| `COMMAND_RUN`, `COMMAND_DESCRIBE`, `COMMAND_CURL` | `apic.lsp.run`, `apic.lsp.describe`, `apic.lsp.curl`: the commands of the code lenses, run through `workspace/executeCommand` with the document URI and the request's target (`file#name` or `file#N`, from the project root) |
| `COMMAND_VALIDATE` | `apic.lsp.validate`: checks every project of the workspace and answers once its diagnostics are published; no arguments |
| `OPERATORS` | `&[&str]`: the assertion operators, in the order the docs list them |

## What the server does

- **Capabilities**: full-text sync (`openClose`, `change: 1`, `save`
  without text), completion (triggers `@ { . space $`), hover,
  `executeCommand` with the four commands, and, unless
  `initializationOptions.codeLens` or `.formatting` is `false`, code lenses
  and formatting. `positionEncoding` is `utf-8` when the client offers it,
  else `utf-16`.
- **`initializationOptions`**: `env`, `envs` (project root → environment),
  `projectRoots` (workspace folder → the project root of every file in it),
  `codeLens`, `formatting`. `workspace/didChangeConfiguration` takes
  `settings.apic.env` and `settings.apic.envs`.
- **Diagnostics**: what `apic validate` reports, with open buffers in
  place of their files, per project (the nearest directory above a file
  holding `apic.yaml` or an http-client env file, inside its workspace
  folder). A set that did not change is not sent again; a project that
  does not load is one diagnostic on its `apic.yaml`.
- **Watched files**: a client that supports dynamic registration is asked
  (`client/registerCapability`, id `"apic-watch"`) to report changes to
  request files, `apic.yaml` and the env files.
- **Run** answers with the result as `apic run --json` prints it (nested,
  dependencies under `ran_first`), **Describe** with what `apic describe
  --json` prints (secrets masked) and **curl** with the redacted command.
  Each shows a one-line `window/showMessage` and writes the report to
  `window/logMessage`.
- Errors a request fails with carry code -32803 and the error's message;
  parameters of the wrong type are -32602 and name the field (`json: cannot
  unmarshal number into Go struct field textDocumentIdentifier.textDocument.uri
  of type string`), as clients have always been told.

## Threads and ordering

`serve` handles messages one at a time, in arrival order, on the calling
thread; each reply is written before the next message is read. Elsewhere:

- a reader thread frames the input, so a cancelled token ends `serve`
  within 50 ms while the input is quiet (the thread is left blocked on the
  reader, which is why the reader must be `'static`);
- a run from a code lens is sent on its own thread, cancelled when `serve`
  ends, and answers its `executeCommand` request when done: log, summary,
  then the reply, in that order;
- a debounced check waits on its own thread. A newer edit replaces it; a
  completion, hover, code lens or command for a document of that project
  runs it first (so the request sees the latest text); `serve` ending drops
  it.

All state is behind one lock, never held while writing to the client;
writes are serialised by a second lock. Diagnostics of one check are sent
sorted by path.

## Differences from before

- Two keys of one JSON object that differ only by case are matched as
  before only for ASCII letters.
- A number written with an exponent in place of a whole number is reported
  in its decimal form (`number 100.0`, not `number 1e2`).
- With `positionEncoding` `utf-8`, a column inside a character is taken as
  the character's start.
- Projects waiting for a check after one `didChangeWatchedFiles` are
  scheduled in path order (the order was arbitrary).

## Tests and the example

`src/server_tests.rs` drives the server over in-memory pipes, as an editor
would: 27 tests, covering every method. `examples/lsp_stdio.rs` is the
server on stdin and stdout (`--env`, `--root`, `--version`), for comparing
a scripted session against another build of apic.
