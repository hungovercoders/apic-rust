use std::time::{Duration, Instant};

use apic_gocompat::{format_duration, json_string, Time};
use apic_httpc::{CancelToken, Request};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{Map, Value};

use crate::apply::{hash_key, Env};
use crate::authcode::authorization_code;
use crate::error::Error;
use crate::spec::Spec;
use crate::urlutil::Form;

const SECOND: i128 = 1_000_000_000;

/// What apic keeps in the session for oauth2 and exec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedToken {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: Time,
}

/// The time an entry without `expires_at` expires at: long ago.
fn zero_time() -> Time {
    Time::from_civil(1, 1, 1, 0, 0, 0, 0, 0)
}

impl Default for CachedToken {
    fn default() -> CachedToken {
        CachedToken {
            access_token: String::new(),
            refresh_token: String::new(),
            id_token: String::new(),
            expires_at: zero_time(),
        }
    }
}

pub(crate) fn encode_token(t: &CachedToken) -> String {
    let mut out = format!("{{\"access_token\":{}", json_string(&t.access_token, true));
    if !t.refresh_token.is_empty() {
        out.push_str(&format!(
            ",\"refresh_token\":{}",
            json_string(&t.refresh_token, true)
        ));
    }
    if !t.id_token.is_empty() {
        out.push_str(&format!(",\"id_token\":{}", json_string(&t.id_token, true)));
    }
    out.push_str(&format!(
        ",\"expires_at\":\"{}\"}}",
        t.expires_at.format_rfc3339_nano()
    ));
    out
}

/// A field of a JSON object by name: the exact name first, then any
/// spelling that differs only in case. A null counts as absent.
fn field<'a>(obj: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    obj.get(name)
        .or_else(|| {
            obj.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
        .filter(|v| !v.is_null())
}

fn string_field(obj: &Map<String, Value>, name: &str) -> Result<String, ()> {
    match field(obj, name) {
        None => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(()),
    }
}

fn int_field(obj: &Map<String, Value>, name: &str) -> Result<i64, ()> {
    match field(obj, name) {
        None => Ok(0),
        Some(v) => v.as_i64().ok_or(()),
    }
}

/// Reads JSON text that must be an object, or null (an empty object).
fn object(text: &str) -> Result<Map<String, Value>, ()> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(obj)) => Ok(obj),
        Ok(Value::Null) => Ok(Map::new()),
        _ => Err(()),
    }
}

pub(crate) fn decode_token(s: &str) -> Result<CachedToken, ()> {
    let obj = object(s)?;
    let expires_at = match field(&obj, "expires_at") {
        None => zero_time(),
        Some(Value::String(text)) => Time::parse_rfc3339(text).map_err(|_| ())?,
        Some(_) => return Err(()),
    };
    Ok(CachedToken {
        access_token: string_field(&obj, "access_token")?,
        refresh_token: string_field(&obj, "refresh_token")?,
        id_token: string_field(&obj, "id_token")?,
        expires_at,
    })
}

/// Renders a cached token entry for `apic session`.
pub fn describe_cached(raw: &str, now: Time) -> String {
    let Ok(t) = decode_token(raw) else {
        return "(unreadable)".to_string();
    };
    let left = t
        .expires_at
        .nanos_since(now)
        .clamp(i128::from(i64::MIN), i128::from(i64::MAX));
    // To the nearest second, halves away from zero.
    let left = if left >= 0 {
        (left + SECOND / 2) / SECOND
    } else {
        (left - SECOND / 2) / SECOND
    };
    if left <= 0 {
        return "expired".to_string();
    }
    format!(
        "token, expires in {}",
        format_duration(Duration::from_secs(left as u64))
    )
}

/// Identifies the token cache entry for a rendered oauth2 spec.
pub fn cache_key(s: &Spec) -> String {
    let mut parts = vec![
        s.option("tokenUrl"),
        s.option("deviceUrl"),
        s.option("clientId"),
        s.option("clientSecret"),
        s.option("clientAuth"),
        s.grant(),
        s.option("scope"),
        s.option("username"),
        s.option("password"),
        s.option("audience"),
    ];
    // Only the authorization code grant has an authUrl; adding it at the
    // end, and only then, keeps every existing cache entry valid.
    let a = s.option("authUrl");
    if !a.is_empty() {
        parts.push(a);
    }
    format!("$oauth2:{}", hash_key(&parts.join("\x00")))
}

/// How close to its expiry a token is no longer used.
const SKEW: i128 = 60 * SECOND;

/// What an OAuth2 grant returned: the access token, and the ID token when
/// the provider sent one (an OpenID Connect scope).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    pub id: String,
}

