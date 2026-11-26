use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use apic_gocompat::{check_json, format_duration, indent_json, json_string, path_error, Time};
use serde_json::Value;

use crate::cookiejar::{default_path, CookieJar, RequestCookie, RequestUrl};
use crate::session::{ensure_dir, env_key, json_type, read_optional, write_private, Error, DIR};
use crate::setcookie::{read_set_cookies, valid_cookie_value_byte, SetCookie};

/// The file that holds the cookie jar, beside session.json.
pub const COOKIE_FILE: &str = "cookies.json";

/// One stored cookie. Domain, path and host-only are the jar's view after
/// the RFC 6265 defaults were applied, so a saved cookie is sent to the
/// same hosts after a restart as before it. No expiry means the cookie
/// lives until `apic session clear`: apic keeps a login's session cookie
/// for the same reason it keeps a captured token.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires: Option<Time>,
    pub secure: bool,
    pub http_only: bool,
    pub host_only: bool,
}

impl Cookie {
    fn key(&self) -> String {
        format!("{}|{}|{}", self.domain, self.path, self.name)
    }

    /// A request URL inside the cookie's scope, for replaying it into a
    /// jar and for asking the jar whether it holds it.
    fn url(&self) -> RequestUrl {
        RequestUrl {
            scheme: if self.secure { "https" } else { "http" }.to_string(),
            host: self.domain.clone(),
            path: self.path.clone(),
        }
    }

    /// The cookie as the server set it, for replaying it into a jar.
    fn set_cookie(&self) -> SetCookie {
        SetCookie {
            name: self.name.clone(),
            value: self.value.clone(),
            quoted: false,
            path: self.path.clone(),
            domain: if self.host_only {
                String::new()
            } else {
                self.domain.clone()
            },
            expires: self.expires,
            max_age: 0,
            secure: self.secure,
            http_only: self.http_only,
        }
    }

    fn unexpired(&self, now: Time) -> bool {
        self.expires.is_none_or(|t| t.after(now))
    }

    /// Describes when the cookie expires, for listings.
    pub fn expiry_text(&self, now: Time) -> String {
        let Some(expires) = self.expires else {
            return "until cleared".to_string();
        };
        // The difference in nanoseconds, held to what a signed 64-bit
        // count can, then rounded to the second, halves away from zero.
        let d = expires
            .nanos_since(now)
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
        const SECOND: i64 = 1_000_000_000;
        let r = d % SECOND;
        let rounded = if d < 0 {
            d - r - if -r * 2 >= SECOND { SECOND } else { 0 }
        } else if r * 2 < SECOND {
            d - r
        } else {
            (d - r).checked_add(SECOND).unwrap_or(i64::MAX)
        };
        if rounded <= 0 {
            return "expired".to_string();
        }
        format!(
            "expires in {}",
            format_duration(Duration::from_nanos(rounded as u64))
        )
    }

    fn to_json(&self) -> String {
        let mut out = format!(
            "{{\"name\":{},\"value\":{},\"domain\":{},\"path\":{}",
            json_string(&self.name, true),
            json_string(&self.value, true),
            json_string(&self.domain, true),
            json_string(&self.path, true)
        );
        if let Some(t) = self.expires {
            out.push_str(&format!(",\"expires\":\"{}\"", t.format_rfc3339_nano()));
        }
        for (key, on) in [
            ("secure", self.secure),
            ("http_only", self.http_only),
            ("host_only", self.host_only),
        ] {
            if on {
                out.push_str(&format!(",\"{key}\":true"));
            }
        }
        out.push('}');
        out
    }

