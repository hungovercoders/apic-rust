# apic-cli

The apic commands (`apic_cli`): the command tree, the flags every command
shares, and one module per command (`run.rs`, `list.rs`, `fmt.rs`, ...).
`crates/apic` is the binary, three lines that call `apic_cli::main`.

```rust
// What the binary does.
let args: Vec<String> = std::env::args().skip(1).collect();
std::process::exit(apic_cli::main(&args));

// What a test does: run a command in-process and read what it printed.
let mut app = apic_cli::App::new();
let (out, err) = (apic_cli::Buffer::new(), apic_cli::Buffer::new());
app.stdout = out.output();
app.stderr = err.output();
app.stdin = apic_cli::Input::text("[{\"id\": 1}]");
let code = app.execute(&apic_httpc::CancelToken::new(), &["run", "echo", "--data", "-", "--json"]);
assert_eq!(code, 0, "{}", err.string());
```

## API

| Item | Signature / notes |
| --- | --- |
| `main(args)` | `fn(&[impl AsRef<str>]) -> i32`: runs a command line on the process's streams under `signal_context`, and returns the exit code |
| `App { stdout, stderr: Output, stdin: Input }` | The streams, public so a test swaps them. `App::new()` is on the process's own |
| `App::execute(&mut self, &CancelToken, args)` | `-> i32`: runs `args` (without the program name). 0 ok, 1 an assertion or capture failed, 2 usage, 3 transport; an error goes to `stderr`, as one `{"error": {...}}` object under `--json` |
| `App::spec()` | `-> &'static CommandSpec`: the command tree as data, below |
| `App::command(&self)` | `-> clap::Command`: the same tree for `clap`, persistent flags marked global. The completion scripts are generated from it |
| `Output` | A shared writer: `Output::stdout()`, `Output::stderr()`, `Output::writer(w)`; `is_terminal()` is true only for a standard stream that is one; `shared()` is the `Arc<Mutex<dyn Write + Send>>` the runner and the feature runner take. Clones write to the same place |
| `Buffer` | Captures output: `output()` to hand to an `App`, then `string()` or `bytes()` |
| `Input` | `Stdin`, `Input::text(..)`, `Input::reader(r)`; `read_all()`; `take()` hands it to a server (`apic lsp`) |
| `signal_context()` | `-> SignalGuard`: `token()` is cancelled by the first Ctrl-C or SIGTERM (as `interrupt signal received` / `terminated signal received`), after which handling is the system's again, so a second one ends the process. `stop()`, and dropping the guard, put the default back |
| `VERSION`, `version()` | Stamped at build time from the `APIC_VERSION` environment variable; `dev` without it |
| `build_info()` | `-> VersionInfo { version, commit, date, go, os, arch }`: what `apic version --json` prints |
| `skill_dirs()` | `.claude/skills` and `.agents/skills`: where the skills are installed |

A run in progress stops when its token is cancelled: it prints what it
has, keeps the session and the history of the requests that completed, and
exits 3 with E305. `demo`, `mock`, `mcp` and `lsp` exit 0.

## The command tree

The tree is declared once, as `CommandSpec` values (each command module has
a `spec()`), and everything else is derived from it: the parser, the help
pages, `apic help`, the `clap` tree for completions, and what the
documentation generators print.

```text
CommandSpec {
    name,                       // "run"
    usage,                      // "run <request|file.http|file.http#name>..."
    short, long, example,       // the texts; long and example may be empty
    args: ArgRule,              // Any, None, Exact(n), Min(n), Max(n), Range(a, b)
    flags: Vec<FlagSpec>,       // its own flags, persistent ones included
    subcommands: Vec<CommandSpec>,
    hidden, disable_flags_in_use_line,
    valid_args: Vec<String>,    // words offered as arguments (the skill names)
}
FlagSpec { name, short: Option<char>, kind: FlagKind, default: String, usage, persistent, hidden }
FlagKind { Bool, String, StringArray, Int, Duration }     // type_name(): "", "string", "stringArray", "int", "duration"
```

| Item | What it gives |
| --- | --- |
| `CommandSpec::runnable()` | Whether the command does something itself (the root and `completion` only hold subcommands) |
| `find(name)`, `find_path(&["session", "clear"])` | A subcommand, or the command a path leads to |
| `local_flags()`, `persistent_flags()` | The command's flags that are not, and that are, persistent, by name |
| `effective_flags(chain)` | Every flag the last command of `chain` accepts; a flag of its own hides a persistent one of the same name (`init --env`) |
| `inherited_flags(chain)` | The persistent flags it takes from the commands above it, by name |
| `use_line(chain)` | `apic run <request|file.http|file.http#name>... [flags]` |
| `command_path(chain)` | `apic session clear` |

A `chain` is the commands from the root down to the one meant, `&[&root,
&session, &clear]`. The root's subcommands are in name order and include
`completion` and `help`; a generator that lists commands skips `help` and
anything `hidden`. `default` is the text the reference prints: `false`,
`[]`, `0s` and an empty string are the zero values it leaves out.

### What the documentation generators read

- `scripts/clidocs`: the root's `persistent_flags()` for the global table;
  for every command, `command_path`, `short`, `use_line`, `local_flags()`
  (name, `short`, `kind.type_name()`, `usage`, `default`), and for the man
  pages `long`, `example`, `inherited_flags` and the subcommands.
- `scripts/skilldocs`: the names of the root's subcommands, to check that
  every `apic <word>` a skill mentions is a command.

## How a command line is read

1. The command is found the way the arguments have always been read: at
   each level the first argument that is not a flag (or a flag's value)
   picks a subcommand if it names one. A flag the command does not know is
   taken to have a value, so `apic --help run` is the root's help.
2. That command's flags are parsed by `clap`, as one flat command: every
   value is read as text and checked afterwards, so the messages are
   apic's (`unknown flag: --nope`, `flag needs an argument: 'e' in -e`,
   `invalid argument "abc" for "--timeout" flag: must be a duration like
   "30s" or "5m"`). Flags and arguments mix freely, a switch takes
   `--json=false`, the last value of a repeated flag wins, and
   `StringArray` flags keep every value.
3. The arguments are counted against the command's `ArgRule` (`accepts 1
   arg(s), received 2`). All of these are usage errors with code E203.
   `--json` counts for the error's form only if it was read before the
   mistake, as flags are read in order.

`--help`, `-h`, `apic help [command]` and a command that only holds
subcommands print the help page, laid out from the tree in `help.rs`.

## Tests

`src/tests/` has one file per area, 98 tests: each runs commands through
`App::execute` on buffers, against `apic_httpd` test servers and temporary
directories. The helpers are in `src/tests/mod.rs` (`execute`,
`execute_with` for stdin and a cancellable token, `HangServer`,
`ProcessGuard` for the few tests that change the working directory or
`HOME`). `errors.rs` runs a real command for every code in the runner's
catalogue: a new code needs a case there.

## Where it differs from before

- The completion scripts are generated by `clap_complete` from the tree:
  they complete commands and flags, not request names or environments, and
  `--no-descriptions` is accepted and changes nothing.
- `apic version` reports the compiler that built the binary where the Go
  version stood (`rustc 1.85.0`; the JSON key is still `go`), and the
  commit, date and `-dirty` mark come from the checkout at build time
  (`unknown` outside one, or `APIC_COMMIT` / `APIC_DATE` when set).
- `apic fmt` refuses a file that is not valid UTF-8 (E210) rather than
  rewrite it.
- A command line argument that is not valid UTF-8 is read with U+FFFD in
  place of the bad bytes.
