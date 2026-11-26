# CI and linting

What the workflows run, why each linter is on, and how the slopstopper
quality suite fits beside them. `task check` is the local equivalent of
`ci.yml`; `task ss:hygiene:test` is what the pre-push hook runs.

## The Rust linters

- `task lint` is `cargo fmt --all --check` and `cargo clippy --workspace
  --all-targets -- -D warnings`: rustfmt's defaults and clippy's default
  set, with every warning an error. Neither tool has a configuration
  file, and no lint is switched off for the workspace, so the
  config is quiet: a new finding means something. `rust-toolchain.toml` pins
  the compiler, so a new clippy release cannot turn CI red on its own; the
  findings arrive with the pull request that bumps the pin.
- An `#[allow(clippy::…)]` goes on the item it excuses, never on a crate,
  with a comment beside it saying why the code is right as written. The
  existing ones are the place to look before adding another.
- Crates carry `#![forbid(unsafe_code)]` wherever they can. `unsafe` is
  confined to four files, each of which allows it for itself: the terminal
  layer's `crates/ui/src/sys/unix.rs` and `windows.rs`, the signal handler
  in `crates/cli/src/signal.rs`, and `crates/openapi/src/yaml.rs`, which
  drives the `unsafe-libyaml` parser.
- Exit codes are a public contract: the runner's `exit_code` looks through
  whatever wraps an error for the transport failure inside it, so putting
  words in front of a transport error cannot silently turn a documented 3
  into a 2. `exit_code_unwraps` is the test for exactly that.

## The workflows

- `ci.yml` runs the tests on Linux, macOS and Windows (`cargo test
  --workspace`, about 800 tests), the lint job (rustfmt, clippy and
  `codespell` over the whole tree), cargo-audit, the licences check (the
  notices generator and `cargo deny check licenses` against `deny.toml`)
  and the example projects. A coverage job uploads `lcov.info`, from
  cargo-llvm-cov, as an artifact. The `learn` job runs the course's marked
  blocks, `tapes` validates the VHS tapes and the episodes' shot lists (vhs
  is installed with `go install`, the one step that needs Go), `vscode`
  builds and tests the extension, `shots` regenerates the screenshots, and
  `image` and `packages` build the container image and the release
  packages without publishing. Every job installs the
  toolchain `rust-toolchain.toml` pins.
- `docs.yml` runs codespell, Vale (errors only: British spelling, project
  terms, repeated words) and, on pull requests, lychee for external links,
  then builds the site to prove it; see `docs-site.md`.
- CodeQL runs weekly and per pull request. Dependabot watches `cargo`,
  `npm` (the root and `website/`) and `github-actions` weekly.
- Workflows declare `permissions: contents: read` at the top and widen only
  where a job needs it (`docs.yml` for Pages, `codeql.yml` for
  security-events, `release.yml` for the release upload, the tag it
  pushes when a release pull request merges and the `actions: write` to
  dispatch the release on it, and the docs commit its
  `docs` job pushes to `main` after a release, and, in its `mcp-registry`
  job, `id-token: write` to publish the MCP registry listing; see
  `releasing.md`).

## The slopstopper quality suite

The `ss-*.yml` workflows, the `task ss:*` shims in `Taskfile.ss.yml`, the
composite actions under `.github/actions/ss-*/` and the two
`.claude/skills/slopstopper-*` playbooks come from
[slopstopper](https://github.com/hungovercoders/slopstopper) and are
refreshed wholesale by its installer, so customise `.slopstopper.yml`, never
those files. `mise.toml` pins the CLI, `task` and Node for CI and local runs
alike; `.ss/.workflows-installed` records which workflows the installer put
in place, so a deliberately deleted one does not come back.

- `.slopstopper.yml` points the browser checks at apic.sh and sets the
  hygiene knobs. The site builds from `website/`, so the root
  `package.json` has a `build` script that installs and builds it, and
  `mise.toml` exports `SERVE_ROOT=website/dist` and `SS_SERVER_HEADERS` for
  the bundled static server the browser checks and DAST run against on pull
  requests, so it sends the same headers as Cloudflare.
- The URL-driven checks also run on every `deployment_status` event, and the
  only deployments this repo raises are `docs.yml`'s GitHub Pages deploys of
  the legacy redirect stubs. That job declares no environment `url`, so the
  status carries only the Actions log link and `ss-resolve-url` audits
  `urls.production` instead; with a `url` the checks audited
  `hungovercoders.github.io/apic/` and failed on every docs deploy.
- `.github/dependency-review-config.yml` is the licence allowlist that
  both the dependency review on new dependencies and `task
  ss:security:licences` (every resolved npm dependency that ships; dev
  dependencies are not judged) read; the crates in the binary are judged
  by `deny.toml` instead (see `licensing.md`); the installer seeds it and never
  overwrites it, so it is the place to accept a licence the default list
  leaves out, as the sharp entries do.
- The entry-file budgets are the reason `AGENTS.md` routes to this directory
  instead of carrying everything: `AGENTS.md` stays under about 2,000
  tokens, `README.md` under 600 (badges excluded) and every doc under `docs/`
  has a trigger-first route from `AGENTS.md` or the map in `docs/README.md`.
- The complexity check is lizard, with the CCN ceiling in `.slopstopper.yml`
  at 30. lizard counts every `match` arm, so a
  match-driven parser reads high: split it into a handler table or one
  function per section rather than raising the ceiling, which only ever
  comes down.
- Before diagnosing a red check, read
  `.claude/skills/slopstopper-triage/SKILL.md`, which maps each workflow to
  the local task, its report under `.ss/reports/` and where the fix lives.