    fn from_json(v: &Value) -> Result<Cookie, String> {
        let mismatch = |field: &str, v: &Value, want: &str| {
            format!(
                "json: cannot unmarshal {} into Go struct field Cookie.envs.{} of type {}",
                json_type(v),
                field,
                want
            )
        };
        let obj = match v {
            Value::Null => return Ok(Cookie::default()),
            Value::Object(obj) => obj,
            other => {
                return Err(format!(
                    "json: cannot unmarshal {} into Go struct field .envs of type session.Cookie",
                    json_type(other)
                ))
            }
        };
        let text = |field: &str| -> Result<String, String> {
            match obj.get(field) {
                None | Some(Value::Null) => Ok(String::new()),
                Some(Value::String(s)) => Ok(s.clone()),
                Some(other) => Err(mismatch(field, other, "string")),
            }
        };
        let flag = |field: &str| -> Result<bool, String> {
            match obj.get(field) {
                None | Some(Value::Null) => Ok(false),
                Some(Value::Bool(b)) => Ok(*b),
                Some(other) => Err(mismatch(field, other, "bool")),
            }
        };
        let expires = match obj.get("expires") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                let t = Time::parse_rfc3339(s)?;
                // The first instant of year 1 is how "no expiry" was
                // written before the field was left out.
                (t.unix() != -62_135_596_800 || t.nanosecond() != 0).then_some(t)
            }
            Some(_) => return Err("Time.UnmarshalJSON: input is not a JSON string".to_string()),
        };
        Ok(Cookie {
            name: text("name")?,
            value: text("value")?,
            domain: text("domain")?,
            path: text("path")?,
            expires,
            secure: flag("secure")?,
            http_only: flag("http_only")?,
            host_only: flag("host_only")?,
        })
    }
}

type Clock = Box<dyn Fn() -> Time + Send>;

/// The cookies one [`EnvJar`] works with while the process runs.
#[derive(Default)]
struct Live {
    inner: CookieJar,
    /// What the server set, in the order it was first set: the engine
    /// cannot be enumerated for saving.
    records: Vec<Cookie>,
}

struct State {
    envs: BTreeMap<String, Vec<Cookie>>,
    live: HashMap<String, Arc<Mutex<Live>>>,
    now: Clock,
    /// Something changed since the last save.
    dirty: bool,
}

struct Shared {
    path: Option<PathBuf>,
    state: Mutex<State>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The cookie jar of a project, one set of cookies per environment, stored
/// in `.apic/cookies.json` next to the session. The behaviour is RFC
/// 6265's, without a public-suffix list: a project that talks to one API
/// has no need for one, and it keeps the binary small.
///
/// Cloning a jar gives another handle on the same cookies.
#[derive(Clone)]
pub struct Jar {
    shared: Arc<Shared>,
}

fn unexpired(cookies: &[Cookie], now: Time) -> Vec<Cookie> {
    cookies
        .iter()
        .filter(|c| c.unexpired(now))
        .cloned()
        .collect()
}

impl Jar {
    /// A jar that is never written to disk.
    pub fn new_memory() -> Jar {
        Jar::with_path(None)
    }

    fn with_path(path: Option<PathBuf>) -> Jar {
        Jar {
            shared: Arc::new(Shared {
                path,
                state: Mutex::new(State {
                    envs: BTreeMap::new(),
                    live: HashMap::new(),
                    now: Box::new(Time::now),
                    dirty: false,
                }),
            }),
        }
    }

    /// Loads the jar for a project root, or an empty one.
    pub fn open(root: &Path) -> Result<Jar, Error> {
        let path = root.join(DIR).join(COOKIE_FILE);
        let jar = Jar::with_path(Some(path.clone()));
        let Some(data) = read_optional(&path)? else {
            return Ok(jar);
        };
        let fail = |msg: String| Error(format!("{}: {}", path.display(), msg));
        check_json(&data).map_err(fail)?;
        let doc: Value = serde_json::from_slice(&data).map_err(|e| fail(format!("json: {e}")))?;
        let envs = match &doc {
            Value::Null => None,
            Value::Object(o) => o.get("envs"),
            other => {
                return Err(fail(format!(
                    "json: cannot unmarshal {} into Go value of type struct {{ Envs map[string][]session.Cookie \"json:\\\"envs\\\"\" }}",
                    json_type(other)
                )))
            }
        };
        let envs = match envs {
            None | Some(Value::Null) => return Ok(jar),
            Some(Value::Object(envs)) => envs,
            Some(other) => {
                return Err(fail(format!(
                    "json: cannot unmarshal {} into Go struct field .envs of type map[string][]session.Cookie",
                    json_type(other)
                )))
            }
        };
        {
            let mut state = jar.shared.state();
            let now = (state.now)();
            for (env, cookies) in envs {
                let list = match cookies {
                    Value::Null => Vec::new(),
                    Value::Array(list) => list.iter().map(Cookie::from_json).collect::<Result<Vec<_>, _>>().map_err(fail)?,
                    other => {
                        return Err(fail(format!(
                            "json: cannot unmarshal {} into Go struct field .envs of type []session.Cookie",
                            json_type(other)
                        )))
                    }
                };
                state.envs.insert(env.clone(), unexpired(&list, now));
            }
        }
        Ok(jar)
    }

