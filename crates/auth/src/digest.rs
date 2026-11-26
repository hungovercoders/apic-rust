//! HTTP Digest authentication (RFC 7616, which obsoletes RFC 2617),
//! written from the RFC in the style of the SigV4 signer: no dependency.
//! The server challenges with WWW-Authenticate: Digest and the client
//! answers with a hash over the credentials, the nonce and the request,
//! so the password never travels. apic answers the challenge inside one
//! run of a request, and remembers the challenge so later requests to the
//! same server in the same invocation authenticate on the first try, with
//! the nonce count going up as the RFC asks.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use apic_httpc::{Error, Request, Response, RoundTrip};
use md5::Md5;
use sha2::{Digest, Sha256};

use crate::apply::hex;
use crate::urlutil::split_url;

/// One parsed `Digest` challenge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    pub opaque: String,
    /// MD5, MD5-sess, SHA-256, SHA-256-sess (upper case).
    pub algorithm: String,
    pub qop: Vec<String>,
    pub stale: bool,
    pub userhash: bool,
    pub charset: String,
}

/// Reads every Digest challenge from the WWW-Authenticate values, in
/// order.
pub(crate) fn parse_digest_challenges(values: &[&str]) -> Vec<DigestChallenge> {
    let mut out = Vec::new();
    for v in values {
        for raw in split_challenges(v) {
            let raw = raw.trim();
            let (scheme, params) = raw.split_once(' ').unwrap_or((raw, ""));
            if !scheme.eq_ignore_ascii_case("Digest") {
                continue;
            }
            let mut c = DigestChallenge {
                algorithm: "MD5".to_string(),
                ..DigestChallenge::default()
            };
            for (k, val) in parse_auth_params(params) {
                match k.to_lowercase().as_str() {
                    "realm" => c.realm = val,
                    "nonce" => c.nonce = val,
                    "opaque" => c.opaque = val,
                    "algorithm" => c.algorithm = val.to_uppercase(),
                    "qop" => {
                        for q in val.split(',') {
                            let q = q.trim();
                            if !q.is_empty() {
                                c.qop.push(q.to_lowercase());
                            }
                        }
                    }
                    "stale" => c.stale = val.eq_ignore_ascii_case("true"),
                    "userhash" => c.userhash = val.eq_ignore_ascii_case("true"),
                    "charset" => c.charset = val,
                    _ => {}
                }
            }
            if !c.nonce.is_empty() {
                out.push(c);
            }
        }
    }
    out
}

/// Separates the challenges in one header value, which RFC 7235 lets a
/// server list with commas, without cutting inside the quoted,
/// comma-bearing parameter values.
fn split_challenges(v: &str) -> Vec<String> {
    let b = v.as_bytes();
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut in_quote = false;
    for i in 0..b.len() {
        let c = b[i];
        if c == b'"' && (i == 0 || b[i - 1] != b'\\') {
            in_quote = !in_quote;
            cur.push(c);
        } else if c == b',' && !in_quote {
            // A comma starts a new challenge only when a scheme name (a
            // word followed by a space, not '=') follows.
            let rest = v[i + 1..].trim();
            let word = rest.split(' ').next().unwrap_or("");
            if !word.is_empty() && !word.contains('=') && rest.contains(' ') {
                out.push(String::from_utf8_lossy(&cur).into_owned());
                cur.clear();
                continue;
            }
            cur.push(c);
        } else {
            cur.push(c);
        }
    }
    let last = String::from_utf8_lossy(&cur);
    let last = last.trim();
    if !last.is_empty() {
        out.push(last.to_string());
    }
    out
}

/// Reads `k=v, k="quoted, value"` pairs.
pub(crate) fn parse_auth_params(s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut s = s;
    while !s.is_empty() {
        s = s.trim_start_matches([' ', '\t', ',']);
        let Some(eq) = s.find('=') else {
            break;
        };
        let key = s[..eq].trim().to_string();
        s = s[eq + 1..].trim_start_matches([' ', '\t']);
        let val;
        if s.starts_with('"') {
            let b = s.as_bytes();
            let mut buf: Vec<u8> = Vec::new();
            let mut i = 1;
            while i < b.len() {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                    buf.push(b[i]);
                    i += 1;
                    continue;
                }
                if b[i] == b'"' {
                    break;
                }
                buf.push(b[i]);
                i += 1;
            }
            val = String::from_utf8_lossy(&buf).into_owned();
            if i < b.len() {
                i += 1;
            }
            s = &s[i..];
        } else {
            let end = s.find(',').unwrap_or(s.len());
            val = s[..end].trim().to_string();
            s = &s[end..];
        }
        out.insert(key, val);
    }
    out
}

/// The hashes a challenge may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DigestHash {
    Md5,
    Sha256,
}

