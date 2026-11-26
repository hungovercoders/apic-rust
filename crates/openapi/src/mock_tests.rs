use std::sync::Arc;

use apic_httpd::TestServer;

use super::*;

const MOCK_SPEC: &str = r##"
openapi: 3.0.3
info: {title: Todos, version: "1"}
servers:
  - url: http://localhost:8089/v1
paths:
  /todos:
    get:
      operationId: listTodos
      responses:
        "200":
          description: ok
          headers:
            X-Total-Count: {schema: {type: integer, example: 2}}
            Content-Type: {schema: {type: string}}
          content:
            application/json:
              schema: {type: array, items: {$ref: "#/components/schemas/Todo"}}
            text/csv:
              example: "id,title\n1,Buy milk\n"
    post:
      operationId: createTodo
      responses:
        "422":
          description: invalid
          content:
            application/json:
              example: {error: "title is required"}
        "201":
          description: created
          content:
            application/json:
              schema: {$ref: "#/components/schemas/Todo"}
  /todos/{todo-id}:
    get:
      operationId: getTodo
      responses:
        "200":
          description: ok
          content:
            application/json:
              examples:
                milk: {value: {id: "1", title: "Buy milk", done: false}}
                docs: {value: {id: "2", title: "Read the apic docs", done: true}}
        "4XX":
          description: client error
          content:
            application/json:
              example: {error: "no such todo"}
    delete:
      operationId: deleteTodo
      responses:
        "204": {description: gone}
  /reports/{name}.csv:
    get:
      responses:
        default:
          description: whatever
          content:
            text/csv:
              schema: {type: string}
  /:
    get:
      responses:
        "200":
          description: root
          content:
            text/plain:
              example: hello
components:
  schemas:
    Todo:
      type: object
      properties:
        id: {type: string, format: uuid}
        title: {type: string, example: Buy milk}
        done: {type: boolean, default: false}
        due: {type: string, format: date-time}
"##;

fn new_mock_server(spec: &str) -> (Arc<Mock>, TestServer) {
    let m = Arc::new(Mock::new(spec.as_bytes()).unwrap());
    let served = Arc::clone(&m);
    let srv = TestServer::new(move |r: &Request| served.serve(r));
    (m, srv)
}

/// Calls the server and returns the response with its body as text.
fn mock_call(
    srv: &TestServer,
    method: &str,
    path: &str,
    hdr: &[(&str, &str)],
) -> (apic_httpc::Response, String) {
    let mut req = apic_httpc::Request::new(method, &format!("{}{path}", srv.url()));
    for (k, v) in hdr {
        req = req.header(k, v);
    }
    let res = apic_httpc::Client::new().send(&req).unwrap();
    let body = res.text();
    (res, body)
}

fn route(method: &str, path: &str, status: u16, name: &str) -> Route {
    Route {
        method: method.to_string(),
        path: path.to_string(),
        status,
        name: name.to_string(),
    }
}

