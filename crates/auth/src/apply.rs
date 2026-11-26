use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apic_gocompat::{parse_duration, quote, Time};
use apic_httpc::{
    follow_redirects, CancelToken, Client, RedirectPolicy, Request, Response, RoundTrip,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::awscreds::resolve_aws;
use crate::command;
use crate::error::Error;
use crate::oauth2::{decode_token, encode_token, oauth2_token, CachedToken};
use crate::sigv4::sign_sig_v4;
use crate::spec::Spec;
use crate::urlutil::query_escape;

/// Stores tokens between invocations (backed by the apic session).
pub trait Cache {
    fn get(&self, key: &str) -> Option<String>;
    /// Stores a value; the error is the text to show when it cannot be
    /// saved.
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
}

/// Where device-code and browser sign-in prompts are written.
pub type Stderr = Arc<Mutex<dyn Write + Send>>;

/// The clock [`Env`] reads.
pub type Clock = Box<dyn Fn() -> Time>;

/// Opens a URL for the person at the terminal.
pub type BrowserOpener = Box<dyn Fn(&str) -> Result<(), String>>;

/// Looks an environment variable up; an unset one is `None` or empty.
pub type Getenv = Box<dyn Fn(&str) -> Option<String>>;

/// What [`apply`] needs from the caller. Every field has a working
/// default, so `Env::default()` applies the types that need nothing.
#[derive(Default)]
pub struct Env {
    pub cache: Option<Arc<dyn Cache>>,
    /// Sends the requests to token endpoints. It is given each request
    /// with its cancellation token set and adds what the project asks for
    /// (a timeout, a proxy); redirects are followed over it. `None` means
    /// a plain client with a thirty-second timeout.
    pub client: Option<Arc<dyn RoundTrip>>,
    pub allow_exec: bool,
    /// Device-code and browser sign-in prompts.
    pub stderr: Option<Stderr>,
    /// The clock; `None` means the system's.
    pub now: Option<Clock>,
    /// Says a person is at a terminal, so a grant that needs a browser may
    /// start one; false under --json, in MCP and in tests.
    pub interactive: bool,
    /// Opens a URL for the person; `None` means the system's opener. Tests
    /// inject one that visits the URL themselves.
    pub open_browser: Option<BrowserOpener>,
    /// Looks an environment variable up (the AWS settings, and the PATH
    /// commands are found on); `None` means the process environment. An
    /// unset variable is `None` or empty.
    pub getenv: Option<Getenv>,
}

impl Env {
    pub(crate) fn now(&self) -> Time {
        match &self.now {
            Some(now) => now(),
            None => Time::now(),
        }
    }

    /// The value of an environment variable, empty when it is not set.
    pub(crate) fn getenv(&self, name: &str) -> String {
        match &self.getenv {
            Some(lookup) => lookup(name).unwrap_or_default(),
            None => std::env::var(name).unwrap_or_default(),
        }
    }

    /// Writes to the prompt writer, when there is one.
    pub(crate) fn prompt(&self, text: &str) {
        if let Some(w) = &self.stderr {
            if let Ok(mut w) = w.lock() {
                let _ = w.write_all(text.as_bytes());
                let _ = w.flush();
            }
        }
    }

    /// Sends a request to a token endpoint, following redirects.
    pub(crate) fn send(&self, req: &Request) -> Result<Response, apic_httpc::Error> {
        match &self.client {
            Some(client) => follow_redirects(&**client, req, &RedirectPolicy::default()),
            None => {
                let mut req = req.clone();
                if req.timeout.is_none() {
                    req.timeout = Some(Duration::from_secs(30));
                }
                Client::new().execute(&req)
            }
        }
    }
}

/// Adds credentials to `req` according to a rendered spec. `body` is the
/// request body as sent (needed for AWS payload hashing).
pub fn apply(
    cancel: &CancelToken,
    s: &Spec,
    req: &mut Request,
    body: &[u8],
    env: &Env,
) -> Result<(), Error> {
    match s.r#type.as_str() {
        "none" => {}
        "bearer" => req
            .headers
            .set("Authorization", &format!("Bearer {}", arg(s, 0))),
        "basic" => req.headers.set(
            "Authorization",
            &format!(
                "Basic {}",
                STANDARD.encode(format!("{}:{}", arg(s, 0), arg(s, 1)))
            ),
        ),
        "apikey" => {
            let key = format!("{}{}", s.option("prefix"), arg(s, 0));
            let (header, query) = s.api_key_placement();
            if !query.is_empty() {
                // Appended to the query as written: rebuilding it would
                // reorder and re-encode what the file says.
                let pair = format!("{}={}", query_escape(&query), query_escape(&key));
                req.url = append_query(&req.url, &pair);
            } else {
                req.headers.set(&header, &key);
            }
        }
        // Answered on the wire by a DigestTransport, since the server's
        // challenge is only known once the request has been sent; the
        // runner wraps the client for a digest spec.
        "digest" => {}
        "aws" => return apply_aws(cancel, s, req, body, env),
        "oauth2" => {
            let tok = oauth2_token(cancel, s, env)?;
            req.headers.set("Authorization", &format!("Bearer {tok}"));
        }
        "exec" => return apply_exec(cancel, s, req, env),
        other => return Err(Error::msg(format!("unknown auth type {}", quote(other)))),
    }
    Ok(())
}