    /// Replaces the jar's clock, which says what has expired. Tests pin it.
    pub fn set_now(&self, now: impl Fn() -> Time + Send + 'static) {
        self.shared.state().now = Box::new(now);
    }

    /// The cookies of `env` as an HTTP client uses them: it says what to
    /// send with a request and records what a response sets, so that it
    /// can be saved.
    pub fn http(&self, env: &str) -> EnvJar {
        let mut state = self.shared.state();
        let k = env_key(env);
        if let Some(live) = state.live.get(&k) {
            return EnvJar {
                parent: self.shared.clone(),
                env: k,
                live: live.clone(),
            };
        }
        let now = (state.now)();
        let mut live = Live::default();
        for c in state.envs.get(&k).into_iter().flatten() {
            match live.records.iter_mut().find(|r| r.key() == c.key()) {
                Some(r) => *r = c.clone(),
                None => live.records.push(c.clone()),
            }
            live.inner.set_cookies(&c.url(), &[c.set_cookie()], now);
        }
        let live = Arc::new(Mutex::new(live));
        state.live.insert(k.clone(), live.clone());
        EnvJar {
            parent: self.shared.clone(),
            env: k,
            live,
        }
    }

    /// The unexpired cookies stored for `env`, sorted by domain, path and
    /// name.
    pub fn cookies(&self, env: &str) -> Vec<Cookie> {
        let state = self.shared.state();
        let now = (state.now)();
        let mut out = unexpired(
            state.envs.get(&env_key(env)).map_or(&[][..], Vec::as_slice),
            now,
        );
        out.sort_by(|a, b| (&a.domain, &a.path, &a.name).cmp(&(&b.domain, &b.path, &b.name)));
        out
    }

    /// The environments that have cookies, sorted.
    pub fn env_names(&self) -> Vec<String> {
        let state = self.shared.state();
        let now = (state.now)();
        state
            .envs
            .iter()
            .filter(|(_, cookies)| cookies.iter().any(|c| c.unexpired(now)))
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Drops the cookies of `env`, or of every env when `env` is "*".
    pub fn clear(&self, env: &str) {
        let mut state = self.shared.state();
        state.dirty = true;
        if env == "*" {
            state.envs.clear();
            state.live.clear();
            return;
        }
        let k = env_key(env);
        state.envs.remove(&k);
        state.live.remove(&k);
    }

    /// Writes the jar to disk with the same protections as the session
    /// file. A memory-only jar is a no-op, and so is a jar nothing has
    /// changed in.
    pub fn save(&self) -> Result<(), Error> {
        let Some(path) = &self.shared.path else {
            return Ok(());
        };
        let mut state = self.shared.state();
        if !state.dirty {
            return Ok(());
        }
        state.dirty = false;
        let now = (state.now)();
        let mut compact = String::from("{\"envs\":{");
        let mut any = false;
        for (env, cookies) in &state.envs {
            let live: Vec<String> = cookies
                .iter()
                .filter(|c| c.unexpired(now))
                .map(Cookie::to_json)
                .collect();
            if live.is_empty() {
                continue;
            }
            if any {
                compact.push(',');
            }
            any = true;
            compact.push_str(&json_string(env, true));
            compact.push_str(&format!(":[{}]", live.join(",")));
        }
        compact.push_str("}}");
        if !any {
            return match fs::remove_file(path) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => {
                    Err(Error(path_error("remove", path, &err)))
                }
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            ensure_dir(dir)?;
        }
        let mut data = indent_json(&compact, "", "  ");
        data.push('\n');
        write_private(path, data.as_bytes()).map_err(|err| Error(path_error("open", path, &err)))
    }
}

/// The cookies of one environment, as an HTTP client uses them. Before a
/// request (and before each redirect it follows) the client asks
/// [`EnvJar::cookie_header`] what to send; after each response it hands
/// the response's `Set-Cookie` lines to [`EnvJar::set_cookie_headers`].
/// Every cookie the server sets is recorded so the jar can be saved.
#[derive(Clone)]
pub struct EnvJar {
    parent: Arc<Shared>,
    env: String,
    live: Arc<Mutex<Live>>,
}

impl EnvJar {
    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> Time {
        (self.parent.state().now)()
    }

