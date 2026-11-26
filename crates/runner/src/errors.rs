//! The error catalogue, the error type every runner operation returns, and
//! the mapping from an error to its code and exit status.

use std::fmt;

use apic_template::MissingError;
use serde::{Deserialize, Serialize};

/// Exit codes returned by the CLI.
pub const EXIT_OK: i32 = 0;
/// An assertion or capture failed.
pub const EXIT_ASSERT: i32 = 1;
/// Parse error, unknown request, missing variable.
pub const EXIT_USAGE: i32 = 2;
/// Could not reach the server, timeout, TLS failure.
pub const EXIT_TRANSPORT: i32 = 3;

/// Code identifies a kind of error apic reports. Codes are stable: a script
/// or an agent can branch on one, and the site's errors page (generated
/// from [`CATALOGUE`]) explains each.
///
/// E1xx is the request as written, E2xx the command and the project around
/// it, E3xx the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    /// `E200`
    Other,
    /// `E101`
    MissingVariable,
    /// `E102`
    Build,
    /// `E103`
    InvalidFile,
    /// `E104`
    Directive,
    /// `E105`
    Auth,
    /// `E106`
    BodyFile,
    /// `E107`
    Ref,
    /// `E201`
    UnknownRequest,
    /// `E202`
    Ambiguous,
    /// `E203`
    Flag,
    /// `E204`
    Environment,
    /// `E205`
    Project,
    /// `E206`
    TlsConfig,
    /// `E207`
    Proxy,
    /// `E208`
    Session,
    /// `E209`
    Data,
    /// `E210`
    File,
    /// `E211`
    Import,
    /// `E212`
    Features,
    /// `E213`
    Terminal,
    /// `E214`
    Server,
    /// `E215`
    Mock,
    /// `E300`
    Transport,
    /// `E301`
    Connect,
    /// `E302`
    Timeout,
    /// `E303`
    Tls,
    /// `E304`
    Protocol,
    /// `E305`
    Cancelled,
}

/// `E200`
pub const CODE_OTHER: Code = Code::Other;
/// `E101`
pub const CODE_MISSING_VARIABLE: Code = Code::MissingVariable;
/// `E102`
pub const CODE_BUILD: Code = Code::Build;
/// `E103`
pub const CODE_INVALID_FILE: Code = Code::InvalidFile;
/// `E104`
pub const CODE_DIRECTIVE: Code = Code::Directive;
/// `E105`
pub const CODE_AUTH: Code = Code::Auth;
/// `E106`
pub const CODE_BODY_FILE: Code = Code::BodyFile;
/// `E107`
pub const CODE_REF: Code = Code::Ref;
/// `E201`
pub const CODE_UNKNOWN_REQUEST: Code = Code::UnknownRequest;
/// `E202`
pub const CODE_AMBIGUOUS: Code = Code::Ambiguous;
/// `E203`
pub const CODE_FLAG: Code = Code::Flag;
/// `E204`
pub const CODE_ENVIRONMENT: Code = Code::Environment;
/// `E205`
pub const CODE_PROJECT: Code = Code::Project;
/// `E206`
pub const CODE_TLS_CONFIG: Code = Code::TlsConfig;
/// `E207`
pub const CODE_PROXY: Code = Code::Proxy;
/// `E208`
pub const CODE_SESSION: Code = Code::Session;
/// `E209`
pub const CODE_DATA: Code = Code::Data;
/// `E210`
pub const CODE_FILE: Code = Code::File;
/// `E211`
pub const CODE_IMPORT: Code = Code::Import;
/// `E212`
pub const CODE_FEATURES: Code = Code::Features;
/// `E213`
pub const CODE_TERMINAL: Code = Code::Terminal;
/// `E214`
pub const CODE_SERVER: Code = Code::Server;
/// `E215`
pub const CODE_MOCK: Code = Code::Mock;
/// `E300`
pub const CODE_TRANSPORT: Code = Code::Transport;
/// `E301`
pub const CODE_CONNECT: Code = Code::Connect;
/// `E302`
pub const CODE_TIMEOUT: Code = Code::Timeout;
/// `E303`
pub const CODE_TLS: Code = Code::Tls;
/// `E304`
pub const CODE_PROTOCOL: Code = Code::Protocol;
/// `E305`
pub const CODE_CANCELLED: Code = Code::Cancelled;

