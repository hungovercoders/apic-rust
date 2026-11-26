# apic-httpfile

Parses and formats `.http` request files: the common subset of the VS Code
REST Client and JetBrains HTTP Client dialects, plus apic's `# @directive`
comments. This page is the map of the public API for crates that build on
it; the doc comments in `src/` carry the detail.

## Sharing and identity

The AST is immutable once parsed and is shared behind `Arc`:

- `parse` and `parse_file` return an `Arc<File>`.
- `File.requests` is a `Vec<Arc<Request>>`. Keep a request by cloning the
  `Arc`, never the `Request`.
- A request reaches its file through `request.file`, an `Arc<FileInfo>`
  (the path and the file variables). `File` dereferences to the same
  `FileInfo`, so `file.path`, `file.vars`, `request.file.path` and
  `request.file.vars` all work, and there is no reference cycle.
- Two requests are the same request when they are the same allocation.
  Compare with `Arc::ptr_eq`; key maps and sets with `RequestKey`
  (`HashMap<RequestKey, T>`, `HashSet<RequestKey>`), which hashes and
  compares by that identity. `Request` has no `PartialEq` on purpose.

Lines and columns are 1-based `usize`; a column counts bytes from the start
of the line and `0` means "none". The small helpers return their error as a
`String` holding the message to report.

## Parsing

| Item | Signature |
| --- | --- |
| `parse` | `fn(path: &str, content: &str) -> (Arc<File>, Vec<Diagnostic>)` |
| `parse_file` | `fn(path: impl AsRef<Path>) -> io::Result<(Arc<File>, Vec<Diagnostic>)>` |
| `span` | `fn(line: &str, sub: &str, after: usize) -> (usize, usize)`: the `[col, end)` columns of `sub`, zeros when absent |
| `valid_capture_name` | `fn(name: &str) -> bool` |
| `CODES` | `&[(&str, &str)]`: every diagnostic code with its description; `code_description(code) -> Option<&'static str>` |
| `KNOWN_DIRECTIVES` | `&[(&str, &str)]`: every directive with what it is for; `known_directive(key) -> Option<&'static str>`, `is_known_directive(key) -> bool` |

## Types

```text
File        { info: Arc<FileInfo>, requests: Vec<Arc<Request>> }   (derefs to FileInfo)
FileInfo    { path: String, vars: Vec<Var> }
Var         { name, value: String, line: usize }
Directive   { key, value: String, line, column: usize }
Capture     { name, selector: String, line, column: usize }
Assert      { expr: String, line, column: usize }
Header      { name, value: String }
SaveTo      { path: String, overwrite: bool, line, column: usize }
Ref         { id: String, force: bool, line, column: usize }
FileRef     { path: String, templated: bool, line, column: usize }
Multipart   { boundary: String, parts: Vec<Part> }                  .files() -> usize
Part        { headers: Vec<Header>, name, filename, content_type, body, file: String,
              file_templated: bool, file_line, file_column, line: usize }
Diagnostic  { path: String, line, column, end_line, end_column: usize,
              severity, code, message: String }
RequestKey  (pub Arc<Request>)   identity key; ::new(&Arc<Request>), .request(), From impls

Request {
    file: Arc<FileInfo>,
    index: usize,                 // 1-based position within the file
    name, title, description: String,
    directives: Vec<Directive>, captures: Vec<Capture>, asserts: Vec<Assert>,
    method, url, http_version: String,
    headers: Vec<Header>,
    body: String, body_line: usize,
    body_file: String, body_file_templated: bool,
    body_file_line, body_file_column: usize,
    save_to: Option<SaveTo>,
    line: usize,                  // the request line
}
```

`Diagnostic` implements `Display` (`path:line:col: severity: message`, the
column left out when zero) and `Serialize`/`Deserialize` with the
`validate --json` field names; `column`, `end_line`, `end_column` and `code`
are omitted when zero or empty. `severity` is `"error"` or `"warning"`.

Every type derives `Debug`, `Clone` and `Default`; all but `File` and
`Request` derive `PartialEq` and `Eq`.

## Request methods

| Method | Returns |
| --- | --- |
| `id()` | `String`: the name, else `<path>#<index>` |
| `directive(key)` | `Option<&str>`: the first directive's value |
| `directive_span(key)` | `(line, col, end)`, zeros when absent |
| `steps()` | `Vec<String>`: every `# @step` phrase |
| `refs()` | `Vec<Ref>`: every `# @ref` and `# @forceRef`, in order |
| `header(name)` | `Option<&str>`, case-insensitive |
| `disabled()` | `bool`: `# @disabled` |
| `sleep()` | `Result<Duration, String>`: `# @sleep`, zero without it |
| `protocol()` | `Result<&'static str, String>`: `""`, `HTTP1` or `HTTP2` |
| `is_multipart()` | `bool` |
| `multipart()` | `Result<Option<Multipart>, String>`: `Ok(None)` when not multipart or the body is a `< file` |
| `body_files()` | `Vec<FileRef>`: the whole-body file, or the multipart file parts |
| `is_graphql()` | `bool` |
| `graphql()` | `Result<(String, String), String>`: the query and the variables as written |

## Other functions and constants

| Item | Signature |
| --- | --- |
| `format` | `fn(src: &str) -> String`: the canonical form, idempotent |
| `format_request` | `fn(src: &str, name: &str) -> Option<String>`: one request by name or number, `None` when nothing matches |
| `directive_order` | `fn() -> Vec<&'static str>`: the order `format` writes directives in |
| `parse_retry` | `fn(s: &str) -> Result<(usize, Duration), String>`: `<attempts> [interval]` |
| `DEFAULT_RETRY_INTERVAL` | `Duration`, one second |
| `parse_sleep` | `fn(s: &str) -> Result<Duration, String>` |
| `HTTP1`, `HTTP2` | `&str`: `"HTTP/1.1"`, `"HTTP/2"` |
| `GRAPHQL_HEADER` | `&str`: `"X-REQUEST-TYPE"` |
| `split_graphql` | `fn(body: &str) -> (String, String)`: the query and the variables |
| `graphql_envelope` | `fn(query: &str, variables: &str) -> Result<String, String>`: the JSON body to send |
| `quote_param` | `fn(s: &str) -> String`: a quoted Content-Disposition parameter value |
| `parse_media_type` | `fn(v: &str) -> Result<(String, BTreeMap<String, String>), String>`: a media type and its parameters, RFC 2231 values decoded |

## Tests

`tests/parse.rs`, `tests/format.rs` and `tests/grammar.rs` cover the parser,
the formatter (with the golden pairs under `testdata/fmt`) and the check
that the VS Code grammar lists exactly `KNOWN_DIRECTIVES`. New parser cases
go in `testdata/sample.http`.
