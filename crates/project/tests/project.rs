use std::collections::HashMap;
use std::fs;
use std::path::Path;

use apic_project::{
    discover, load, load_overlay, within, Config, ConfigField, FieldKind, CONFIG_FIELDS,
    CONFIG_FILE,
};

fn write(dir: &Path, name: &str, content: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn validate_checks_assert_selector() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "api.http",
        "\n# @name t\n# @assert bogus == 1\nGET https://example.com\n",
    );
    let p = load(dir.path()).unwrap();
    let diags = p.validate();
    assert!(
        !diags.is_empty()
            && diags[0].severity == "error"
            && diags[0].message.contains("unknown selector"),
        "diagnostics: {diags:?}"
    );
    // The span covers just the selector, `bogus`, on `# @assert bogus == 1`.
    let d = &diags[0];
    assert!(
        d.code == "unknown-selector"
            && d.line == 3
            && d.column == 11
            && d.end_line == 3
            && d.end_column == 16,
        "span/code: {d:?}"
    );
}

/// Pins the code and span of each project-level check, since editors and
/// the github/sarif formats key on them.
#[test]
fn validate_spans_and_codes() {
    let dir = tempfile::tempdir().unwrap();
    let src = "### a\n# @name dup\n# @step I run {x}\n# @auth bogus\n# @capture v = nope.$\nGET https://example.com\n\n### b\n# @name dup\n# @assert status =\nPOST https://example.com\n\n< ./missing.json\n\n### c\n# @name c\n# @ref nope\n# @ref d\nGET https://example.com\n\n### d\n# @name d\n# @ref c\n# @retry soon\nGET https://example.com\n\n### e\n# @name e\n# @ref e\nGET https://example.com\n\n### f\n# @name f\nPOST https://example.com\nContent-Type: multipart/form-data; boundary=b\n\n--b\nContent-Disposition: form-data; name=\"f\"; filename=\"x\"\n\n< ./nope.bin\n--b--\n\n### g\n# @name g\nPOST https://example.com\nContent-Type: multipart/form-data\n\n--b\n--b--\n";
    write(dir.path(), "api.http", src);
    let p = load(dir.path()).unwrap();
    type Span = (usize, usize, usize);
    let want: [(&str, &[Span]); 10] = [
        ("duplicate-name", &[(2, 9, 12), (9, 9, 12)]),
        ("ambiguous-step", &[(3, 9, 18)]),
        ("bad-auth", &[(4, 9, 14)]),
        ("unknown-selector", &[(5, 16, 22)]),
        ("bad-assert", &[(10, 11, 19)]),
        ("missing-body-file", &[(13, 3, 17), (40, 3, 13)]),
        ("bad-multipart", &[(48, 0, 0)]),
        ("bad-ref", &[(17, 8, 12)]),
        ("ref-cycle", &[(18, 8, 9), (23, 8, 9), (29, 8, 9)]),
        ("bad-retry", &[(24, 10, 14)]),
    ];
    let mut got: HashMap<String, Vec<Span>> = HashMap::new();
    for d in p.validate() {
        got.entry(d.code.clone())
            .or_default()
            .push((d.line, d.column, d.end_column));
        if d.column > 0 {
            assert_eq!(d.end_line, d.line, "{}: end line", d.code);
        }
    }
    for (code, spans) in want {
        let found = got.get(code).map_or(&[][..], Vec::as_slice);
        assert_eq!(found, spans, "{code}");
    }
    for code in got.keys() {
        assert!(
            apic_httpfile::code_description(code).is_some(),
            "code {code:?} is not documented in apic_httpfile::CODES"
        );
    }
}

#[test]
fn validate_warns_when_default_exec_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), CONFIG_FILE, "auth:\n  default: exec whoami\n");
    let p = load(dir.path()).unwrap();
    let diags = p.validate();
    assert!(
        !diags.is_empty()
            && diags[0].severity == "warning"
            && diags[0]
                .message
                .contains("auth.default: @auth exec will be refused"),
        "diagnostics: {diags:?}"
    );
}

