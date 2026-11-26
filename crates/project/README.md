# apic-project

Discovers the `.http` and `.rest` files of a project, reads its
`apic.yaml`, indexes the requests by name, resolves command-line targets
and runs the project-level checks behind `apic validate`. This page is the
map of the public API for crates that build on it; the doc comments in
`src/` carry the detail.

## Sharing, paths and errors

- `load` and `load_overlay` return an `Arc<Project>`. A project is
  immutable once loaded: keep it by cloning the `Arc`. `Project` is also
  `Clone` and `Default`, and `Project::new` builds one from parsed files
  (a test that needs a project without a directory uses it).
- Files and requests are `apic_httpfile`'s `Arc<File>` and `Arc<Request>`.
  `lookup`, `resolve`, `requests` and `captured_by` return clones of those
  `Arc`s, so a request found twice is the same allocation: compare with
  `Arc::ptr_eq`, key maps with `apic_httpfile::RequestKey`.
- Paths are text. `Project.root` is an absolute `String`; a file's `path`
  is relative to the root with forward slashes on every platform, and is
  what diagnostics and request ids carry. Functions that take a path the
  caller holds (`load`, `load_overlay`, `discover`) take `impl
  AsRef<Path>`; functions that compare paths (`within`, `confine`,
  `real_prefix`, `Project::file`) take `&str`.
- `Error` is `NotFound(NotFoundError)` or `Message(String)`; `Display` is
  the message to show. `ConfineError` is separate: `OutsideRoot` or
  `Io { path, source }`.

## Loading

| Item | Signature |
| --- | --- |
| `load` | `fn(root: impl AsRef<Path>) -> Result<Arc<Project>, Error>` |
| `load_overlay` | `fn(root: impl AsRef<Path>, overlay: &HashMap<String, String>) -> Result<Arc<Project>, Error>`: `overlay` maps an absolute path to the text to parse in its place (an unsaved editor buffer); a request file in it that is not on disk yet is loaded too |
| `discover` | `fn(dir: impl AsRef<Path>) -> Result<Vec<String>, Error>`: every `*.http` and `*.rest` under `dir`, sorted, skipping hidden directories, `node_modules`, `vendor`, `testdata` and `.apic`; symlinks are not followed |
| `CONFIG_FILE` | `&str`: `"apic.yaml"` |

`load` makes the root absolute, reads `apic.yaml` if there is one, scans
`config.dir` (or the root), and parses each file under its path from the
root. A bad `apic.yaml` fails the load with `apic.yaml: yaml: ...`.

## Project

```text
Project {
    root: String,                  // absolute
    config: Config,
    files: Vec<Arc<File>>,         // sorted by path
    diagnostics: Vec<Diagnostic>,  // what the parser reported
}
```

| Method | Returns |
| --- | --- |
| `Project::new(root, config, files, diagnostics)` | `Project`, indexed as `load` indexes it |
| `requests()` | `Vec<Arc<Request>>`: every request in file order |
| `lookup(name)` | `Result<Arc<Request>, Error>`: by `# @name`; an ambiguous name is an error |
| `resolve(target)` | `Result<Vec<Arc<Request>>, Error>`: `name`, `file.http` (every request), `file.http#name`, `file.http#N` |
| `file(path)` | `Option<&Arc<File>>`: by path from the root, or absolute |
| `captured_by(name)` | `Option<Arc<Request>>`: the first request capturing that variable |
| `validate()` | `Vec<Diagnostic>`: the parse diagnostics plus the project checks, sorted by path, line and column |

`NotFoundError { msg, closest }` says a target names no request or file;
`closest` (also `suggestions()`) holds the nearest known names, best first.
`Error::suggestions()` gives the same slice, empty for other errors, and
`Error::not_found()` the `NotFoundError` when that is what it is. This is
what the runner's usage errors read to fill `suggestions`.

## Configuration

```text
Config     { env, dir, timeout, retry: String, cookies: bool, history: i64,
             proxy: String, no_proxy: Vec<String>, max_body_bytes: i64,
             auth: AuthConfig, test: TestConfig, tls: TlsConfig }
AuthConfig { default: String, allow_exec: bool }
TestConfig { paths: Vec<String> }
TlsConfig  { ca_file, cert_file, key_file: String, verify_host: Option<bool>,
             hosts: BTreeMap<String, TlsHost> }
TlsHost    { ca_file, cert_file, key_file: String, verify_host: Option<bool> }
```

