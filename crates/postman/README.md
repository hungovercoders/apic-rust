# apic-postman

Turns a Postman collection (v2.1, or v2.0 where the shapes coincide) and
its environments into `.http` files and env files, for `apic import`. This
page is the map of the public API; the doc comments in `src/` carry the
detail.

## API

```rust
pub fn import(collection_path: &str, opts: Options) -> Result<ImportResult, Error>;
pub fn looks_like_collection(data: &[u8]) -> bool;   // the import command's dispatch
pub fn looks_like_environment(data: &[u8]) -> bool;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub out_dir: String,          // directory to write into ("" is ".")
    pub env_files: Vec<String>,   // Postman environment exports, one environment each
    pub force: bool,              // overwrite existing files
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ImportResult {
    pub files: Vec<String>,             // "files": null while empty
    pub requests: usize,                // "requests", kept files included
    pub base_url: String,               // "base_url", left out when empty (as are the rest)
    pub env_file: String,               // "env_file"
    pub private_env_file: String,       // "private_env_file"
    pub skipped: Vec<String>,           // "skipped": existing files kept
    pub unsupported: Vec<Unsupported>,  // "unsupported"
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Unsupported {
    pub request: String,   // request id, or "collection"
    pub what: String,      // the script line, auth type or body mode
    pub reason: String,
}

pub struct Error;          // Display is the message; message() -> &str
```

`ImportResult` is the summary `apic import --json`
prints; its JSON is part of the output contract, field order included.

`import` reports a file it cannot read as `open PATH: reason`, a
collection it cannot use as `PATH: message` (`not a Postman collection
(no info.schema and no item list)`, `a Postman v1 collection is not
supported: ...`, `unsupported collection schema "...": ...`, or
`json: cannot unmarshal number into Go struct field Info.info.name of type
string` for a value of the wrong type, the first one in the file), and a
bad environment file as `PATH: not a Postman environment export`.

## The model

The collection format as plain owned structs, all `Clone + Debug +
Default + PartialEq + Eq`:

```text
Collection  { info: Info, item: Option<Vec<Item>>, variable: Vec<Variable>,
              auth: Option<Auth>, event: Vec<Event> }      // from_json(&[u8])
Info        { name, schema, description: String }
Item        { name, description: String, item: Vec<Item>, request: Option<Request>,
              event: Vec<Event>, auth: Option<Auth> }      // is_folder()
Request     { method: String, header: Vec<Header>, url: Url, body: Option<Body>,
              auth: Option<Auth>, description: String }
Header      { key, value: String, disabled: bool }
Url         { raw: String, query: Vec<Query>, variable: Vec<Variable> }
Query       { key, value: String, disabled: bool }
Variable    { key, value, type: String, disabled: bool, enabled: Option<bool> }  // active()
Body        { mode, raw: String, options: Option<BodyOptions>, urlencoded, formdata: Vec<FormField>,
              file: Option<BodyFile>, graphql: Option<GraphQL>, disabled: bool }
BodyOptions { raw: RawOptions }     RawOptions { language: String }
FormField   { key, value, type, src, content_type: String, disabled: bool }
BodyFile    { src: String }         GraphQL { query, variables: String }
Auth        { type: String, bearer, basic, apikey, awsv4, oauth2, digest: Vec<AuthParam> }
AuthParam   { key, value: String }
Event       { listen: String, script: Script }             Script { exec: Vec<String> }
Environment { name: String, values: Option<Vec<Variable>>, scope: String }  // from_json(&[u8])
```

`Collection::from_json` and `Environment::from_json` return the message
as the error. What the format allows in two shapes is read in both: a
request or a URL as a bare string, a script as one string or a list of
lines, and the text fields marked below as a string, a number, a boolean,
`{"content": ...}`, `{"src": ...}` or a list (its first element); anything
else is kept as its JSON text. Those fields are `description`,
`Variable::value`, `AuthParam::value` and `FormField::src`.

Keys match in any capitalisation, unknown keys are ignored and `null`
leaves a field unset. `Collection::item` and `Environment::values` are
`None` when the file has no such list, which is how a v1 collection and a
file that is no environment are told apart.

## What `import` writes

- One `.http` file per folder path (`todos-archive.http` for
  `Todos / Archive`), requests at the top level in a file named after the
  collection. Requests are named in kebab case, numbered when a name
  repeats.
- `:param` in a URL becomes `{{param}}` with its value as a file
  variable; `{{$guid}}` and `{{$randomUUID}}` become `{{$uuid}}`; disabled
  headers and query parameters stay as comments.
- Bodies: raw (with the content type its language implies), urlencoded,
  form-data (file parts read `./name` next to the `.http` file), a file,
  GraphQL. A raw body the parser would misread goes to
  `<file>.<request>.body.txt|json`.
- Auth: bearer, basic, digest, AWS v4 and OAuth 2 (client credentials and
  password grants) become `# @auth`; an API key becomes a header or a
  query parameter. The collection's auth becomes `auth.default` in
  `apic.yaml`, and a request that opts out says `# @auth none`.
- `pm.test` scripts: status, status text, response time, headers, body
  text and JSON paths become `# @assert`; `pm.*.set` from the response
  becomes `# @capture`. Each line that is not translated is reported
  under `unsupported`, as are pre-request scripts, other auth types and
  other body modes.
- Collection variables go to `$shared` in `http-client.env.json`, each
  environment file to an environment of its own (named after it, the
  first one `env:` in `apic.yaml`), and values of type `secret` to
  `http-client.private.env.json`, created readable by its owner alone.

An existing file is kept and listed under `skipped` unless `force`.

## Tests

`tests/import.rs` is the importer end to end on `testdata/collection.json`
and smaller collections; the unit tests in `src/` cover the script
translation, the decoding of each shape and its error messages, and the
JSON reader.
