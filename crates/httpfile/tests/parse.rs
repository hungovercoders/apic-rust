use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use apic_httpfile::{
    code_description, graphql_envelope, parse, parse_file, parse_sleep, quote_param, span,
    split_graphql, valid_capture_name, Diagnostic, FileRef, Ref, RequestKey, SaveTo,
};

/// Parses the sample under the relative path the expectations spell out.
fn sample() -> (Arc<apic_httpfile::File>, Vec<Diagnostic>) {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/sample.http"))
        .unwrap();
    parse("testdata/sample.http", &src)
}

#[test]
fn parse_file_reads_from_disk() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/sample.http");
    let (f, diags) = parse_file(path).unwrap();
    assert_eq!(f.path, path);
    assert_eq!(f.requests.len(), 9);
    assert_eq!(diags.len(), 2);
    assert!(parse_file(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/none.http")).is_err());
}

#[test]
fn parse_sample() {
    let (f, diags) = sample();
    assert!(
        f.vars.len() == 2
            && f.vars[0].name == "baseUrl"
            && f.vars[0].value == "https://api.example.com",
        "vars = {:?}",
        f.vars
    );
    assert_eq!(f.requests.len(), 9);

    let login = &f.requests[0];
    assert!(
        login.name == "login"
            && login.method == "POST"
            && login.url == "{{baseUrl}}/auth/login"
            && login.line == 8,
        "login = {login:?}"
    );
    assert_eq!(login.description, "Log in and keep the token");
    assert!(
        login.captures.len() == 1
            && login.captures[0].name == "token"
            && login.captures[0].selector == "body.$.access_token",
        "captures = {:?}",
        login.captures
    );
    assert_eq!(
        login.body,
        r#"{"user": "{{user}}", "password": "{{password}}"}"#
    );
    assert!(login.headers.len() == 1 && login.headers[0].name == "Content-Type");
    // Every request shares the file's path and variables.
    assert!(Arc::ptr_eq(&login.file, &f.info) && login.file.vars.len() == 2);

    let get = &f.requests[1];
    assert_eq!(
        get.url,
        "{{baseUrl}}/users/{{userId}}?expand=profile&fields=id,email"
    );
    assert!(get.asserts.len() == 3 && get.asserts[1].expr == "body.$.id == {{userId}}");
    assert!(get.headers.len() == 2 && get.headers[1].name == "Accept");
    assert_eq!(get.description, "Fetch a single user by id");
    assert_eq!(get.body, "");
    assert_eq!(
        get.refs(),
        vec![Ref {
            id: "login".to_string(),
            force: false,
            line: 16,
            column: 8
        }]
    );
    assert!(login.refs().is_empty());

    let health = &f.requests[2];
    assert!(
        health.method == "GET"
            && health.url == "{{baseUrl}}/health"
            && health.http_version == "HTTP/1.1"
            && health.name.is_empty(),
        "health = {health:?}"
    );
    assert_eq!(health.id(), "testdata/sample.http#3");
    assert_eq!(login.id(), "login");

    let up = &f.requests[3];
    assert!(
        up.body_file == "./payload.json" && up.body.is_empty(),
        "upload = {up:?}"
    );
    assert_eq!(up.directive("retry"), Some("3 500ms"));
    assert_eq!(up.directive_span("retry"), (33, 10, 17));
    assert_eq!(up.directive_span("sleep"), (0, 0, 0));

    assert!(!up.disabled());
    assert_eq!(up.sleep(), Ok(Duration::ZERO));
    let slow = &f.requests[8];
    assert!(slow.disabled());
    assert_eq!(slow.sleep(), Ok(Duration::from_millis(1500)));
    for bad in ["soon", "-1s", ""] {
        assert!(
            parse_sleep(bad).is_err(),
            "parse_sleep({bad:?}) should fail"
        );
    }
    assert_eq!(
        parse_sleep("soon").unwrap_err(),
        "want a duration such as 500ms or 2s, got \"soon\""
    );

    let gql = &f.requests[6];
    assert!(gql.method == "GRAPHQL" && gql.is_graphql() && gql.name == "todos-by-state");
    assert_eq!(
        gql.graphql(),
        Ok((
            "query Todos($done: Boolean) {\n  todos(done: $done) { id title }\n}".to_string(),
            r#"{"done": {{done}}}"#.to_string()
        ))
    );
    let rc = &f.requests[7];
    assert!(rc.method == "POST" && rc.is_graphql());
    assert_eq!(
        rc.graphql(),
        Ok(("{ todos { id } }".to_string(), String::new()))
    );

    let warnings = diags.iter().filter(|d| d.severity == "warning").count();
    let errors = diags.iter().filter(|d| d.severity == "error").count();
    assert!(warnings == 1 && errors == 1, "diags = {diags:?}");
    // Diagnostics carry a code and the span of the offending text, so an
    // editor can underline `@frobnicate` and `nope` rather than whole lines.
    let want = [
        Diagnostic {
            path: "testdata/sample.http".to_string(),
            line: 20,
            column: 3,
            end_line: 20,
            end_column: 14,
            severity: "warning".to_string(),
            code: "unknown-directive".to_string(),
            message: "unknown directive @frobnicate (ignored)".to_string(),
        },
        Diagnostic {
            path: "testdata/sample.http".to_string(),
            line: 41,
            column: 12,
            end_line: 41,
            end_column: 16,
            severity: "error".to_string(),
            code: "bad-capture".to_string(),
            message: "@capture must look like `name = selector`, got \"nope\"".to_string(),
        },
    ];
    assert_eq!(diags, want);
    assert_eq!(
        want[1].to_string(),
        "testdata/sample.http:41:12: error: @capture must look like `name = selector`, got \"nope\""
    );
    // Columns on the AST point at the parts later checks report on.
    assert_eq!(login.captures[0].column, 20);
    assert_eq!(get.asserts[0].column, 11);
    assert_eq!(
        (up.body_file_line, up.body_file_column, up.body_line),
        (37, 3, 37)
    );
    assert_eq!((login.body_line, health.body_line), (11, 0));

    // A multipart body is kept verbatim and read into parts on demand: a
    // text part is a template, a `< file` part reads the file.
    let mp = &f.requests[5];
    assert!(
        mp.is_multipart() && mp.body_file.is_empty() && mp.body.starts_with("--WebAppBoundary\n"),
        "multipart request = {mp:?}"
    );
    let m = mp.multipart().unwrap().unwrap();
    assert!(
        m.boundary == "WebAppBoundary" && m.parts.len() == 2 && m.files() == 1,
        "multipart = {m:?}"
    );
    let (title, file) = (&m.parts[0], &m.parts[1]);
    assert!(
        title.name == "title"
            && title.body == "Quarterly report for {{user}}"
            && title.file.is_empty()
            && title.line == 49,
        "title part = {title:?}"
    );
    assert!(
        file.name == "file"
            && file.filename == "report.pdf"
            && file.content_type == "application/pdf"
            && file.file == "./report.pdf"
            && file.body.is_empty(),
        "file part = {file:?}"
    );
    assert_eq!((file.file_line, file.file_column, file.line), (57, 3, 53));
    assert_eq!(
        mp.body_files(),
        vec![FileRef {
            path: "./report.pdf".to_string(),
            templated: false,
            line: 57,
            column: 3
        }]
    );
    assert_eq!(
        up.body_files(),
        vec![FileRef {
            path: "./payload.json".to_string(),
            templated: false,
            line: 37,
            column: 3
        }]
    );
    assert!(!login.is_multipart());
    assert_eq!(login.multipart(), Ok(None));
}

#[test]
fn diagnostic_json_shape() {
    let (_, diags) = sample();
    assert_eq!(
        serde_json::to_string(&diags[0]).unwrap(),
        r#"{"path":"testdata/sample.http","line":20,"column":3,"end_line":20,"end_column":14,"severity":"warning","code":"unknown-directive","message":"unknown directive @frobnicate (ignored)"}"#
    );
    let whole = Diagnostic {
        path: "apic.yaml".to_string(),
        line: 1,
        severity: "error".to_string(),
        message: "m".to_string(),
        ..Diagnostic::default()
    };
    assert_eq!(
        serde_json::to_string(&whole).unwrap(),
        r#"{"path":"apic.yaml","line":1,"severity":"error","message":"m"}"#
    );
    assert_eq!(whole.to_string(), "apic.yaml:1: error: m");
}

#[test]
fn request_keys_compare_by_identity() {
    let (f, _) = parse("x.http", "GET http://x\n\n###\nGET http://x\n");
    let (a, b) = (&f.requests[0], &f.requests[1]);
    let mut seen = HashSet::new();
    assert!(seen.insert(RequestKey::new(a)));
    assert!(!seen.insert(RequestKey::from(Arc::clone(a))));
    assert!(seen.insert(RequestKey::new(b)));
    let mut results: HashMap<RequestKey, usize> = HashMap::new();
    results.insert(a.into(), 1);
    assert_eq!(results.get(&RequestKey::new(a)), Some(&1));
    assert_eq!(results.get(&RequestKey::new(b)), None);
}

#[test]
fn multipart_errors() {
    let cases = BTreeMap::from([
        ("no boundary", "POST http://x\nContent-Type: multipart/form-data\n\n--a\nContent-Disposition: form-data; name=\"a\"\n\n1\n--a--\n"),
        ("empty", "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n"),
        ("no delimiter", "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\nplain text\n"),
        ("not closed", "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n--a\nContent-Disposition: form-data; name=\"a\"\n\n1\n"),
        ("bad header", "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n--a\nnot a header\n\n1\n--a--\n"),
        ("no parts", "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n--a--\n"),
        ("stray content", "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n--a\nContent-Disposition: form-data; name=\"a\"\n\n1\n--a\n--b\n--a--\n"),
    ]);
    for (name, src) in cases {
        let (f, _) = parse("x.http", src);
        assert_eq!(f.requests.len(), 1, "{name}");
        assert!(f.requests[0].multipart().is_err(), "{name}: want an error");
    }
    // A whole-body file under a multipart content type is the editors'
    // other spelling: the file holds the encoded body, so there are no parts.
    let (f, _) = parse(
        "x.http",
        "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n< ./body.bin\n",
    );
    assert_eq!(f.requests[0].multipart(), Ok(None));
    // Multi-line text content keeps its inner line breaks; the newline
    // before a delimiter belongs to the delimiter.
    let (f, _) = parse("x.http", "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n--a\nContent-Disposition: form-data; name=\"a\"\n\nline 1\n\nline 3\n--a--\n");
    let m = f.requests[0].multipart().unwrap().unwrap();
    assert!(
        m.parts.len() == 1 && m.parts[0].body == "line 1\n\nline 3",
        "{m:?}"
    );
}

#[test]
fn multipart_error_messages() {
    let message = |src: &str| parse("x.http", src).0.requests[0].multipart().unwrap_err();
    assert_eq!(
        message("POST http://x\nContent-Type: multipart/form-data; boundary=a\n\nplain text\n"),
        "multipart body has no `--a` delimiter"
    );
    assert_eq!(
        message("POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n--a\nnot a header\n\n1\n--a--\n"),
        "multipart body: line 5: expected a part header (`Name: value`) or a blank line before the content, got \"not a header\""
    );
    assert_eq!(
        message("POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n--a\nContent-Disposition: form-data; name=\"a\"\n\n1\n"),
        "multipart body is not closed: expected `--a--` at the end"
    );
}

#[test]
fn quote_param_keeps_non_ascii() {
    assert_eq!(quote_param(r#"a "b" \c é"#), r#""a \"b\" \\c é""#);
    // A quoted name written that way reads back as it was.
    let src = format!(
        "POST http://x\nContent-Type: multipart/form-data; boundary=a\n\n--a\nContent-Disposition: form-data; name={}; filename={}\n\n1\n--a--\n",
        quote_param("résumé"),
        quote_param("a \"b\".txt")
    );
    let m = parse("x.http", &src).0.requests[0]
        .multipart()
        .unwrap()
        .unwrap();
    assert_eq!(m.parts[0].name, "résumé");
    assert_eq!(m.parts[0].filename, "a \"b\".txt");
}

#[test]
fn span_finds_value_after_key() {
    // `# @name name`: the value repeats the key, and must be found after it.
    let (f, _) = parse("x.http", "# @name name\nGET http://x\n");
    assert_eq!(f.requests[0].directives[0].column, 9);
    assert_eq!(span("  X: 1", "X: 1", 0), (3, 7));
    assert_eq!(span("abc", "", 0), (0, 0));
    assert_eq!(span("abc", "zzz", 0), (0, 0));
    assert_eq!(span("abc", "c", 9), (0, 0));
}

#[test]
fn diagnostic_spans() {
    let src = "### orphan\n# @name a\n\n### b\n# @name\nGET http://x\n  Bogus header line\n\n> {% client.log(1) %}\n";
    let (_, diags) = parse("x.http", src);
    let got: BTreeMap<&str, &Diagnostic> = diags.iter().map(|d| (d.code.as_str(), d)).collect();
    let cases = [
        ("orphan-directives", 2, 3, 8),
        ("bad-name", 5, 3, 8),
        ("bad-header", 7, 3, 20),
        ("editor-script", 9, 1, 22),
    ];
    for (code, line, column, end_column) in cases {
        let Some(d) = got.get(code) else {
            panic!("no {code} diagnostic in {diags:?}");
        };
        assert_eq!(
            (d.line, d.column, d.end_line, d.end_column),
            (line, column, line, end_column),
            "{code}"
        );
    }
    for code in got.keys() {
        assert!(
            code_description(code).is_some(),
            "code {code:?} is not in CODES"
        );
    }
}

#[test]
fn parse_crlf_and_implicit_get() {
    let (f, _) = parse("x.http", "GET https://a.b/c\r\nX-A: 1\r\n\r\nhello\r\n\r\n");
    assert!(
        f.requests.len() == 1
            && f.requests[0].body == "hello"
            && f.requests[0].headers[0].value == "1",
        "{:?}",
        f.requests
    );
}

#[test]
fn parse_token_header_names() {
    let (f, diags) = parse(
        "t.http",
        "### a\nGET http://x\nX.Correlation-ID: abc\nX-Api_Key: k\n",
    );
    assert!(
        diags.is_empty() && f.requests.len() == 1 && f.requests[0].headers.len() == 2,
        "header names may use any RFC 7230 token character: {diags:?} {:?}",
        f.requests
    );
    assert_eq!(f.requests[0].headers[0].name, "X.Correlation-ID");
    // `#` is a token character too, but a line starting with one is a
    // comment in this dialect; elsewhere in a name it is fine.
    let (f, diags) = parse("t.http", "### a\nGET http://x\n#X-Trace: v\nX#Y: 1\n");
    assert!(
        diags.is_empty()
            && f.requests.len() == 1
            && f.requests[0].headers.len() == 1
            && f.requests[0].headers[0].name == "X#Y",
        "a leading # is a comment, an inner # is part of the name: {diags:?} {:?}",
        f.requests[0].headers
    );
}

/// Pins the promise the comparison page makes: "apic reads the common
/// subset and ignores what it does not know, so a file with editor-only
/// features still parses". Before this, a VS Code REST Client or JetBrains
/// response handler was sent as part of the request body: apic POSTed the
/// user's script to their own API.
#[test]
fn editor_handler_blocks_are_ignored() {
    let src = "### Login\n# @name login\nPOST https://api.example.com/login\nContent-Type: application/json\n\n{\"user\": \"alice\"}\n\n> {%\n    client.global.set(\"token\", response.body.access_token);\n%}\n";
    let (f, diags) = parse("x.http", src);
    assert_eq!(f.requests.len(), 1);
    let got = &f.requests[0].body;
    assert_eq!(
        got, r#"{"user": "alice"}"#,
        "handler block leaked into the body"
    );
    for bad in ["client.global.set", "%}", "> {%"] {
        assert!(
            !got.contains(bad),
            "body should not contain {bad:?}:\n{got:?}"
        );
    }
    // It is skipped, and said to be skipped: `apic validate` surfaces this.
    assert!(
        diags.len() == 1 && diags[0].severity == "warning",
        "expected one warning diagnostic, got {diags:?}"
    );
    assert_eq!(
        diags[0].message,
        "ignoring response handler block (apic has no scripting; see https://apic.sh/comparison/)"
    );
}

/// Pins the nastier half: `< {%` used to hit the `< ./file` body-file
/// branch and became the body file `{%`, failing at run time with "body
/// file: no such file". A real `< ./body.json` must still work.
#[test]
fn pre_request_script_is_not_a_body_file() {
    let (f, _) = parse(
        "x.http",
        "# @name a\nPOST https://api.example.com/x\n\n< {%\n  request.variables.set(\"n\", 1);\n%}\n",
    );
    assert_eq!(
        f.requests[0].body_file, "",
        "a pre-request script is not a body file"
    );
    assert_eq!(
        f.requests[0].body, "",
        "a pre-request script should not become the body"
    );

    let (f2, diags) = parse(
        "y.http",
        "# @name b\nPOST https://api.example.com/x\n\n< ./body.json\n",
    );
    assert!(
        diags.is_empty(),
        "a real body file reference should parse cleanly: {diags:?}"
    );
    assert_eq!(f2.requests[0].body_file, "./body.json");
    assert!(!f2.requests[0].body_file_templated);

    let (f3, _) = parse(
        "z.http",
        "POST https://api.example.com/x\n\n<@ ./body.json\n",
    );
    assert_eq!(f3.requests[0].body_file, "./body.json");
    assert!(f3.requests[0].body_file_templated);
    assert_eq!(f3.requests[0].body_file_column, 4);
}

#[test]
fn graphql() {
    // The variables are the object after the last blank line; a blank line
    // inside the query is not a split point unless an object follows it.
    assert_eq!(
        split_graphql("query {\n  a\n}\n\n{\"x\": 1}"),
        ("query {\n  a\n}".to_string(), r#"{"x": 1}"#.to_string())
    );
    assert_eq!(
        split_graphql("query {\n  a\n}\n\nfragment F on T { b }"),
        (
            "query {\n  a\n}\n\nfragment F on T { b }".to_string(),
            String::new()
        )
    );
    assert_eq!(
        graphql_envelope("{ a }", r#"{"x": 1}"#).unwrap(),
        r#"{"query":"{ a }","variables":{"x":1}}"#
    );
    assert_eq!(
        graphql_envelope("{ a }", "").unwrap(),
        r#"{"query":"{ a }"}"#
    );
    assert_eq!(
        graphql_envelope("{ a(x: \"<\") }\n", "").unwrap(),
        concat!(r#"{"query":"{ a(x: \"\u"#, r#"003c\") }\n"}"#)
    );
    assert!(
        graphql_envelope("{ a }", "[1]").is_err(),
        "an array is not a variables object"
    );
    for bad in [
        "GRAPHQL http://x\n",
        "GRAPHQL http://x\n\n{ a }\n\n{nope}\n",
    ] {
        let (f, _) = parse("g.http", bad);
        assert!(
            f.requests[0].graphql().is_err(),
            "{bad:?} should not be a valid GraphQL request"
        );
    }
    let (f, _) = parse(
        "g.http",
        "POST http://x\nx-request-type: graphql\n\n{ a }\n",
    );
    assert!(f.requests[0].is_graphql(), "the header is case-insensitive");
    let (f, _) = parse("g.http", "POST http://x\n\n{ a }\n");
    assert!(!f.requests[0].is_graphql(), "a plain POST is not GraphQL");
}

#[test]
fn save_to() {
    let src = "### a\n# @name a\nGET http://x\n\n>> ./out/a.json\n\n### b\n# @name b\nPOST http://x\n\n{\"k\": 1}\n\n>>! ../b.bin\n\n### c\nGET http://x\n\n>>\n\n### d\nGET http://x\n\n>> one.txt\n>>! two.txt\n";
    let (f, diags) = parse("s.http", src);
    assert_eq!(f.requests.len(), 4);
    let (a, b, c, d) = (
        &f.requests[0],
        &f.requests[1],
        &f.requests[2],
        &f.requests[3],
    );
    assert_eq!(
        a.save_to,
        Some(SaveTo {
            path: "./out/a.json".to_string(),
            overwrite: false,
            line: 5,
            column: 4
        })
    );
    assert_eq!(a.body, "");
    assert_eq!(
        b.save_to,
        Some(SaveTo {
            path: "../b.bin".to_string(),
            overwrite: true,
            line: 13,
            column: 5
        })
    );
    assert_eq!(b.body, r#"{"k": 1}"#);
    assert!(c.save_to.is_none());
    assert_eq!(d.save_to.as_ref().map(|s| s.path.as_str()), Some("one.txt"));
    for dg in &diags {
        if dg.code == "bad-save-path" && dg.line == 23 {
            assert_eq!(
                (dg.column, dg.end_column),
                (5, 12),
                "second >> span: {dg:?}"
            );
        }
        // Not an editor script any more: no warning for a redirect line.
        assert_ne!(
            dg.code, "editor-script",
            "redirect reported as a script: {dg:?}"
        );
    }
    let codes: Vec<&str> = diags.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["bad-save-path", "bad-save-path"], "{diags:?}");
    assert_eq!(
        diags[0].message,
        ">> needs a file path to save the response body to"
    );
    assert_eq!(
        diags[1].message,
        "the response body is already saved to one.txt on line 23; one >>! per request"
    );
}

/// The version on a request line is HTTP/1.1 or HTTP/2; anything else is
/// an error at the version's span.
#[test]
fn http_version() {
    let (f, diags) = parse("v.http", "GET http://x/ HTTP/1.0\n\n###\nGET http://x/ HTTP/2.0\n\n###\n  POST http://x/HTTP/3 HTTP/3\n\n###\nhttp://x/ HTTP/1.1\n");
    let got: Vec<String> = f
        .requests
        .iter()
        .map(|r| match r.protocol() {
            Ok(v) => format!("{v}:false"),
            Err(_) => ":true".to_string(),
        })
        .collect();
    assert_eq!(
        got.join(","),
        "HTTP/1.1:false,HTTP/2:false,:true,HTTP/1.1:false"
    );
    assert!(
        diags.len() == 1
            && diags[0].code == "bad-http-version"
            && diags[0].line == 7
            && diags[0].column == 24
            && diags[0].end_column == 30,
        "diags: {diags:?}"
    );
    assert_eq!(
        diags[0].message,
        "HTTP version \"HTTP/3\" is not supported: write HTTP/1.1 or HTTP/2, or leave it out"
    );
}

#[test]
fn steps_and_capture_names() {
    let (f, _) = parse(
        "x.http",
        "# @step a user exists\n# @forceRef login\n# @step another phrase\nGET http://x\n",
    );
    let r = &f.requests[0];
    assert_eq!(r.steps(), ["a user exists", "another phrase"]);
    assert!(r.refs()[0].force);
    for good in ["token", "_a", "a.b-c_1"] {
        assert!(valid_capture_name(good), "{good}");
    }
    for bad in ["", "1a", "a b", "-a", "é"] {
        assert!(!valid_capture_name(bad), "{bad}");
    }
}
