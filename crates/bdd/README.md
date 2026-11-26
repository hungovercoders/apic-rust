# apic-bdd

Runs Gherkin `.feature` files against a project's `.http` requests
(`apic_bdd`): the machinery behind `apic test` and the `run_features` MCP
tool. The Gherkin reader, the scenario runner and the four report formats
are part of the crate; nothing outside it parses or runs a feature.

```rust
let project = apic_project::load(".")?;                       // Arc<Project>
let opts = Options {
    format: "pretty".into(),
    tags: "@smoke && ~@slow".into(),
    ..Options::new(Config { env: "dev".into(), ..Config::new(project) })
};
let (code, err) = apic_bdd::run(&CancelToken::new(), &opts);  // exit code, typed error
```

## API

| Item | Signature / notes |
| --- | --- |
| `Config` | Shared by every scenario: `project: Arc<Project>`, `env`, `vars: BTreeMap`, `use_session`, `timeout`, `insecure`, `redact`, `cookies`, `stderr: Option<Output>`, `ca_cert`, `cert`, `key`, `proxy`, `no_proxy`. `Config::new(project)`, `Default`, `Clone` |
| `Options` | One run: `config`, `paths` (files or directories, relative to the root), `features: Vec<Feature>` (in memory; used when `paths` is empty), `format` (one of `FORMATS`, default `pretty`), `output: Option<Output>` (default stdout), `tags`, `stop_on_failure`, `no_colors`. `Options::new(config)`, `Default`, `Clone` |
| `Feature { name, contents }` | A feature held in memory; `name` stands in for its path in reports |
| `Output` | `Arc<Mutex<dyn Write + Send>>`: the caller keeps its own handle on what it passes in (the same type the runner takes for `stderr`) |
| `run(&CancelToken, &Options)` | `-> (i32, Option<apic_runner::Error>)`. The error is set for exit 2 (no features, a path outside the project, a bad phrase, tag expression or feature file, unknown environment or request, missing variable) and exit 3 (transport); assertion failures and undefined steps are exit 1 with no error |
| `run_summary(&CancelToken, &Options)` | `-> (Option<Summary>, Vec<u8>, i32, Option<Error>)`: runs with the cucumber report in memory. The summary of what ran comes back alongside a step's usage or transport error |
| `summarize(&[u8])` | `-> Result<Summary, String>` from a cucumber JSON report |
| `Summary { ok, scenarios, passed, failed, skipped, undefined, failures }`, `Failure { feature, scenario, step, status, error }` | `Serialize`, keys in that order; `failures` and `error` left out when empty |
| `feature_files(&Options)` | `-> Result<Vec<String>, String>`: every `.feature` file the run would read, after the same root checks |
| `phrases(&Project)` | `-> BTreeMap<String, Arc<Request>>`: each `# @step` phrase and the request it runs |
| `VOCABULARY` | `&[VocabularyEntry { pattern, purpose }]`: the built-in steps, for `apic test --steps` and the docs |
| `FORMATS` | `["pretty", "progress", "cucumber", "junit"]` |
| `EXIT_PASSED`, `EXIT_FAILED`, `EXIT_USAGE`, `EXIT_TRANSPORT` | 0, 1, 2, 3 |

## How a run goes

1. The format, the paths (inside the project root, symlinks resolved),
   the environment, every `# @step` phrase, the tag expression and the
   syntax of every selected feature are checked first: any problem is a
   usage error before a single request is sent.
2. Each scenario gets a runner of its own with an in-memory session and
   cookie jar (`use_session` shares `.apic/` instead). Steps are matched
   against the built-in vocabulary (`apic_phrase::builtin()`, bound to
   their functions by name in `src/steps.rs`) and then the project's
   phrases. No match is `undefined`, several are `ambiguous`; both fail the
   run. After a failure the rest of the scenario is `skipped`.
3. `stop_on_failure` skips the scenarios after the first failed one, so
   the report still lists them.
4. Under `redact` the whole report is held back and masked once the run
   ends: text reports by substitution, the cucumber and JUnit reports
   value by value so their structure cannot change.

## Layout

- `src/gherkin/`: the feature-file reader. `parser.rs` is a line matcher
  driving the state table in `table.rs`; `dialects.rs` has the keywords of
  every language a `# language:` header may name; `pickles.rs` expands
  outlines and prepends backgrounds. Both tables are generated data.
- `src/engine.rs`: the scenario runner and the store of results the
  formatters read.
- `src/fmt/`: `pretty`, `progress`, `cucumber` (JSON) and `junit` (XML),
  and the summary with its snippets for undefined steps.
- `src/state.rs`, `src/steps.rs`, `src/phrases.rs`, `src/jsonmatch.rs`,
  `src/mask.rs`, `src/suite.rs`, `src/summary.rs`: the run's shared state
  and masking, the step functions, the phrases, JSON comparison, report
  masking, the entry points and the summary.

## Report details worth knowing

- Ids are numbered across the files of a run in the order the reader
  completes nodes, and the formatters list step results in the *text*
  order of those ids (`10` before `9`), which shapes the JUnit report and
  when an outline's header is printed. `src/gherkin/tests.rs` pins the
  numbering.
- The label after `#` on a pretty step line, and `match.location` in the
  cucumber report, are fixed strings per step function
  (`steps.go:75 -> …stepStatus`, `phrases.go:69 -> …handlerFor.func1`):
  report text that tools already compare, so it stays as it is.
- `crates/demoapi/testdata/extra.*` holds one feature and its four
  reports as recorded; `crates/demoapi` replays them.

## What differs from before

- A step function given the wrong kind of argument (a doc string where it
  reads a table, or the other way round) fails with `runtime error:
  invalid memory address or nil pointer dereference` and no stack trace
  after it.
- A line of a feature file may be any length.
- Snippets for more than twelve undefined steps whose function names
  collide are listed in a stable order.
- A cucumber report that cannot be parsed back is reported in the JSON
  reader's words.
