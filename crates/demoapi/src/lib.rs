//! Backs `apic demo`: a tiny, fully in-memory HTTP API and its matching
//! example .http project (embedded from `project/`), so apic can be tried
//! with no network access and no git clone.
//!
//! [`new`] returns the API as an [`apic_httpd::Handler`], with its own
//! todos and jobs; [`write_project`] writes the example project into a
//! directory.

#![forbid(unsafe_code)]

mod project;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use apic_gocompat::{json_string, Time};
use apic_httpd::{Handler, Mux, Request, Response};
use serde_json::Value;

pub use project::write_project;

const DEMO_USER: &str = "alice";
const DEMO_PASSWORD: &str = "s3cret";
const DEMO_CLIENT_ID: &str = "demo-client";
const DEMO_CLIENT_SECRET: &str = "demo-secret";
const DEMO_API_KEY: &str = "demo-key";

/// Caps GET /slow so a typo cannot park a request for an hour.
const MAX_SLOW_MILLIS: i64 = 10_000;
/// The page size of GET /todos when limit is not given.
const DEFAULT_PAGE_SIZE: i64 = 20;
/// Caps what limit= may ask for.
const MAX_PAGE_SIZE: i64 = 100;

static VERSION: RwLock<String> = RwLock::new(String::new());

/// Sets the version GET /health reports. The CLI sets it to the apic
/// version; it is `dev` otherwise.
pub fn set_version(version: &str) {
    *VERSION.write().unwrap_or_else(PoisonError::into_inner) = version.to_string();
}

/// The version GET /health reports.
pub fn version() -> String {
    let v = VERSION.read().unwrap_or_else(PoisonError::into_inner);
    if v.is_empty() {
        "dev".to_string()
    } else {
        v.clone()
    }
}

#[derive(Clone)]
struct Todo {
    id: String,
    title: String,
    done: bool,
}

impl Todo {
    fn json(&self) -> String {
        format!(
            "{{\"id\":{},\"title\":{},\"done\":{}}}",
            string(&self.id),
            string(&self.title),
            self.done
        )
    }
}

/// A background task whose state advances on every poll, so a request
/// file can show "wait until done" without a real queue behind it.
struct Job {
    id: String,
    state: &'static str,
    polls: u32,
}

impl Job {
    fn json(&self) -> String {
        format!(
            "{{\"id\":{},\"state\":\"{}\"}}",
            string(&self.id),
            self.state
        )
    }
}

struct State {
    todos: BTreeMap<i64, Todo>,
    next_id: i64,
    jobs: BTreeMap<String, Job>,
    next_job: i64,
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn string(s: &str) -> String {
    json_string(s, true)
}

/// A JSON response: the body and a newline.
fn json(status: u16, body: impl AsRef<str>) -> Response {
    Response::json(status, format!("{}\n", body.as_ref()))
}

fn error(status: u16, message: &str) -> Response {
    json(status, format!("{{\"error\":{}}}", string(message)))
}

/// The first JSON value of a body, when it is an object; whatever follows
/// the value is not read.
fn body_object(r: &Request) -> Option<serde_json::Map<String, Value>> {
    let mut values = serde_json::Deserializer::from_slice(&r.body).into_iter::<Value>();
    match values.next() {
        Some(Ok(Value::Object(obj))) => Some(obj),
        _ => None,
    }
}

/// A member of a request body by name, whatever its letter case; an exact
/// match wins.
fn member<'a>(obj: &'a serde_json::Map<String, Value>, name: &str) -> Option<&'a Value> {
    obj.get(name).or_else(|| {
        obj.iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    })
}

/// A string member: `None` when it is absent or null, and the empty
/// string when it is some other kind of value.
fn string_member(obj: Option<&serde_json::Map<String, Value>>, name: &str) -> Option<String> {
    match member(obj?, name)? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        _ => Some(String::new()),
    }
}

/// A boolean member: `None` when it is absent or null, and `false` when
/// it is some other kind of value.
fn bool_member(obj: Option<&serde_json::Map<String, Value>>, name: &str) -> Option<bool> {
    match member(obj?, name)? {
        Value::Null => None,
        other => Some(other.as_bool().unwrap_or(false)),
    }
}

fn require_bearer(
    next: impl Fn(&Request) -> Response + Send + Sync + 'static,
) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |r: &Request| {
        let auth = r.header("Authorization").unwrap_or("");
        if !auth.starts_with("Bearer ") || auth == "Bearer " {
            return error(401, "unauthenticated");
        }
        next(r)
    }
}

/// Parses a query value as a positive integer, else returns `def`.
fn positive_int(s: Option<String>, def: i64) -> i64 {
    match s.as_deref().map(str::parse::<i64>) {
        Some(Ok(n)) if n >= 1 => n,
        _ => def,
    }
}

