//! The suite end to end: features run against a local API through the
//! project's requests, and the reports and exit codes that come out.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use apic_httpc::CancelToken;
use apic_httpd::{Mux, Request, Response, TestServer};
use apic_project::Project;

use crate::jsonmatch::{json_contains, json_equal};
use crate::state::check_var_name;
use crate::{
    run as run_features, run_summary, Config, Feature, Options, Output, Summary, EXIT_FAILED,
    EXIT_PASSED, EXIT_TRANSPORT, EXIT_USAGE,
};

fn server() -> TestServer {
    let users: Arc<Mutex<(BTreeMap<String, serde_json::Value>, i64)>> =
        Arc::new(Mutex::new((BTreeMap::new(), 1)));
    let mut mux = Mux::new();
    mux.handle_fn("POST /login", |_| Response::json(200, r#"{"token":"t-1"}"#))
        .unwrap();
    mux.handle_fn("POST /login-form", |_| {
        Response::new(204).add_header("Set-Cookie", "sid=c-1; Path=/")
    })
    .unwrap();
    mux.handle_fn("GET /me", |r: &Request| {
        if r.cookie("sid").as_deref() != Some("c-1") {
            return Response::new(401);
        }
        Response::json(200, r#"{"user":"alice"}"#)
    })
    .unwrap();
    let store = users.clone();
    mux.handle_fn("POST /users", move |r: &Request| {
        let mut input: serde_json::Value =
            serde_json::from_slice(&r.body).unwrap_or_else(|_| serde_json::json!({}));
        let mut guard = store.lock().unwrap();
        let id = guard.1;
        guard.1 += 1;
        input["id"] = serde_json::json!(id);
        guard.0.insert(id.to_string(), input.clone());
        Response::json(201, format!("{input}\n"))
    })
    .unwrap();
    let store = users.clone();
    mux.handle_fn("GET /users/{id}", move |r: &Request| {
        if r.header("Authorization") != Some("Bearer t-1") {
            return Response::new(401);
        }
        match store.lock().unwrap().0.get(r.path_value("id")) {
            Some(u) => Response::json(200, format!("{u}\n")),
            None => Response::json(404, r#"{"error":"not found"}"#),
        }
    })
    .unwrap();
    mux.handle_fn("GET /catalog", |_| {
        Response::json(200, r#"{"items": [{"id": "a", "name": "apple", "done": true, "price": 1.5}, {"id": "b", "name": "banana", "done": false, "price": 0.25}, {"id": "c", "name": "avocado", "done": true, "price": 2}], "owner": {"id": "u1", "tags": []}, "note": null}"#)
            .add_header("Link", "<https://x/catalog?page=2>; rel=next")
            .add_header("Link", "<https://x/catalog?page=9>; rel=last")
    })
    .unwrap();
    TestServer::new(mux.into_handler())
}

const API_HTTP: &str = r#"
### login
# @name login
# @step I am logged in
# @capture token = body.$.token
POST {{baseUrl}}/login

### create
# @name create-user
# @step a user named {name} exists
# @assert status == 201
# @capture userId = body.$.id
POST {{baseUrl}}/users
Content-Type: application/json

{"name": "{{name}}", "role": "{{role}}"}

### get
# @name get-user
# @step I fetch the user
# @step I fetch user {userId}
GET {{baseUrl}}/users/{{userId}}
Authorization: Bearer {{token}}

### get, logging in by itself
# @name get-user-ref
# @ref login
# @step I fetch user {userId} logged in
GET {{baseUrl}}/users/{{userId}}
Authorization: Bearer {{token}}

### a form login answered with a session cookie
# @name cookie-login
# @step I log in with a form
POST {{baseUrl}}/login-form

### only a cookie gets in
# @name me
# @step I ask who I am
GET {{baseUrl}}/me

### a list to select from
# @name catalog
# @step I list the catalog
GET {{baseUrl}}/catalog
"#;

fn write(dir: &Path, name: &str, contents: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn load(dir: &Path) -> Arc<Project> {
    apic_project::load(dir.to_string_lossy().as_ref()).unwrap()
}

/// A project with the API file and two environments: `dev` at the server
/// and `other` at an address nothing listens on.
fn new_project(srv: &TestServer) -> (tempfile::TempDir, Arc<Project>) {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "api.http", API_HTTP);
    write(
        dir.path(),
        "http-client.env.json",
        &format!(
            r#"{{"dev":{{"baseUrl":"{}","role":"member"}},"other":{{"baseUrl":"http://127.0.0.1:1"}}}}"#,
            srv.url()
        ),
    );
    let p = load(dir.path());
    (dir, p)
}

/// A project of the given files, with a `dev` environment at the server.
fn project_with(srv: &TestServer, files: &[(&str, &str)]) -> (tempfile::TempDir, Arc<Project>) {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "http-client.env.json",
        &format!(r#"{{"dev":{{"baseUrl":"{}","role":"member"}}}}"#, srv.url()),
    );
    for (name, contents) in files {
        write(dir.path(), name, contents);
    }
    let p = load(dir.path());
    (dir, p)
}

fn sink() -> Output {
    Arc::new(Mutex::new(std::io::sink()))
}

fn buffer() -> Arc<Mutex<Vec<u8>>> {
    Arc::new(Mutex::new(Vec::new()))
}

fn text(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
}

fn config(p: &Arc<Project>, env: &str) -> Config {
    Config {
        env: env.to_string(),
        stderr: Some(sink()),
        ..Config::new(p.clone())
    }
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn options(config: Config, name: &str, feature: &str) -> Options {
    Options {
        features: vec![Feature {
            name: name.to_string(),
            contents: feature.as_bytes().to_vec(),
        }],
        ..Options::new(config)
    }
}

fn summary(opts: &Options) -> (Option<Summary>, String, i32, Option<apic_runner::Error>) {
    let (sum, report, code, err) = run_summary(&CancelToken::new(), opts);
    (
        sum,
        String::from_utf8_lossy(&report).into_owned(),
        code,
        err,
    )
}

/// Runs one feature and expects no usage or transport error.
fn run(p: &Arc<Project>, feature: &str, env: &str) -> (Summary, i32) {
    let (sum, report, code, err) = summary(&options(config(p, env), "test.feature", feature));
    if let Some(err) = err {
        panic!("run: {err}\nreport: {report}");
    }
    (sum.expect("a summary"), code)
}

fn err_text(err: &Option<apic_runner::Error>) -> String {
    err.as_ref().map(ToString::to_string).unwrap_or_default()
}

#[test]
fn vocabulary_and_phrases() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Users
  Background:
    Given I am logged in

  Scenario: Create and fetch
    Given a user named "alice" exists
    Then the response status is 201
    And the response is successful
    And the response body "$.name" is "alice"
    And the response body "name" starts with "al"
    And the response body "role" is "member"
    And the response header "content-type" contains "json"
    And the response body "$.id" exists
    And the response body "$.missing" does not exist
    And the response time is under 5000 ms
    And the response body contains:
      """
      {"name": "alice", "id": {{userId}}}
      """
    When I fetch the user
    Then the response status is 200
    And the response body "$.id" is "{{userId}}"
    And the response body is:
      """
      {"name": "alice", "role": "member", "id": {{userId}}}
      """

  Scenario: Variables and tables
    Given the variable "role" is "{{role}}-x"
    Then the variable "role" is "member-x"
    Given the variables:
      | name | value |
      | role | admin |
    When I run "create-user" with:
      | name | bob |
    Then the response body "role" is "admin"
    And the response body "$.name" is not "alice"
    When I capture the response body "$.id" as "bobId"
    And I fetch user {{bobId}}
    Then the response body "$.name" is "bob"

  Scenario Outline: Unknown users
    When I fetch user <id>
    Then the response status is 404
    And the response is a client error
    And the response body "$.error" matches "^not"
    Examples:
      | id  |
      | 0   |
      | 999 |
"##,
        "dev",
    );
    assert!(
        code == EXIT_PASSED && sum.ok && sum.scenarios == 4 && sum.passed == 4,
        "code={code} summary={sum:?}"
    );
}

#[test]
fn failures_and_undefined() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Failing
  Scenario: Assertion fails
    Given I am logged in
    When I fetch user 0
    Then the response status is 200

  Scenario: Request assert fails
    When I run "create-user" with:
      | name | x |
    Then the response status is 201

  Scenario: No response yet
    Then the response status is 200

  Scenario: Undefined step
    Given something apic does not know
"##,
        "dev",
    );
    assert!(
        code == EXIT_FAILED && !sum.ok && sum.failed == 3 && sum.passed == 1 && sum.undefined == 1,
        "code={code} summary={sum:?}"
    );
    let joined: Vec<String> = sum
        .failures
        .iter()
        .map(|f| format!("{}: {}", f.scenario, f.error))
        .collect();
    let joined = joined.join("\n");
    for want in ["expected status == 200, got \"404\"", "no response yet"] {
        assert!(
            joined.contains(want),
            "missing {want:?} in failures:\n{joined}"
        );
    }
}

#[test]
fn isolated_session_and_environment_step() {
    let srv = server();
    let (dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Isolation
  Scenario: First logs in
    Given I am logged in
    When I fetch user 0
    Then the response status is 404

  Scenario: Second has no token from the first
    When I run "get-user" with:
      | userId | 0     |
      | token  | stale |
    Then the response status is 401
"##,
        "dev",
    );
    assert!(
        code == EXIT_PASSED && sum.passed == 2,
        "code={code} summary={sum:?}"
    );
    assert!(
        !dir.path().join(".apic").exists(),
        "tests must not write the session"
    );
    // Switching to an unreachable environment is a transport error (exit 3).
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "s.feature",
        "Feature: s\n  Scenario: switch\n    Given the environment is \"other\"\n    When I run \"login\"\n",
    ));
    assert!(
        code == EXIT_TRANSPORT && err_text(&err).contains("request failed"),
        "environment switch: code={code} err={err:?}"
    );
}