impl Code {
    /// The code as it is printed: `E101`.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Other => "E200",
            Code::MissingVariable => "E101",
            Code::Build => "E102",
            Code::InvalidFile => "E103",
            Code::Directive => "E104",
            Code::Auth => "E105",
            Code::BodyFile => "E106",
            Code::Ref => "E107",
            Code::UnknownRequest => "E201",
            Code::Ambiguous => "E202",
            Code::Flag => "E203",
            Code::Environment => "E204",
            Code::Project => "E205",
            Code::TlsConfig => "E206",
            Code::Proxy => "E207",
            Code::Session => "E208",
            Code::Data => "E209",
            Code::File => "E210",
            Code::Import => "E211",
            Code::Features => "E212",
            Code::Terminal => "E213",
            Code::Server => "E214",
            Code::Mock => "E215",
            Code::Transport => "E300",
            Code::Connect => "E301",
            Code::Timeout => "E302",
            Code::Tls => "E303",
            Code::Protocol => "E304",
            Code::Cancelled => "E305",
        }
    }

    /// The code a string names, if it is one.
    pub fn parse(s: &str) -> Option<Code> {
        CATALOGUE.iter().map(|e| e.code).find(|c| c.as_str() == s)
    }

    /// The entry's address on the documentation site.
    pub fn url(self) -> String {
        format!("{DOCS_URL}#{}", self.as_str().to_lowercase())
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Code {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Code, D::Error> {
        let s = String::deserialize(d)?;
        Code::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown error code {s:?}")))
    }
}

/// Entry is one row of the error catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub code: Code,
    pub exit: i32,
    /// A few words, e.g. "missing variable".
    pub title: &'static str,
    /// One line: what to do about it.
    pub hint: &'static str,
    /// What it means and how it comes about, for the errors page.
    pub about: &'static str,
}

/// Where the errors page is published; an entry's anchor is its code in
/// lower case.
pub const DOCS_URL: &str = "https://apic.sh/errors/";