/// Runs a rendered oauth2 spec's grant, or reuses the cached result, as
/// `# @auth oauth2` does. JetBrains' {{$auth.token("name")}} and
/// {{$auth.idToken("name")}} read it. `need_id` says the caller wants the
/// ID token, so a cached entry without one is not enough.
pub fn oauth2_tokens(
    cancel: &CancelToken,
    s: &Spec,
    env: &Env,
    need_id: bool,
) -> Result<Tokens, Error> {
    let t = tokens(cancel, s, env, need_id)?;
    Ok(Tokens {
        access: t.access_token,
        id: t.id_token,
    })
}

pub(crate) fn oauth2_token(cancel: &CancelToken, s: &Spec, env: &Env) -> Result<String, Error> {
    Ok(tokens(cancel, s, env, false)?.access_token)
}

fn tokens(cancel: &CancelToken, s: &Spec, env: &Env, need_id: bool) -> Result<CachedToken, Error> {
    let key = cache_key(s);
    let mut cached = CachedToken::default();
    if let Some(raw) = env.cache.as_ref().and_then(|c| c.get(&key)) {
        if let Ok(c) = decode_token(&raw) {
            if !c.access_token.is_empty()
                && c.expires_at.after(env.now().add_nanos(SKEW))
                && (!need_id || !c.id_token.is_empty())
            {
                return Ok(c);
            }
            cached = c;
        }
    }
    let mut tok = None;
    if !cached.refresh_token.is_empty() {
        let form = Form::new(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &cached.refresh_token),
        ]);
        // A refresh that fails, or brings no ID token when one is wanted,
        // falls through to a fresh grant.
        tok = token_request(cancel, s, env, form, None)
            .ok()
            .filter(|t| !need_id || !t.id_token.is_empty());
    }
    let tok = match tok {
        Some(tok) => tok,
        None => fresh_grant(cancel, s, env)?,
    };
    let lifetime = if tok.expires_in == 0 {
        3600 * SECOND
    } else {
        i128::from(tok.expires_in) * SECOND
    };
    let entry = CachedToken {
        access_token: tok.access_token,
        refresh_token: if tok.refresh_token.is_empty() {
            cached.refresh_token
        } else {
            tok.refresh_token
        },
        id_token: tok.id_token,
        expires_at: env.now().add_nanos(lifetime),
    };
    if let Some(cache) = &env.cache {
        cache
            .set(&key, &encode_token(&entry))
            .map_err(Error::Cache)?;
    }
    if need_id && entry.id_token.is_empty() {
        return Err(Error::msg(format!(
            "oauth2: {} returned no id_token (ask for the openid scope)",
            s.option("tokenUrl")
        )));
    }
    Ok(entry)
}

fn fresh_grant(cancel: &CancelToken, s: &Spec, env: &Env) -> Result<TokenResponse, Error> {
    match s.grant() {
        "client_credentials" => {
            let form = Form::new(&[("grant_type", "client_credentials")]);
            token_request(cancel, s, env, form, None).map_err(|e| e.err)
        }
        "password" => {
            let form = Form::new(&[
                ("grant_type", "password"),
                ("username", s.option("username")),
                ("password", s.option("password")),
            ]);
            token_request(cancel, s, env, form, None).map_err(|e| e.err)
        }
        "device_code" => device_code(cancel, s, env),
        "authorization_code" => authorization_code(cancel, s, env),
        other => Err(Error::msg(format!(
            "@auth oauth2: unknown grant {} (client_credentials, password, device_code or authorization_code)",
            apic_gocompat::quote(other)
        ))),
    }
}

/// What a token endpoint answered.
#[derive(Clone, Debug, Default)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_in: i64,
    pub error: String,
    pub error_description: String,
}

impl TokenResponse {
    fn parse(text: &str) -> Result<TokenResponse, ()> {
        let obj = object(text)?;
        // Read for its type only: a token_type that is not a string makes
        // the answer unreadable, like any other field.
        string_field(&obj, "token_type")?;
        Ok(TokenResponse {
            access_token: string_field(&obj, "access_token")?,
            refresh_token: string_field(&obj, "refresh_token")?,
            id_token: string_field(&obj, "id_token")?,
            expires_in: int_field(&obj, "expires_in")?,
            error: string_field(&obj, "error")?,
            error_description: string_field(&obj, "error_description")?,
        })
    }
}

/// A token request that failed. `provider_error` is the endpoint's own
/// `error` code when it answered with one, which the device flow reads.
#[derive(Debug)]
pub(crate) struct TokenError {
    pub provider_error: Option<String>,
    pub err: Error,
}

impl TokenError {
    fn new(text: String) -> TokenError {
        TokenError {
            provider_error: None,
            err: Error::Message(text),
        }
    }
}

/// A form POST that asks for JSON, cancelled with the caller.
fn form_post(cancel: &CancelToken, url: &str, form: &Form) -> Request {
    let mut req = Request::post(url, form.encode())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json");
    req.cancel = Some(cancel.clone());
    req
}

