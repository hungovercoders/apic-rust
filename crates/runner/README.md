# apic-runner

Executes parsed requests (`apic_runner`): resolves variables, applies auth,
sends, evaluates captures and assertions, runs `# @ref` dependencies and
flows, records history, and defines the result and error shapes every
command prints. This page is the map for the crates built on it; the doc
comments in `src/` carry the detail.

```rust
let project = apic_project::load(".")?;                       // Arc<Project>
let mut r = Runner::new(project, Options { env: "dev".into(), ..Options::default() })?;
r.interactive = false;                                        // configure, then share
let r = Arc::new(r);
let cancel = apic_httpc::CancelToken::new();
let reqs = r.target("login")?;                                // Vec<Arc<Request>>
let (res, err) = r.run(&cancel, &reqs[0]);                    // Outcome
println!("{}", res.unwrap().to_json());                       // what `--json` prints
std::process::exit(exit_code(err.as_ref()));
```

## Threading and ownership

- `Runner` is `Send + Sync`; every operation takes `&self`. Share it as
  `Arc<Runner>`: the UI sends on one thread while it calls `resolve` and
  `describe` on another.
- Configuration is plain public fields, set with `&mut` after `Runner::new`
  and before sharing: `opts`, `stderr`, `interactive`, `progress`,
  `on_result`, `sleep`, `now`, `zone`. The callbacks are `Box<dyn Fn .. +
  Send + Sync>`; one that collects what it sees keeps its own `Mutex`.
- Run state (captures, named responses, `set_var` values, targets named,
  HTTP clients) is behind locks inside the runner, never held across a
  request or a callback.
- `session` is `Option<SharedSession>` = `Arc<Mutex<apic_session::Store>>`
  (`shared_session(store)` wraps one); lock it to read, clear or save.
  `jar` is `apic_session::Jar`, already a shared handle. `history` is
  `Option<apic_history::Store>`.
- Requests are `Arc<apic_httpfile::Request>` compared by identity. A
  `Result` is owned data (`Clone`; clones share the response bytes).
  Dependencies nest as `deps: Vec<Result>`.

## Runner

| Item | Signature / notes |
| --- | --- |
| `Runner::new(Arc<Project>, Options)` | `-> Result<Runner, Error>`. Refuses a project with parse errors (E103), an unknown environment (E204, with `suggestions`), a bad proxy (E207), an unreadable session (E208) |
| `run(&CancelToken, &Arc<Request>)` | `-> Outcome` = `(Option<Result>, Option<Error>)`. Both are set when a `# @ref` dependency errored: the result keeps `deps` and carries `error` |
| `run_all(&CancelToken, &[Arc<Request>])` | `-> (Vec<Result>, Option<Error>)`: a flow; stops at the first failure unless `opts.keep_going`; always stops once cancelled; a request that errored without a result gets one with `errors` and `error` |
| `target(&str)` | `-> Result<Vec<Arc<Request>>, Error>`: `Project::resolve`, remembering requests named directly so `run_all` sends them even when `# @disabled`. A lookup that fails is `Error::Project` (code E200 until the caller wraps it) |
| `resolve(&Arc<Request>)` | `-> Result<Resolved, Error>`: no sending; `Resolved::missing()` lists unresolved placeholders |
| `dry_run(&Arc<Request>)` | `-> Result<Result, Error>`: `dry_run: true`, warnings for what a `# @ref` would supply |
| `describe(&Arc<Request>)` | `-> Description` |
| `env_vars()`, `auth_configs()`, `auth_config(name)` | `Vec<VarInfo>`, `Vec<AuthConfigInfo>`, `Option<AuthConfigInfo>` |
| `proxy_info(url)` | `Option<ProxyInfo>`; `""` asks for the https setting |
| `auth_source(&Request)` | `(raw, source)`, both empty when none |
| `missing_error(&Arc<Request>, &[String])` | The E101 error with its hints |
| `render(text)` | Placeholders in free text; unknown ones are `Error::Missing` (`err.missing()`) |
| `capture`, `captured`, `set_var`, `unset_var`, `vars`, `results`, `set_result` | The capture layer, the --var layer (`unset_var` takes back a `set_var`) and the named responses (`HashMap<String, Arc<Result>>`, each as it stood when its request finished) |
| `project_file(rel)` | A file confined to the project root |
| `current_time()` | The runner's clock (`now`, else the system's) |
| `local_time(Time)` | Free function: the same instant with the machine's zone offset, for showing a time locally |
| `history_key(&Project, &Request)` | Free function: `name` or `file#name` |
| `set_version(v)` / `version()` | The `apic/<version>` User-Agent; `dev` until set |
| `sleep_cancel(&CancelToken, Duration)` | The default `sleep` |
| `DEFAULT_MAX_BODY_BYTES`, `MASKED` (`***`), `BASE64` | Constants |

`Options` (all `Default`): `env`, `vars: BTreeMap`, `no_session`,
`timeout: Duration` (zero = apic.yaml, then 30 s), `insecure`, `keep_going`,
`redact`, `asserts`, `captures`, `output`, `max_body_bytes: i64`,
`session: Option<SharedSession>`, `cookies`, `cookie_jar: Option<Jar>`,
`retry`, `no_retry`, `ca_cert`, `cert`, `key`, `proxy`, `no_proxy`,
`no_history`. `opts.vars` and `set_var` are both the --var layer; `set_var`
wins and works through `&self`.