/// Every code, in order. The site's errors page is generated from it (task
/// docs:errors), so a message and its documentation cannot drift.
pub const CATALOGUE: &[Entry] = &[
    Entry {
        code: Code::MissingVariable,
        exit: EXIT_USAGE,
        title: "missing variable",
        hint: "Pass it with --var name=value, add it to an env file, or run the request that captures it first.",
        about: "A `{{placeholder}}` in the request has no value from any source: `--var`, the environment's entry in the env files, `.env`, the session, or a request earlier in the same run. The message names each missing variable and, when another request's `# @capture` provides it, that request. A `# @ref` to that request makes it run first by itself; `apic describe <request>` shows where every variable comes from.",
    },
    Entry {
        code: Code::Build,
        exit: EXIT_USAGE,
        title: "request cannot be built",
        hint: "Check the placeholder, URL, header or body the message names.",
        about: "The request's text could not be turned into a request: a built-in with bad arguments (`{{$randomInt 5 1}}`), an unknown built-in, a URL that does not parse once its placeholders are filled in, or a GraphQL body whose variables are not a JSON object.",
    },
    Entry {
        code: Code::InvalidFile,
        exit: EXIT_USAGE,
        title: "request file has errors",
        hint: "Run apic validate to see every problem with its line and column.",
        about: "The `.http` file failed to parse where the request is: a malformed directive, a multipart body without a boundary, a GraphQL request without a query. apic refuses to send from a file with errors rather than guess; `apic validate` lists them all.",
    },
    Entry {
        code: Code::Directive,
        exit: EXIT_USAGE,
        title: "bad directive value",
        hint: "Fix the directive's value; https://apic.sh/format/ lists what each takes.",
        about: "A directive is well formed but its value cannot be used: `# @timeout soon`, `# @retry 0`, `# @sleep -1s`, an `# @assert` that does not parse, an HTTP version other than HTTP/1.1 or HTTP/2 on the request line, or `HTTP/2` over plain `http://`.",
    },
    Entry {
        code: Code::Auth,
        exit: EXIT_USAGE,
        title: "authentication failed",
        hint: "Check the # @auth spec (or auth.default), its credentials, and the token endpoint.",
        about: "The request's auth could not be applied: an `# @auth` spec that does not parse, `# @auth exec` without `auth.allowExec`, an OAuth2 token endpoint that refused the grant, a browser sign-in with no terminal, or a JetBrains `Security.Auth` configuration that does not exist or cannot be used.",
    },
    Entry {
        code: Code::BodyFile,
        exit: EXIT_USAGE,
        title: "body file problem",
        hint: "Check that the file exists, relative to the .http file, and lies inside the project.",
        about: "A file the request reads could not be used: a `< file` body, a multipart part's file, or a schema for `matchesSchema`. Paths are relative to the `.http` file and must resolve (symlinks included) inside the project root, so a request file cannot read something outside it.",
    },
    Entry {
        code: Code::Ref,
        exit: EXIT_USAGE,
        title: "@ref problem",
        hint: "Point # @ref at exactly one request, and break any cycle.",
        about: "A `# @ref` or `# @forceRef` names no request, several requests, or leads back to the request it started from. `apic validate` reports these as `bad-ref` and `ref-cycle` before anything runs.",
    },
    Entry {
        code: Code::Other,
        exit: EXIT_USAGE,
        title: "error",
        hint: "The message says what went wrong; if it is unclear, please open an issue.",
        about: "An error without a more specific code. It should be rare: if you meet one, the message is worth an issue so it can get a code of its own.",
    },
    Entry {
        code: Code::UnknownRequest,
        exit: EXIT_USAGE,
        title: "unknown request",
        hint: "Run apic list to see the request ids, and use name, file.http, file.http#name or file.http#N.",
        about: "The target names no request: no `# @name` of that name, no such `.http` file, or no request at that position in the file. When a known name is close to the one given (a typo, the wrong case, `_` for `-`), the message asks \"did you mean\" and the `--json` object lists them under `suggestions`, best first; otherwise the message lists what there is: the request ids, the files, or the named requests of the file given.",
    },
    Entry {
        code: Code::Ambiguous,
        exit: EXIT_USAGE,
        title: "several requests match",
        hint: "Pick one with file.http#name, or give the command a single request.",
        about: "The command works on one request (`describe`, `curl`, `snippet`) and the target names a whole file or a name used in several files.",
    },
    Entry {
        code: Code::Flag,
        exit: EXIT_USAGE,
        title: "bad flag or argument",
        hint: "Check the command's flags with apic <command> --help.",
        about: "A flag has a value the command cannot use, two flags cannot be combined (`--output` with a flow or `--data`, `--use-session` with `--no-session`), an argument is missing or extra, a history entry number names no entry, or the command does not exist.",
    },
    Entry {
        code: Code::Environment,
        exit: EXIT_USAGE,
        title: "unknown environment",
        hint: "Use one of the environments the message lists, or add it to http-client.env.json.",
        about: "`--env` (or `env:` in `apic.yaml`) names an environment that neither env file declares, or there are no env files at all. The message lists the environments found and, when one is close to the name given, asks \"did you mean\"; the `--json` object lists those under `suggestions`. `apic env` shows each environment with its files.",
    },
    Entry {
        code: Code::Project,
        exit: EXIT_USAGE,
        title: "project configuration problem",
        hint: "Fix apic.yaml or the env file the message names; apic validate helps.",
        about: "The project could not be loaded: `apic.yaml` or an env file is not valid YAML/JSON, or a setting in it (a timeout, a retry policy) cannot be read.",
    },
    Entry {
        code: Code::TlsConfig,
        exit: EXIT_USAGE,
        title: "TLS configuration problem",
        hint: "Check the CA file, client certificate and key the message names.",
        about: "A TLS setting cannot be used: a CA file with no PEM certificates, a certificate or key that does not load, a key that is world-readable or needs a passphrase apic cannot supply, or a path outside the project.",
    },
    Entry {
        code: Code::Proxy,
        exit: EXIT_USAGE,
        title: "proxy configuration problem",
        hint: "Give the proxy as an http, https, socks5 or socks5h URL.",
        about: "`--proxy`, `proxy:` in `apic.yaml` or `HTTP(S)_PROXY` holds a URL apic cannot use. apic refuses the request rather than send it directly.",
    },
    Entry {
        code: Code::Session,
        exit: EXIT_USAGE,
        title: "session or cookie store problem",
        hint: "Check .apic/ is writable, or run with --no-session.",
        about: "`.apic/session.json`, the cookie jar or the response history in `.apic/history` could not be read or written, or a session command ran with `--no-session`.",
    },
    Entry {
        code: Code::Data,
        exit: EXIT_USAGE,
        title: "bad data file",
        hint: "Give --data a CSV file with a header row, or a JSON array of objects.",
        about: "The rows for `apic run --data` could not be read: an empty file, a header column without a name, a row with the wrong number of fields, or JSON that is not an array of objects.",
    },
    Entry {
        code: Code::File,
        exit: EXIT_USAGE,
        title: "cannot read or write a file",
        hint: "Check the path and its permissions.",
        about: "A file named on the command line (a report, a data file, a file to format, stdin) could not be read or written.",
    },
    Entry {
        code: Code::Import,
        exit: EXIT_USAGE,
        title: "import failed",
        hint: "Check the input is an OpenAPI 3 document, a Postman v2 collection or a curl command.",
        about: "`apic import` could not read what it was given, or refused to overwrite files without `--force`.",
    },
    Entry {
        code: Code::Features,
        exit: EXIT_USAGE,
        title: "feature file problem",
        hint: "Check the feature files, the step text and the tag expression.",
        about: "`apic test` could not run the features: no `.feature` files, a feature that does not parse, an invalid tag expression, a step table or variable name it cannot use, or a step naming a target that is not a request.",
    },
    Entry {
        code: Code::Terminal,
        exit: EXIT_USAGE,
        title: "needs a terminal",
        hint: "Run apic ui in a terminal; use apic run or apic list --json in scripts.",
        about: "`apic ui` is interactive: it refuses `--json` and a stdout that is not a terminal.",
    },
    Entry {
        code: Code::Server,
        exit: EXIT_USAGE,
        title: "server could not start",
        hint: "Check the address or port is free and allowed.",
        about: "`apic mcp --http`, `apic demo` or `apic mock` could not listen where it was asked to.",
    },
    Entry {
        code: Code::Mock,
        exit: EXIT_USAGE,
        title: "mock could not be built",
        hint: "Check the file is an OpenAPI 3 document with at least one operation under paths.",
        about: "`apic mock` could not use the document it was given: it is not OpenAPI 3, a `$ref` in it does not resolve, or it declares no operations to serve.",
    },
    Entry {
        code: Code::Transport,
        exit: EXIT_TRANSPORT,
        title: "request failed",
        hint: "The message has the underlying error; check the URL and the network.",
        about: "The request could not complete for a reason none of the E3xx codes below describes.",
    },
    Entry {
        code: Code::Connect,
        exit: EXIT_TRANSPORT,
        title: "could not connect",
        hint: "Check the host name, the port and that the server is running.",
        about: "The host name did not resolve, the connection was refused or reset, or the network is unreachable. Nothing reached the server.",
    },
    Entry {
        code: Code::Timeout,
        exit: EXIT_TRANSPORT,
        title: "timed out",
        hint: "Raise --timeout, # @timeout or timeout: in apic.yaml, or check the server.",
        about: "The server did not answer within the timeout (30 seconds unless `--timeout`, `timeout:` in `apic.yaml` or `# @timeout` says otherwise).",
    },
    Entry {
        code: Code::Tls,
        exit: EXIT_TRANSPORT,
        title: "TLS handshake failed",
        hint: "Trust the server's CA with --cacert or tls.caFile, or check the client certificate.",
        about: "The TLS handshake failed: the server's certificate is not trusted, has the wrong name or has expired, or the server refused the client certificate. `--insecure` skips verification for one command, as a last resort.",
    },
    Entry {
        code: Code::Protocol,
        exit: EXIT_TRANSPORT,
        title: "protocol problem",
        hint: "Check the server speaks what the request asks for.",
        about: "The server's answer could not be used: it did not negotiate HTTP/2 for a request line that asks for it, the response was malformed, or the body was larger than apic reads.",
    },
    Entry {
        code: Code::Cancelled,
        exit: EXIT_TRANSPORT,
        title: "cancelled",
        hint: "Run the command again.",
        about: "The run was interrupted (Ctrl-C, or `esc` in the UI) while a request, a `# @sleep` or a retry wait was in progress. A flow prints its summary so far, the session and the history keep what the completed requests captured and returned, and `--report` is still written. A second Ctrl-C ends the process at once.",
    },
];