#[test]
fn redact_hides_values_in_failures() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let cfg = Config {
        redact: true,
        ..config(&p, "dev")
    };
    let (sum, _, code, err) = summary(&options(
        cfg,
        "r.feature",
        r##"
Feature: Redacted
  Scenario: Compare
    Given I am logged in
    When a user named "secret-name" exists
    Then the response body "$.name" is "other-value"
  Scenario: Body match
    Given I am logged in
    When a user named "secret-name" exists
    Then the response body contains:
      """
      {"name": "expected-secret"}
      """
  Scenario: Request assert
    When I run "get-user" with:
      | userId | 0          |
      | token  | secret-tok |
    Then the response status is 200
"##,
    ));
    let sum = sum.expect("a summary");
    assert!(
        err.is_none() && code == EXIT_FAILED && sum.failed == 3,
        "code={code} err={err:?} sum={sum:?}"
    );
    for f in &sum.failures {
        for leak in [
            "secret-name",
            "other-value",
            "expected-secret",
            "secret-tok",
            "\"name\":",
        ] {
            assert!(
                !f.error.contains(leak),
                "--redact leaked {leak:?} in {:?}",
                f.error
            );
        }
    }
}

#[test]
fn environment_step_usage_error() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "e.feature",
        "Feature: e\n  Scenario: s\n    Given the environment is \"missing\"\n",
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains(r#"environment "missing""#),
        "code={code} err={err:?}"
    );
}

#[test]
fn empty_feature_dir_is_usage_error() {
    let srv = server();
    let (dir, p) = new_project(&srv);
    fs::create_dir_all(dir.path().join("features")).unwrap();
    let (_, _, code, err) = summary(&Options::new(config(&p, "dev")));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("no .feature files"),
        "code={code} err={err:?}"
    );
}

#[test]
fn run_file_flow_stops_at_failure() {
    let srv = server();
    let (dir, _) = new_project(&srv);
    write(
        dir.path(),
        "flow.http",
        r#"
### one
# @name flow-login
# @capture token = body.$.token
POST {{baseUrl}}/login

### two
# @name flow-missing
# @assert status == 200
GET {{baseUrl}}/users/0
Authorization: Bearer {{token}}

### three
# @name flow-never
GET {{baseUrl}}/users/0
Authorization: Bearer {{token}}
"#,
    );
    let p = load(dir.path());
    let (sum, code) = run(
        &p,
        r#"
Feature: Files
  Scenario: A file runs in order and stops at the first failure
    When I run the file "flow.http"
  Scenario: The last response is the failing one
    When I run the file "flow.http"
    Then the response status is 404
"#,
        "dev",
    );
    assert!(
        code == EXIT_FAILED && sum.failed == 2 && sum.failures.len() == 2,
        "code={code} sum={sum:?}"
    );
    for f in &sum.failures {
        assert!(
            f.error.contains("flow-missing failed") && !f.error.contains("flow-never"),
            "flow should stop at flow-missing: {:?}",
            f.error
        );
    }
}

/// "I run the file" is a flow, so it skips a # @disabled request and the
/// last response is the last one sent; running the request by name sends
/// it.
#[test]
fn run_file_skips_disabled() {
    let srv = server();
    let (dir, _) = new_project(&srv);
    write(
        dir.path(),
        "flow.http",
        r#"
### one
# @name flow-login
POST {{baseUrl}}/login

### off
# @name flow-off
# @disabled
GET {{baseUrl}}/users/0
"#,
    );
    let p = load(dir.path());
    let (sum, code) = run(
        &p,
        r#"
Feature: Disabled
  Scenario: The file skips it
    When I run the file "flow.http"
    Then the response status is 200
    And the response body "token" is "t-1"
  Scenario: By name it runs
    When I run "flow-off"
    Then the response status is 401
"#,
        "dev",
    );
    assert!(
        code == 0 && sum.failed == 0 && sum.passed == 2,
        "code={code} sum={sum:?}"
    );
}

