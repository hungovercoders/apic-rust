use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};

use apic_httpc::{CancelToken, Client, Request as HttpRequest};
use apic_httpd::{Mux, Request, Response, TestServer};
use serde_json::{json, Value};

use crate::protocol::{decode_message, Session};
use crate::{check_bind, handler, new, read_file, serve_http, Config, Server};

fn write(dir: &Path, name: &str, contents: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn config(dir: &Path, env: &str) -> Config {
    Config {
        dir: dir.to_string_lossy().into_owned(),
        env: env.to_string(),
        version: "test".to_string(),
    }
}

/// A client on one session: sends a request, returns the response.
struct TestClient {
    session: Session,
    next_id: i64,
}

impl TestClient {
    fn connect(server: &Server) -> TestClient {
        let mut client = TestClient {
            session: Session::new(server.inner.clone()),
            next_id: 0,
        };
        let init = client.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "apic", "{init}");
        client.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        client
    }

    fn send(&self, line: &str) -> Option<String> {
        self.session.handle(&decode_message(line).unwrap())
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let line =
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        let response = self.send(&line.to_string()).expect("a response");
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], self.next_id);
        response
    }

    /// Calls a tool and returns its whole result.
    fn call_tool(&mut self, name: &str, args: Value) -> Value {
        let response = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert!(response["error"].is_null(), "{name}: {response}");
        response["result"].clone()
    }

    /// Calls a tool and returns what its text decodes to, or
    /// `{"_error": text}` when the tool reports an error.
    fn call(&mut self, name: &str, args: Value) -> Value {
        let result = self.call_tool(name, args);
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if result["isError"] == true {
            return json!({"_error": text});
        }
        serde_json::from_str(&text).unwrap()
    }
}

