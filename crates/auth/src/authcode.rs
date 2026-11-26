//! The OAuth2 authorization code grant with PKCE (RFC 6749 section 4.1 and
//! RFC 7636): the flow behind "sign in as yourself" for user-facing APIs.
//! It needs a browser once: apic listens on a loopback port, sends the
//! person to the provider's authorization URL, receives the code on the
//! redirect, and exchanges it for a token, which is then cached and
//! refreshed like the other grants. It is a human flow by design: under
//! --json, in MCP or without a terminal it never starts, and the request
//! fails with a message saying to run it once interactively.

use std::process::{Command, Stdio};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use apic_gocompat::{format_duration, quote};
use apic_httpc::CancelToken;
use apic_httpd::{Handler, Listener, Mux, Response, Server};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::apply::Env;
use crate::error::Error;
use crate::oauth2::{token_request, TokenResponse};
use crate::spec::{atoi, Spec};
use crate::urlutil::{parse_query, path_unescape, split_url, Form};

/// How long apic waits for the browser to come back.
const AUTH_CODE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The message of [`Error::NotInteractive`]: the grant needs a browser and
/// no person is there to use one.
pub const ERR_NOT_INTERACTIVE: &str = "oauth2: grant=authorization_code needs a browser sign-in and no cached token exists; run the request once interactively (apic run <id>) to sign in, and the token is cached and refreshed from then on";

/// What the redirect brought back.
type Outcome = Result<String, String>;

pub(crate) fn authorization_code(
    cancel: &CancelToken,
    s: &Spec,
    env: &Env,
) -> Result<TokenResponse, Error> {
    if !env.interactive {
        return Err(Error::NotInteractive);
    }
    let (verifier, challenge) = pkce()?;
    let state = random_token(16)?;
    // The spec's check validated redirectPort and redirectUrl when the
    // spec was parsed. A redirectUrl is sent as written, since the
    // provider matches it exactly; apic listens on its port and path.
    let mut port = atoi(s.option("redirectPort"))
        .and_then(|n| u16::try_from(n).ok())
        .unwrap_or(0);
    let mut path = "/callback".to_string();
    let redirect_url = s.option("redirectUrl");
    if !redirect_url.is_empty() {
        (port, path) = redirect_target(redirect_url).unwrap_or((0, String::new()));
    }
    let listener = Listener::bind(&format!("127.0.0.1:{port}"))
        .map_err(|e| Error::msg(format!("oauth2: listening for the redirect: {e}")))?;
    let redirect_uri = if redirect_url.is_empty() {
        format!("http://{}{}", listener.addr(), path)
    } else {
        redirect_url.to_string()
    };

    let mut q = Form::new(&[
        ("response_type", "code"),
        ("client_id", s.option("clientId")),
        ("redirect_uri", &redirect_uri),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]);
    let sc = s.option("scope");
    if !sc.is_empty() {
        q.set("scope", sc);
    }
    let a = s.option("audience");
    if !a.is_empty() {
        q.set("audience", a);
    }
    let auth_url = authorization_url(s.option("authUrl"), &q)?;

    let (done, arrived) = sync_channel::<Outcome>(1);
    let mut mux: Mux<Handler> = Mux::new();
    let want_state = state.clone();
    mux.handle_fn(&path, move |r| {
        let param = |name: &str| r.query_get(name).unwrap_or_default();
        let out: Outcome = if param("state") != want_state {
            Err("oauth2: the redirect carried the wrong state; someone else may have sent it, so the sign-in was refused".to_string())
        } else if !param("error").is_empty() {
            Err(format!(
                "oauth2: the provider refused: {} {}",
                param("error"),
                param("error_description")
            ))
        } else if param("code").is_empty() {
            Err("oauth2: the redirect carried no code".to_string())
        } else {
            Ok(param("code"))
        };
        let resp = match &out {
            Err(e) => Response::html(
                400,
                format!(
                    "<!doctype html><title>apic</title><p>Sign-in failed: {}</p>",
                    html_escape(e)
                ),
            ),
            Ok(_) => Response::html(
                200,
                "<!doctype html><title>apic</title><p>Signed in. You can close this tab and go back to the terminal.</p>",
            ),
        };
        // Only the first answer counts.
        let _ = done.try_send(out);
        resp
    })
    .map_err(|e| Error::msg(format!("oauth2: listening for the redirect: {e}")))?;
    // Serves until this function returns.
    let _server = Server::start(listener, mux.into_handler());

    env.prompt(&format!(
        "\nTo sign in, open {auth_url}\n(the redirect URI is {redirect_uri}; register it with the provider if it asks)\nWaiting for the browser..."
    ));
    // The URL is printed either way, so a machine without a browser can
    // still copy it somewhere that has one.
    let _ = match &env.open_browser {
        Some(opener) => opener(&auth_url),
        None => open_browser(&auth_url),
    };

    let deadline = Instant::now() + AUTH_CODE_TIMEOUT;
    let res = loop {
        match arrived.recv_timeout(Duration::from_millis(50)) {
            Ok(out) => break Some(out),
            Err(RecvTimeoutError::Disconnected) => break None,
            Err(RecvTimeoutError::Timeout) => {
                if cancel.is_cancelled() || Instant::now() >= deadline {
                    break None;
                }
            }
        }
    };
    env.prompt("\n");
    let code = match res {
        None => {
            return Err(Error::msg(format!(
                "oauth2: no sign-in arrived within {}",
                format_duration(AUTH_CODE_TIMEOUT)
            )))
        }
        Some(Err(e)) => return Err(Error::Message(e)),
        Some(Ok(code)) => code,
    };
    let form = Form::new(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("code_verifier", &verifier),
    ]);
    token_request(cancel, s, env, form, Some(deadline)).map_err(|e| e.err)
}