pub(crate) fn token_request(
    cancel: &CancelToken,
    s: &Spec,
    env: &Env,
    mut form: Form,
    deadline: Option<Instant>,
) -> Result<TokenResponse, TokenError> {
    let token_url = s.option("tokenUrl");
    let sc = s.option("scope");
    if !sc.is_empty() {
        form.set("scope", sc);
    }
    let a = s.option("audience");
    if !a.is_empty() {
        form.set("audience", a);
    }
    let mut basic = None;
    if s.option("clientAuth") == "basic" {
        basic = Some(format!(
            "Basic {}",
            STANDARD.encode(format!(
                "{}:{}",
                s.option("clientId"),
                s.option("clientSecret")
            ))
        ));
    } else {
        form.set("client_id", s.option("clientId"));
        let cs = s.option("clientSecret");
        if !cs.is_empty() {
            form.set("client_secret", cs);
        }
    }
    let mut req = form_post(cancel, token_url, &form);
    if let Some(basic) = basic {
        req.headers.set("Authorization", &basic);
    }
    req.deadline = deadline;
    let resp = env
        .send(&req)
        .map_err(|e| TokenError::new(format!("oauth2: token request: {e}")))?;
    let data = resp.text();
    let Ok(tok) = TokenResponse::parse(&data) else {
        return Err(TokenError::new(format!(
            "oauth2: {} returned {} with a non-JSON body: {}",
            token_url,
            resp.status_code,
            truncate(&data)
        )));
    };
    if !tok.error.is_empty() {
        return Err(TokenError {
            err: Error::msg(format!(
                "oauth2: {}: {} {}",
                token_url, tok.error, tok.error_description
            )),
            provider_error: Some(tok.error),
        });
    }
    if resp.status_code >= 300 || tok.access_token.is_empty() {
        return Err(TokenError::new(format!(
            "oauth2: {} returned {} without an access_token: {}",
            token_url,
            resp.status_code,
            truncate(&data)
        )));
    }
    Ok(tok)
}

/// What a device authorization endpoint answered.
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: i64,
    interval: i64,
}

impl DeviceResponse {
    fn parse(text: &str) -> Result<DeviceResponse, ()> {
        let obj = object(text)?;
        string_field(&obj, "error")?;
        Ok(DeviceResponse {
            device_code: string_field(&obj, "device_code")?,
            user_code: string_field(&obj, "user_code")?,
            verification_uri: string_field(&obj, "verification_uri")?,
            verification_uri_complete: string_field(&obj, "verification_uri_complete")?,
            expires_in: int_field(&obj, "expires_in")?,
            interval: int_field(&obj, "interval")?,
        })
    }
}

fn device_code(cancel: &CancelToken, s: &Spec, env: &Env) -> Result<TokenResponse, Error> {
    let mut form = Form::new(&[("client_id", s.option("clientId"))]);
    let sc = s.option("scope");
    if !sc.is_empty() {
        form.set("scope", sc);
    }
    let a = s.option("audience");
    if !a.is_empty() {
        form.set("audience", a);
    }
    let resp = env
        .send(&form_post(cancel, s.option("deviceUrl"), &form))
        .map_err(|e| Error::msg(format!("oauth2: device authorization: {e}")))?;
    let data = resp.text();
    let dev = match DeviceResponse::parse(&data) {
        Ok(dev) if !dev.device_code.is_empty() => dev,
        _ => {
            return Err(Error::msg(format!(
                "oauth2: device authorization failed ({}): {}",
                resp.status_code,
                truncate(&data)
            )))
        }
    };
    let uri = if dev.verification_uri_complete.is_empty() {
        &dev.verification_uri
    } else {
        &dev.verification_uri_complete
    };
    env.prompt(&format!(
        "\nTo sign in, open {} and enter the code {}\nWaiting for approval",
        uri, dev.user_code
    ));
    let mut interval = Duration::from_secs(dev.interval.max(1) as u64);
    let deadline = if dev.expires_in == 0 {
        env.now().add_nanos(300 * SECOND)
    } else {
        env.now().add_nanos(i128::from(dev.expires_in) * SECOND)
    };
    loop {
        if cancel.wait_timeout(interval) {
            return Err(Error::Cancelled);
        }
        if env.now().after(deadline) {
            return Err(Error::msg(
                "oauth2: device code expired before it was approved",
            ));
        }
        let form = Form::new(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &dev.device_code),
        ]);
        match token_request(cancel, s, env, form, None) {
            Ok(tok) => {
                env.prompt(" approved.\n");
                return Ok(tok);
            }
            Err(e) => match e.provider_error.as_deref() {
                Some("authorization_pending") => env.prompt("."),
                Some("slow_down") => interval += Duration::from_secs(5),
                _ => return Err(e.err),
            },
        }
    }
}

/// The start of a response body, for an error message.
fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.len() > 200 {
        let mut end = 200;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        return format!("{}…", &s[..end]);
    }
    s.to_string()
}
