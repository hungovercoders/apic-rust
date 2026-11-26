# Working in this repository

apic is a Rust CLI that runs `.http` request files for humans and AI agents.
This file carries what most tasks need; the routes at the end say when to
open anything else. `CLAUDE.md` is `@AGENTS.md`.

## Layout

The tree is one Cargo workspace. `crates/apic` is the entry point;
`crates/httpfile` parses and formats `.http` files; `crates/project`
discovers and validates a project; `crates/runner` resolves variables,
applies auth, sends, captures and asserts, with `env`, `template`,
`session`, `auth`, `selector` and `assert` beside it; `crates/httpc` is the
HTTP client and `crates/httpd` the small server behind `apic demo`,
`apic mock` and the tests; `crates/output` renders for the CLI, the UI and
MCP; `crates/lsp` is `apic lsp`, the language server; `crates/bdd` and
`crates/phrase` are `apic test`; `crates/cli` holds the commands (one
file per command) and `crates/ui` the terminal UI; `crates/demoapi` is
the offline API and project behind `apic demo`; `examples/` are the static
sample projects CI validates and format-checks; `editors/vscode/` is the
extension, released on its own `vscode-v*` tags; `setup-apic/` the GitHub
Action; `skills/` the pack of Agent Skills users install into their own
projects, one directory each (`apic` on using apic, with a hand-written
`SKILL.md` and a generated `references/cheatsheet.md`; `api-design` and
`api-security` on building and securing the API, hand-written with their
references), embedded by `crates/skills` for `apic skill` and `apic init`;
`website/` is [apic.sh](https://apic.sh/), the user docs, with the course
under `src/content/docs/learn/`; `docs/` is reference for working on apic
rather than using it; and the generators are under `scripts/`, each a
crate of its own.

Two things worth knowing: the OpenAPI and Postman readers are hand-written
walkers, not libraries (the OpenAPI one also serves `apic mock`, from
`crates/openapi/src/mock.rs`), and the VS Code extension never parses `.http`
files itself (it reads `--json`).

## Commands

- `task build` / `cargo build --release -p apic` (the task copies the binary to `bin/apic`)
- `task test` / `cargo test --workspace`: tests use local `apic_httpd` servers, no network needed
- `task lint`: `cargo fmt --check` and clippy with warnings denied; `task fmt` actually formats the tree (`lint` only checks)
- `task cover`, `task vuln`: `lcov.info` with a per-file summary (cargo-llvm-cov), cargo-audit
- `task validate-examples`: the example projects, as CI checks them
- `task check`: what CI runs — lint, test, licences and validate-examples
- `task ss:hygiene:test`: the slopstopper hygiene suite, which the pre-push hook runs once `git config core.hooksPath .githooks` is set (CONTRIBUTING's setup does); `task --list` shows every `ss:*` check
- Generated files each have a task: `task docs:cli` (the site's `cli.md`), `task docs:errors` (its `errors.md`, from the runner's catalogue), `task skill` (`skills/apic/references/cheatsheet.md`), `task docs:changelog` (the site's `changelog.md` and the docs' version pins, from `CHANGELOG.md` and the published release) and `task schemas` have a test that fails when the output is stale; `task man`, `task shots`, `task tapes`, `task episodes` and `task notices` write what is not committed

## Conventions

- Keep the `.http` dialect compatible with VS Code REST Client and JetBrains: new features go in `# @directive` comments before the request line, never new syntax in the request itself. Document any addition in the site's `format.md`.
- The `--json` output shape and exit codes are a public contract; change them only with a note on the site's `agents.md` page.
- A change a user would notice gets a line under Unreleased in `CHANGELOG.md`: the release's notes are that section. A line under Unreleased, or dating a release, needs nothing else (the site moves after the release); editing the intro or a published section needs `task docs:changelog`.
- Every command must work non-interactively (no prompts) and respect `--json`. `apic ui` is the single, deliberate exception: it is interactive, has no `--json`, and exits 2 when stdout is not a terminal. `apic mcp` and `apic lsp` are servers that speak their own JSON protocol on stdout, so `--json` has nothing to change for them.
- A new error gets a code from the catalogue in `crates/runner/src/errors.rs`: a usage error built with its code, never a bare message; a new code needs an entry in the catalogue and a case in the `crates/cli` errors test, which runs a real command for every code.
- New commands need a section in the site's `cli.md` and a line in its `cheatsheet.md`; a new or changed flag needs `task docs:cli`.
- The UI is tested through `update` and `view`, and driven headlessly by `press`/`resize` in `crates/ui/src/headless.rs`; the screenshot generator uses the same entry points, so a screenshot cannot drift from what the UI draws.
- The UI has its own terminal layer rather than a TUI framework, and its styles are apic's own small ANSI module. Keep anything that queries the terminal at start-up (for its background colour, say) out of the binary: every apic command would pay for the wait on terminals that do not answer.
- Keep the dependency list small: prefer a few hundred lines of code over a large SDK (the AWS signer and the OpenAPI reader are the precedents), written from the spec rather than copied from another project. Permissive licences only (MIT, BSD, Apache-2.0, ISC, Unicode-3.0; `deny.toml` is the list and `task licences` enforces it). Check the stripped binary size with `task build && ls -la bin/apic` when adding a dependency.
- Add a test next to any parser or runner change; parser cases go in `crates/httpfile/testdata/sample.http`.
- A new step in the vocabulary needs: its regex and shapes in `crates/phrase/src/builtin.rs`, a handler bound by name in `crates/bdd/src/steps.rs`, a row in the `bdd` crate's vocabulary, a scenario in its tests, and the table in the site's `testing.md`.
- Prose is British English: Vale fails on `color` or `behavior` outside code spans, and codespell runs over the whole tree in CI. Keep `LICENSE` at the root: CI's `licences` job fails without it.

## Routes: read before you act

| When you are… | Do this |
| --- | --- |
| changing the runner, adding a directive, a step or a command, or touching `--json` or an exit code | Read [docs/architecture.md](docs/architecture.md): the crate map, the request lifecycle and the three contracts every change is checked against |
| editing the site, the course, a lesson, the skill, a generated page or any prose check | Read [docs/docs-site.md](docs/docs-site.md): the Astro layout, the link contract the binary depends on, the generators and Vale, codespell and lychee |
| changing a workflow, a linter setting, adding an `#[allow]`, or fixing a failing slopstopper check | Read [docs/ci.md](docs/ci.md): what CI runs, why each linter is on, and the quality suite's config |
| adding a dependency, or touching a licence or notices file | Read [docs/licensing.md](docs/licensing.md): the permissive-only rule and the notices generator |
| cutting a release, or changing the installers, the packages, the image, the cask or the extension's release | Read [docs/releasing.md](docs/releasing.md): signing, SBOMs, the archive names three installers depend on, and the moving `v0` tag |
| writing a lesson for the course | Read [docs/learn/_template.md](docs/learn/_template.md): the template every lesson is copied from and the rules it follows |
| cutting a lesson's YouTube episode | Read [docs/learn/_episode-template.md](docs/learn/_episode-template.md): the title, thumbnail and description conventions |

For any task not covered above, read [docs/README.md](docs/README.md) for the routing table for every doc in this repo.