Callback types: `ProgressFn = Box<dyn Fn(&Progress) + Send + Sync>`
(`Progress { req, attempt, max, failure }`), `OnResultFn = Box<dyn
Fn(&Result, Option<&Error>) + Send + Sync>`, `SleepFn = Box<dyn
Fn(&CancelToken, Duration) -> Result<(), SleepError> + Send + Sync>`, `NowFn
= Box<dyn Fn() -> apic_gocompat::Time + Send + Sync>`. With `now` set,
`zone` is the abbreviation `$localDatetime` prints for layouts with `MST`.

## Result and its JSON

`Result { ok, request: Resolved, response: Option<Response>, captures,
asserts: Vec<apic_assert::Result>, errors, warnings, error:
Option<ErrorInfo>, dry_run, recorded, record_key, unrecorded, body_limit,
attempts, saved_to, deps, skipped, iteration: Option<Iteration>, redact }`
plus `raw() -> Option<&apic_selector::Response>`, `req() ->
Option<&Arc<Request>>`, `auth_note()`, `problem()`, `display_captures()`,
`display_response()`, `display_raw_body() -> Vec<u8>`, `display_asserts()`.
Build one by hand with `..Result::default()`.

`Result::to_json()` is the exact `--json` text: compact, `<`, `>`, `&`,
U+2028 and U+2029 escaped, sensitive headers masked, everything masked
under `redact`. `Serialize` for `Result` and `Resolved` emits that same
text (through `serde_json`'s raw values), so it is already HTML-escaped
whatever the caller's settings. To indent it, run
`apic_gocompat::indent_json` over the text; a pretty serializer leaves it
compact. Key order: `ok, errors, warnings, error, dry_run, attempts,
saved_to, ran_first, skipped, request, captures, response, asserts,
iteration`; in `request`: `name, file, line, method, url, auth,
http_version, tls, proxy, headers, body`. `src/tests/json_shape.rs` pins it.

`Response { status, status_text, headers: BTreeMap, body: Body,
body_encoding, body_truncated, duration_ms, size, proto, timings:
Option<Timings> }`; `Body` is `None` (`null`), `Json(Arc<str>)` (written as
JSON, compacted) or `Text(Arc<str>)`; `is_json()`, `as_json()`,
`as_text()`, `Body::json(..)`, `Body::text(..)`, `From<serde_json::Value>`.
`Response`, `Timings`, `Iteration`, `TlsInfo`, `ProxyInfo`, `ErrorInfo`,
`VarInfo`, `Description` and `AuthConfigInfo` derive both `Serialize` and
`Deserialize`. `Description.variables` is `null` when empty.

`parse_result(&str) -> Result<Result, String>` reads a stored result back
(history entries): a JSON body comes back with its keys sorted and numbers
as written; `raw()` is rebuilt.

`Resolved`: `display_headers(redact)`, `display_body(redact)`,
`display_url(redact)`, `body_bytes()`, `missing()`, `to_json(redact)`,
`secret_headers: BTreeSet<String>`, `parts: Vec<FormPart>`, `auth_spec`.
`TlsInfo` and `ProxyInfo` implement `Display` (the line `describe` shows);
`ProxyInfo::raw() -> Option<&str>` is the proxy URL with its credentials.

## Errors and exit codes

```rust
pub enum Error {
    Usage(UsageError),          // { code: Code, msg, suggestions }
    Transport(TransportError),  // { code: Option<Code>, message }, classified
    Missing(apic_template::MissingError),
    Project(apic_project::Error),
    Context { message, source }, // "prefix: source"; code and exit are the source's
    Other(String),
}
```

- `exit_code(Option<&Error>) -> i32`: `EXIT_OK` for `None`,
  `EXIT_TRANSPORT` (3) for anything that is or wraps a transport error,
  else `EXIT_USAGE` (2). `EXIT_ASSERT` (1) is the caller's, for `!ok`.
- `code_of(&Error) -> Code`, `info(&Error) -> ErrorInfo` (`{code, title,
  message, hint, exit, url, suggestions}`), `suggestions_of(&Error)`.
- `usage(code, msg) -> UsageError` (set `.suggestions`, then `.into()`);
  `UsageError::wrapping(code, msg, &inner)` keeps the inner error's code
  and suggestions (wrap a `apic_project::Error` as `Error::Project` first);
  `Error::context(prefix, err)`; `err.usage()`, `err.transport()`,
  `err.missing()`, `err.is_cancelled()`.
- `TransportError::new(msg)`, `with_code`, `from_http(&apic_httpc::Error)`,
  `cancelled()`, `deadline_exceeded()`, `error_code()`, `is_cancelled()`,
  `is_timeout()`; it displays as `request failed: <message>`.
- `Code` is an enum (`Code::MissingVariable` ... `Code::Cancelled`) with
  `as_str()` (`"E101"`), `url()`, `Code::parse`, `Display`, serde as the
  string; `CODE_*` constants name each. `CATALOGUE` / `catalogue()` is
  every `Entry { code, exit, title, hint, about }` in order; `lookup(code)`.

## Tests

`src/tests/` holds one file per area (auth, cookies, flow, graphql,
history, jbauth, multipart, predicates, proto, proxy, refs, retry, run,
save, timings, tls, vars, json_shape), all against local `apic_httpd`
servers. A test that changes the process environment takes `EnvGuard`;
the helpers make every other test wait for it.