/// The catalogue as a slice: every code, in order.
pub fn catalogue() -> &'static [Entry] {
    CATALOGUE
}

/// Returns a code's entry.
pub fn lookup(c: Code) -> Option<Entry> {
    CATALOGUE.iter().copied().find(|e| e.code == c)
}

/// A problem with the request definition or invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError {
    /// The catalogue entry.
    pub code: Code,
    pub msg: String,
    /// The names the user probably meant, best first, when the one they
    /// gave matches nothing (an unknown request or environment). They
    /// reach --json and MCP as the error's `suggestions`, so an agent can
    /// retry without listing.
    pub suggestions: Vec<String>,
}

impl UsageError {
    /// A usage error with its code.
    pub fn new(code: Code, msg: impl Into<String>) -> UsageError {
        UsageError {
            code,
            msg: msg.into(),
            suggestions: Vec::new(),
        }
    }

    /// A usage error whose message was built around `inner`. When `inner`
    /// itself carries a code (a missing variable inside a header, say),
    /// that code is kept: it says more than the outer one. Suggestions
    /// `inner` carries are kept the same way, so wrapping codes an error
    /// without losing them.
    pub fn wrapping(code: Code, msg: impl Into<String>, inner: &Error) -> UsageError {
        UsageError {
            code: inner.usage().map_or(code, |u| u.code),
            msg: msg.into(),
            suggestions: suggestions_of(inner),
        }
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

/// Builds a usage error with its code.
pub fn usage(code: Code, msg: impl Into<String>) -> UsageError {
    UsageError::new(code, msg)
}

/// A usage error, as the crate's error.
pub(crate) fn uerr(code: Code, msg: impl Into<String>) -> Error {
    Error::Usage(UsageError::new(code, msg))
}

/// A usage error built around another error, as the crate's error.
pub(crate) fn uwrap(code: Code, msg: impl Into<String>, inner: &Error) -> Error {
    Error::Usage(UsageError::wrapping(code, msg, inner))
}

/// A network-level failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportError {
    /// Set where the cause is known; otherwise the failure is classified
    /// from what went wrong.
    pub code: Option<Code>,
    /// The failure itself: `Get "http://x/": dial tcp ...`.
    pub message: String,
    /// What the failure was classified as.
    kind: Code,
}

impl TransportError {
    /// A failure known only by its message. A message that mentions
    /// `tls: ` is a TLS failure; any other is the generic transport code.
    pub fn new(message: impl Into<String>) -> TransportError {
        let message = message.into();
        let kind = if message.contains("tls: ") {
            Code::Tls
        } else {
            Code::Transport
        };
        TransportError {
            code: None,
            message,
            kind,
        }
    }