    /// The cookies to send with a request to `u`, in the order to send
    /// them.
    pub fn cookies(&self, u: &RequestUrl) -> Vec<RequestCookie> {
        let now = self.now();
        self.live().inner.cookies(u, now)
    }

    /// The `Cookie` header value for a request to `u`: the jar's cookies
    /// as `name=value` pairs joined with `; `, after `existing` (the value
    /// the request already carries) when there is one. None when the jar
    /// has nothing to add.
    pub fn cookie_header(&self, u: &RequestUrl, existing: Option<&str>) -> Option<String> {
        let cookies = self.cookies(u);
        if cookies.is_empty() {
            return None;
        }
        let mut header = existing.unwrap_or("").to_string();
        for c in cookies {
            if !header.is_empty() {
                header.push_str("; ");
            }
            header.push_str(&c.name.replace(['\n', '\r'], "-"));
            header.push('=');
            header.push_str(&sanitize_cookie_value(&c.value, c.quoted));
        }
        Some(header)
    }

    /// Stores what the `Set-Cookie` lines of a response to a request for
    /// `u` set. A response without any is nothing to store.
    pub fn set_cookie_headers<S: AsRef<str>>(&self, u: &RequestUrl, lines: &[S]) {
        let cookies = read_set_cookies(lines);
        if !cookies.is_empty() {
            self.set_cookies(u, &cookies);
        }
    }

    /// Stores the cookies a response to a request for `u` set, and records
    /// the ones the jar accepted so they survive the process.
    pub fn set_cookies(&self, u: &RequestUrl, cookies: &[SetCookie]) {
        let now = self.now();
        let mut live = self.live();
        live.inner.set_cookies(u, cookies, now);
        for c in cookies {
            let mut rec = Cookie {
                name: c.name.clone(),
                value: c.value.clone(),
                secure: c.secure,
                http_only: c.http_only,
                ..Cookie::default()
            };
            rec.domain = c
                .domain
                .strip_prefix('.')
                .unwrap_or(&c.domain)
                .to_lowercase();
            if rec.domain.is_empty() {
                rec.domain = u.hostname().to_lowercase();
                rec.host_only = true;
            }
            rec.path = c.path.clone();
            if !rec.path.starts_with('/') {
                rec.path = default_path(&u.path);
            }
            let key = rec.key();
            if c.max_age < 0 {
                live.records.retain(|r| r.key() != key);
                continue;
            } else if c.max_age > 0 {
                rec.expires = Some(now.add_nanos(i128::from(c.max_age) * 1_000_000_000));
            } else {
                rec.expires = c.expires;
            }
            if !rec.unexpired(now) {
                live.records.retain(|r| r.key() != key);
                continue;
            }
            // Only what the jar accepted (domain rules, secure scheme) is
            // worth keeping: ask it back for the cookie's own scope.
            let accepted = live
                .inner
                .cookies(&rec.url(), now)
                .iter()
                .any(|sent| sent.name == rec.name && sent.value == rec.value);
            if !accepted {
                continue;
            }
            match live.records.iter_mut().find(|r| r.key() == key) {
                Some(r) => *r = rec,
                None => live.records.push(rec),
            }
        }
        let mut state = self.parent.state();
        state.envs.insert(self.env.clone(), live.records.clone());
        state.dirty = true;
    }
}

/// A cookie value as it is sent: without the bytes a value cannot hold,
/// and in double quotes when it has a space or a comma, or was set in
/// them.
fn sanitize_cookie_value(v: &str, quoted: bool) -> String {
    let v: String = if v.bytes().all(valid_cookie_value_byte) {
        v.to_string()
    } else {
        v.bytes()
            .filter(|b| valid_cookie_value_byte(*b))
            .map(char::from)
            .collect()
    };
    if v.contains([' ', ',']) || quoted {
        return format!("\"{v}\"");
    }
    v
}