/// Returns the hash the challenge names, or `None` when apic does not
/// implement it (RFC 7616 also lists SHA-512-256, which few servers offer;
/// it is skipped in favour of the next challenge).
pub(crate) fn digest_hash(algorithm: &str) -> Option<DigestHash> {
    match algorithm.strip_suffix("-SESS").unwrap_or(algorithm) {
        "MD5" => Some(DigestHash::Md5),
        "SHA-256" => Some(DigestHash::Sha256),
        _ => None,
    }
}

pub(crate) fn hex_hash(h: DigestHash, data: &[u8]) -> String {
    match h {
        DigestHash::Md5 => hex(&Md5::digest(data)),
        DigestHash::Sha256 => hex(&Sha256::digest(data)),
    }
}

/// Computes the response field of RFC 7616 section 3.4.1 for one request,
/// and returns the Authorization header value; `None` when the challenge
/// names an algorithm apic does not implement.
#[allow(clippy::too_many_arguments)]
pub(crate) fn digest_response(
    c: &DigestChallenge,
    user: &str,
    pass: &str,
    method: &str,
    uri: &str,
    body: &[u8],
    cnonce: &str,
    nc: u32,
) -> Option<String> {
    let h = digest_hash(&c.algorithm)?;
    let hash = |s: &str| hex_hash(h, s.as_bytes());
    // qop: prefer auth; auth-int only when the server offers nothing
    // else or the request carries a body to protect.
    let has_auth = c.qop.iter().any(|q| q == "auth");
    let has_auth_int = c.qop.iter().any(|q| q == "auth-int");
    let qop = if has_auth_int && (!body.is_empty() || !has_auth) {
        "auth-int"
    } else if has_auth {
        "auth"
    } else {
        ""
    };
    let nc_text = format!("{nc:08x}");
    let mut a1 = hash(&format!("{}:{}:{}", user, c.realm, pass));
    if c.algorithm.ends_with("-SESS") {
        a1 = hash(&format!("{}:{}:{}", a1, c.nonce, cnonce));
    }
    let mut a2 = format!("{method}:{uri}");
    if qop == "auth-int" {
        a2.push(':');
        a2.push_str(&hex_hash(h, body));
    }
    let ha2 = hash(&a2);
    let response = if qop.is_empty() {
        hash(&format!("{}:{}:{}", a1, c.nonce, ha2))
    } else {
        hash(&format!(
            "{}:{}:{}:{}:{}:{}",
            a1, c.nonce, nc_text, cnonce, qop, ha2
        ))
    };
    let username = if c.userhash {
        hash(&format!("{}:{}", user, c.realm))
    } else {
        user.to_string()
    };
    let mut b = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\"",
        quote_param(&username),
        quote_param(&c.realm),
        quote_param(&c.nonce),
        quote_param(uri),
        response
    );
    if c.userhash {
        b.push_str(", userhash=true");
    }
    b.push_str(&format!(", algorithm={}", c.algorithm));
    if !qop.is_empty() {
        b.push_str(&format!(", qop={qop}, nc={nc_text}, cnonce=\"{cnonce}\""));
    }
    if !c.opaque.is_empty() {
        b.push_str(&format!(", opaque=\"{}\"", quote_param(&c.opaque)));
    }
    Some(b)
}

fn quote_param(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `scheme://host` of a URL, the host in lower case: how a
/// [`DigestTransport`] names the server a request addresses.
pub fn digest_host(url: &str) -> String {
    split_url(url).origin()
}

/// Remembers the challenges seen in one invocation, per server, so a
/// second request to the same server sends its Authorization on the first
/// try with the nonce count incremented. A runner holds one.
#[derive(Debug, Default)]
pub struct DigestState {
    seen: Mutex<HashMap<String, DigestEntry>>, // by scheme://host
}

#[derive(Debug)]
struct DigestEntry {
    challenge: DigestChallenge,
    nc: u32,
}

impl DigestState {
    /// Returns an empty state.
    pub fn new() -> DigestState {
        DigestState::default()
    }

    fn seen(&self) -> MutexGuard<'_, HashMap<String, DigestEntry>> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, key: &str) -> Option<(DigestChallenge, u32)> {
        let mut seen = self.seen();
        let e = seen.get_mut(key)?;
        e.nc += 1;
        Some((e.challenge.clone(), e.nc))
    }

    pub(crate) fn set(&self, key: &str, c: DigestChallenge) -> u32 {
        self.seen().insert(
            key.to_string(),
            DigestEntry {
                challenge: c,
                nc: 1,
            },
        );
        1
    }
}

/// What a transport has done so far.
#[derive(Debug, Default)]
struct Progress {
    host: String,
    rounds: u32,
    challenged: bool,
    answered: bool,
}