    /// A failure with the code that describes it.
    pub fn with_code(code: Code, message: impl Into<String>) -> TransportError {
        TransportError {
            code: Some(code),
            ..TransportError::new(message)
        }
    }

    /// The failure of a request the HTTP client could not complete.
    pub fn from_http(err: &apic_httpc::Error) -> TransportError {
        TransportError {
            code: None,
            message: err.to_string(),
            kind: classify(err),
        }
    }

    /// The run was cancelled: `context canceled`.
    pub fn cancelled() -> TransportError {
        TransportError {
            code: None,
            message: "context canceled".to_string(),
            kind: Code::Cancelled,
        }
    }

    /// The deadline passed: `context deadline exceeded`.
    pub fn deadline_exceeded() -> TransportError {
        TransportError {
            code: None,
            message: "context deadline exceeded".to_string(),
            kind: Code::Timeout,
        }
    }

    /// The catalogue code for the failure.
    pub fn error_code(&self) -> Code {
        self.code.unwrap_or(self.kind)
    }

    /// Reports whether the failure is a cancellation (Ctrl-C, `esc`).
    pub fn is_cancelled(&self) -> bool {
        self.kind == Code::Cancelled
    }

    /// Reports whether the failure is a deadline that passed.
    pub fn is_timeout(&self) -> bool {
        self.kind == Code::Timeout
    }