fn api_server() -> TestServer {
    let mut mux = Mux::new();
    mux.handle_fn("POST /login", |_| Response::json(200, r#"{"token":"t-1"}"#))
        .unwrap();
    mux.handle_fn("GET /me", |r: &Request| {
        if r.header("Authorization") != Some("Bearer t-1") {
            return Response::new(401);
        }
        Response::new(200).with_body(r#"{"name":"alice"}"#)
    })
    .unwrap();
    TestServer::new(mux.into_handler())
}

const API_HTTP: &str = r#"
### login
# @name login
# @capture token = body.$.token
POST {{baseUrl}}/login

### me
# @name me
# @assert status == 200
# @assert body.$.name == alice
GET {{baseUrl}}/me
Authorization: Bearer {{token}}

### me, logging in by itself
# @name me-ref
# @ref login
# @assert status == 200
GET {{baseUrl}}/me
Authorization: Bearer {{token}}
"#;

fn error_text(v: &Value) -> &str {
    v["_error"].as_str().unwrap_or("")
}

#[test]
fn server_end_to_end() {
    let srv = api_server();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(dir, "api.http", API_HTTP);
    write(
        dir,
        "http-client.env.json",
        &format!(r#"{{"dev":{{"baseUrl":"{}"}}}}"#, srv.url()),
    );

    let server = new(config(dir, "dev")).unwrap();
    let mut c = TestClient::connect(&server);

    let tools = c.request("tools/list", json!({}));
    assert_eq!(
        tools["result"]["tools"].as_array().unwrap().len(),
        9,
        "{tools}"
    );

    let list = c.call("list_requests", json!({}));
    assert_eq!(
        list["requests"].as_array().unwrap().len(),
        3,
        "list: {list}"
    );
    // A request with # @ref logs in by itself and reports what ran first.
    let me_ref = c.call("run_request", json!({"name": "me-ref"}));
    let ran_first = me_ref["ran_first"].as_array().cloned().unwrap_or_default();
    assert!(
        me_ref["ok"] == true && ran_first.len() == 1 && ran_first[0]["ok"] == true,
        "me-ref: {me_ref}"
    );
    c.call("clear_session", json!({}));
    let desc = c.call("describe_request", json!({"name": "me"}));
    assert_eq!(
        desc["ready"], false,
        "me should not be ready before login: {desc}"
    );
    let e = c.call("run_request", json!({"name": "me"}));
    assert!(
        error_text(&e).contains("login"),
        "want missing-variable hint, got {e}"
    );
    // A failed call carries the catalogue entry as structured content,
    // and its text ends with the code.
    let res = c.call_tool("run_request", json!({"name": "me"}));
    let info = &res["structuredContent"]["error"];
    assert!(
        res["isError"] == true
            && info["code"] == "E101"
            && info["exit"] == 2
            && !info["hint"].as_str().unwrap_or("").is_empty(),
        "structured error: {res}"
    );
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("(E101 missing variable: "),
        "text should end with the code: {text}"
    );
    let login = c.call("run_request", json!({"name": "login"}));
    assert!(
        login["ok"] == true && login["captures"]["token"] == "t-1",
        "login: {login}"
    );
    let me = c.call("run_request", json!({"name": "me"}));
    assert_eq!(me["ok"], true, "me: {me}");
    let e = c.call(
        "run_request",
        json!({"name": "me", "vars": {"token": "bad"}}),
    );
    assert_eq!(e["ok"], false, "bad token should fail asserts: {e}");
    let flow = c.call("run_file", json!({"file": "api.http"}));
    assert!(
        flow["ok"] == true && flow["results"].as_array().unwrap().len() == 3,
        "flow: {flow}"
    );
    // The listing carries the # @ref targets.
    for r in list["requests"].as_array().unwrap() {
        if r["id"] == "me-ref" {
            assert_eq!(r["refs"], json!(["login"]), "me-ref refs: {r}");
        }
    }
    let valid = c.call("validate_project", json!({}));
    assert!(
        valid["ok"] == true
            && valid["files"] == 1
            && valid["requests"] == 3
            && valid["diagnostics"].as_array().unwrap().is_empty(),
        "validate_project: {valid}"
    );
    write(
        dir,
        "bad.http",
        "### bad\n# @name bad\n# @assert stauts == 200\nGET {{baseUrl}}/x\n",
    );
    let valid = c.call("validate_project", json!({}));
    assert!(
        valid["ok"] == false && valid["diagnostics"].as_array().unwrap().len() == 1,
        "validate_project after a bad file: {valid}"
    );
    let d = &valid["diagnostics"][0];
    assert!(
        d["path"] == "bad.http"
            && d["line"] == 3
            && d["column"] == 11
            && d["code"] == "unknown-selector",
        "diagnostic: {d}"
    );
    fs::remove_file(dir.join("bad.http")).unwrap();
    // Masked by default, live only when asked, so the command is safe to
    // paste into a log or a ticket as the tool hands it over.
    let curl = c.call("curl_request", json!({"name": "me"}));
    let cmd = curl["command"].as_str().unwrap_or("");
    assert!(
        cmd.starts_with("curl -sS")
            && !cmd.contains("t-1")
            && cmd.contains("***")
            && cmd.contains(&format!("{}/me", srv.url())),
        "curl_request: {curl}"
    );
    let curl = c.call("curl_request", json!({"name": "me", "raw": true}));
    assert!(
        curl["command"]
            .as_str()
            .unwrap_or("")
            .contains("Authorization: Bearer t-1"),
        "curl_request raw: {curl}"
    );
    let py = c.call("curl_request", json!({"name": "me", "lang": "python"}));
    let code = py["command"].as_str().unwrap_or("");
    assert!(
        py["lang"] == "python" && code.contains("requests.request(") && !code.contains("t-1"),
        "curl_request python: {py}"
    );
    let e = c.call("curl_request", json!({"name": "me", "lang": "cobol"}));
    assert!(
        error_text(&e).contains("unknown language"),
        "curl_request cobol: {e}"
    );
    // A missing variable is an error naming it, never a command with a
    // placeholder left in.
    c.call("clear_session", json!({}));
    let e = c.call("curl_request", json!({"name": "me"}));
    assert!(
        error_text(&e).contains("login"),
        "curl_request with a missing variable: {e}"
    );
    c.call("run_request", json!({"name": "login"}));
    let e = c.call("curl_request", json!({"name": "nope"}));
    assert!(error_text(&e).contains("nope"), "curl_request unknown: {e}");
    let envs = c.call("list_environments", json!({}));
    assert_eq!(envs["current"], "dev", "envs: {envs}");
    c.call("clear_session", json!({}));
    let d = c.call("describe_request", json!({"name": "me"}));
    assert_eq!(d["ready"], false, "session should be cleared");

    write(
        dir,
        "features/me.feature",
        r#"
Feature: Me
  Scenario: Login then me
    Given I run "login"
    When I run "me"
    Then the response status is 200
    And the response body "$.name" is "alice"
  Scenario: Fails
    When I run "login"
    Then the response status is 500
"#,
    );
    let feat = c.call("run_features", json!({}));
    assert!(
        feat["ok"] == false && feat["passed"] == 1 && feat["failed"] == 1 && feat["exit_code"] == 1,
        "run_features: {feat}"
    );
    // Isolated by default: the feature's login capture never reaches the
    // shared session.
    let d = c.call("describe_request", json!({"name": "me"}));
    assert_eq!(
        d["ready"], false,
        "run_features must not touch the shared session by default"
    );
    let shared = c.call(
        "run_features",
        json!({"use_session": true, "tags": "~@none"}),
    );
    assert_eq!(
        shared["scenarios"], 2,
        "run_features with use_session: {shared}"
    );
    let d = c.call("describe_request", json!({"name": "me"}));
    assert_eq!(
        d["ready"], true,
        "use_session must share captures with the other tools: {d}"
    );
    c.call("clear_session", json!({}));
    // A transport failure still returns the summary of what ran, with
    // the error and exit code.
    write(
        dir,
        "http-client.env.json",
        &format!(
            r#"{{"dev":{{"baseUrl":"{}"}},"down":{{"baseUrl":"http://127.0.0.1:1"}}}}"#,
            srv.url()
        ),
    );
    let down = c.call("run_features", json!({"env": "down"}));
    assert!(
        down["ok"] == false
            && down["exit_code"] == 3
            && down["scenarios"] == 2
            && down["error"]
                .as_str()
                .unwrap_or("")
                .contains("request failed"),
        "run_features transport: {down}"
    );
    let f = &feat["failures"][0];
    assert!(
        f["scenario"] == "Fails" && f["error"].as_str().unwrap_or("").contains("status == 500"),
        "failure detail: {f}"
    );

    let uri = format!(
        "file://{}",
        dir.join("api.http").to_string_lossy().replace('\\', "/")
    );
    let rr = c.request("resources/read", json!({"uri": uri}));
    assert!(
        rr["result"]["contents"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("@name login"),
        "resource content: {rr}"
    );
}

#[cfg(unix)]
#[test]
fn read_file_rejects_path_traversal() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.txt", "x");
    let link = root.path().join("leak.txt");
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), &link).unwrap();
    let err = read_file(
        &root.path().to_string_lossy(),
        &format!("file://{}", link.to_string_lossy()),
    )
    .unwrap_err();
    assert!(
        err.contains("outside project"),
        "expected outside-project rejection, got {err}"
    );
}

/// The resource reader is not a general "any file under the root" reader.
/// The private env file, .env and the session cache all live under the
/// project root and hold credentials.
#[test]
fn read_file_serves_only_project_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "api.http", "# @name get\nGET https://example.com/x\n");
    let secrets = [
        (
            "http-client.private.env.json",
            r#"{"dev":{"token":"super-secret"}}"#,
        ),
        (".env", "API_KEY=super-secret\n"),
        (".apic/session.json", r#"{"dev":{"token":"super-secret"}}"#),
        ("notes.txt", "super-secret"),
    ];
    for (name, content) in secrets {
        write(root, name, content);
    }
    let root_text = root.to_string_lossy().into_owned();
    let uri = |name: &str| {
        format!(
            "file://{}",
            root.join(name).to_string_lossy().replace('\\', "/")
        )
    };
    for (name, _) in secrets {
        let err = read_file(&root_text, &uri(name))
            .expect_err(&format!("{name} should not be readable as a resource"));
        assert!(
            !err.contains("super-secret"),
            "{name}: error should not echo the file contents: {err}"
        );
    }
    // The project's own .http file is still served.
    let text = read_file(&root_text, &uri("api.http")).expect("api.http should be readable");
    assert!(
        text.contains("GET https://example.com/x"),
        "unexpected contents: {text}"
    );
}