#[test]
fn typed_step_errors_map_to_exit_codes() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "u.feature",
        "Feature: u\n  Scenario: s\n    When I run \"no-such-request\"\n",
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("no-such-request"),
        "unknown request: code={code} err={err:?}"
    );
    let (_, _, code, err) = summary(&options(
        config(&p, "other"),
        "t.feature",
        "Feature: t\n  Scenario: s\n    When I run \"login\"\n",
    ));
    assert!(
        code == EXIT_TRANSPORT && err.is_some(),
        "unreachable server: code={code} err={err:?}"
    );
}

/// Runs one feature in a text format and returns the report.
fn report(
    cfg: Config,
    format: &str,
    name: &str,
    feature: &str,
) -> (String, i32, Option<apic_runner::Error>) {
    let out = buffer();
    let opts = Options {
        format: format.to_string(),
        no_colors: true,
        output: Some(out.clone()),
        ..options(cfg, name, feature)
    };
    let (code, err) = run_features(&CancelToken::new(), &opts);
    (text(&out), code, err)
}

#[test]
fn redact_masks_secrets_in_report_text() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[
            ("api.http", API_HTTP),
            (
                "http-client.private.env.json",
                r#"{"dev":{"apiKey":"hunter2-secret"}}"#,
            ),
        ],
    );
    let cfg = Config {
        redact: true,
        ..config(&p, "dev")
    };
    let (out, code, err) = report(
        cfg,
        "pretty",
        "m.feature",
        r##"
Feature: Masking
  Scenario: A captured token and a private value never appear in the report
    Given I am logged in
    When I run "get-user" with:
      | userId | 0              |
      | token  | hunter2-secret |
    Then the response status is 404
    And the response body "$.error" is "t-1"
"##,
    );
    assert!(
        err.is_none() && code == EXIT_FAILED,
        "code={code} err={err:?}"
    );
    for leak in ["hunter2-secret", "t-1"] {
        assert!(!out.contains(leak), "--redact leaked {leak:?}:\n{out}");
    }
    assert!(
        out.contains("***"),
        "expected masked values in report:\n{out}"
    );
}

#[test]
fn feature_path_must_be_inside_project() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let outside = tempfile::tempdir().unwrap();
    write(
        outside.path(),
        "x.feature",
        "Feature: x\n  Scenario: s\n    Given I am logged in\n",
    );
    let absolute = outside
        .path()
        .join("x.feature")
        .to_string_lossy()
        .into_owned();
    let base = outside
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let relative = Path::new("..")
        .join(base)
        .join("x.feature")
        .to_string_lossy()
        .into_owned();
    for path in [absolute, relative] {
        let opts = Options {
            paths: vec![path.clone()],
            ..Options::new(config(&p, "dev"))
        };
        let (_, _, code, err) = summary(&opts);
        assert!(
            code == EXIT_USAGE && err_text(&err).contains("outside the project root"),
            "{path}: code={code} err={err:?}"
        );
    }
}

#[test]
fn empty_tag_selection_is_not_a_failure() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let opts = Options {
        tags: "@nothing".to_string(),
        ..options(
            config(&p, "dev"),
            "t.feature",
            "Feature: t\n  Scenario: s\n    Given I am logged in\n",
        )
    };
    let (sum, _, code, err) = summary(&opts);
    let sum = sum.expect("a summary");
    assert!(
        err.is_none() && code == EXIT_PASSED && sum.ok && sum.scenarios == 0,
        "code={code} err={err:?} sum={sum:?}"
    );
}

#[test]
fn usage_errors() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (_, _, code, err) = summary(&options(
        config(&p, "nope"),
        "x.feature",
        "Feature: x\n  Scenario: y\n    Given I am logged in\n",
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains(r#"environment "nope""#),
        "code={code} err={err:?}"
    );
    let opts = Options {
        paths: vec!["nowhere".to_string()],
        ..Options::new(config(&p, "dev"))
    };
    let (_, _, code, err) = summary(&opts);
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("no features"),
        "code={code} err={err:?}"
    );
}

#[test]
fn json_match() {
    json_contains(
        br#"{"a":1,"b":{"c":[1,2],"d":"x"},"extra":true}"#,
        br#"{"b":{"c":[1,2]}}"#,
    )
    .unwrap();
    let why = json_contains(br#"{"a":1}"#, br#"{"a":2}"#).unwrap_err();
    assert!(why.contains("$.a: expected 2, got 1"), "{why}");
    let why = json_equal(br#"{"a":1,"b":2}"#, br#"{"a":1}"#).unwrap_err();
    assert!(why.contains("unexpected key"), "{why}");
    assert!(
        json_equal(
            br#" {"b": 2, "a": [1, {"x": null}]} "#,
            br#"{"a":[1,{"x":null}],"b":2}"#
        )
        .is_ok(),
        "semantic equality should ignore order and whitespace"
    );
}

#[test]
fn redact_capture_step_and_short_values() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[
            ("api.http", API_HTTP),
            ("http-client.private.env.json", r#"{"dev":{"pin":"7"}}"#),
        ],
    );
    let cfg = Config {
        redact: true,
        ..config(&p, "dev")
    };
    let (out, _, err) = report(
        cfg,
        "pretty",
        "m.feature",
        r##"
Feature: Capture step
  Scenario: A value captured by the capture step is masked afterwards
    Given I am logged in
    And a user named "alice" exists
    When I capture the response body "$.name" as "who"
    Then the response body "$.name" is "alice"
    And the response body "$.name" is "bob"
"##,
    );
    assert!(err.is_none(), "{err:?}");
    assert!(
        out.contains(r#"Then the response body "$.name" is "***""#),
        "value captured by the capture step should be masked in later lines:\n{out}"
    );
    // A one-character secret is deliberately not masked by substitution:
    // it would corrupt counts, line numbers and JSON in the report.
    assert!(
        out.contains("1 scenarios (1 failed)") && out.contains("m.feature:3"),
        "report structure must survive short secrets:\n{out}"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_feature_outside_project_rejected() {
    let srv = server();
    let (dir, p) = new_project(&srv);
    let outside = tempfile::tempdir().unwrap();
    write(
        outside.path(),
        "leak.feature",
        "Feature: x\n  Scenario: s\n    Given I am logged in\n",
    );
    fs::create_dir_all(dir.path().join("features")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("leak.feature"),
        dir.path().join("features").join("leak.feature"),
    )
    .unwrap();
    let (_, _, code, err) = summary(&Options::new(config(&p, "dev")));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("outside the project root"),
        "code={code} err={err:?}"
    );
}

#[test]
fn missing_variable_in_step_argument_is_usage_error() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "v.feature",
        "Feature: v\n  Scenario: s\n    Given I am logged in\n    Then the response body \"$.token\" is \"{{nope}}\"\n",
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("{{nope}}"),
        "code={code} err={err:?}"
    );
}

#[test]
fn phrase_conflicting_with_builtin_is_usage_error() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[(
            "api.http",
            "### a\n# @name a\n# @step I run {thing}\nGET {{baseUrl}}/login\n",
        )],
    );
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "c.feature",
        "Feature: c\n  Scenario: s\n    When I run \"login\"\n",
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("the built-in step"),
        "code={code} err={err:?}"
    );
}