    /// The same failure with other text, its classification kept.
    pub(crate) fn with_message(mut self, message: String) -> TransportError {
        self.message = message;
        self
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "request failed: {}", self.message)
    }
}

/// Reads the kind of a network failure from the client's error.
fn classify(err: &apic_httpc::Error) -> Code {
    use apic_httpc::ErrorKind;
    match err.kind {
        ErrorKind::Cancelled => Code::Cancelled,
        ErrorKind::Timeout => Code::Timeout,
        ErrorKind::Tls(_) => Code::Tls,
        _ if err.message.contains("tls: ") => Code::Tls,
        ErrorKind::Dns | ErrorKind::Connect => Code::Connect,
        ErrorKind::Proxy | ErrorKind::Other => Code::Transport,
    }
}

/// Why a wait between attempts, or a `# @sleep`, ended early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepError {
    /// The run was cancelled.
    Cancelled,
    /// The deadline passed.
    DeadlineExceeded,
}

impl From<SleepError> for TransportError {
    fn from(e: SleepError) -> TransportError {
        match e {
            SleepError::Cancelled => TransportError::cancelled(),
            SleepError::DeadlineExceeded => TransportError::deadline_exceeded(),
        }
    }
}

/// Everything the runner reports. The variants keep apart what callers
/// tell apart: [`exit_code`], [`code_of`] and [`info`] read them.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// A problem with the request definition or invocation.
    Usage(UsageError),
    /// A network-level failure.
    Transport(TransportError),
    /// Placeholders with no value; what [`crate::Runner::render`] returns
    /// for text that names unknown variables.
    Missing(MissingError),
    /// A target that names no request, or another project error;
    /// [`crate::Runner::target`] returns it as it is.
    Project(apic_project::Error),
    /// Another error with words put in front of it: `{{expr}}: <source>`
    /// for a placeholder that could not be resolved. The code, the exit
    /// status and the suggestions are those of `source`.
    Context {
        /// The whole text, the source's included.
        message: String,
        source: Box<Error>,
    },
    /// Any other error, as its message.
    Other(String),
}

impl Error {
    /// `prefix: err`, keeping what `err` is.
    pub fn context(prefix: impl fmt::Display, err: Error) -> Error {
        Error::Context {
            message: format!("{prefix}: {err}"),
            source: Box::new(err),
        }
    }