#[test]
fn load_skips_testdata_dir() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "testdata/broken.http",
        "\n# @name t\n# @capture nope\nGET https://example.com\n",
    );
    let p = load(dir.path()).unwrap();
    assert!(
        p.files.is_empty() && p.diagnostics.is_empty(),
        "expected testdata dir to be skipped, got files={:?} diagnostics={:?}",
        p.files,
        p.diagnostics
    );
}

fn validate_reports(src: &str, needle: &str) {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "api.http", src);
    let p = load(dir.path()).unwrap();
    let diags = p.validate();
    assert!(
        diags
            .iter()
            .any(|d| d.severity == "error" && d.message.contains(needle)),
        "{needle:?} not reported: {diags:?}"
    );
}

#[test]
fn validate_duplicate_phrase_on_same_request() {
    validate_reports(
        "### a\n# @name a\n# @step I do it\n# @step I do it\nGET http://x\n",
        r#"@step "I do it" matches the same text as @step "I do it""#,
    );
}

#[test]
fn validate_duplicate_phrase_by_matcher() {
    validate_reports(
        "### a\n# @name a\n# @step I do {x}\nGET http://x\n\n### b\n# @name b\n# @step I do {y}\nGET http://y\n",
        r#"@step "I do {y}" matches the same text as @step "I do {x}" on a (api.http:3)"#,
    );
}

#[test]
fn validate_phrase_conflicts_with_builtin() {
    validate_reports(
        "### a\n# @name a\n# @step I run {thing}\nGET http://x\n",
        "the built-in step run",
    );
}

#[test]
fn validate_rejects_too_many_phrase_params() {
    validate_reports(
        "### a\n# @name a\n# @step {a} {b} {c} {d} {e} {f} {g}\nGET http://x\n",
        "more than 6 parameters",
    );
}

/// A body path apic cannot read is caught by validate, with the construct
/// named; the JSONPath forms it can read pass.
#[test]
fn validate_reads_body_paths() {
    let dir = tempfile::tempdir().unwrap();
    let src = "### a\n# @name a\n# @assert body.$.items[?(@.done == true)].length == 2\n# @assert body.$..id exists\n# @assert header.set-cookie.# == 2\n# @capture last = body.$.items[-1].id\n# @assert body.$.items[0,1] exists\n# @capture bad = body.$.items[?(@.a ~= 1)]\nGET https://example.com\n";
    write(dir.path(), "api.http", src);
    let p = load(dir.path()).unwrap();
    let diags = p.validate();
    assert_eq!(diags.len(), 2, "diagnostics: {diags:?}");
    let d = &diags[0];
    assert!(
        d.code == "unknown-selector"
            && d.line == 7
            && d.message.contains("unions are not supported"),
        "union: {d:?}"
    );
    let d = &diags[1];
    assert!(
        d.code == "unknown-selector" && d.line == 8 && d.message.contains("unsupported filter"),
        "filter: {d:?}"
    );
}