All derive `Debug`, `Clone`, `Default`, `PartialEq` and `Eq`. A list or
mapping that the file does not set is empty (there is no separate "unset").

| Item | What it is |
| --- | --- |
| `Config::from_yaml(data: &[u8]) -> Result<Config, String>` | Decodes the content of an `apic.yaml`; the error is the message (`yaml: ...`) |
| `CONFIG_FIELDS` | `&[ConfigField]`: every key of `apic.yaml` in declaration order, sections nested. The schema generator walks it |
| `ConfigField { key, name, kind }` | The key in the file (`maxBodyBytes`), the struct field (`max_body_bytes`), and its `FieldKind` |
| `FieldKind` | `String`, `Bool`, `Int`, `Slice(&FieldKind)`, `Optional(&FieldKind)`, `Map(&FieldKind)` (any name to the inner kind), `Struct(&[ConfigField])` |
| `Config::FIELDS`, `AuthConfig::FIELDS`, ... | The same table, section by section |

The structs, the table and the decoder are generated from one list of
fields by the `config_struct!` macro in `src/config.rs`: add a key there
and all three have it. `tests/project.rs` checks the table against the
structs all the same.

### How `apic.yaml` is read

The first YAML document is decoded; an empty file is an empty
configuration.

- A key apic does not know is ignored. A key written twice is an error.
- A null (`key:`, `~`, `null`) leaves the key unset.
- A string key takes any scalar as written: `timeout: 30` is `"30"`,
  `env: true` is `"true"`.
- A boolean key takes `true`/`false` in the three usual capitalisations,
  and also `y`, `yes`, `on`, `n`, `no`, `off` (quoted or not).
- A number key takes decimal, `0x`, `0o`, `0b`, `0777` (octal) and `1_000`;
  a number with a fraction is cut to its whole part.
- A null item of a list is left out.
- `<<` merges and anchors work.
- A value of the wrong type is an error, and every such value is reported
  at once, with its line:

  ```text
  yaml: unmarshal errors:
    line 1: cannot unmarshal !!str `soon` into int
    line 2: cannot unmarshal !!int `3` into bool
  ```

- A syntax error reads `yaml: line 2: did not find expected node content`.

`testdata/yaml_reference.json` holds 282 files with the configuration or
the error each has always produced; `tests/yaml_reference.rs` replays
them. The parsing is `serde_yaml_ng`'s; `src/yaml.rs` turns its values back
into a tree of nodes with their text, tags and lines, and `src/config.rs`
decodes that.

Known differences from the recorded behaviour, all in files nobody is
likely to write: a scalar under an explicit core tag that it does not fit
(`cookies: !!bool yes`) and an anchor that contains itself are reported in
the parser's words; a file in UTF-16 is decoded, but its quoted numbers may
be taken for numbers.

`resolve_yaml_scalar(text) -> YamlScalar` is the resolution of one plain
scalar by those rules (`Null`, `Bool`, `Int`, `Uint`, `Float`,
`Timestamp`, `Merge` or `Str`, with `tag()` for the `!!int` form), public
for the OpenAPI reader, which walks YAML nodes of its own.

## Confinement

| Item | Signature |
| --- | --- |
| `within` | `fn(root: &str, path: &str) -> Option<String>`: the path relative to `root` when it lies under it, lexically |
| `confine` | `fn(root: &str, dir: &str, rel: &str) -> Result<String, ConfineError>`: `rel` against `dir`, symlinks followed, refused when the real path leaves `root` |
| `real_prefix` | `fn(abs: &str) -> String`: the symlinks of the longest existing ancestor resolved, the rest kept |
| `ConfineError` | `OutsideRoot` (`is_outside_root()`), `Io { path, source }` |
| `ERR_OUTSIDE_ROOT` | `&str`: `"resolves outside project root"`, the `Display` of `OutsideRoot` |

## `path`

`apic_project::path` holds the operations on paths as text that the crate
uses, public so callers resolve paths the same way: `clean`, `join`
(`join("/a", "/b")` is `/a/b`: an absolute second element does not
replace the first), `join_all`, `abs`, `dir`, `base`, `ext`, `rel`,
`to_slash`, `is_abs` and `eval_symlinks`. On Windows they treat both
separators alike but know nothing of drive letters.

## Tests

`tests/project.rs` covers loading, the overlay, discovery, target
resolution and every validate check with its code and span;
`tests/yaml_reference.rs` covers `apic.yaml`. The unit tests in `src/`
cover the path helpers, confinement and the YAML tree.
