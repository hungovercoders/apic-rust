# apic-ui

The interactive terminal UI behind `apic ui` (`apic_ui`): a request list on
the left, a tabbed detail pane on the right (preview, response, checks,
session, history) and a status bar. The model is driven entirely by
messages and renders to a string; `program` is the only part that touches a
terminal, and there is no TUI framework underneath.

```rust
// What `apic ui` does.
let mut cfg = apic_ui::Config::new(runner.clone());      // Arc<apic_runner::Runner>
cfg.root = runner.project.root.clone();
cfg.env = runner.opts.env.clone();
cfg.new_runner = Some(Box::new(move |env| build_runner(env)));  // -> Result<Arc<Runner>, apic_runner::Error>
cfg.editor = "vi".to_string();
let mut model = apic_ui::Model::new(cfg);
apic_ui::run(&cancel, &mut model, apic_ui::stdin_file()?, &mut std::io::stdout())?;

// Without a terminal: the tests and the screenshot generator.
model.resize(120, 40);
model.press("enter");                 // sends the request and waits for it
model.press_all("j 3");
model.press_watch("f", |m| frames.push(m.view()));
let screen = model.view();
```

## API

| Item | Signature / notes |
| --- | --- |
| `Config` | `{ root, env, runner: Arc<Runner>, new_runner: Option<NewRunner>, editor, redact, demo, theme }`, all public. `Config::new(runner)` leaves the rest empty, with the default theme. `root` is only what the status bar shows; an empty `env` means the runner's; an empty `editor` disables the o key |
| `NewRunner` | `Box<dyn FnMut(&str) -> Result<Arc<Runner>, apic_runner::Error>>`: rebuilds the runner for an environment, on e, E, r and when the editor closes. Set the runner's `interactive` to false: the UI owns the screen |
| `Model::new(cfg)` | Builds the model: the rows, a description of every request, the session snapshot. No terminal, no I/O beyond what describing reads |
| `update(Msg) -> Option<Cmd>` | Handles one message and returns the work it started |
| `view() -> String` | The whole screen: `height` lines of at most `width` cells joined by `\n`, no trailing newline. `loading…` before the first size, a one-line refusal under 70x16, empty once quitting. It takes `&mut self` because it refreshes the pane's cached content |
| `live() -> bool` | Whether the screen changes on its own (a run in flight, or a token's expiry counting down); the program ticks only then |
| `selected()`, `env()`, `running()`, `result(&Arc<Request>)`, `status()` | For tests: the request under the cursor, the environment, whether a run is in flight, a request's recorded result, the transient status text |
| `resize(w, h)`, `press(key)`, `press_all("j enter 3")`, `press_watch(key, \|m\| ...)` | The headless driver, below |
| `parse_key(name) -> Key` | A binding name (`j`, `enter`, `esc`, `tab`, `shift+tab`, `backspace`, `up`, `down`, `left`, `right`, `home`, `end`, `pgup`, `pgdown`, `ctrl+c`, `ctrl+d`, `ctrl+u`) as a keypress; anything else is its first character |
| `Key { kind: KeyType, rune }` | `Key::new(KeyType::Enter)`, `Key::rune('j')`; `Display` is the binding name |
| `Msg`, `Cmd` | `Cmd` is `Box<dyn FnOnce() -> Option<Msg> + Send>`. `Msg` is what the model takes (`Size`, `Tick`, `Key`, `Result`, `EditorDone`, `ClearStatus`) and what a command hands back to its driver (`Quit`, `Exec`, `Cmd`, `Batch`, `Async`, `Delay`, `Panic`) |
| `quit()` | The command that ends the UI |
| `run(&CancelToken, &mut Model, File, &mut dyn Write) -> Result<(), Error>` | The event loop, below |
| `stdin_file() -> io::Result<File>` | The process's standard input as a `File`, for `run` |
| `Error` | Why the UI could not start; `Display` is the message (`stdin is not a terminal`, `raw mode: ...`, `input: ...`) |

Sizes are `i32`, as in `apic_output::style`.

## The program

`run` refuses an input that is not a terminal, puts it into raw mode,
switches to the alternate screen and hides the cursor, then loops: keys
from the terminal, messages from finished work, the terminal's size every
250 ms (there is no signal handler) and a tick every 110 ms while the model
is live. It returns when the model quits (q, ctrl+c) or the token is
cancelled, which it notices within a tick. A frame is written only when it
differs from the last one, each line erased to its end.

On the way out, by return or by panic, the cursor and the main screen come
back and the terminal modes are restored. A panic on a worker thread is
caught there, carried to the loop and raised again only after the terminal
is back, with where it was raised and the stack there; so is a panic on the
loop itself. For this the crate wraps the process's panic hook once, the
first time work is guarded: panics on threads the UI is guarding are kept
for that report, every other panic goes to the hook that was installed
before.

The o key suspends all of that: the key reader is stopped (on Unix its
pending read is interrupted through `poll`, so no byte typed at the editor
is taken by apic), the terminal is handed back, and the editor runs with
the terminal as its input and the process's own standard output and error.
`out` is where the screen is written, so it should be standard output.

Nothing asks the terminal a question at start-up, here or in
`apic_output::style`.

## Without a terminal

`resize` says how big the terminal is. `press` feeds one key and resolves
everything it starts inline: a request sent by enter, each step of a flow.
What needs the real program (a timer, the editor, quitting) ends the chain.
`press_watch` also calls back after each piece of background work lands
(for f, once per request) and once at the end; the callback is handed the
model, since `press_watch` holds it. A run is the real thing: `press`
returns when the response has arrived.

## Threads

The model lives on one thread and is not `Send` (its `new_runner` and its
clock are plain closures). A request is sent on a worker thread through the
`Arc<Runner>` while the model goes on describing and resolving on its own;
`Msg` and `Cmd` are `Send`.

## Layout of the source

`ui.rs` (the model, `update`, `view`, the status bar), `list.rs` (the left
pane), `panes.rs` (the tab strip and tabs), `history.rs` (the history tab),
`run.rs` (what the keys do), `keys.rs` (the bindings), `editor.rs`,
`headless.rs`, `viewport.rs`, and the terminal layer: `term.rs` (keys and
decoding them), `input.rs` and `input_unix.rs` (the key source),
`program.rs` (the loop) and `sys/` (`unix.rs` over libc, `windows.rs` over
windows-sys). `sys/` is the only code in apic with `unsafe`; the crate
denies it everywhere else.

## Where it differs from before

- A result is told apart from the one it replaces by a counter, not by its
  address: the pane redraws its content whenever any description, result
  or error changes, which shows the same text.
- The clear-session and editor messages carry the error's text, not the
  error. An editor that cannot be started reads `exec: "name": executable
  file not found in $PATH`; one that fails reads `exit status N`.
- On Windows the window size is asked of the console's output when the
  input handle cannot say, where it used to fall back to 80x24.
- A session value whose name is wider than the pane is cut to nothing but
  its ellipsis rather than ending the program.
- Cancelling `run`'s token is noticed at the next tick, up to 110 ms later.

## Tests

`src/tests/` ports every test of the original, over the demo project served
by `apic_demoapi`. `golden_tests.rs` replays the scripts in
`testdata/golden/*.txt` and compares each screen byte for byte with the
recordings beside them (`golden` without colour, `colour` under 24-bit
colour), which were drawn by the UI apic had before; its header says how a
script is written and how to dump the screens of another one. The Windows
modules are type-checked for `x86_64-pc-windows-gnu` and nothing more.