/// Answers a server's Digest challenge on behalf of one request: it sends
/// the request, and on a 401 that carries a Digest challenge it sends it
/// once more with the Authorization header computed from the credentials.
/// It is a layer over a client, used wherever the client would be.
pub struct DigestTransport {
    base: Box<dyn RoundTrip>,
    pub user: String,
    pub pass: String,
    /// The challenges seen this invocation; `None` means every request is
    /// challenged.
    pub state: Option<Arc<DigestState>>,
    progress: Mutex<Progress>,
    /// Tests fix it.
    pub(crate) cnonce: Option<Box<dyn Fn() -> String>>,
}

impl DigestTransport {
    /// A transport that sends over `base` and answers with these
    /// credentials. It is pinned to the first host it sends to, and keeps
    /// no state, until [`with_host`](Self::with_host) and
    /// [`with_state`](Self::with_state) say otherwise.
    pub fn new(base: Box<dyn RoundTrip>, user: &str, pass: &str) -> DigestTransport {
        DigestTransport {
            base,
            user: user.to_string(),
            pass: pass.to_string(),
            state: None,
            progress: Mutex::new(Progress::default()),
            cnonce: None,
        }
    }

    /// Pins the transport to the `scheme://host` the request addresses
    /// (see [`digest_host`]); a challenge from any other host (one a
    /// redirect led to) is left unanswered, since the credentials belong
    /// to the host the file names. Without it the transport is pinned to
    /// the first request it sends.
    pub fn with_host(self, host: &str) -> DigestTransport {
        self.progress().host = host.to_string();
        self
    }

    /// Shares the challenges seen this invocation.
    pub fn with_state(mut self, state: Arc<DigestState>) -> DigestTransport {
        self.state = Some(state);
        self
    }

    fn progress(&self) -> MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The `scheme://host` the transport answers; empty until it is
    /// pinned.
    pub fn host(&self) -> String {
        self.progress().host.clone()
    }

    /// How many requests the transport has sent.
    pub fn rounds(&self) -> u32 {
        self.progress().rounds
    }

    /// Whether a 401 was answered.
    pub fn challenged(&self) -> bool {
        self.progress().challenged
    }

    /// Whether the Authorization header went out (on the first or the
    /// second try).
    pub fn answered(&self) -> bool {
        self.progress().answered
    }

    /// Returns a copy of `req` with the Digest Authorization header for
    /// the challenge.
    fn authorized(&self, req: &Request, c: &DigestChallenge, nc: u32) -> Option<Request> {
        let cnonce = match &self.cnonce {
            Some(f) => f(),
            None => random_cnonce(),
        };
        let uri = split_url(&req.url).request_uri();
        let value = digest_response(
            c,
            &self.user,
            &self.pass,
            &req.method,
            &uri,
            &req.body,
            &cnonce,
            nc,
        )?;
        let mut out = req.clone();
        out.headers.set("Authorization", &value);
        Some(out)
    }
}

impl RoundTrip for DigestTransport {
    fn round_trip(&self, req: &Request) -> Result<Response, Error> {
        let key = digest_host(&req.url);
        let pinned = {
            let mut p = self.progress();
            if p.host.is_empty() {
                p.host = key.clone();
            }
            p.rounds += 1;
            p.host == key
        };
        if !pinned {
            return self.base.round_trip(req);
        }
        let mut pre: Option<DigestChallenge> = None;
        let mut first = None;
        if let Some((c, nc)) = self.state.as_ref().and_then(|s| s.get(&key)) {
            if let Some(r) = self.authorized(req, &c, nc) {
                first = Some(r);
                pre = Some(c);
                self.progress().answered = true;
            }
        }
        let resp = self.base.round_trip(first.as_ref().unwrap_or(req))?;
        if resp.status_code != 401 {
            return Ok(resp);
        }
        let challenges = parse_digest_challenges(&resp.headers.values("WWW-Authenticate"));
        let Some(chosen) = challenges
            .into_iter()
            .find(|c| digest_hash(&c.algorithm).is_some())
        else {
            return Ok(resp);
        };
        // A stale nonce, or a server that wants a fresh challenge
        // answered, is the one case a second request is worth sending; a
        // 401 to a correct answer would be the wrong password, which the
        // caller sees.
        if pre.is_some_and(|pre| !chosen.stale && pre.nonce == chosen.nonce) {
            return Ok(resp);
        }
        let nc = match &self.state {
            Some(state) => state.set(&key, chosen.clone()),
            None => 1,
        };
        let Some(retry) = self.authorized(req, &chosen, nc) else {
            return Ok(resp);
        };
        {
            let mut p = self.progress();
            p.rounds += 1;
            p.challenged = true;
            p.answered = true;
        }
        let mut answer = self.base.round_trip(&retry)?;
        answer.timings = resp.timings.then(&answer.timings);
        Ok(answer)
    }
}

fn random_cnonce() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    hex(&b)
}