#[test]
fn load_overlay_parses_buffers_in_place_of_files() {
    let dir = tempfile::tempdir().unwrap();
    // The root as the project knows it, so the overlay's paths match.
    let root = fs::canonicalize(dir.path()).unwrap();
    let at = |rel: &str| root.join(rel).to_string_lossy().into_owned();
    write(&root, "api.http", "### a\n# @name on-disk\nGET http://x\n");
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = elsewhere
        .path()
        .join("elsewhere.http")
        .to_string_lossy()
        .into_owned();
    let overlay: HashMap<String, String> = [
        (
            at("api.http"),
            "### a\n# @name edited\n# @capture nope\nGET http://x\n",
        ),
        (at("new.http"), "### b\n# @name unsaved\nGET http://y\n"),
        (at("notes.txt"), "not a request file"),
        (
            at("testdata/broken.http"),
            "### x\n# @name edited\n# @capture nope\nGET http://x\n",
        ),
        (
            at(".github/smoke.http"),
            "### y\n# @name hidden\nGET http://x\n",
        ),
        (
            at("node_modules/pkg/x.http"),
            "### z\n# @name vendored\nGET http://x\n",
        ),
        (outside, "### c\n# @name outside\nGET http://z\n"),
    ]
    .into_iter()
    .map(|(path, text)| (path, text.to_string()))
    .collect();
    let p = load_overlay(&root, &overlay).unwrap();
    let names: Vec<String> = p.requests().iter().map(|r| r.name.clone()).collect();
    assert_eq!(
        names.join(","),
        "edited,unsaved",
        "want the buffer's and the unsaved file's only"
    );
    assert!(
        p.diagnostics.len() == 1
            && p.diagnostics[0].path == "api.http"
            && p.diagnostics[0].code == "bad-capture",
        "diagnostics = {:?}, want the buffer's bad capture",
        p.diagnostics
    );
}

// The tests below cover what the suite above leaves to the packages that
// build on this one: discovery, apic.yaml and target resolution.

#[test]
fn discovers_request_files_sorted() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "b.http",
        "a.rest",
        "notes.txt",
        "sub/c.http",
        "sub/deep/d.http",
        ".hidden/e.http",
        "vendor/f.http",
        "node_modules/g.http",
        ".apic/h.http",
        "A.http",
    ] {
        write(dir.path(), name, "GET http://x\n");
    }
    let root = dir.path().to_string_lossy().into_owned();
    let found = discover(dir.path()).unwrap();
    let rel: Vec<&str> = found
        .iter()
        .map(|p| p.strip_prefix(&root).unwrap().trim_start_matches('/'))
        .collect();
    assert_eq!(
        rel,
        [
            "A.http",
            "a.rest",
            "b.http",
            "sub/c.http",
            "sub/deep/d.http"
        ]
    );
    // A file is its own project of one; anything else holds nothing.
    let one = dir.path().join("b.http");
    assert_eq!(discover(&one).unwrap(), [one.to_string_lossy()]);
    assert!(discover(dir.path().join("notes.txt")).unwrap().is_empty());
    let err = discover(dir.path().join("nope")).unwrap_err().to_string();
    assert_eq!(err, format!("lstat {root}/nope: no such file or directory"));
}

#[test]
fn load_reads_the_configuration() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        CONFIG_FILE,
        "env: dev\ndir: api\ntimeout: 30\nhistory: 5\nunknown: ignored\ntls:\n  verifyHost: no\n  hosts:\n    '*.internal': {caFile: ca.pem}\n",
    );
    write(dir.path(), "api/users.http", "# @name list\nGET http://x\n");
    write(
        dir.path(),
        "other/skipped.http",
        "# @name no\nGET http://x\n",
    );
    let p = load(dir.path()).unwrap();
    assert_eq!(p.config.env, "dev");
    assert_eq!(p.config.timeout, "30");
    assert_eq!(p.config.history, 5);
    assert_eq!(p.config.tls.verify_host, Some(false));
    assert_eq!(p.config.tls.hosts["*.internal"].ca_file, "ca.pem");
    assert_eq!(p.config.tls.hosts["*.internal"].verify_host, None);
    // Only `dir` is scanned, and paths stay relative to the root.
    assert_eq!(p.files.len(), 1);
    assert_eq!(p.files[0].path, "api/users.http");
    assert_eq!(p.lookup("list").unwrap().id(), "list");
    assert!(std::path::Path::new(&p.root).is_absolute());
}