#[test]
fn phrase_targets_survive_duplicate_names() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[
            ("a.http", "### a\n# @name login\n# @step I log in via a\n# @capture token = body.$.token\nPOST {{baseUrl}}/login\n"),
            ("b.http", "### b\n# @name login\n# @step I log in via b\nPOST {{baseUrl}}/login\n"),
        ],
    );
    let (sum, code) = run(
        &p,
        "Feature: d\n  Scenario: s\n    Given I log in via a\n    And I log in via b\n    Then the response status is 200\n",
        "dev",
    );
    assert!(code == EXIT_PASSED && sum.ok, "code={code} sum={sum:?}");
}

#[test]
fn environment_step_renders_variables() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        "Feature: e\n  Scenario: s\n    Given the variable \"target\" is \"dev\"\n    And the environment is \"{{target}}\"\n    When I run \"login\"\n    Then the response status is 200\n",
        "dev",
    );
    assert!(code == EXIT_PASSED && sum.ok, "code={code} sum={sum:?}");
}

#[test]
fn redacted_transport_error_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "api.http",
        "### a\n# @name ping\nGET {{baseUrl}}/ping?token={{secret}}\n",
    );
    write(
        dir.path(),
        "http-client.env.json",
        r#"{"dev":{"baseUrl":"http://127.0.0.1:1"}}"#,
    );
    write(
        dir.path(),
        "http-client.private.env.json",
        r#"{"dev":{"secret":"hunter2-value"}}"#,
    );
    let p = load(dir.path());
    let cfg = Config {
        redact: true,
        ..config(&p, "dev")
    };
    let (sum, _, code, err) = summary(&options(
        cfg,
        "t.feature",
        "Feature: t\n  Scenario: s\n    When I run \"ping\"\n",
    ));
    let msg = err_text(&err);
    assert!(
        code == EXIT_TRANSPORT
            && err.is_some()
            && !msg.contains("hunter2-value")
            && msg.contains("token=***"),
        "code={code} err={err:?}"
    );
    let sum = sum.expect("the summary should still describe the run");
    assert!(sum.scenarios == 1 && sum.failed == 1, "{sum:?}");
}

#[test]
fn redact_cucumber_json_stays_valid_with_numeric_secret() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[
            ("api.http", API_HTTP),
            ("http-client.private.env.json", r#"{"dev":{"pin":"123"}}"#),
        ],
    );
    std::env::set_var("APIC_VAR_shellSecret", "from-shell-secret");
    let cfg = Config {
        redact: true,
        ..config(&p, "dev")
    };
    let (sum, report, code, err) = summary(&options(
        cfg,
        "n.feature",
        r##"
Feature: Numbers
  Scenario: A numeric secret and a shell secret
    Given I am logged in
    When I run "get-user" with:
      | userId | 123               |
      | token  | from-shell-secret |
    Then the response status is 123
"##,
    ));
    std::env::remove_var("APIC_VAR_shellSecret");
    assert!(
        err.is_none() && code == EXIT_FAILED,
        "code={code} err={err:?} report={report}"
    );
    let sum = sum.expect("cucumber JSON must stay parseable under --redact");
    assert!(sum.scenarios == 1 && sum.failed == 1, "{sum:?}\n{report}");
    for leak in ["from-shell-secret", "| userId | 123"] {
        assert!(
            !report.contains(leak),
            "--redact leaked {leak:?}:\n{report}"
        );
    }
    assert!(
        report.contains(r#""line": 3"#),
        "numbers in the report must survive masking:\n{report}"
    );
}

#[test]
fn run_target_renders_variables_and_status_not() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Targets
  Scenario: The request id can come from a variable
    Given the variable "which" is "login"
    When I run "{{which}}"
    Then the response status is not 500
    And the response status is 200
    When I run "{{which}}" with:
      | role | admin |
    Then the response is successful
"##,
        "dev",
    );
    assert!(code == EXIT_PASSED && sum.ok, "code={code} sum={sum:?}");
}

#[test]
fn capture_step_persists_with_use_session() {
    let srv = server();
    let (dir, p) = new_project(&srv);
    let cfg = Config {
        use_session: true,
        ..config(&p, "dev")
    };
    let (sum, _, code, err) = summary(&options(
        cfg,
        "s.feature",
        r##"
Feature: Session
  Scenario: Capture into the shared session
    Given I am logged in
    When I capture the response body "$.token" as "kept"
  Scenario: A later scenario sees it
    When I run "get-user" with:
      | userId | 0        |
      | token  | {{kept}} |
    Then the response status is 404
"##,
    ));
    assert!(
        err.is_none() && code == EXIT_PASSED && sum.as_ref().is_some_and(|s| s.ok),
        "code={code} err={err:?} sum={sum:?}"
    );
    let data = fs::read_to_string(dir.path().join(".apic").join("session.json")).unwrap();
    assert!(
        data.contains(r#""kept": "t-1""#),
        "capture step should persist under --use-session: {data}"
    );
}

#[test]
fn capture_step_uses_capture_layer() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let cfg = Config {
        vars: vars(&[("pinned", "from-var")]),
        ..config(&p, "dev")
    };
    let (sum, _, code, err) = summary(&options(
        cfg,
        "c.feature",
        r##"
Feature: Capture precedence
  Scenario: A capture step cannot override --var, and a later request capture replaces it
    Given I am logged in
    When I capture the response body "$.token" as "pinned"
    Then the variable "check" is "{{pinned}}"
    And the variable "check" is "from-var"
    When I capture the response body "$.token" as "token"
    And I run "get-user" with:
      | userId | 0 |
    Then the response status is 404
"##,
    ));
    assert!(
        err.is_none() && code == EXIT_PASSED && sum.as_ref().is_some_and(|s| s.ok),
        "code={code} err={err:?} sum={sum:?}"
    );
}

#[test]
fn redact_registers_secrets_for_default_env() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[
            ("api.http", API_HTTP),
            ("apic.yaml", "env: dev\n"),
            (
                "http-client.private.env.json",
                r#"{"dev":{"apiKey":"hunter2-default-env"}}"#,
            ),
        ],
    );
    // The environment is left empty: apic.yaml supplies it.
    let cfg = Config {
        redact: true,
        ..config(&p, "")
    };
    let (out, _, err) = report(
        cfg,
        "pretty",
        "d.feature",
        r##"
Feature: Default env
  Scenario: Private values of the configured default environment are masked
    Given I am logged in
    When I run "get-user" with:
      | userId | 0                   |
      | token  | hunter2-default-env |
    Then the response status is 404
"##,
    );
    assert!(err.is_none(), "{err:?}");
    assert!(
        !out.contains("hunter2-default-env"),
        "private value of the default environment leaked:\n{out}"
    );
}