/// The authorization URL: `auth_url` with the request's parameters added
/// to its query, replacing any of the same name it already had.
fn authorization_url(auth_url: &str, params: &Form) -> Result<String, Error> {
    let bad = || Error::msg(format!("oauth2: bad authUrl {}", quote(auth_url)));
    if auth_url.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(bad());
    }
    let parts = split_url(auth_url);
    if has_bad_escape(parts.path) {
        return Err(bad());
    }
    let mut query = Form::default();
    for (k, v) in parse_query(parts.query.unwrap_or("")) {
        query.add(&String::from_utf8_lossy(&k), &String::from_utf8_lossy(&v));
    }
    query.replace_with(params);
    let base = match auth_url.find(['?', '#']) {
        Some(i) => &auth_url[..i],
        None => auth_url,
    };
    let mut out = format!("{}?{}", base, query.encode());
    if let Some(f) = parts.fragment {
        out.push('#');
        out.push_str(f);
    }
    Ok(out)
}

/// Whether a `%` is not followed by two hexadecimal digits.
fn has_bad_escape(s: &str) -> bool {
    let b = s.as_bytes();
    (0..b.len()).any(|i| {
        b[i] == b'%'
            && !(b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
    })
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            c => out.push(c),
        }
    }
    out
}

/// Returns a code verifier and its S256 challenge (RFC 7636).
pub(crate) fn pkce() -> Result<(String, String), Error> {
    let verifier = random_token(32)?;
    let sum = Sha256::digest(verifier.as_bytes());
    Ok((verifier, URL_SAFE_NO_PAD.encode(sum)))
}

fn random_token(n: usize) -> Result<String, Error> {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).map_err(|e| Error::msg(format!("oauth2: random: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(b))
}

/// Asks the system to open a URL: xdg-open, open or rundll32. Failure is
/// not fatal; the URL is always printed as well.
pub fn open_browser(url: &str) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Reads a redirectUrl: plain http on this machine (localhost, 127.0.0.1
/// or [::1]) with a port, since apic is the one listening. It returns the
/// port and the path to answer on.
pub(crate) fn redirect_target(raw: &str) -> Result<(u16, String), String> {
    let not_http = || {
        format!(
            "redirectUrl {} must be an http:// URL on this machine",
            quote(raw)
        )
    };
    if raw.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(not_http());
    }
    let u = split_url(raw);
    if u.scheme != "http" {
        return Err(not_http());
    }
    let (hostname, port) = match u.host.strip_prefix('[') {
        Some(rest) => {
            let Some((name, after)) = rest.split_once(']') else {
                return Err(not_http());
            };
            (name, after.strip_prefix(':'))
        }
        None => match u.host.rsplit_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (u.host, None),
        },
    };
    if port.is_some_and(|p| !p.bytes().all(|b| b.is_ascii_digit())) {
        return Err(not_http());
    }
    match hostname {
        "localhost" | "127.0.0.1" | "::1" => {}
        _ => {
            return Err(format!(
                "redirectUrl {} must point at localhost, where apic listens for the sign-in",
                quote(raw)
            ))
        }
    }
    let port = port
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p >= 1)
        .ok_or_else(|| format!("redirectUrl {} needs a port", quote(raw)))?;
    let path = match path_unescape(u.path) {
        p if p.is_empty() => "/".to_string(),
        p => p,
    };
    Ok((port, path))
}