#[test]
fn load_reports_a_bad_configuration() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), CONFIG_FILE, "history: soon\ncookies: 3\n");
    assert_eq!(
        load(dir.path()).unwrap_err().to_string(),
        "apic.yaml: yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `soon` into int\n  line 2: cannot unmarshal !!int `3` into bool"
    );
    write(dir.path(), CONFIG_FILE, "env: dev\nauth: [\n");
    assert_eq!(
        load(dir.path()).unwrap_err().to_string(),
        "apic.yaml: yaml: line 2: did not find expected node content"
    );
}

#[test]
fn resolves_targets() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "users.http",
        "### one\n# @name get-user\nGET http://x/1\n\n### two\nGET http://x/2\n\n### three\n# @name dup\n# @capture token = body.$.t\nGET http://x/3\n",
    );
    write(
        dir.path(),
        "sub/orders.http",
        "# @name dup\nGET http://y\n\n###\n# @name list-orders\nGET http://y\n",
    );
    write(dir.path(), "plain.http", "GET http://z\n");
    let p = load(dir.path()).unwrap();
    let root = p.root.clone();
    let ids = |target: &str| -> Vec<String> {
        p.resolve(target).unwrap().iter().map(|r| r.id()).collect()
    };
    assert_eq!(ids("get-user"), ["get-user"]);
    assert_eq!(ids("users.http"), ["get-user", "users.http#2", "dup"]);
    assert_eq!(ids("./users.http"), ["get-user", "users.http#2", "dup"]);
    assert_eq!(ids(&format!("{root}/users.http#2")), ["users.http#2"]);
    assert_eq!(ids("users.http#get-user"), ["get-user"]);
    assert_eq!(ids("sub/orders.http#1"), ["dup"]);
    assert!(p.file("users.http").is_some());
    assert!(p.file("users.txt").is_none());
    assert!(p.file("nope.http").is_none());
    // The same request found twice is the same allocation.
    assert!(std::sync::Arc::ptr_eq(
        &p.lookup("get-user").unwrap(),
        &p.resolve("users.http#1").unwrap()[0]
    ));
    assert_eq!(p.captured_by("token").unwrap().id(), "dup");
    assert!(p.captured_by("nope").is_none());

    let err = |target: &str| p.resolve(target).unwrap_err();
    assert_eq!(
        err("dup").to_string(),
        r#"request name "dup" is defined more than once (sub/orders.http:2, users.http:11); use file#name"#
    );
    assert!(err("dup").not_found().is_none());
    assert_eq!(
        err("users.http#9").to_string(),
        "users.http has 3 requests, no #9"
    );
    assert_eq!(
        err("users.http#0").to_string(),
        "users.http has 3 requests, no #0"
    );
    let e = err("get-usr");
    assert_eq!(
        e.to_string(),
        r#"no request named "get-usr"; did you mean get-user? (run `apic list` for every id)"#
    );
    assert_eq!(e.suggestions(), ["get-user"]);
    assert_eq!(e.not_found().unwrap().closest, ["get-user"]);
    let e = err("zzzzzzzz");
    assert_eq!(
        e.to_string(),
        r#"no request named "zzzzzzzz"; known: dup, get-user, list-orders (run `apic list`)"#
    );
    assert!(e.suggestions().is_empty() && e.not_found().is_some());
    assert_eq!(
        err("users.http#get-usr").to_string(),
        r#"no request named "get-usr" in users.http; did you mean get-user?"#
    );
    assert_eq!(
        err("users.http#zzzzzzzz").to_string(),
        r#"no request named "zzzzzzzz" in users.http; it has: get-user, dup"#
    );
    assert_eq!(
        err("plain.http#zzzzzzzz").to_string(),
        r#"no request named "zzzzzzzz" in plain.http, which has no named requests; use plain.http#N"#
    );
    assert_eq!(
        err("user.http#1").to_string(),
        format!(r#"no .http file "user.http" in {root}; did you mean users.http?"#)
    );
    assert_eq!(
        err("qqqqqqqqqqqq.http#1").to_string(),
        format!(
            r#"no .http file "qqqqqqqqqqqq.http" in {root}; files: plain.http, sub/orders.http, users.http (run `apic list`)"#
        )
    );
}