#[test]
fn mock_routes_and_default_responses() {
    let (m, srv) = new_mock_server(MOCK_SPEC);
    assert!(
        m.routes.len() == 6 && m.skipped.is_empty(),
        "routes {:?} skipped {:?}",
        m.routes,
        m.skipped
    );
    let want = [
        route("GET", "/todos", 200, "listTodos"),
        route("POST", "/todos", 201, "createTodo"),
        route("GET", "/todos/{todo-id}", 200, "getTodo"),
        route("DELETE", "/todos/{todo-id}", 204, "deleteTodo"),
        route("GET", "/reports/{name}.csv", 200, ""),
        route("GET", "/", 200, ""),
    ];
    for (i, r) in want.iter().enumerate() {
        assert_eq!(&m.routes[i], r, "route {i}");
    }

    // The schema becomes a body, with the importer's placeholders fixed.
    let (res, body) = mock_call(&srv, "GET", "/todos", &[]);
    assert!(
        res.status_code == 200
            && res.header("Content-Type") == Some("application/json")
            && res.header("X-Total-Count") == Some("2"),
        "status {} headers {:?}",
        res.status_code,
        res.headers
    );
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "body {body:?}");
    assert!(
        list[0]["title"] == "Buy milk"
            && list[0]["done"] == false
            && list[0]["id"] == "00000000-0000-4000-8000-000000000000"
            && list[0]["due"] == "2024-01-01T00:00:00Z",
        "body {body:?}"
    );

    // The first 2xx wins over an earlier 4xx; the server prefix is served too.
    let (res, _) = mock_call(&srv, "POST", "/v1/todos", &[]);
    assert_eq!(res.status_code, 201, "POST /v1/todos");
    // The first named example is the body; 204 has none.
    let (res, body) = mock_call(&srv, "GET", "/todos/1", &[]);
    assert!(
        res.status_code == 200 && body.contains(r#""title": "Buy milk""#),
        "GET /todos/1 = {} {body:?}",
        res.status_code
    );
    let (res, body) = mock_call(&srv, "DELETE", "/todos/1", &[]);
    assert!(
        res.status_code == 204 && body.is_empty(),
        "DELETE = {} {body:?}",
        res.status_code
    );
    // A segment that is not a whole {parameter} still matches, and a
    // default response serves 200 with the schema's string.
    let (res, body) = mock_call(&srv, "GET", "/reports/daily.csv", &[]);
    assert!(
        res.status_code == 200
            && res.header("Content-Type") == Some("text/csv")
            && body == "string",
        "report = {} {:?} {body:?}",
        res.status_code,
        res.header("Content-Type")
    );
    // The root path is exact, not a prefix.
    let (res, body) = mock_call(&srv, "GET", "/", &[]);
    assert!(
        res.status_code == 200 && body == "hello",
        "root = {} {body:?}",
        res.status_code
    );
}

#[test]
fn mock_prefer_and_accept() {
    let (_m, srv) = new_mock_server(MOCK_SPEC);
    // An exact code.
    let (res, body) = mock_call(&srv, "POST", "/todos", &[("Prefer", "code=422")]);
    assert!(
        res.status_code == 422 && body.contains("title is required"),
        "422 = {} {body:?}",
        res.status_code
    );
    // A class key serves the asked code with its body.
    let (res, body) = mock_call(&srv, "GET", "/todos/9", &[("Prefer", "code=404")]);
    assert!(
        res.status_code == 404 && body.contains("no such todo"),
        "404 = {} {body:?}",
        res.status_code
    );
    // default serves any code.
    let (res, _) = mock_call(&srv, "GET", "/reports/x.csv", &[("Prefer", "code=503")]);
    assert_eq!(res.status_code, 503);
    // A named example, with the two separators.
    for h in ["code=200, example=docs", "code=200; example=\"docs\""] {
        let (res, body) = mock_call(&srv, "GET", "/todos/2", &[("Prefer", h)]);
        assert!(
            res.status_code == 200 && body.contains("Read the apic docs"),
            "{h:?} = {} {body:?}",
            res.status_code
        );
    }
    // Choices the document does not offer.
    for (path, prefer, want) in [
        ("/todos", "code=500", "no 500 response"),
        ("/todos", "code=abc", "not a status code"),
        ("/todos/1", "example=nope", "no example named"),
        ("/todos", "example=any", "has no example named"),
    ] {
        let (res, body) = mock_call(&srv, "GET", path, &[("Prefer", prefer)]);
        assert!(
            res.status_code == 406 && body.contains(want),
            "{path} {prefer} = {} {body:?}, want 406 with {want:?}",
            res.status_code
        );
    }
    // Accept picks the media type; an unknown one falls back to the first.
    let (res, body) = mock_call(&srv, "GET", "/todos", &[("Accept", "text/csv")]);
    assert!(
        res.header("Content-Type") == Some("text/csv") && body == "id,title\n1,Buy milk\n",
        "csv = {:?} {body:?}",
        res.header("Content-Type")
    );
    let (res, _) = mock_call(
        &srv,
        "GET",
        "/todos",
        &[("Accept", "image/png, text/*;q=0.5")],
    );
    assert_eq!(res.header("Content-Type"), Some("text/csv"), "text/*");
    let (res, _) = mock_call(&srv, "GET", "/todos", &[("Accept", "image/png")]);
    assert_eq!(
        res.header("Content-Type"),
        Some("application/json"),
        "fallback"
    );
}