#[test]
fn empty_or_invalid_variable_names_are_rejected() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    for feature in [
        "Feature: n\n  Scenario: s\n    Given the variable \"\" is \"x\"\n",
        "Feature: n\n  Scenario: s\n    Given the variables:\n      | | x |\n",
        "Feature: n\n  Scenario: s\n    Given I am logged in\n    When I capture the response body \"$.token\" as \"\"\n",
        "Feature: n\n  Scenario: s\n    Given the variable \"bad name\" is \"x\"\n",
    ] {
        let (_, _, code, err) = summary(&options(config(&p, "dev"), "n.feature", feature));
        assert!(code != EXIT_PASSED, "should not pass:\n{feature} (code={code} err={err:?})");
    }
}

#[test]
fn scoped_vars_background_failures_and_masked_errors() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    // Phrase and table variables do not leak into later steps; a table may
    // supply the target.
    let (sum, code) = run(
        &p,
        r##"
Feature: Scoping
  Scenario: Phrase parameters are scoped to their request
    Given I am logged in
    And a user named "alice" exists
    Then the variable "probe" is "{{role}}"
    And the variable "probe" is "member"
    When I run "{{which}}" with:
      | which | login |
    Then the response status is 200
"##,
        "dev",
    );
    assert!(code == EXIT_PASSED && sum.ok, "code={code} sum={sum:?}");
    // A failing Background marks every scenario failed in the summary.
    let (sum, code) = run(
        &p,
        r##"
Feature: Background failure
  Background:
    Given I am logged in
    And the response status is 500
  Scenario: one
    Then the response status is 200
  Scenario: two
    Then the response status is 200
"##,
        "dev",
    );
    assert!(
        code == EXIT_FAILED
            && sum.failed == 2
            && sum.passed == 0
            && sum.failures.len() == 2
            && sum.failures[0].scenario == "one",
        "background failure not attributed: code={code} sum={sum:?}"
    );
    // Table errors and unknown-request errors are usage errors, masked
    // under --redact.
    let cfg = Config {
        redact: true,
        vars: vars(&[("secretTarget", "hunter2-target")]),
        ..config(&p, "dev")
    };
    let (_, _, code, err) = summary(&options(
        cfg,
        "m.feature",
        "Feature: m\n  Scenario: s\n    When I run \"{{secretTarget}}\"\n",
    ));
    let msg = err_text(&err);
    assert!(
        code == EXIT_USAGE
            && err.is_some()
            && !msg.contains("hunter2-target")
            && msg.contains("***"),
        "resolve error must be masked: code={code} err={err:?}"
    );
    let cfg = Config {
        redact: true,
        vars: vars(&[("secretEnv", "hunter2-env")]),
        ..config(&p, "dev")
    };
    let (_, _, code, err) = summary(&options(
        cfg,
        "e.feature",
        "Feature: e\n  Scenario: s\n    Given the environment is \"{{secretEnv}}\"\n",
    ));
    assert!(
        code == EXIT_USAGE && err.is_some() && !err_text(&err).contains("hunter2-env"),
        "environment error must be masked: code={code} err={err:?}"
    );
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "t.feature",
        "Feature: t\n  Scenario: s\n    When I run \"login\" with:\n      | bad name | x |\n",
    ));
    assert!(
        code == EXIT_USAGE && err.is_some(),
        "table error must be a usage error: code={code} err={err:?}"
    );
    // A non-feature file path is a usage error.
    let opts = Options {
        paths: vec!["api.http".to_string()],
        ..Options::new(config(&p, "dev"))
    };
    let (_, _, code, err) = summary(&opts);
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("not a .feature file"),
        "non-feature path: code={code} err={err:?}"
    );
}

#[test]
fn short_secrets_masked_in_errors() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let cfg = Config {
        redact: true,
        vars: vars(&[("e", "zz")]),
        ..config(&p, "dev")
    };
    let (_, _, code, err) = summary(&options(
        cfg,
        "s.feature",
        "Feature: s\n  Scenario: s\n    Given the environment is \"{{e}}\"\n",
    ));
    let msg = err_text(&err);
    assert!(
        code == EXIT_USAGE && err.is_some() && !msg.contains(r#""zz""#) && msg.contains("***"),
        "short secret must be masked in error text: code={code} err={err:?}"
    );
}

#[test]
fn malformed_variable_table_is_usage_error() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "t.feature",
        "Feature: t\n  Scenario: s\n    Given the variables:\n      | only |\n",
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("two cells"),
        "code={code} err={err:?}"
    );
}

#[test]
fn redact_masks_values_captured_after_they_appear() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let cfg = Config {
        redact: true,
        ..config(&p, "dev")
    };
    let (out, code, err) = report(
        cfg,
        "pretty",
        "late.feature",
        r##"
Feature: Late capture
  Scenario: The literal appears before the capture registers it
    Given I am logged in
    Then the response body "$.token" is "t-1"
    When I capture the response body "$.token" as "kept"
"##,
    );
    assert!(
        err.is_none() && code == EXIT_PASSED,
        "code={code} err={err:?}\n{out}"
    );
    assert!(
        !out.contains("t-1") && out.contains("***"),
        "a value captured later must be masked in earlier lines:\n{out}"
    );
}