/// A positional argument; empty for a spec built by hand without it.
fn arg(s: &Spec, i: usize) -> &str {
    s.args.get(i).map(String::as_str).unwrap_or("")
}

/// Adds `pair` to a URL's query, before any fragment.
fn append_query(url: &str, pair: &str) -> String {
    let (base, fragment) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (url, None),
    };
    let mut out = match base.split_once('?') {
        None => format!("{base}?{pair}"),
        Some((_, "")) => format!("{base}{pair}"),
        Some(_) => format!("{base}&{pair}"),
    };
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

fn apply_aws(
    cancel: &CancelToken,
    s: &Spec,
    req: &mut Request,
    body: &[u8],
    env: &Env,
) -> Result<(), Error> {
    let settings = resolve_aws(cancel, s, env)?;
    let service = match s.option("service") {
        "" => "execute-api",
        service => service,
    };
    sign_sig_v4(
        req,
        body,
        &settings.creds,
        service,
        &settings.region,
        env.now(),
    );
    Ok(())
}

fn apply_exec(cancel: &CancelToken, s: &Spec, req: &mut Request, env: &Env) -> Result<(), Error> {
    if !env.allow_exec {
        return Err(Error::msg(format!(
            "@auth exec is disabled: set `auth:\n  allowExec: true` in apic.yaml to let request files run {}",
            quote(&s.args.join(" "))
        )));
    }
    let header = match s.option("header") {
        "" => "Authorization",
        header => header,
    };
    let prefix = match s.options.get("prefix") {
        Some(prefix) => prefix.as_str(),
        None => "Bearer",
    };
    let key = format!("$exec:{}", hash_key(&s.args.join("\x00")));
    // A ttl that is zero or negative caches nothing.
    let mut ttl = Duration::ZERO;
    let t = s.option("ttl");
    if !t.is_empty() {
        let bad = || Error::msg(format!("@auth exec: bad ttl {}", quote(t)));
        match t.strip_prefix('-') {
            Some(rest) if !rest.starts_with(['-', '+']) => {
                parse_duration(rest).map_err(|_| bad())?;
            }
            _ => ttl = parse_duration(t).map_err(|_| bad())?,
        }
    }
    let caching = ttl > Duration::ZERO;
    let mut token = String::new();
    if let (true, Some(cache)) = (caching, &env.cache) {
        if let Some(raw) = cache.get(&key) {
            if let Ok(c) = decode_token(&raw) {
                if c.expires_at.after(env.now()) {
                    token = c.access_token;
                }
            }
        }
    }
    if token.is_empty() {
        let command = arg(s, 0);
        // The command comes from the project's own .http file and only
        // runs when apic.yaml opts in with auth.allowExec; see the gate
        // above.
        let out =
            command::output(cancel, &env.getenv("PATH"), command, &s.args[1..]).map_err(|f| {
                Error::msg(format!(
                    "@auth exec {}: {}: {}",
                    command,
                    f.message,
                    f.stderr.trim()
                ))
            })?;
        token = String::from_utf8_lossy(&out).trim().to_string();
        if token.is_empty() {
            return Err(Error::msg(format!(
                "@auth exec {command}: command printed nothing"
            )));
        }
        if let (true, Some(cache)) = (caching, &env.cache) {
            let entry = CachedToken {
                access_token: token.clone(),
                expires_at: env.now() + ttl,
                ..CachedToken::default()
            };
            cache
                .set(&key, &encode_token(&entry))
                .map_err(Error::Cache)?;
        }
    }
    if !prefix.is_empty() {
        token = format!("{prefix} {token}");
    }
    req.headers.set(header, &token);
    Ok(())
}

/// A short, stable name for a cache entry: the start of the SHA-256 of
/// what identifies it.
pub(crate) fn hash_key(s: &str) -> String {
    let sum = Sha256::digest(s.as_bytes());
    hex(&sum[..8])
}

/// Lower-case hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