#[test]
fn lookup_explains_an_empty_project() {
    let dir = tempfile::tempdir().unwrap();
    let p = load(dir.path()).unwrap();
    assert_eq!(
        p.lookup("x").unwrap_err().to_string(),
        format!(
            r#"no request named "x": no .http files found under {} (new here? apic init scaffolds a project, and apic ui --demo shows a working one)"#,
            p.root
        )
    );
    write(dir.path(), "a.http", "GET http://x\n");
    let p = load(dir.path()).unwrap();
    assert_eq!(
        p.lookup("x").unwrap_err().to_string(),
        format!(
            r#"no request named "x": no named requests found under {} (add `# @name x` above a request line)"#,
            p.root
        )
    );
}

#[test]
fn validate_checks_files_and_configuration() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        CONFIG_FILE,
        "retry: never\nauth:\n  default: bogus\n",
    );
    write(dir.path(), "schema.json", "{}");
    write(
        dir.path(),
        "api/a.http",
        "# @name a\n# @assert body matchesSchema ../schema.json\n# @assert body matchesSchema ./nope.json\n# @assert body matchesSchema ../../out.json\n# @assert body matchesSchema {{dir}}/x.json\n# @sleep soon\n# @auth exec whoami\nGET http://x\n\n>> ../../outside.txt\n",
    );
    let p = load(dir.path()).unwrap();
    let got: Vec<String> = p.validate().iter().map(ToString::to_string).collect();
    let bad_auth = apic_auth::parse("bogus").unwrap_err();
    let bad_sleep = apic_httpfile::parse_sleep("soon").unwrap_err();
    assert_eq!(
        got,
        [
            "api/a.http:3:30: error: schema file ./nope.json not found".to_string(),
            "api/a.http:4:30: error: schema file ../../out.json resolves outside the project root".to_string(),
            format!("api/a.http:6:10: error: @sleep: {bad_sleep}"),
            "api/a.http:7:9: warning: @auth exec will be refused until apic.yaml sets auth.allowExec: true".to_string(),
            "api/a.http:10:4: error: >> ../../outside.txt resolves outside the project root; the response body is only written inside it".to_string(),
            format!("apic.yaml:0: error: auth.default: {bad_auth}"),
            "apic.yaml:0: error: retry \"never\": attempts must be a whole number of at least 1, got \"never\"".to_string(),
        ]
    );
}

#[test]
fn within_is_lexical() {
    assert_eq!(within("/r", "/r/a/b.http").as_deref(), Some("a/b.http"));
    assert_eq!(within("/r", "/r").as_deref(), Some("."));
    assert_eq!(within("/r/", "/r/x/../y").as_deref(), Some("y"));
    assert_eq!(within("/r", "/r/..a").as_deref(), Some("..a"));
    assert_eq!(within("/r", "/r/../s"), None);
    assert_eq!(within("/r", "/"), None);
    assert_eq!(within("/r", "relative"), None);
}

/// The names of a struct's fields, in order, read off its `Debug` form.
fn debug_fields(debug: &str) -> Vec<String> {
    debug
        .lines()
        .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
        .filter_map(|l| l.trim().split_once(':').map(|(name, _)| name.to_string()))
        .collect()
}

fn names(fields: &[ConfigField]) -> Vec<&str> {
    fields.iter().map(|f| f.name).collect()
}

fn section(key: &str) -> &'static [ConfigField] {
    match CONFIG_FIELDS.iter().find(|f| f.key == key).unwrap().kind {
        FieldKind::Struct(fields) => fields,
        other => panic!("{key} is {other:?}"),
    }
}