#[test]
fn json_match_compares_numbers_exactly() {
    assert!(
        json_equal(
            br#"{"id": 9007199254740993}"#,
            br#"{"id": 9007199254740992}"#
        )
        .is_err(),
        "large integers must not be rounded to the same value"
    );
    json_equal(
        br#"{"n": 1.0, "big": 12345678901234567890}"#,
        br#"{"n": 1, "big": 12345678901234567890}"#,
    )
    .expect("1.0 and 1 are the same number");
    json_contains(br#"{"n": 1e2, "s": "x"}"#, br#"{"n": 100}"#)
        .expect("1e2 and 100 are the same number");
    let why = json_equal(b"[1]", br#"["1"]"#).unwrap_err();
    assert!(
        why.contains("expected \"1\", got 1"),
        "a number is not a string: {why}"
    );
    assert!(
        json_equal(br#"{"n": 1e1000000000}"#, br#"{"n": 1e1000000000}"#).is_ok(),
        "an absurd exponent is compared as text, not expanded"
    );
    assert!(
        json_equal(br#"{"n": 1e1000000000}"#, br#"{"n": 2e1000000000}"#).is_err(),
        "different absurd numbers differ as text"
    );
    for bad in ["{}]", "{} {}", "[1],", "{}}"] {
        assert!(
            json_equal(bad.as_bytes(), b"{}").is_err(),
            "trailing data must be rejected: {bad:?}"
        );
    }
    json_equal(b" {} \n", b"{}").expect("surrounding whitespace is fine");
}

#[test]
fn redact_masks_xml_escaped_secrets_in_junit() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let cfg = Config {
        redact: true,
        vars: vars(&[("token", "amp&secret<1>")]),
        ..config(&p, "dev")
    };
    let (out, code, err) = report(
        cfg,
        "junit",
        "x.feature",
        r##"
Feature: Escaped
  Scenario: The name mentions amp&secret<1> and the formatter escapes it
    Given I am logged in
"##,
    );
    assert!(
        err.is_none() && code == EXIT_PASSED,
        "code={code} err={err:?}\n{out}"
    );
    assert!(
        !out.contains("amp&amp;secret") && !out.contains("amp&secret") && out.contains("***"),
        "the XML-escaped secret leaked:\n{out}"
    );
}

#[test]
fn redact_keeps_report_structure_when_secrets_look_structural() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let feature = r##"
Feature: Structural
  Scenario: A failing scenario stays failed
    Given I am logged in
    Then the response status is 500
"##;
    let cfg = Config {
        redact: true,
        vars: vars(&[("a", "failed"), ("b", "scenario"), ("c", "Given ")]),
        ..config(&p, "dev")
    };
    let (sum, report_json, code, err) = summary(&options(cfg, "s.feature", feature));
    let sum = sum.expect("a summary");
    assert!(
        err.is_none() && code == EXIT_FAILED && sum.failed == 1 && !sum.ok,
        "a secret equal to a status must not hide the failure: code={code} err={err:?} sum={sum:?}\n{report_json}"
    );
    let cfg = Config {
        redact: true,
        vars: vars(&[
            ("a", "testcase"),
            ("b", "testsuite"),
            ("c", "failure"),
            ("d", "failed"),
        ]),
        ..config(&p, "dev")
    };
    let (out, code, err) = report(cfg, "junit", "s.feature", feature);
    assert!(
        err.is_none() && code == EXIT_FAILED,
        "code={code} err={err:?}\n{out}"
    );
    assert!(
        out.matches("<testsuite ").count() == 1
            && out.matches("<testcase ").count() == 1
            && out.matches("</testcase>").count() == 1
            && out.contains("<failure message=")
            && out.contains("</failure>")
            && out.trim_end().ends_with("</testsuites>"),
        "the JUnit report must stay well-formed with its failure:\n{out}"
    );
    assert!(
        out.contains(r#"status="failed""#) && !out.contains("stays failed"),
        "structural attributes stay, names are masked:\n{out}"
    );
}

#[test]
fn unnamed_request_failure_is_identified() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[(
            "flow.http",
            "### unnamed\n# @assert status == 999\nGET {{baseUrl}}/users/0\n",
        )],
    );
    let (sum, _, code, err) = summary(&options(
        config(&p, "dev"),
        "f.feature",
        "Feature: f\n  Scenario: s\n    When I run the file \"flow.http\"\n",
    ));
    let sum = sum.expect("a summary");
    assert!(
        err.is_none()
            && code == EXIT_FAILED
            && sum.failures.len() == 1
            && sum.failures[0].error.starts_with("flow.http:3 failed"),
        "code={code} err={err:?} sum={sum:?}"
    );
}

#[test]
fn malformed_tag_expression_is_usage_error() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let feature = "Feature: t\n  @smoke\n  Scenario: s\n    Given I am logged in\n";
    for bad in [
        "@smoke && ",
        "@",
        ",",
        "~",
        "@a && ~",
        "@a b",
        "@smoke||@slow",
        "@smoke&@slow",
        "!@slow",
        "(@a)",
    ] {
        let opts = Options {
            tags: bad.to_string(),
            ..options(config(&p, "dev"), "t.feature", feature)
        };
        let (_, _, code, err) = summary(&opts);
        assert!(
            code == EXIT_USAGE && err_text(&err).contains("invalid tag expression"),
            "{bad:?}: code={code} err={err:?}"
        );
    }
    for good in [
        "@smoke",
        "smoke",
        "~@slow",
        "@smoke && ~@slow",
        "@smoke,@other",
        " @smoke , ~slow ",
    ] {
        let opts = Options {
            tags: good.to_string(),
            ..options(config(&p, "dev"), "t.feature", feature)
        };
        let (_, _, code, err) = summary(&opts);
        assert!(
            err.is_none() && code == EXIT_PASSED,
            "{good:?}: code={code} err={err:?}"
        );
    }
}

#[test]
fn environment_switch_keeps_scenario_state() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Switch
  Scenario: Variables, captures and the last response survive an environment switch
    Given I am logged in
    And the variable "who" is "alice"
    When the environment is "dev"
    Then the variable "check" is "{{token}}-{{who}}"
    And the variable "check" is "t-1-alice"
    And the response status is 200
"##,
        "dev",
    );
    assert!(code == EXIT_PASSED && sum.ok, "code={code} sum={sum:?}");
}

/// A project with two environments at the same server.
fn two_env_project(srv: &TestServer) -> (tempfile::TempDir, Arc<Project>) {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "api.http", API_HTTP);
    write(
        dir.path(),
        "http-client.env.json",
        &format!(
            r#"{{"dev":{{"baseUrl":"{0}","role":"member"}},"alt":{{"baseUrl":"{0}","role":"member"}}}}"#,
            srv.url()
        ),
    );
    let p = load(dir.path());
    (dir, p)
}

#[test]
fn environment_switch_carries_session_values() {
    let srv = server();
    let (_dir, p) = two_env_project(&srv);
    let cfg = Config {
        use_session: true,
        ..config(&p, "dev")
    };
    let (sum, _, code, err) = summary(&options(
        cfg,
        "s.feature",
        r##"
Feature: Session across environments
  Scenario: Log in under dev
    Given I am logged in
  Scenario: A later scenario switches environments and still sees the token
    When the environment is "alt"
    Then the variable "check" is "{{token}}"
    And the variable "check" is "t-1"
"##,
    ));
    assert!(
        err.is_none() && code == EXIT_PASSED && sum.as_ref().is_some_and(|s| s.ok),
        "code={code} err={err:?} sum={sum:?}"
    );
}

#[test]
fn environment_switch_carries_auth_cache() {
    let srv = server();
    let (dir, _) = two_env_project(&srv);
    write(
        dir.path(),
        ".apic/session.json",
        r#"{"envs":{"dev":{"$oauth2:abc":"cached-token"}}}"#,
    );
    let p = load(dir.path());
    let cfg = Config {
        use_session: true,
        ..config(&p, "dev")
    };
    let (_, _, code, err) = summary(&options(
        cfg,
        "s.feature",
        r##"
Feature: Auth cache across environments
  Scenario: The cache follows the scenario into the new environment and is saved
    When the environment is "alt"
"##,
    ));
    assert!(
        err.is_none() && code == EXIT_PASSED,
        "code={code} err={err:?}"
    );
    let data = fs::read_to_string(dir.path().join(".apic").join("session.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(
        saved["envs"]["alt"]["$oauth2:abc"], "cached-token",
        "the auth cache must be persisted under the new environment even without a later capture: {data}"
    );
}

#[test]
fn malformed_feature_is_usage_error_with_position() {
    let srv = server();
    let (dir, p) = new_project(&srv);
    let (sum, _, code, err) = summary(&options(
        config(&p, "dev"),
        "bad.feature",
        "Feature: x\n  Scenario: s\n    Given I am logged in\nFeature: y\n",
    ));
    let msg = err_text(&err);
    assert!(
        code == EXIT_USAGE && msg.contains("bad.feature") && msg.contains("(4:1)") && sum.is_none(),
        "code={code} err={err:?} sum={sum:?}"
    );
    write(
        dir.path(),
        "features/broken.feature",
        "Feature: x\n  Scenario: s\n    Given I am logged in\n  Scenario Outline: o\n    Given I am logged in\n    Examples:\n      | a |\n      | 1 | 2 |\n",
    );
    let (_, _, code, err) = summary(&Options::new(config(&p, "dev")));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("broken.feature"),
        "code={code} err={err:?}"
    );
}

