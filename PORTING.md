# Porting apic from Go to Rust: the rules every porter follows

The Go original is at `/home/user/apic` (read-only reference; never edit it).
The Rust port is this repository, `/home/user/apic-rust`. A built Go binary
is at `/home/user/apic/bin/apic` for differential checks (run both, compare).

The goal is a **faithful port**: same behaviour, same output text, same
`--json` shapes and key order, same exit codes, same error codes and
messages, same file formats on disk (`.apic/session.json` and friends).
Users, the docs site, the VS Code extension and the skills must not be able
to tell the difference. When in doubt, do what the Go code does.

## Layout

- Go `internal/<pkg>` becomes the crate `crates/<pkg>` (package `apic-<pkg>`,
  lib `apic_<pkg>`). One Go file becomes one Rust module file with the same
  base name where sensible (`parse.go` -> `src/parse.rs`), re-exported from
  `lib.rs` so callers write `apic_httpfile::Request`.
- Testdata was already copied: Go `internal/<pkg>/testdata` is at
  `crates/<pkg>/testdata`. Use `concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/...")`.
- Skeleton crates exist for every package with an empty `[dependencies]`.
  Add path deps as `apic-foo = { path = "../foo" }` and third-party deps as
  `name.workspace = true`.
- Third-party versions live only in the root `Cargo.toml`
  `[workspace.dependencies]`. If you need one that is missing, add one line
  there (re-read the file immediately before editing: other agents edit it
  too; never rewrite the whole file). Keep dependencies few and small,
  permissive licences only (MIT/BSD/Apache-2.0/ISC/Unicode). Prefer a few
  hundred lines of your own code over a big crate. No `anyhow`, no
  `thiserror`, no `reqwest`, no `once_cell`/`lazy_static` (use
  `std::sync::OnceLock`/`LazyLock`), no `chrono` (ask for `time` if you
  really need calendar formatting, or hand-write it).
- Only touch the crates you were assigned (plus the root `Cargo.toml` deps
  line). If an upstream crate has a bug or lacks an API you need, make the
  smallest fix there and mention it in your final report.

## Building while others work

Several agents build in this workspace at once and share `target/`. Cargo
serialises on a lock, so a command may wait; that is fine. Always build and
test only your crates: `cargo test -p apic-<pkg>`, `cargo clippy -p apic-<pkg>
--all-targets`. Never run workspace-wide commands (`cargo build` with no
`-p`, `cargo fmt --all`): other crates may be mid-edit. Format with
`rustfmt --edition 2021 crates/<pkg>/src/*.rs` (and tests).

## Conventions

- **Errors.** Library crates define their own error types (plain structs or
  enums with a hand-written `Display` and `std::error::Error`). Keep the Go
  message text exactly. Where Go code inspects error types (`errors.As`),
  keep that distinction as enum variants. Functions that return Go `error`
  return `Result<T, YourError>`; `String` is acceptable as the error for
  small leaf helpers whose callers only print it.
- **Naming.** Go `FooBar` -> Rust `foo_bar` for functions/fields, `FooBar`
  for types. Keep names recognisable so docs that mention e.g.
  `runner.Catalogue` or `KnownDirectives` still make sense
  (`apic_runner::CATALOGUE` / `catalogue()`, `KNOWN_DIRECTIVES`).
- **Pointers and sharing.** Go passes `*Request`, `*File`, `*Project`
  freely and compares pointers. Use `Arc<T>` (shared, immutable after
  parse) where identity or sharing matters, and document the choice in the
  crate's top doc comment so downstream crates follow it. Avoid lifetimes
  in public types.
- **JSON.** `serde` + `serde_json`. Go struct -> `#[derive(Serialize)]`
  struct with fields in the *same order* and `#[serde(rename = "...")]` to
  the Go tag; `omitempty` -> `skip_serializing_if`. Go encodes maps with
  sorted keys: `serde_json::Map` is a `BTreeMap` here (do NOT enable
  `preserve_order`), and use `BTreeMap` for map fields. Go's
  `json.Marshal` escapes `<`, `>`, `&` as `<` etc. unless the encoder
  called `SetEscapeHTML(false)`: check each call site and match it (there is
  `apic_gocompat` for shared helpers; add an `escape_html` helper there in a
  new module file if you are the first to need it). `MarshalIndent` uses
  the indent given (usually two spaces).
- **Text formats.** Use `apic_gocompat` for Go-specific formats:
  `format_duration`/`parse_duration` (Go `time.Duration`), `quote` (`%q`),
  `format_float_g` (`%v` of float64), `format_float_json`. Add further
  helpers there, each in its own new module file, re-exported from lib.rs.
- **Regex.** Go `regexp` -> the `regex` crate (same RE2 syntax). Compile
  once in a `LazyLock`.
- **Maps.** Where Go iterates a map and sorts, or output depends on order,
  be deliberate: `BTreeMap` for sorted, `Vec` for insertion order.
- **HTTP.** The client is `apic_httpc` and the server/test-server is
  `apic_httpd` (both hyper-based, written for this port). Go
  `net/http/httptest` servers become `apic_httpd` test servers. No other
  HTTP crates.
- **Time/clock, randomness, env vars**: keep the same injection points the
  Go code has for tests (e.g. a `now` function field).
- **Comments and prose** are British English (colour, behaviour). Port the
  Go doc comments: they explain the why. Do not mention Go, the port, or
  "ported from" in code comments; write the code as if it had always been
  Rust.
- **Unsafe**: none, except where the terminal layer needs libc.
- `#![forbid(unsafe_code)]` where possible; code must be clippy-clean
  (`cargo clippy -p <crate> --all-targets -- -D warnings`) and rustfmt-clean.

## Tests

Port the Go tests (`*_test.go`) for your packages: they are the proof the
port behaves the same. Same cases, same expected values, snake_case names
(`TestParseSample` -> `parse_sample`). Unit tests of private functions go in
`#[cfg(test)] mod tests` in the module (or `src/<module>_tests.rs` included
with `#[cfg(test)] mod`); tests of the public API may go in `tests/`. If a
Go test cannot be ported meaningfully (it tests a Go-only mechanism), skip
it and say so in your report. All your tests must pass before you finish.

## When you finish

Reply with a short report: what you ported, the public API surface in a few
lines (the main types and functions a downstream crate will call), any
deviation from the Go behaviour, anything skipped, and any change you made
outside your crates. Do not commit: the lead commits.