/// Reads `true` and `false` in the spellings a query string may use.
fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Escapes a string for a query component: a space is `+`, and every byte
/// outside the unreserved set is `%XX`.
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A query string with its keys sorted, each key's values in the order
/// they were given.
fn encode_query(pairs: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted
        .iter()
        .map(|(k, v)| format!("{}={}", query_escape(k), query_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The todos in id order, optionally filtered by done.
fn list_todos(state: &Shared, done: Option<bool>) -> Vec<Todo> {
    lock(state)
        .todos
        .values()
        .filter(|t| done.is_none_or(|d| t.done == d))
        .cloned()
        .collect()
}

fn todos_json(todos: &[Todo]) -> String {
    let items: Vec<String> = todos.iter().map(Todo::json).collect();
    format!("[{}]", items.join(","))
}

fn graphql_errors(message: &str) -> String {
    format!("{{\"errors\":[{{\"message\":{}}}]}}", string(message))
}

/// `GET /todos`: one page of the todos, with the paging headers.
fn todos_page(st: &Shared, r: &Request) -> Response {
    let mut done = None;
    if let Some(v) = r.query_get("done").filter(|v| !v.is_empty()) {
        match parse_bool(&v) {
            Some(b) => done = Some(b),
            None => return error(400, "done must be true or false"),
        }
    }
    let page = positive_int(r.query_get("page"), 1);
    let limit = positive_int(r.query_get("limit"), DEFAULT_PAGE_SIZE).min(MAX_PAGE_SIZE);
    let all = list_todos(st, done);
    let len = all.len() as i64;
    // Bound the window without multiplying first: a huge page
    // must not overflow.
    let start = if page - 1 <= len / limit {
        ((page - 1) * limit).min(len)
    } else {
        len
    };
    let end = (start + limit).min(len);
    let mut resp = Response::new(200)
        .with_header("X-Total-Count", &len.to_string())
        .with_header("X-Page", &page.to_string());
    if end < len {
        let mut next: Vec<(String, String)> = r
            .query()
            .into_iter()
            .filter(|(k, _)| k != "page" && k != "limit")
            .collect();
        next.push(("page".to_string(), (page + 1).to_string()));
        next.push(("limit".to_string(), limit.to_string()));
        resp = resp.with_header(
            "Link",
            &format!("</todos?{}>; rel=\"next\"", encode_query(&next)),
        );
    }
    resp.with_header("Content-Type", "application/json")
        .with_body(format!(
            "{}\n",
            todos_json(&all[start as usize..end as usize])
        ))
}

/// `POST /upload`: echoes the parts of a multipart form.
fn upload(r: &Request) -> Response {
    let parts = match r.multipart() {
        Ok(parts) => parts,
        Err(err) => {
            return error(400, &format!("expected multipart/form-data: {err}"));
        }
    };
    let mut fields: BTreeMap<&str, String> = BTreeMap::new();
    let mut files: Vec<(&str, String)> = Vec::new();
    for part in &parts {
        match part.filename.as_deref().filter(|f| !f.is_empty()) {
            None => {
                fields
                    .entry(part.name.as_str())
                    .or_insert_with(|| String::from_utf8_lossy(&part.data).into_owned());
            }
            Some(filename) => {
                let mut file = format!(
                    "{{\"field\":{},\"filename\":{},\"size\":{}",
                    string(&part.name),
                    string(filename),
                    part.data.len()
                );
                if let Some(ct) = part.headers.get("Content-Type").filter(|c| !c.is_empty()) {
                    file.push_str(&format!(",\"content_type\":{}", string(ct)));
                }
                file.push('}');
                files.push((part.name.as_str(), file));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(b.0));
    let fields: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("{}:{}", string(k), string(v)))
        .collect();
    let files: Vec<String> = files.into_iter().map(|(_, f)| f).collect();
    json(
        201,
        format!(
            "{{\"fields\":{{{}}},\"files\":[{}]}}",
            fields.join(","),
            files.join(",")
        ),
    )
}

/// `POST /graphql`: the demo schema's two fields.
fn graphql(st: &Shared, r: &Request) -> Response {
    let bad = || json(400, graphql_errors("expected a JSON body with a query"));
    let Some(body) = body_object(r) else {
        return bad();
    };
    let query = match member(&body, "query") {
        None | Some(Value::Null) => "",
        Some(Value::String(q)) => q.as_str(),
        Some(_) => return bad(),
    };
    let variables = match member(&body, "variables") {
        None | Some(Value::Null) => None,
        Some(Value::Object(vars)) => Some(vars),
        Some(_) => return bad(),
    };
    if query.trim().is_empty() {
        return bad();
    }
    let mut data: Vec<String> = Vec::new();
    if query.contains("health") {
        data.push("\"health\":\"ok\"".to_string());
    }
    if query.contains("todos") {
        let done = variables
            .and_then(|v| v.get("done"))
            .and_then(Value::as_bool);
        data.push(format!("\"todos\":{}", todos_json(&list_todos(st, done))));
    }
    if data.is_empty() {
        return json(
            200,
            graphql_errors("unknown field: the demo schema has todos(done: Boolean) and health"),
        );
    }
    json(200, format!("{{\"data\":{{{}}}}}", data.join(",")))
}

/// Returns the demo API as a handler.
///
/// Routes (bearer means `Authorization: Bearer <anything>`, as `login`
/// returns):
///
/// ```text
/// POST /auth/login                        {"user","password"} -> {"access_token"}
/// GET  /me                                bearer
/// GET  /basic-auth/{user}/{password}      HTTP basic auth
/// POST /oauth/token                       client-credentials grant
/// GET  /keyed                             X-Api-Key: demo-key
/// GET  /todos?done=&page=&limit=          bearer; X-Total-Count and Link headers
/// POST /todos                             bearer; 422 with field errors on an empty title
/// GET|PUT|DELETE /todos/{id}              bearer; 404 when unknown
/// POST /jobs, GET /jobs/{id}              bearer; state goes queued -> running -> done, one step per poll
/// POST /upload                            multipart/form-data, echoes the parts
/// POST /graphql                           bearer; {"query","variables"} with a todos(done:) field
/// GET  /reports/daily.csv                 text/csv
/// GET  /slow?ms=                          sleeps, capped at 10 s
/// GET  /status/{code}, GET /redirect      any status; a 302 to /status/200
/// GET  /health                            version and uptime, no auth
/// ```
pub fn new() -> Handler {
    let mut mux = Mux::new();
    let started = Instant::now();
    let seed = |id: i64, title: &str, done| {
        (
            id,
            Todo {
                id: id.to_string(),
                title: title.to_string(),
                done,
            },
        )
    };
    let state: Shared = Arc::new(Mutex::new(State {
        todos: BTreeMap::from([
            seed(1, "Buy milk", false),
            seed(2, "Read the apic docs", true),
        ]),
        next_id: 3,
        jobs: BTreeMap::new(),
        next_job: 1,
    }));
    // A todo's key is its id as a number; an id that is not one names
    // nothing.
    let key = |r: &Request| {
        r.path_value("id")
            .parse::<i64>()
            .ok()
            .filter(|n| n.to_string() == r.path_value("id"))
    };
    let mut route = |pattern: &str, h: Box<dyn Fn(&Request) -> Response + Send + Sync>| {
        mux.handle(pattern, h).expect("a valid route");
    };

    route(
        "POST /auth/login",
        Box::new(|r| {
            let body = body_object(r);
            let user = string_member(body.as_ref(), "user").unwrap_or_default();
            let password = string_member(body.as_ref(), "password").unwrap_or_default();
            if user != DEMO_USER || password != DEMO_PASSWORD {
                return error(401, "bad credentials");
            }
            json(200, r#"{"access_token":"mock-token"}"#)
        }),
    );

    route(
        "GET /me",
        Box::new(require_bearer(|_| {
            json(200, format!("{{\"email\":\"{DEMO_USER}@example.com\"}}"))
        })),
    );

    route(
        "GET /basic-auth/{user}/{password}",
        Box::new(|r| match r.basic_auth() {
            Some((user, pass))
                if user == r.path_value("user") && pass == r.path_value("password") =>
            {
                json(200, r#"{"authenticated":true}"#)
            }
            _ => error(401, "bad credentials"),
        }),
    );

    route(
        "POST /oauth/token",
        Box::new(|r| {
            if r.form_value("client_id") != DEMO_CLIENT_ID
                || r.form_value("client_secret") != DEMO_CLIENT_SECRET
            {
                return error(401, "invalid_client");
            }
            json(
                200,
                r#"{"access_token":"mock-oauth-token","expires_in":3600}"#,
            )
        }),
    );

    route(
        "GET /keyed",
        Box::new(|r| {
            if r.header("X-Api-Key") != Some(DEMO_API_KEY) {
                return error(401, "missing or bad X-Api-Key");
            }
            json(200, format!("{{\"key\":\"{DEMO_API_KEY}\",\"ok\":true}}"))
        }),
    );

    let st = state.clone();
    route(
        "GET /todos",
        Box::new(require_bearer(move |r| todos_page(&st, r))),
    );

    let st = state.clone();
    route(
        "POST /todos",
        Box::new(require_bearer(move |r| {
            let body = body_object(r);
            let title = string_member(body.as_ref(), "title").unwrap_or_default();
            if title.trim().is_empty() {
                return json(
                    422,
                    r#"{"error":"validation failed","fields":{"title":"must not be empty"}}"#,
                );
            }
            let mut state = lock(&st);
            let id = state.next_id;
            state.next_id += 1;
            let todo = Todo {
                id: id.to_string(),
                title,
                done: false,
            };
            state.todos.insert(id, todo.clone());
            json(201, todo.json())
        })),
    );

    let st = state.clone();
    route(
        "GET /todos/{id}",
        Box::new(require_bearer(move |r| {
            let found = key(r).and_then(|id| lock(&st).todos.get(&id).cloned());
            match found {
                Some(todo) => json(200, todo.json()),
                None => error(404, "not found"),
            }
        })),
    );

    let st = state.clone();
    route(
        "PUT /todos/{id}",
        Box::new(require_bearer(move |r| {
            let mut state = lock(&st);
            let Some(todo) = key(r).and_then(|id| state.todos.get_mut(&id)) else {
                return error(404, "not found");
            };
            let body = body_object(r);
            if let Some(title) = string_member(body.as_ref(), "title") {
                todo.title = title;
            }
            if let Some(done) = bool_member(body.as_ref(), "done") {
                todo.done = done;
            }
            json(200, todo.json())
        })),
    );

    let st = state.clone();
    route(
        "DELETE /todos/{id}",
        Box::new(require_bearer(move |r| {
            if let Some(id) = key(r) {
                lock(&st).todos.remove(&id);
            }
            Response::new(204)
        })),
    );

    let st = state.clone();
    route(
        "POST /jobs",
        Box::new(require_bearer(move |_| {
            let mut state = lock(&st);
            let id = state.next_job.to_string();
            state.next_job += 1;
            let job = Job {
                id: id.clone(),
                state: "queued",
                polls: 0,
            };
            let body = job.json();
            state.jobs.insert(id.clone(), job);
            json(202, body).with_header("Location", &format!("/jobs/{id}"))
        })),
    );

    let st = state.clone();
    route(
        "GET /jobs/{id}",
        Box::new(require_bearer(move |r| {
            let mut state = lock(&st);
            let Some(job) = state.jobs.get_mut(r.path_value("id")) else {
                return error(404, "not found");
            };
            // The first poll sees it running, the second sees it done, so
            // a request file can demonstrate polling deterministically.
            job.polls += 1;
            job.state = if job.polls >= 2 { "done" } else { "running" };
            json(200, job.json())
        })),
    );

    route("POST /upload", Box::new(upload));

    let st = state.clone();
    route(
        "POST /graphql",
        Box::new(require_bearer(move |r| graphql(&st, r))),
    );

    let st = state.clone();
    route(
        "GET /reports/daily.csv",
        Box::new(move |_| {
            let all = list_todos(&st, None);
            let done = all.iter().filter(|t| t.done).count();
            let today = Time::now().utc().civil();
            Response::new(200)
                .with_header("Content-Type", "text/csv; charset=utf-8")
                .with_header("Content-Disposition", "attachment; filename=\"daily.csv\"")
                .with_body(format!(
                    "date,total,done,open\n{:04}-{:02}-{:02},{},{},{}\n",
                    today.year,
                    today.month,
                    today.day,
                    all.len(),
                    done,
                    all.len() - done
                ))
        }),
    );

    route(
        "GET /slow",
        Box::new(|r| {
            let ms = positive_int(r.query_get("ms"), 1000).min(MAX_SLOW_MILLIS);
            std::thread::sleep(Duration::from_millis(ms as u64));
            json(200, format!("{{\"slept_ms\":{ms}}}"))
        }),
    );

    route(
        "GET /health",
        Box::new(move |_| {
            json(
                200,
                format!(
                    "{{\"service\":\"apic-demo\",\"status\":\"ok\",\"uptime_seconds\":{},\"version\":{}}}",
                    started.elapsed().as_secs(),
                    string(&version())
                ),
            )
        }),
    );

    route(
        "GET /status/{code}",
        Box::new(|r| {
            let code = r
                .path_value("code")
                .parse::<u16>()
                .ok()
                .filter(|c| (100..=999).contains(c))
                .unwrap_or(200);
            json(code, format!("{{\"status\":{code}}}"))
        }),
    );

    route(
        "GET /redirect",
        Box::new(|r| Response::redirect(r, "/status/200", 302)),
    );

    mux.into_handler()
}

#[cfg(test)]
mod tests;