#[test]
fn selectors_are_rendered_and_response_refs_survive_env_switch() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Rendered selectors
  Scenario: Selectors may contain variables and named responses survive a switch
    Given the variable "idPath" is "$.token"
    And the variable "hdr" is "content-type"
    And I am logged in
    Then the response body "{{idPath}}" is "t-1"
    And the response body "{{idPath}}" exists
    And the response header "{{hdr}}" contains "json"
    When I capture the response body "{{idPath}}" as "tok"
    Then the variable "check" is "{{tok}}"
    And the variable "check" is "t-1"
    When the environment is "dev"
    Then the variable "ref" is "{{login.response.body.$.token}}"
    And the variable "ref" is "t-1"
"##,
        "dev",
    );
    assert!(code == EXIT_PASSED && sum.ok, "code={code} sum={sum:?}");
}

#[test]
fn configured_test_paths_and_explicit_override() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[
            ("api.http", API_HTTP),
            ("apic.yaml", "test:\n  paths: [specs]\n"),
            ("specs/ok.feature", "Feature: ok\n  Scenario: passes\n    Given I am logged in\n"),
            ("features/bad.feature", "Feature: bad\n  Scenario: fails\n    Given I am logged in\n    Then the response status is 500\n"),
        ],
    );
    assert_eq!(p.config.test.paths, ["specs"], "test.paths not loaded");
    let (sum, _, code, err) = summary(&Options::new(config(&p, "dev")));
    let sum = sum.expect("a summary");
    assert!(
        err.is_none() && code == EXIT_PASSED && sum.scenarios == 1 && sum.failed == 0,
        "configured paths: code={code} err={err:?} sum={sum:?}"
    );
    let opts = Options {
        paths: vec!["features".to_string()],
        ..Options::new(config(&p, "dev"))
    };
    let (sum, _, code, err) = summary(&opts);
    let sum = sum.expect("a summary");
    assert!(
        err.is_none() && code == EXIT_FAILED && sum.scenarios == 1 && sum.failed == 1,
        "explicit paths must override test.paths: code={code} err={err:?} sum={sum:?}"
    );
}

#[test]
fn phrase_runs_request_from_file_name_with_hash() {
    let srv = server();
    let (_dir, p) = project_with(
        &srv,
        &[("odd#name.http", "### a\n# @name login\n# @step I log in oddly\n# @capture token = body.$.token\nPOST {{baseUrl}}/login\n")],
    );
    let (sum, code) = run(
        &p,
        "Feature: h\n  Scenario: s\n    Given I log in oddly\n    Then the response status is 200\n    And the variable \"check\" is \"{{token}}\"\n    When I run the file \"odd#name.http\"\n    Then the response status is 200\n",
        "dev",
    );
    assert!(code == EXIT_PASSED && sum.ok, "code={code} sum={sum:?}");
}

#[test]
fn variable_names_cannot_look_like_response_references() {
    let err = check_var_name("foo.response.bar").unwrap_err();
    assert!(
        err.to_string().contains("reserved"),
        "expected a usage error: {err}"
    );
    check_var_name("foo.bar").expect("dots are otherwise fine");
    let err = check_var_name("a;b").unwrap_err();
    assert!(
        !err.to_string().contains("- ;"),
        "the message must not advertise characters the grammar rejects: {err}"
    );
}

#[test]
fn stop_on_failure_counts_skipped_scenarios() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let opts = Options {
        stop_on_failure: true,
        ..options(
            config(&p, "dev"),
            "s.feature",
            r##"
Feature: Stop
  Scenario: Fails first
    Given I am logged in
    Then the response status is 500
  Scenario: Never runs
    Given I am logged in
    Then the response status is 200
"##,
        )
    };
    let (sum, _, code, err) = summary(&opts);
    let sum = sum.expect("a summary");
    assert!(
        err.is_none()
            && code == EXIT_FAILED
            && sum.scenarios == 2
            && sum.failed == 1
            && sum.passed == 0
            && sum.skipped == 1
            && !sum.ok,
        "the scenario that never ran must be reported as skipped: code={code} err={err:?} sum={sum:?}"
    );
}

#[test]
fn redact_masks_capture_step_errors() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let cfg = Config {
        redact: true,
        vars: vars(&[("short", "zq"), ("long", "hidden-path-secret")]),
        ..config(&p, "dev")
    };
    let (sum, _, code, err) = summary(&options(
        cfg,
        "c.feature",
        r##"
Feature: Capture errors
  Scenario: Missing capture path with a short secret
    Given I am logged in
    When I capture the response body "$.{{short}}" as "a"
  Scenario: Missing capture path with a long secret
    Given I am logged in
    When I capture the response body "$.{{long}}" as "b"
"##,
    ));
    let sum = sum.expect("a summary");
    assert!(
        err.is_none() && code == EXIT_FAILED && sum.failed == 2,
        "code={code} err={err:?} sum={sum:?}"
    );
    for f in &sum.failures {
        assert!(
            !f.error.contains("zq") && !f.error.contains("hidden-path-secret"),
            "capture errors must be masked: {:?}",
            f.error
        );
    }
}

#[test]
fn redact_masks_short_secret_in_path() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let cfg = Config {
        redact: true,
        vars: vars(&[("userId", "zq")]),
        ..config(&p, "dev")
    };
    let (sum, _, code, err) = summary(&options(
        cfg,
        "p.feature",
        r##"
Feature: Path secrets
  Scenario: A two-character secret in the path never appears in a failure
    Given I am logged in
    When I run "get-user"
    Then the response status is 200
"##,
    ));
    let sum = sum.expect("a summary");
    assert!(
        err.is_none() && code == EXIT_FAILED && sum.failures.len() == 1,
        "code={code} err={err:?} sum={sum:?}"
    );
    let e = &sum.failures[0].error;
    assert!(
        !e.contains("/users/zq") && !e.contains("zq"),
        "the path secret leaked: {e:?}"
    );
}

#[test]
fn run_file_step_refuses_request_ids() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "f.feature",
        "Feature: f\n  Scenario: s\n    When I run the file \"login\"\n",
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("not a .http/.rest file"),
        "code={code} err={err:?}"
    );
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "f.feature",
        "Feature: f\n  Scenario: s\n    When I run the file \"api.http#login\"\n",
    ));
    assert!(
        code == EXIT_USAGE && err.is_some(),
        "a fragment is not a whole file: code={code} err={err:?}"
    );
}

