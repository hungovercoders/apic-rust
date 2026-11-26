# apic-openapi

Reads an OpenAPI 3 document (YAML or JSON) and does two things with it:
scaffolds `.http` files for `apic import`, and serves the document back as
a stateless mock for `apic mock`. This page is the map of the public API;
the doc comments in `src/` carry the detail.

## API

```rust
// --- apic import ----------------------------------------------------
pub fn import(spec_path: &str, opts: Options) -> Result<ImportResult, Error>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub out_dir: String,    // directory to write into ("" is ".")
    pub env_name: String,   // environment of http-client.env.json ("" is "dev")
    pub force: bool,        // overwrite existing files
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ImportResult {
    pub files: Vec<String>,     // "files": null while empty
    pub requests: usize,        // "requests"
    pub base_url: String,       // "base_url"
    pub env_file: String,       // "env_file", left out when empty
    pub skipped: Vec<String>,   // "skipped", left out when empty
}

// --- apic mock ------------------------------------------------------
pub struct Mock {               // Send + Sync, immutable once built
    pub routes: Vec<Route>,
    pub skipped: Vec<String>,   // "METHOD /path: reason" for what could not be registered
}
impl Mock {
    pub fn new(data: &[u8]) -> Result<Mock, Error>;
    pub fn serve(&self, r: &apic_httpd::Request) -> apic_httpd::Response;
    pub fn into_handler(self) -> apic_httpd::Handler;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Route {
    pub method: String,         // "method"
    pub path: String,           // "path", as the document spells it
    pub status: u16,            // "status", served when no Prefer header asks otherwise
    pub name: String,           // "name" (the operationId), left out when empty
}

// --- errors ---------------------------------------------------------
pub struct Error;               // Display is the message; message() -> &str
```

`ImportResult` is the summary `apic import --json`
prints; its JSON is part of the output contract, field order included.
The paths in it are `out_dir` joined with the file name the way
`apic_project::path::join` joins (`./out/../x` is cleaned to `x`).

`import` reports a file it cannot read as `open PATH: reason`, a
directory it cannot make as `mkdir PATH: reason`, and anything wrong with
the document as `PATH: message`. `Mock::new` reports the message alone:
the caller adds the path. YAML syntax errors read
`yaml: line 3: did not find expected key`.

To keep a `Mock` and serve it, share it: `let m = Arc::new(mock)` and
hand the server `move |r: &Request| m.serve(r)`. A report of the routes
as JSON wants `null`, not `[]`, when `routes` is empty (a document whose
every operation was skipped).

## What `import` writes

- One `<tag>.http` per tag (the first tag of each operation, `api`
  without one), tags sorted, the name in kebab case and numbered when two
  tags collide. Each operation is a named request: `### summary`,
  `# @name`, `# @description`, `# @assert status == 201` from the first
  2xx response, parameters as `{{variables}}` (optional ones commented
  out), `Content-Type`, `Accept`, and an example body.
- The body comes from the first media type that can give one: its
  `example`, its first named example, or a value derived from the schema
  (`example`, `examples`, `default`, `enum`, then by type; `allOf` merged,
  the first of `oneOf`/`anyOf`). Generated UUIDs and timestamps are
  `{{$uuid}}` and `{{$isoTimestamp}}`.
- `<file>.<request>.body.json|txt` for a body that must be sent verbatim
  (it holds a literal `{{`, or text the parser would misread), and
  `<file>.<request>.<part>.bin` placeholders for the file parts of a
  multipart body.
- `http-client.env.json` with `baseUrl` from the first server.

An existing file is kept and listed under `skipped` unless `force`. A
cyclic, external or unresolvable `$ref` is an error, and nothing is
written. Text from the document never adds a line, a directive or a
template to what is generated.

## What the mock serves

Every operation under `paths`, at its path and, when the first server URL
has a path such as `/v1`, under that prefix too. The response is the first
2xx (else `default`, else the first declared): its headers, and a body
from the media type's example or schema, placeholders fixed. `Prefer:
code=404` picks another documented response, `Prefer: example=name` a
named example, `Accept` a media type. A path the document does not
declare is a 404 and a known path under another method a 405 with
`Allow`, both with a JSON `{"error": ...}` body; a choice the document
does not offer is a 406.

## How the document is read

`src/yaml.rs` builds a tree of nodes that keeps key order, each scalar's
text and style, tags, lines and aliases; `src/model.rs` walks it,
following aliases and local `$ref` pointers (`#/a/b`, `#`, with `~0`,
`~1` and percent-escapes), with keys next to a `$ref` overriding its
target in OpenAPI 3.1 and ignored in 3.0. Numbers written as JSON numbers
keep their spelling; dates stay text.

The parser is libyaml's, through the `unsafe-libyaml` crate that the
reader of `apic.yaml` already links. Its API is C's, so
`yaml::events` (about a hundred lines) is the one place in the crate with
`unsafe`; the crate is `#![deny(unsafe_code)]` otherwise. The reader of
`apic.yaml` in `apic_project` does not fit here: it expands aliases (a
recursive anchor is an error to it) and knows a scalar's style only by
looking at the source text. Plain scalars are resolved by
`apic_project::resolve_yaml_scalar`, so both readers agree on what `0x1F`
or `2025-01-01` is.

## Tests

`tests/import.rs` is the importer end to end, every case a document and
what must come out of it; `src/mock_tests.rs` runs the mock behind a real
server; the unit tests in `src/` cover the YAML tree, scalar decoding,
the JSON writer and the naming helpers.