#[test]
fn mock_unknown_path_and_method() {
    let (_m, srv) = new_mock_server(MOCK_SPEC);
    let (res, body) = mock_call(&srv, "GET", "/nope", &[]);
    assert!(
        res.status_code == 404 && body.contains("GET /nope is not in the document"),
        "404 = {} {body:?}",
        res.status_code
    );
    assert_eq!(body, "{\"error\":\"GET /nope is not in the document\"}\n");
    assert_eq!(res.header("Content-Type"), Some("application/json"));
    let (res, body) = mock_call(&srv, "PUT", "/todos", &[]);
    assert!(
        res.status_code == 405
            && res.header("Allow") == Some("GET, POST")
            && body.contains("PUT /todos is not in the document"),
        "405 = {} allow {:?} {body:?}",
        res.status_code,
        res.header("Allow")
    );
}

#[test]
fn mock_rejects_bad_documents() {
    for (spec, want) in [
        (r#"{"not": "a spec"}"#, "not an OpenAPI 3 document"),
        ("swagger: '2.0'\npaths: {}\n", "Swagger 2.0"),
        ("openapi: 3.0.3\ninfo: {title: t, version: '1'}\npaths: {}\n", "no operations"),
        ("openapi: 3.0.3\ninfo: {title: t, version: '1'}\npaths:\n  /a:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              schema: {$ref: '#/components/schemas/Missing'}\n", "unresolvable"),
    ] {
        match Mock::new(spec.as_bytes()) {
            Err(err) if err.to_string().contains(want) => {}
            Err(err) => panic!("{spec:?}: got {err}, want {want:?}"),
            Ok(_) => panic!("{spec:?}: no error, want {want:?}"),
        }
    }
}

// Two templates that the mux cannot hold side by side: the second is
// reported rather than panicking or silently vanishing.
#[test]
fn mock_reports_conflicting_paths() {
    let m = Mock::new(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /a/{x}:
    get: {responses: {"200": {description: ok}}}
  /{y}/b:
    get: {responses: {"200": {description: ok}}}
"#
        .as_bytes(),
    )
    .unwrap();
    assert!(
        m.routes.len() == 1
            && m.skipped.len() == 1
            && m.skipped[0].contains("GET /{y}/b: its path conflicts"),
        "routes {:?} skipped {:?}",
        m.routes,
        m.skipped
    );
}

#[test]
fn mock_petstore() {
    let data = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/petstore.yaml"
    ))
    .unwrap();
    let m = Mock::new(&data).unwrap();
    let srv = TestServer::new(m.into_handler());
    let (res, body) = mock_call(&srv, "GET", "/v1/pets", &[]);
    assert!(
        res.status_code == 200 && body.contains(r#""name": "Rex""#),
        "{} {body:?}",
        res.status_code
    );
    let (res, _) = mock_call(&srv, "GET", "/health", &[]);
    assert_eq!(res.status_code, 200, "health");
}