#[test]
fn table_rows_render_in_order() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Ordered tables
  Scenario: Later rows see the rows above them
    Given I am logged in
    And the variables:
      | name  | value      |
      | a     | get        |
      | b     | {{a}}-user |
      | c     | {{b}}      |
      | which | {{c}}      |
    Then the variable "which" is "get-user"
    When I run "{{target}}" with:
      | id     | 0         |
      | userId | {{id}}    |
      | target | {{which}} |
    Then the response status is 404
    And the response status is not 200
"##,
        "dev",
    );
    assert!(code == EXIT_PASSED && sum.ok, "code={code} sum={sum:?}");
    // A row that refers to one below it is a missing variable.
    let (_, _, code, err) = summary(&options(
        config(&p, "dev"),
        "t.feature",
        r##"
Feature: t
  Scenario: s
    Given the variables:
      | x | {{y}} |
      | y | 1     |
"##,
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("missing variables: y"),
        "forward reference: code={code} err={err:?}"
    );
}

#[test]
fn empty_file_does_not_keep_the_previous_response() {
    let srv = server();
    let (dir, _) = new_project(&srv);
    write(dir.path(), "empty.http", "# only a comment\n");
    let p = load(dir.path());
    let (sum, _, code, err) = summary(&options(
        config(&p, "dev"),
        "e.feature",
        r##"
Feature: Empty
  Scenario: A file with no requests cannot stand in for a response
    When I run "login"
    And I run the file "empty.http"
    Then the response status is 200
"##,
    ));
    assert!(
        code == EXIT_USAGE && err_text(&err).contains("no requests") && !sum.as_ref().is_some_and(|s| s.ok),
        "the empty file must be a usage error, not a pass on the login response: code={code} err={err:?} sum={sum:?}"
    );
}

/// A request's `# @ref` applies in a scenario too, so a step can be the
/// first line of a scenario without a login step before it.
#[test]
fn step_runs_refs_first() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Refs
  Scenario: fetch without logging in first
    Given a user named "bob" exists
    When I fetch user {{userId}} logged in
    Then the response status is 200
    And the response body "$.name" is "bob"
"##,
        "dev",
    );
    assert!(code == 0 && sum.failed == 0, "code={code} summary={sum:?}");
}

#[test]
fn cookie_steps_and_per_scenario_jar() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let cfg = Config {
        cookies: true,
        ..config(&p, "dev")
    };
    let (sum, report, code, err) = summary(&options(
        cfg,
        "cookies.feature",
        r##"
Feature: Cookies
  Scenario: A session cookie carries over within a scenario
    When I log in with a form
    Then the response status is 204
    And the response cookie "sid" exists
    And the response cookie "sid" is "c-1"
    And the response cookie "sid" starts with "c-"
    And the response cookie "nope" does not exist
    When I capture the response cookie "sid" as "sid"
    And I ask who I am
    Then the response status is 200

  Scenario: The next scenario starts with an empty jar
    When I ask who I am
    Then the response status is 401
"##,
    ));
    assert!(err.is_none(), "run: {err:?}\nreport: {report}");
    let sum = sum.expect("a summary");
    assert!(
        code == 0 && sum.failed == 0 && sum.passed == 2,
        "code={code} summary={sum:?}\n{report}"
    );
    // Without the jar the cookie is still visible to the steps, just not
    // sent.
    let (sum, code) = run(
        &p,
        r##"
Feature: No jar
  Scenario: Cookies are inspected but not kept
    When I log in with a form
    Then the response cookie "sid" is "c-1"
    When I ask who I am
    Then the response status is 401
"##,
        "dev",
    );
    assert!(code == 0 && sum.failed == 0, "code={code} summary={sum:?}");
}

/// The JSONPath forms work the same in steps as in # @assert: filters,
/// recursive descent, negative indexes, slices, .length and multi-valued
/// headers, with or without the leading $.
#[test]
fn selector_forms() {
    let srv = server();
    let (_dir, p) = new_project(&srv);
    let (sum, code) = run(
        &p,
        r##"
Feature: Selectors
  Scenario: JSONPath
    When I list the catalog
    Then the response body "$.items[-1].id" is "c"
    And the response body "items[?(@.done == true)].id" contains "c"
    And the response body "$.items[?(@.name =~ /^a/)].length" is "2"
    And the response body "$..id" contains "u1"
    And the response body "..price" contains "0.25"
    And the response body "$.items[1:3].length" is "2"
    And the response body "items.length" is "3"
    And the response body "$.items[?(@.price > 5)]" does not exist
    And the response header "link.#" is "2"
    And the response header "link[-1]" contains "rel=last"
"##,
        "dev",
    );
    assert!(code == 0 && sum.failed == 0, "code={code} summary={sum:?}");
}

/// A step for each predicate: types, emptiness, length and a JSON Schema
/// read from the project root, passing and failing with the reason.
#[test]
fn shape_steps() {
    let srv = server();
    let (dir, p) = new_project(&srv);
    write(
        dir.path(),
        "schemas/catalog.json",
        r##"{"type": "object", "required": ["items"], "properties": {"items": {"type": "array", "items": {"$ref": "#/$defs/item"}}}, "$defs": {"item": {"type": "object", "required": ["id", "price"], "properties": {"id": {"type": "string"}, "price": {"type": "number"}}}}}"##,
    );
    write(
        dir.path(),
        "schemas/item.json",
        r#"{"type": "object", "required": ["id"]}"#,
    );
    let (sum, code) = run(
        &p,
        r##"
Feature: Shapes
  Scenario: The catalog has the right shape
    When I list the catalog
    Then the response body "items" has length 3
    And the response body "items[0].name" has length 5
    And the response body "$.items" is an array
    And the response body "$.owner" is an object
    And the response body "items[0].price" is a number
    And the response body "items[2].price" is an integer
    And the response body "items[0].id" is a string
    And the response body "items[0].done" is a boolean
    And the response body "note" is null
    And the response body "owner.tags" is empty
    And the response body "items" is not empty
    And the response body matches the schema "schemas/catalog.json"
    And the response body "items[1]" matches the schema "schemas/item.json"
"##,
        "dev",
    );
    assert!(code == 0 && sum.failed == 0, "code={code} summary={sum:?}");
    let (sum, code) = run(
        &p,
        r##"
Feature: Shapes
  Scenario: Wrong type
    When I list the catalog
    Then the response body "items[0].price" is an integer

  Scenario: Wrong schema
    When I list the catalog
    Then the response body "owner" matches the schema "schemas/catalog.json"

  Scenario: Wrong length
    When I list the catalog
    Then the response body "items" has length 2
"##,
        "dev",
    );
    assert!(code == 1 && sum.failed == 3, "code={code} summary={sum:?}");
    for (i, want) in [
        r#"expected body.$.items[0].price isInteger, got "number""#,
        "items",
        r#"expected body.$.items length == 2, got "3""#,
    ]
    .iter()
    .enumerate()
    {
        assert!(
            sum.failures[i].error.contains(want),
            "failure {i}: {:?} lacks {want:?}",
            sum.failures[i].error
        );
    }
}