    /// The usage error this is or wraps, if any.
    pub fn usage(&self) -> Option<&UsageError> {
        match self {
            Error::Usage(u) => Some(u),
            Error::Context { source, .. } => source.usage(),
            _ => None,
        }
    }

    /// The transport error this is or wraps, if any.
    pub fn transport(&self) -> Option<&TransportError> {
        match self {
            Error::Transport(t) => Some(t),
            Error::Context { source, .. } => source.transport(),
            _ => None,
        }
    }

    /// The missing placeholders this is or wraps, if any.
    pub fn missing(&self) -> Option<&MissingError> {
        match self {
            Error::Missing(m) => Some(m),
            Error::Context { source, .. } => source.missing(),
            _ => None,
        }
    }

    /// Reports whether this is a cancelled run.
    pub fn is_cancelled(&self) -> bool {
        self.transport().is_some_and(TransportError::is_cancelled)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usage(e) => e.fmt(f),
            Error::Transport(e) => e.fmt(f),
            Error::Missing(e) => e.fmt(f),
            Error::Project(e) => e.fmt(f),
            Error::Context { message, .. } => f.write_str(message),
            Error::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<UsageError> for Error {
    fn from(e: UsageError) -> Error {
        Error::Usage(e)
    }
}

impl From<TransportError> for Error {
    fn from(e: TransportError) -> Error {
        Error::Transport(e)
    }
}

impl From<apic_project::Error> for Error {
    fn from(e: apic_project::Error) -> Error {
        Error::Project(e)
    }
}

impl From<MissingError> for Error {
    fn from(e: MissingError) -> Error {
        Error::Missing(e)
    }
}

/// Reads the suggestions an error carries, if any: a usage error's own,
/// or the nearest names of a project lookup that found nothing.
pub fn suggestions_of(err: &Error) -> Vec<String> {
    match err {
        Error::Usage(u) => u.suggestions.clone(),
        Error::Project(p) => p.suggestions().to_vec(),
        Error::Context { source, .. } => suggestions_of(source),
        _ => Vec::new(),
    }
}

/// An error as --json, MCP and a result report it: the catalogue entry
/// plus the message. It is part of the --json contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub code: Code,
    pub title: String,
    pub message: String,
    pub hint: String,
    pub exit: i32,
    pub url: String,
    /// The names the user probably meant, best first, when the error is
    /// about one that matches nothing: an unknown request (E201, or E107
    /// when a `# @ref` names it) or environment (E204). Absent otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

/// Describes `err` with its catalogue entry. The exit status is the
/// entry's, which is what [`exit_code`] gives for the same error.
pub fn info(err: &Error) -> ErrorInfo {
    let c = code_of(err);
    let e = lookup(c);
    ErrorInfo {
        code: c,
        title: e.map_or("", |e| e.title).to_string(),
        message: err.to_string(),
        hint: e.map_or("", |e| e.hint).to_string(),
        exit: e.map_or(0, |e| e.exit),
        url: c.url(),
        suggestions: suggestions_of(err),
    }
}

/// The catalogue code for an error: its own when it carries one,
/// [`Code::Other`] for any other.
pub fn code_of(err: &Error) -> Code {
    if let Some(t) = err.transport() {
        return t.error_code();
    }
    err.usage().map_or(Code::Other, |u| u.code)
}

/// Maps an error to the CLI exit code; `None` is success.
///
/// The exit codes are a public contract, so this looks through whatever
/// wraps the error: putting words in front of a transport error anywhere
/// on the way up must not turn a documented 3 into a 2.
pub fn exit_code(err: Option<&Error>) -> i32 {
    match err {
        None => EXIT_OK,
        Some(e) if e.transport().is_some() => EXIT_TRANSPORT,
        Some(_) => EXIT_USAGE,
    }
}