#[test]
fn mux_patterns() {
    for (input, pattern, partial) in [
        ("/pets", "/pets", false),
        ("/pets/{petId}", "/pets/{p0}", false),
        ("/pets/{pet-id}/x", "/pets/{p0}/x", false),
        ("/files/{name}.json", "/files/{p0}", true),
        ("/a/{b}/c/{d}", "/a/{p0}/c/{p1}", false),
        ("/v{major}.{minor}", "/{p0}", true),
        ("/pets/", "/pets/{$}", false),
        ("/", "/{$}", false),
        ("pets", "/pets", false),
        ("/with space/{x}", "/withspace/{p0}", false),
    ] {
        assert_eq!(
            mux_pattern(input),
            (pattern.to_string(), partial),
            "mux_pattern({input:?})"
        );
    }
}

// A template with literal text around its parameter matches only paths
// with that text, and shares its wildcard segment with a plain sibling.
#[test]
fn mock_partial_segment_templates() {
    let (m, srv) = new_mock_server(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: http://localhost/v2}]
paths:
  /files/{name}.json:
    get:
      responses:
        "200": {description: ok, content: {application/json: {example: {kind: json}}}}
  /files/{id}:
    get:
      responses:
        "200": {description: ok, content: {application/json: {example: {kind: plain}}}}
  /reports/{name}.csv:
    get:
      responses:
        "200": {description: ok, content: {text/csv: {example: "a,b\n"}}}
"#,
    );
    assert!(
        m.routes.len() == 3 && m.skipped.is_empty(),
        "routes {:?} skipped {:?}",
        m.routes,
        m.skipped
    );
    for (path, want, code) in [
        ("/files/a.json", r#""kind": "json""#, 200),
        ("/v2/files/a.json", r#""kind": "json""#, 200),
        ("/files/a.xml", r#""kind": "plain""#, 200),
        ("/files/a", r#""kind": "plain""#, 200),
        ("/reports/daily.csv", "a,b\n", 200),
        ("/reports/daily.txt", "is not in the document", 404),
        ("/v2/reports/daily.csv", "a,b\n", 200),
    ] {
        let (res, body) = mock_call(&srv, "GET", path, &[]);
        assert!(
            res.status_code == code && body.contains(want),
            "GET {path} = {} {body:?}, want {code} with {want:?}",
            res.status_code
        );
    }
}

// A declared /v1/items keeps its path; the prefixed alias of /items yields
// to it silently and /items is still served and reported.
#[test]
fn mock_alias_yields_to_declared_path() {
    let (m, srv) = new_mock_server(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: http://x/v1}]
paths:
  /v1/items:
    get:
      responses:
        "200": {description: ok, content: {text/plain: {example: first}}}
  /items:
    get:
      responses:
        "200": {description: ok, content: {text/plain: {example: second}}}
"#,
    );
    assert!(
        m.routes.len() == 2 && m.skipped.is_empty(),
        "routes {:?} skipped {:?}",
        m.routes,
        m.skipped
    );
    let (_, body) = mock_call(&srv, "GET", "/v1/items", &[]);
    assert_eq!(body, "first", "/v1/items");
    let (_, body) = mock_call(&srv, "GET", "/items", &[]);
    assert_eq!(body, "second", "/items");
    // Two operations on what the mux sees as the same path: the second is
    // reported, and the 405 Allow header names the methods actually served.
    let (m2, srv2) = new_mock_server(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /pets/{id}:
    get: {responses: {"200": {description: ok}}}
  /pets/{petId}:
    get: {responses: {"200": {description: ok}}}
    delete: {responses: {"204": {description: ok}}}
"#,
    );
    assert!(
        m2.routes.len() == 2
            && m2.skipped.len() == 1
            && m2.skipped[0].contains("GET /pets/{petId}: its path is the same"),
        "routes {:?} skipped {:?}",
        m2.routes,
        m2.skipped
    );
    let (res, _) = mock_call(&srv2, "PUT", "/pets/1", &[]);
    assert!(
        res.status_code == 405 && res.header("Allow") == Some("GET, DELETE"),
        "405 = {} allow {:?}",
        res.status_code,
        res.header("Allow")
    );
}

// Headers the server derives from what it sends are not taken from the
// document, and a structured example under a non-JSON media type is no body
// rather than JSON under a misleading content type.
#[test]
fn mock_framing_headers_and_non_json_bodies() {
    let (_m, srv) = new_mock_server(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /sized:
    get:
      responses:
        "200":
          description: ok
          headers:
            Content-Length: {schema: {type: integer, example: 3}}
            Transfer-Encoding: {schema: {type: string, example: chunked}}
            X-Kept: {schema: {type: string, example: yes}}
          content:
            application/json:
              example: {message: "longer than three bytes"}
  /xml:
    get:
      responses:
        "200":
          description: ok
          content:
            application/xml:
              schema: {type: object, properties: {id: {type: integer}}}
  /count:
    get:
      responses:
        "200":
          description: ok
          content:
            text/plain:
              example: 42
"#,
    );
    let (res, body) = mock_call(&srv, "GET", "/sized", &[]);
    assert!(
        res.status_code == 200
            && body.contains("longer than three bytes")
            && res.header("X-Kept") == Some("yes")
            && res.header("Transfer-Encoding") != Some("chunked"),
        "sized = {} {body:?} {:?}",
        res.status_code,
        res.headers
    );
    let (res, body) = mock_call(&srv, "GET", "/xml", &[]);
    assert!(
        res.status_code == 200 && body.is_empty() && res.header("Content-Type").is_none(),
        "xml = {} {body:?} {:?}",
        res.status_code,
        res.header("Content-Type")
    );
    let (res, body) = mock_call(&srv, "GET", "/count", &[]);
    assert!(
        body == "42" && res.header("Content-Type") == Some("text/plain"),
        "count = {body:?} {:?}",
        res.header("Content-Type")
    );
}

// A path the document does not declare is a 404, not a redirect to a
// trailing-slash form it does declare; and an invalid path is reported for
// what it is.
#[test]
fn mock_no_redirects_and_invalid_paths() {
    let (m, srv) = new_mock_server(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /pets/:
    get: {responses: {"200": {description: ok, content: {text/plain: {example: slash}}}}}
  //evil.example/x:
    get: {responses: {"200": {description: ok}}}
"#,
    );
    assert!(
        m.routes.len() == 1
            && m.skipped.len() == 1
            && m.skipped[0].contains("ServeMux rejects its path")
            && !m.skipped[0].contains("conflicts"),
        "routes {:?} skipped {:?}",
        m.routes,
        m.skipped
    );
    let (res, body) = mock_call(&srv, "GET", "/pets/", &[]);
    assert!(
        res.status_code == 200 && body == "slash",
        "/pets/ = {} {body:?}",
        res.status_code
    );
    let (res, body) = mock_call(&srv, "GET", "/pets", &[]);
    assert!(
        res.status_code == 404 && body.contains("GET /pets is not in the document"),
        "/pets = {} {body:?}",
        res.status_code
    );
}

#[test]
fn url_paths() {
    for (url, want) in [
        ("https://api.example.com/v1", Some("/v1")),
        ("https://api.example.com/v1/x%20y?q=1#f", Some("/v1/x y")),
        ("https://api.example.com", Some("")),
        ("/v2", Some("/v2")),
        ("api.example.com/v1", Some("api.example.com/v1")),
        ("mailto:someone", Some("")),
        ("https://api:port/v1", None),
        ("https://api.example.com/%zz", None),
        ("localhost:8080/v1", Some("")),
        (":x", None),
    ] {
        assert_eq!(url_path(url).as_deref(), want, "url_path({url:?})");
    }
}

#[test]
fn route_json_shape() {
    assert_eq!(
        serde_json::to_string(&[route("GET", "/a", 200, "getA"), route("GET", "/b", 204, "")])
            .unwrap(),
        r#"[{"method":"GET","path":"/a","status":200,"name":"getA"},{"method":"GET","path":"/b","status":204}]"#
    );
}

#[test]
fn a_mock_is_shared_between_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<Mock>();
}