/// The allowlist is rebuilt per read rather than snapshotted at the
/// start, matching the tools, which re-load the project on every call.
#[test]
fn read_file_serves_file_added_after_start() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "api.http", "# @name get\nGET https://example.com/x\n");
    new(config(root, "")).unwrap();
    write(
        root,
        "late.http",
        "# @name late\nGET https://example.com/late\n",
    );
    let uri = format!(
        "file://{}",
        root.join("late.http").to_string_lossy().replace('\\', "/")
    );
    let text = read_file(&root.to_string_lossy(), &uri)
        .expect("a file added after start should be readable");
    assert!(
        text.contains("example.com/late"),
        "unexpected contents: {text}"
    );
}

fn ping_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "api.http",
        "### ping\n# @name ping\nGET {{baseUrl}}/ping\n",
    );
    write(
        tmp.path(),
        "http-client.env.json",
        r#"{"dev":{"baseUrl":"http://api.example.test"}}"#,
    );
    tmp
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;

/// POSTs one JSON-RPC payload the way an MCP client does.
fn post(url: &str, token: &str, session: &str, body: &str) -> apic_httpc::Response {
    let mut req = HttpRequest::post(url, body)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if !token.is_empty() {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    if !session.is_empty() {
        req = req.header("Mcp-Session-Id", session);
    }
    Client::new().send(&req).unwrap()
}

/// The messages of an event stream.
fn events(resp: &apic_httpc::Response) -> Vec<Value> {
    String::from_utf8_lossy(&resp.body)
        .split("\n\n")
        .filter_map(|event| event.strip_prefix("event: message\ndata: "))
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}

#[test]
fn http_transport_behind_token() {
    let tmp = ping_project();
    let h = handler(config(tmp.path(), "dev"), "s3cret-token").unwrap();
    let srv = TestServer::new(h);
    let url = srv.url();

    for token in ["", "wrong"] {
        let resp = post(&url, token, "", INITIALIZE);
        assert_eq!(resp.status_code, 401, "token {token:?} should be refused");
        assert_eq!(
            String::from_utf8_lossy(&resp.body),
            "unauthorized: this apic mcp server needs its bearer token\n"
        );
    }
    let resp = post(&url, "s3cret-token", "", INITIALIZE);
    assert_eq!(resp.status_code, 200);
    assert_eq!(resp.headers.get("Content-Type"), Some("text/event-stream"));
    let session = resp.headers.get("Mcp-Session-Id").unwrap_or("").to_string();
    assert_eq!(session.len(), 26, "session id {session:?}");
    assert_eq!(events(&resp)[0]["result"]["serverInfo"]["version"], "test");
    let resp = post(
        &url,
        "s3cret-token",
        &session,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    assert_eq!((resp.status_code, resp.body.len()), (202, 0));

    let resp = post(
        &url,
        "s3cret-token",
        &session,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );
    let tools = events(&resp)[0]["result"]["tools"]
        .as_array()
        .cloned()
        .unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        tools.len() == 9 && names.contains(&"validate_project") && names.contains(&"curl_request"),
        "tools over HTTP: {names:?}"
    );
    let resp = post(
        &url,
        "s3cret-token",
        &session,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"curl_request","arguments":{"name":"ping","raw":true}}}"#,
    );
    let text = events(&resp)[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        text.contains("http://api.example.test/ping"),
        "curl over HTTP: {text}"
    );
    // A plain request without a token sees nothing but the 401.
    let resp = Client::new().send(&HttpRequest::get(&url)).unwrap();
    assert!(
        resp.status_code == 401
            && resp.headers.get("WWW-Authenticate") == Some("Bearer realm=\"apic mcp\""),
        "GET without token: {}",
        resp.status_code
    );
}

