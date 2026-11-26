use std::sync::{Arc, Mutex};

use apic_httpd::{Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;
use crate::EXIT_USAGE;

#[derive(Clone, Debug)]
struct Got {
    method: String,
    content_type: String,
    marker: String,
    body: serde_json::Value,
}

/// A GraphQL request, in either spelling, goes out as a POST with the
/// {"query","variables"} envelope, placeholders rendered in both halves
/// and the editor's marker header left behind.
#[test]
fn graphql_requests() {
    let seen: Arc<Mutex<Vec<Got>>> = Arc::default();
    let sink = seen.clone();
    let srv = TestServer::new(move |r: &HttpRequest| {
        sink.lock().unwrap().push(Got {
            method: r.method.clone(),
            content_type: r.header("Content-Type").unwrap_or("").to_string(),
            marker: r.header("X-Request-Type").unwrap_or("").to_string(),
            body: serde_json::from_slice(&r.body).unwrap_or_default(),
        });
        HttpResponse::json(200, r#"{"data": {"todos": [{"id": 1}]}}"#)
    });
    let api = r#"
@done = true
@field = id

### JetBrains
# @name jet
# @assert body.$.data.todos.# == 1
GRAPHQL SRV/graphql

query Todos($done: Boolean) {
  todos(done: $done) { {{field}} }
}

{"done": {{done}}}

### REST Client
# @name rest
POST SRV/graphql
X-REQUEST-TYPE: GraphQL
Content-Type: application/json; charset=utf-8

{ todos { id } }

### From a file
# @name filed
GRAPHQL SRV/graphql

<@ ./query.graphql

### Lower-case marker
# @name lower
POST SRV/graphql
x-request-type: graphql

{ todos { id } }

### Broken variables
# @name broken
GRAPHQL SRV/graphql

{ todos { id } }

{"done": {{done}}, nope}

### Broken without placeholders, so validate can tell
# @name static
GRAPHQL SRV/graphql

{ todos { id } }

{nope}
"#
    .replace("SRV", &srv.url());
    let dir = write_project(&[
        ("query.graphql", "query { todos { {{field}} } }"),
        ("api.http", &api),
    ]);
    let p = load(dir.path());
    let r = Runner::new(
        p.clone(),
        Options {
            no_session: true,
            ..Options::default()
        },
    )
    .unwrap();
    for name in ["jet", "rest", "filed"] {
        let res = run_ok(&r, name);
        assert!(res.ok, "{name}: {res:?}");
        assert!(
            res.request.method == "POST" && res.request.body.starts_with(r#"{"query":"#),
            "{name}: resolved as {} {:?}",
            res.request.method,
            res.request.body
        );
    }
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 3);
    assert!(
        got[0].method == "POST"
            && got[0].content_type == "application/json"
            && got[0].marker.is_empty(),
        "jet on the wire: {:?}",
        got[0]
    );
    assert_eq!(
        got[0].body["query"],
        "query Todos($done: Boolean) {\n  todos(done: $done) { id }\n}"
    );
    assert_eq!(got[0].body["variables"].to_string(), r#"{"done":true}"#);
    assert!(
        got[1].content_type == "application/json; charset=utf-8"
            && got[1].marker.is_empty()
            && got[1].body["query"] == "{ todos { id } }",
        "rest on the wire: {:?}",
        got[1]
    );
    assert!(
        got[1].body.get("variables").is_none(),
        "no variables block, no variables key"
    );
    assert_eq!(got[2].body["query"], "query { todos { id } }");
    // The wire form is what describe shows; list keeps GRAPHQL.
    let jet = lookup(&r, "jet");
    let d = r.describe(&jet);
    assert!(
        d.method == "POST"
            && d.body.starts_with(r#"{"query": "query Todos"#)
            && d.body.ends_with(r#", "variables": {"done": {{done}}}}"#)
            && d.headers["Content-Type"] == "application/json",
        "describe: {} {:?} {:?}",
        d.method,
        d.body,
        d.headers
    );
    assert_eq!(jet.method, "GRAPHQL");
    let d = r.describe(&lookup(&r, "lower"));
    assert!(
        d.headers.len() == 1 && d.headers["Content-Type"] == "application/json",
        "describe drops the marker whatever its case: {:?}",
        d.headers
    );
    // Variables that are not JSON once rendered are a usage error before
    // anything is sent.
    let err = run_err(&r, "broken");
    assert!(
        err.to_string().contains("variables") && exit(Some(&err)) == EXIT_USAGE,
        "broken: {err}"
    );
    assert_eq!(seen.lock().unwrap().len(), 3, "the broken request was sent");
    // validate cannot judge variables with placeholders in them, so only
    // the static one is reported, at its body line.
    let diags = p.validate();
    assert!(
        diags.len() == 1 && diags[0].code == "bad-graphql" && diags[0].line == 49,
        "validate: {diags:?}"
    );
}