/// The table the schema generator walks lists every field of the structs,
/// in the order they are declared, under the key the file uses.
#[test]
fn config_fields_match_the_structs() {
    let config = Config::default();
    assert_eq!(names(CONFIG_FIELDS), debug_fields(&format!("{config:#?}")));
    assert_eq!(
        names(section("auth")),
        debug_fields(&format!("{:#?}", config.auth))
    );
    assert_eq!(
        names(section("test")),
        debug_fields(&format!("{:#?}", config.test))
    );
    assert_eq!(
        names(section("tls")),
        debug_fields(&format!("{:#?}", config.tls))
    );
    let FieldKind::Map(FieldKind::Struct(host)) = section("tls")[4].kind else {
        panic!("tls.hosts is {:?}", section("tls")[4].kind);
    };
    assert_eq!(
        names(host),
        debug_fields(&format!("{:#?}", apic_project::TlsHost::default()))
    );

    let keys: Vec<&str> = CONFIG_FIELDS.iter().map(|f| f.key).collect();
    assert_eq!(
        keys,
        [
            "env",
            "dir",
            "timeout",
            "retry",
            "cookies",
            "history",
            "proxy",
            "noProxy",
            "maxBodyBytes",
            "auth",
            "test",
            "tls"
        ]
    );
    let kinds: Vec<FieldKind> = CONFIG_FIELDS.iter().map(|f| f.kind).collect();
    assert_eq!(kinds[0], FieldKind::String);
    assert_eq!(kinds[4], FieldKind::Bool);
    assert_eq!(kinds[5], FieldKind::Int);
    assert_eq!(kinds[7], FieldKind::Slice(&FieldKind::String));
    assert_eq!(kinds[8], FieldKind::Int);
    let tls: Vec<(&str, FieldKind)> = section("tls").iter().map(|f| (f.key, f.kind)).collect();
    assert_eq!(
        tls[3],
        ("verifyHost", FieldKind::Optional(&FieldKind::Bool))
    );
    assert_eq!(tls[4].0, "hosts");
    let auth: Vec<&str> = section("auth").iter().map(|f| f.key).collect();
    assert_eq!(auth, ["default", "allowExec"]);
    assert_eq!(section("test")[0].key, "paths");
    assert_eq!(
        host.iter().map(|f| f.key).collect::<Vec<_>>(),
        ["caFile", "certFile", "keyFile", "verifyHost"]
    );
}

/// A file that sets every key in the table sets a field for each, so no
/// key in the table is one the decoder does not know.
#[test]
fn every_key_in_the_table_is_decoded() {
    fn yaml(fields: &[ConfigField], indent: usize, out: &mut String) {
        for f in fields {
            let pad = " ".repeat(indent);
            match f.kind {
                FieldKind::Struct(inner) => {
                    out.push_str(&format!("{pad}{}:\n", f.key));
                    yaml(inner, indent + 2, out);
                }
                FieldKind::Map(FieldKind::Struct(inner)) => {
                    out.push_str(&format!("{pad}{}:\n{pad}  name:\n", f.key));
                    yaml(inner, indent + 4, out);
                }
                FieldKind::String => out.push_str(&format!("{pad}{}: text\n", f.key)),
                FieldKind::Bool | FieldKind::Optional(FieldKind::Bool) => {
                    out.push_str(&format!("{pad}{}: true\n", f.key));
                }
                FieldKind::Int => out.push_str(&format!("{pad}{}: 7\n", f.key)),
                FieldKind::Slice(FieldKind::String) => {
                    out.push_str(&format!("{pad}{}: [text]\n", f.key));
                }
                other => panic!("no sample for {other:?}"),
            }
        }
    }
    let mut text = String::new();
    yaml(CONFIG_FIELDS, 0, &mut text);
    let set = Config::from_yaml(text.as_bytes()).unwrap();
    let set = format!("{set:#?}");
    // One value for each key that holds one.
    assert_eq!(set.matches("\"text\"").count(), 14, "{set}");
    assert_eq!(set.matches("true").count(), 4, "{set}");
    assert_eq!(set.matches(": 7").count(), 2, "{set}");
}