/// The transport's own answers: what it refuses and how, sessions that
/// begin with `initialize` and end with DELETE.
#[test]
fn http_transport_rules() {
    let tmp = ping_project();
    let srv = TestServer::new(handler(config(tmp.path(), "dev"), "").unwrap());
    let url = srv.url();
    let text = |resp: &apic_httpc::Response| {
        format!(
            "{} {}",
            resp.status_code,
            String::from_utf8_lossy(&resp.body)
        )
    };
    let send = |req: HttpRequest| text(&Client::new().send(&req).unwrap());
    let json = |req: HttpRequest| {
        req.header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
    };

    assert_eq!(
        send(HttpRequest::get(&url).header("Accept", "text/event-stream")),
        "400 Bad Request: GET requires an Mcp-Session-Id header\n"
    );
    assert_eq!(
        send(HttpRequest::get(&url).header("Accept", "application/json")),
        "400 Accept must contain 'text/event-stream' for GET requests\n"
    );
    assert_eq!(
        send(HttpRequest::post(&url, INITIALIZE)),
        "415 Content-Type must be 'application/json'\n"
    );
    assert_eq!(
        send(
            HttpRequest::post(&url, INITIALIZE)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
        ),
        "400 Accept must contain both 'application/json' and 'text/event-stream'\n"
    );
    assert_eq!(
        send(HttpRequest::new("PUT", &url)),
        "405 Method Not Allowed\n"
    );
    assert_eq!(
        send(HttpRequest::new("DELETE", &url)),
        "400 Bad Request: DELETE requires an Mcp-Session-Id header\n"
    );
    assert_eq!(
        send(json(HttpRequest::post(&url, INITIALIZE)).header("Mcp-Session-Id", "nope")),
        "404 session not found\n"
    );
    assert_eq!(
        send(json(HttpRequest::post(&url, INITIALIZE)).header("Mcp-Protocol-Version", "1999-01-01")),
        "400 Bad Request: Unsupported protocol version (supported versions: 2026-07-28,2025-11-25,2025-06-18,2025-03-26,2024-11-05)\n"
    );
    assert_eq!(
        send(json(HttpRequest::post(&url, INITIALIZE)).header("Host", "evil.example")),
        "403 Forbidden: invalid Host header \"evil.example\"\n"
    );
    // Without a session a request is answered, but only `initialize`
    // leaves one behind.
    assert_eq!(
        send(json(HttpRequest::post(&url, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#))),
        "200 event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":0,\"message\":\"method \\\"tools/list\\\" is invalid during session initialization\"}}\n\n"
    );

    let resp = post(&url, "", "", INITIALIZE);
    let session = resp.headers.get("Mcp-Session-Id").unwrap_or("").to_string();
    let in_session =
        |body: &str| json(HttpRequest::post(&url, body)).header("Mcp-Session-Id", &session);
    assert_eq!(
        send(in_session(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)),
        "200 event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n\n"
    );
    assert_eq!(
        send(in_session(r#"{"jsonrpc":"2.0","id":4,"method":"nope"}"#)),
        "400 JSON RPC not handled: \"nope\" unsupported\n"
    );
    assert_eq!(
        send(in_session(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call"}"#
        )),
        "400 invalid request: missing required \"params\"\n"
    );
    assert_eq!(send(in_session("")), "400 POST requires a non-empty body\n");
    assert_eq!(
        send(in_session("[]")),
        "400 malformed payload: empty batch\n"
    );
    assert_eq!(
        send(in_session(r#"{"id":1,"method":"ping"}"#)),
        "400 malformed payload: invalid message version tag \"\"; expected \"2.0\"\n"
    );
    assert_eq!(
        send(in_session(r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#)),
        "400 malformed payload: parse error: invalid ID type bool\n"
    );
    // Before 2025-06-18 several messages may come as one batch.
    assert_eq!(
        send(in_session(
            r#"[{"jsonrpc":"2.0","id":6,"method":"ping"},{"jsonrpc":"2.0","id":7,"method":"prompts/list"},{"jsonrpc":"2.0","method":"notifications/cancelled"}]"#
        )),
        "200 event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":6,\"result\":{}}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ttlMs\":0,\"cacheScope\":\"public\",\"prompts\":[]}}\n\n"
    );
    assert_eq!(
        send(in_session(r#"[{"jsonrpc":"2.0","id":6,"method":"ping"}]"#)
            .header("Mcp-Protocol-Version", "2025-06-18")),
        "400 JSON-RPC batching is not supported in 2025-06-18 and later (request version: 2025-06-18)\n"
    );
    assert_eq!(
        send(in_session(r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#)
            .header("Mcp-Protocol-Version", "2026-07-28")),
        "400 {\"jsonrpc\":\"2.0\",\"id\":6,\"error\":{\"code\":-32022,\"message\":\"protocol version \\\"2026-07-28\\\" is not supported by this server\",\"data\":{\"supported\":[\"2025-11-25\",\"2025-06-18\",\"2025-03-26\",\"2024-11-05\"],\"requested\":\"2026-07-28\"}}}"
    );
    assert_eq!(
        send(in_session(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#)),
        "202 "
    );
    assert_eq!(
        send(
            in_session(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).header("Last-Event-ID", "x")
        ),
        "400 can't send Last-Event-ID for POST request\n"
    );
    let stream = |extra: Option<(&str, &str)>| {
        let mut req = HttpRequest::get(&url)
            .header("Accept", "text/event-stream")
            .header("Mcp-Session-Id", &session);
        if let Some((k, v)) = extra {
            req = req.header(k, v);
        }
        req
    };
    assert_eq!(
        send(stream(Some(("Last-Event-ID", "abc")))),
        "400 malformed Last-Event-ID \"abc\"\n"
    );
    assert_eq!(
        send(stream(Some(("Last-Event-ID", "abc_1")))),
        "400 stream replay unsupported\n"
    );
    // The session's own stream stays open until the session ends.
    let (tx, rx) = mpsc::channel();
    let request = stream(None);
    let reader = std::thread::spawn(move || {
        let resp = Client::new().send(&request).unwrap();
        tx.send(()).unwrap();
        (
            resp.status_code,
            String::from_utf8_lossy(&resp.body).into_owned(),
        )
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(rx.try_recv().is_err(), "the stream should still be open");
    assert_eq!(
        send(stream(None)),
        "409 stream ID conflicts with ongoing stream\n"
    );
    let delete = || HttpRequest::new("DELETE", &url).header("Mcp-Session-Id", &session);
    assert_eq!(send(delete()), "204 ");
    assert_eq!(reader.join().unwrap(), (200, ": ok\n\n".to_string()));
    assert_eq!(send(delete()), "404 session not found\n");
    assert_eq!(
        send(in_session(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)),
        "404 session not found\n"
    );
}

#[test]
fn check_bind_guards_the_interface() {
    for addr in [
        "127.0.0.1:8765",
        "localhost:8765",
        "[::1]:8765",
        "127.0.0.2:0",
    ] {
        assert!(
            check_bind(addr, "").is_ok(),
            "{addr} without a token should be allowed"
        );
    }
    for addr in [
        "0.0.0.0:8765",
        ":8765",
        "[::]:8765",
        "10.0.0.5:8765",
        "myhost:8765",
    ] {
        let err = check_bind(addr, "").expect_err(addr);
        assert!(
            err.contains("--token"),
            "{addr} without a token should be refused, got {err}"
        );
        assert!(
            check_bind(addr, "tok").is_ok(),
            "{addr} with a token should be allowed"
        );
    }
    assert_eq!(
        check_bind("nonsense", "tok").unwrap_err(),
        "--http: \"nonsense\" is not host:port"
    );
}

#[test]
fn serve_http_listens_and_stops() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "api.http",
        "### ping\n# @name ping\nGET http://x/ping\n",
    );
    let cfg = config(tmp.path(), "");
    let cancel = CancelToken::new();
    let (bound_tx, bound_rx) = mpsc::channel();
    let (stop, served) = (cancel.clone(), cfg.clone());
    let server = std::thread::spawn(move || {
        let ready = move |addr: &str| bound_tx.send(addr.to_string()).unwrap();
        serve_http(&stop, served, "127.0.0.1:0", "", Some(&ready))
    });
    let addr = bound_rx.recv().unwrap();
    let resp = Client::new()
        .send(&HttpRequest::get(&format!("http://{addr}/")))
        .unwrap();
    assert_ne!(
        resp.status_code, 401,
        "no token on loopback should not demand one"
    );
    cancel.cancel();
    server.join().unwrap().expect("serve");
    assert!(
        serve_http(&CancelToken::new(), cfg, "0.0.0.0:0", "", None).is_err(),
        "0.0.0.0 without a token must not start"
    );
}

/// What a client is told about the tools and the server reads exactly as
/// recorded in `testdata/`.
#[test]
fn listings_read_as_recorded() {
    let tmp = ping_project();
    let server = new(Config {
        version: "dev".to_string(),
        ..config(tmp.path(), "dev")
    })
    .unwrap();
    let session = Session::new(server.inner.clone());
    let send = |line: &str| session.handle(&decode_message(line).unwrap()).unwrap();
    let testdata = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata");
    let recorded = |name: &str| {
        fs::read_to_string(format!("{testdata}/{name}"))
            .unwrap()
            .trim_end()
            .to_string()
    };
    assert_eq!(send(INITIALIZE), recorded("initialize.json"));
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#),
        recorded("tools_list.json")
    );
}

/// A writer the test keeps a handle on.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs a whole stdio session over the given lines.
fn stdio(server: &Server, lines: &[&str]) -> (Vec<String>, Result<(), String>) {
    let out = Shared::default();
    let input = Cursor::new(lines.join("\n").into_bytes());
    let result = server.run(&CancelToken::new(), input, out.clone());
    let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    (text.lines().map(str::to_string).collect(), result)
}

#[test]
fn stdio_lifecycle() {
    let tmp = ping_project();
    let server = new(config(tmp.path(), "dev")).unwrap();
    let error = |id: &str, code: i64, message: &str| {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":{code},\"message\":{}}}}}",
            serde_json::to_string(message).unwrap()
        )
    };

    // Before `initialize` only a ping is answered.
    let session = Session::new(server.inner.clone());
    let send = |line: &str| session.handle(&decode_message(line).unwrap());
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).unwrap(),
        r#"{"jsonrpc":"2.0","id":2,"result":{}}"#
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#).unwrap(),
        error(
            "3",
            0,
            "method \"tools/list\" is invalid during session initialization"
        )
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap(),
        error("1", -32600, "invalid request: missing required \"params\"")
    );
    for (client, agreed) in [
        ("2024-11-05", "2024-11-05"),
        ("2025-03-26", "2025-03-26"),
        ("2099-01-01", "2025-11-25"),
        ("2026-07-28", "2025-11-25"),
        ("", "2025-11-25"),
    ] {
        let session = Session::new(server.inner.clone());
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{client}"}}}}"#
        );
        let response: Value =
            serde_json::from_str(&session.handle(&decode_message(&line).unwrap()).unwrap())
                .unwrap();
        assert_eq!(
            response["result"]["protocolVersion"], agreed,
            "client {client:?}"
        );
    }

    // After it, each method answers as the protocol has it.
    let send = |line: &str| session.handle(&decode_message(line).unwrap());
    send(INITIALIZE).unwrap();
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        None
    );
    assert_eq!(
        send(INITIALIZE).unwrap(),
        error("1", 0, "duplicate \"initialize\" received")
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":7,"method":"nope/nothing"}"#).unwrap(),
        error("7", -32601, "method not found: \"nope/nothing\"")
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":"s8","method":"tools/call","params":{"name":"nope","arguments":{}}}"#)
            .unwrap(),
        error("\"s8\"", -32602, "unknown tool \"nope\"")
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":15,"method":"tools/call"}"#).unwrap(),
        error("15", -32600, "invalid request: missing required \"params\"")
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":46,"method":"tools/call","params":null}"#).unwrap(),
        error(
            "46",
            -32600,
            "handling 'tools/call': invalid request: missing required \"params\""
        )
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":26,"method":"ping","params":[1]}"#).unwrap(),
        error(
            "26",
            -32602,
            "handling 'ping': invalid params: unmarshaling \"[1]\" into a *mcp.PingParams: json: cannot unmarshal \"[1]\" into Go value of type mcp.PingParams"
        )
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":23,"method":"tools/list","params":{"cursor":"zzz"}}"#)
            .unwrap(),
        error("23", -32602, "invalid params")
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":44,"method":"notifications/initialized"}"#).unwrap(),
        error(
            "44",
            -32600,
            "invalid request: unexpected id for \"notifications/initialized\""
        )
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":34,"method":"prompts/get","params":{"name":"x"}}"#).unwrap(),
        error("34", -32602, "unknown prompt \"x\"")
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":39,"method":"completion/complete","params":{"ref":{}}}"#)
            .unwrap(),
        error("39", -32601, "method not found: \"completion/complete\"")
    );
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":21,"method":"resources/read","params":{"uri":"file:///etc/passwd"}}"#)
            .unwrap(),
        r#"{"jsonrpc":"2.0","id":21,"error":{"code":-32602,"message":"Resource not found","data":{"uri":"file:///etc/passwd"}}}"#
    );
    for (method, key) in [
        ("prompts/list", "prompts"),
        ("resources/templates/list", "resourceTemplates"),
    ] {
        assert_eq!(
            send(&format!(
                r#"{{"jsonrpc":"2.0","id":5,"method":"{method}"}}"#
            ))
            .unwrap(),
            format!(
                r#"{{"jsonrpc":"2.0","id":5,"result":{{"ttlMs":0,"cacheScope":"public","{key}":[]}}}}"#
            )
        );
    }
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":16,"method":"logging/setLevel","params":{"level":"debug"}}"#)
            .unwrap(),
        r#"{"jsonrpc":"2.0","id":16,"result":{}}"#
    );
    // Nothing is said to a notification, a response, or an id of null;
    // a fractional id is answered under its whole part.
    assert_eq!(send(r#"{"jsonrpc":"2.0","method":"ping"}"#), None);
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","method":"notifications/nope"}"#),
        None
    );
    assert_eq!(send(r#"{"jsonrpc":"2.0","id":32,"result":{}}"#), None);
    assert_eq!(send(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#), None);
    assert_eq!(
        send(r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#).unwrap(),
        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#
    );
    // Arguments are checked against the tool's schema before it runs.
    let invalid = |id: i64, arguments: &str| {
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"describe_request","arguments":{arguments}}}}}"#
        );
        let response: Value = serde_json::from_str(&send(&line).unwrap()).unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(
            response["result"]["structuredContent"].is_null(),
            "{response}"
        );
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        invalid(10, "{}"),
        "validating \"arguments\": validating root: required: missing properties: [\"name\"]"
    );
    assert_eq!(
        invalid(11, r#"{"name":5}"#),
        "validating \"arguments\": validating root: validating /properties/name: type: 5 has type \"integer\", want \"string\""
    );
    assert_eq!(
        invalid(12, r#"{"name":"ping","extra":1}"#),
        "validating \"arguments\": validating root: unexpected additional properties [\"extra\"]"
    );
    assert_eq!(
        invalid(28, "[]"),
        "validating \"arguments\": unmarshaling arguments: json: cannot unmarshal \"[]\" into Go value of type map[string]interface {}"
    );
}

#[test]
fn stdio_sessions() {
    let tmp = ping_project();
    let server = new(config(tmp.path(), "dev")).unwrap();
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

    // Blank lines are skipped; the session ends with its input.
    let (lines, result) = stdio(
        &server,
        &[
            INITIALIZE,
            initialized,
            "",
            "   ",
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        ],
    );
    assert_eq!(result, Ok(()));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1], r#"{"jsonrpc":"2.0","id":2,"result":{}}"#);

    // A line that is not a message ends the session with the reason.
    let (lines, result) = stdio(&server, &["not json"]);
    assert_eq!(
        (lines.len(), result),
        (
            0,
            Err("invalid character 'o' in literal null (expecting 'u')".to_string())
        )
    );
    let (_, result) = stdio(
        &server,
        &[INITIALIZE, initialized, r#"{"id":19,"method":"ping"}"#],
    );
    assert_eq!(
        result,
        Err("invalid message version tag \"\"; expected \"2.0\"".to_string())
    );
    let (_, result) = stdio(
        &server,
        &[INITIALIZE, r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#],
    );
    assert_eq!(result, Err("parse error: invalid ID type bool".to_string()));

    // A batch is answered as one line, until the version that drops them.
    let (lines, result) = stdio(
        &server,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            initialized,
            r#"[{"jsonrpc":"2.0","id":6,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/cancelled"},{"jsonrpc":"2.0","id":7,"method":"ping"}]"#,
        ],
    );
    assert_eq!(result, Ok(()));
    assert_eq!(
        lines.last().map(String::as_str),
        Some(r#"[{"jsonrpc":"2.0","id":6,"result":{}},{"jsonrpc":"2.0","id":7,"result":{}}]"#)
    );
    let (_, result) = stdio(
        &server,
        &[
            INITIALIZE,
            initialized,
            r#"[{"jsonrpc":"2.0","id":20,"method":"ping"}]"#,
        ],
    );
    assert_eq!(
        result,
        Err("JSON-RPC batching is not supported in 2025-06-18 and later (request version: 2025-06-18)".to_string())
    );
}
