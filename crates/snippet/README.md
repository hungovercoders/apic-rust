# apic-snippet

Renders a resolved request as code that sends the same thing without apic
(`apic_snippet`): the generators behind `apic curl`, `apic snippet --lang`
and the UI's snippet view. Languages: a curl command, an HTTPie command,
PowerShell's `Invoke-RestMethod`, Python `requests`, JavaScript `fetch`
and a Go program using `net/http`.

```rust
let resolved = runner.resolve(&req)?;                       // apic_runner::Resolved
println!("{}", apic_snippet::curl(&resolved, false));        // `apic curl`
let py = apic_snippet::render("python", &resolved, true)?;   // `apic snippet --lang python --redact`
```

## API

```rust
/// The snippet languages, in the order the UI cycles them.
pub const LANGUAGES: [&str; 6] = ["curl", "httpie", "powershell", "python", "js", "go"];

/// The snippet for `lang`; no trailing newline.
pub fn render(lang: &str, r: &apic_runner::Resolved, redact: bool) -> Result<String, Error>;

/// The POSIX-shell curl command (what `render("curl", ..)` returns).
pub fn curl(r: &apic_runner::Resolved, redact: bool) -> String;

/// An unknown language. Displays as
/// `unknown language "cobol": one of curl, httpie, powershell, python, js, go`.
pub struct Error;   // Clone, Debug, PartialEq, Eq, Display, std::error::Error
```

The functions borrow the `Resolved` and return owned text. Nothing checks
`Resolved::missing()`: refusing a request with unresolved placeholders is
the caller's job.

## Rules every language follows

- Without `redact` the snippet runs as printed, credentials included
  (headers go out verbatim, and a proxy is written with its own
  credentials, from `ProxyInfo::raw()`).
- With `redact` header values, the body, query values and text form
  parts are masked (`***`) and credentials are read from the
  environment: `TOKEN` for a bearer or OAuth2 token, `APIC_USER` and
  `APIC_PASSWORD` for basic and digest, `APIC_API_KEY` for an API key.
- OAuth2 and exec credentials are always `TOKEN` from the environment
  (curl runs the exec command in `$(...)`); AWS SigV4 is curl's
  `--aws-sigv4`, and a comment elsewhere.
- What a language cannot do on its own (SigV4, digest in fetch, a CA file
  in PowerShell, HTTP/2 pinning) is said in a comment at the top.
- A multipart body is written from `Resolved::parts`, each language with
  its own boundary, so the request's `Content-Type` header is left out.
- The Go program is written as gofmt would write it.

## Tests

`src/curl_tests.rs` and `src/snippet_tests.rs`. `golden` compares every
language, fixture and redact setting with `testdata/<fixture>[.redacted].<lang>`
(84 files); `APIC_UPDATE_GOLDEN=1 cargo test -p apic-snippet` rewrites
them. `snippets_parse` runs each snippet through its own language where
the tool is installed (`python3`, `node --check`, `bash -n`, `gofmt`).
