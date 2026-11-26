# apic-auth

Authentication for outgoing requests (`apic_auth`): the `# @auth <type>
[args]` directive and the `auth.default` setting. Types: none, bearer,
basic, apikey, digest, aws (own SigV4 signer), oauth2 (client credentials,
password, device code, authorization code with PKCE; tokens cached and
refreshed) and exec.

```rust
let spec = apic_auth::parse("oauth2 tokenUrl={{url}} clientId={{id}}")?;   // Err: apic_auth::Error
let spec = spec.render(|text| render_placeholders(text))?;                  // Err: the closure's own
apic_auth::apply(&cancel, &spec, &mut request, &body, &env)?;               // sets headers / the query
```

## API

```rust
// --- specs ----------------------------------------------------------
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    pub r#type: String,                       // lower case, one of TYPES
    pub args: Vec<String>,                    // positional
    pub options: BTreeMap<String, String>,    // key=value, sorted
    pub raw: String,
}
impl Spec {
    pub fn option(&self, key: &str) -> &str;                    // "" when unset
    pub fn api_key_placement(&self) -> (String, String);        // (header, query); one is empty
    pub fn render<E>(&self, f: impl FnMut(&str) -> Result<String, E>) -> Result<Spec, E>;
    pub fn texts(&self) -> Vec<String>;                         // args, then options by name
}
pub fn parse(raw: &str) -> Result<Spec, Error>;
pub const TYPES: [(&str, &str); 8];                             // (name, description), sorted by name
pub fn type_names() -> Vec<&'static str>;
pub fn type_description(name: &str) -> Option<&'static str>;

// --- applying -------------------------------------------------------
pub fn apply(cancel: &CancelToken, s: &Spec, req: &mut apic_httpc::Request,
             body: &[u8], env: &Env) -> Result<(), Error>;

pub trait Cache {                                               // the session, scoped to the environment
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
}
#[derive(Default)]
pub struct Env {
    pub cache: Option<Arc<dyn Cache>>,
    pub client: Option<Arc<dyn RoundTrip>>,   // token endpoints; None: plain client, 30 s timeout
    pub allow_exec: bool,
    pub stderr: Option<Stderr>,               // sign-in prompts
    pub now: Option<Clock>,                   // None: the system clock
    pub interactive: bool,                    // a person is at a terminal
    pub open_browser: Option<BrowserOpener>,  // None: open_browser
    pub getenv: Option<Getenv>,               // None: the process environment
}
pub type Stderr = Arc<Mutex<dyn Write + Send>>;
pub type Clock = Box<dyn Fn() -> apic_gocompat::Time>;
pub type BrowserOpener = Box<dyn Fn(&str) -> Result<(), String>>;
pub type Getenv = Box<dyn Fn(&str) -> Option<String>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {                              // Display + std::error::Error + From<String>
    NotInteractive,                           // ERR_NOT_INTERACTIVE
    Cancelled,                                // "context canceled"
    Cache(String),                            // Cache::set failed; the text is the cache's
    Message(String),
}
pub const ERR_NOT_INTERACTIVE: &str;

// --- oauth2 ---------------------------------------------------------
pub struct Tokens { pub access: String, pub id: String }
pub fn oauth2_tokens(cancel: &CancelToken, s: &Spec, env: &Env, need_id: bool) -> Result<Tokens, Error>;
pub fn cache_key(s: &Spec) -> String;                           // "$oauth2:<16 hex>"
pub fn describe_cached(raw: &str, now: Time) -> String;         // "token, expires in 59m30s" / "expired" / "(unreadable)"
pub fn open_browser(url: &str) -> Result<(), String>;           // xdg-open / open / rundll32

// --- JetBrains Security.Auth ----------------------------------------
pub struct JetBrains { pub spec: Spec, pub use_id_token: bool, pub ignored: Vec<String> }
pub fn from_jetbrains<E: Display>(fields: &serde_json::Map<String, Value>,
    render: impl Fn(&str) -> Result<String, E>) -> Result<JetBrains, Error>;

// --- AWS ------------------------------------------------------------
pub struct AwsCredentials { pub access_key_id: String, pub secret_access_key: String, pub session_token: String }
pub fn sign_sig_v4(req: &mut Request, body: &[u8], creds: &AwsCredentials,
                   service: &str, region: &str, at: Time);

// --- digest ---------------------------------------------------------
pub struct DigestState;                       // Default; share as Arc<DigestState>, one per invocation
impl DigestState { pub fn new() -> DigestState; }
pub fn digest_host(url: &str) -> String;      // "scheme://host[:port]", lower case

pub struct DigestTransport { pub user: String, pub pass: String, pub state: Option<Arc<DigestState>>, /* … */ }
impl DigestTransport {
    pub fn new(base: Box<dyn RoundTrip>, user: &str, pass: &str) -> DigestTransport;
    pub fn with_host(self, host: &str) -> DigestTransport;      // digest_host(url) of the request
    pub fn with_state(self, state: Arc<DigestState>) -> DigestTransport;
    pub fn host(&self) -> String;
    pub fn rounds(&self) -> u32;              // requests sent
    pub fn challenged(&self) -> bool;         // a 401 was answered
    pub fn answered(&self) -> bool;           // Authorization went out
}
impl RoundTrip for DigestTransport {}
```

## How a caller uses it

- **A request with `# @auth`**: `parse`, `Spec::render`, build the
  `apic_httpc::Request` with its body set, then `apply`. `apply` sets
  headers on the request, or for `apikey … query=` appends to `req.url`.
  For `aws` the request must already carry the body and every header that
  is to be signed.
- **Digest** is answered on the wire: `apply` does nothing for it. Wrap
  the client and send through the wrapper:

  ```rust
  let digest = DigestTransport::new(Box::new(client.clone()), &spec.args[0], &spec.args[1])
      .with_host(&digest_host(&req.url))
      .with_state(runner_digest_state.clone());
  let resp = follow_redirects(&digest, &req, &policy)?;
  // then digest.challenged(), digest.answered(), digest.rounds() for the note
  ```

  The answer to a challenge carries the timings of both round trips
  (`Timings::then`).
- **Token endpoints** are reached through `Env::client`, over which
  redirects are followed. Each request arrives with its `cancel` token set
  (and a `deadline` during a browser sign-in); a caller that needs a
  timeout or a proxy wraps its `Client` in a `RoundTrip` that sets them.
- **`$auth.token("name")`**: `from_jetbrains(fields, render)` then
  `oauth2_tokens(cancel, &jb.spec, env, want_id)`.
- **Cache entries** are JSON, `{"access_token":…,"refresh_token":…,
  "id_token":…,"expires_at":"RFC 3339"}`, under `cache_key(spec)` for
  oauth2 and `$exec:<hash of the command>` for exec.

## Environment variables read (through `Env::getenv`)

`AWS_PROFILE`, `AWS_REGION`, `AWS_DEFAULT_REGION`, `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`,
`AWS_SHARED_CREDENTIALS_FILE`, `AWS_CONFIG_FILE`, `HOME` (`USERPROFILE` on
Windows), and `PATH`, on which the `@auth exec` command and the `aws` CLI
are looked for.
