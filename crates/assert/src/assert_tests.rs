use std::cell::Cell;
use std::collections::BTreeMap;

use apic_selector::{Headers, Response};

use crate::assert::compare;
use crate::{eval, eval_with, parse, parse_number, redact, schemas, Options, Result};

#[test]
fn parse_and_eval() {
    let resp = Response {
        status: 200,
        headers: Headers::from_iter([("Content-Type", "application/json")]),
        body: br#"{"id": 7, "name": "alice", "tags": ["a"]}"#.to_vec(),
        ..Response::default()
    };
    let cases = [
        ("status == 200", true),
        ("status != 200", false),
        ("status < 300", true),
        ("status >= 201", false),
        ("body.$.id == 7.0", true),
        ("body.$.name == alice", true),
        ("body.$.name == \"alice\"", true),
        ("body.$.name contains lic", true),
        ("body.$.name startsWith al", true),
        ("body.$.name endsWith ce", true),
        ("body.$.name matches ^a.*e$", true),
        ("header.content-type contains json", true),
        ("body.$.tags exists", true),
        ("body.$.missing exists", false),
        ("body.$.missing not exists", true),
        ("body.$.missing == 1", false),
        ("body.$.name < 3", false),
    ];
    for (expr, pass) in cases {
        let e = parse(expr).unwrap_or_else(|err| panic!("{expr}: {err}"));
        let r = eval(&e, &e.value, &resp);
        assert_eq!(r.pass, pass, "{expr}: {r:?}");
    }
    for bad in [
        "status",
        "status ~= 1",
        "",
        "status ==",
        "status exists extra",
        "status not exists extra",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} should not parse");
    }
}

#[test]
fn compare_numbers_exactly() {
    let cases = [
        ("9007199254740993", "==", "9007199254740992", false),
        ("9007199254740993", "!=", "9007199254740992", true),
        ("9007199254740993", ">", "9007199254740992", true),
        ("1.0", "==", "1", true),
        ("1e2", "==", "100", true),
        ("99", "<", "1e2", true),
        ("abc", "==", "abc", true),
        ("1/2", "==", "0.5", false),
        ("1e1000", ">", "1e999", true),
        ("1e-1000", "<", "1", true),
        ("-0.5", "<", "+.5", true),
    ];
    for (actual, op, expected, want) in cases {
        let (got, why) = compare(actual, op, expected);
        assert!(
            got == want && why.is_empty(),
            "{actual} {op} {expected}: got {got} ({why}), want {want}"
        );
    }
    assert!(
        !compare("Inf", "<", "5").1.is_empty(),
        "Inf is not an exact number and must not compare numerically"
    );
    // Untrusted values with absurd exponents or lengths are not expanded.
    for huge in [
        "1e1000000000".to_string(),
        "1e-1000000000".to_string(),
        format!("1{}", "0".repeat(5000)),
    ] {
        let short = &huge[..huge.len().min(20)];
        assert!(
            parse_number(&huge).is_none(),
            "{short}… must not be parsed as an exact number"
        );
        assert!(
            !compare(&huge, "<", "5").1.is_empty(),
            "{short}… must not compare numerically"
        );
        assert!(
            compare(&huge, "==", &huge).0,
            "{short}… still compares as text"
        );
    }
    assert!(
        parse_number("1e4096").is_some(),
        "an exponent within the bound is parsed"
    );
    assert!(
        parse_number(&format!("1e1{}", "0".repeat(50))).is_none(),
        "an over-long exponent is rejected before it is converted"
    );
    assert!(
        parse_number(&format!("1e{}1", "0".repeat(50))).is_some(),
        "leading zeroes never make an exponent over-long"
    );
    assert!(
        parse_number("1e+0004096").is_some(),
        "leading zeroes do not count towards the exponent bound"
    );
    assert!(
        compare("1e+0004096", "==", "1e4096").0,
        "1e+0004096 and 1e4096 are the same number"
    );
    assert!(
        parse_number("1e-0000000").is_some(),
        "an all-zero exponent is fine"
    );
    assert!(parse_number("1e4097").is_none() && parse_number("1e-4097").is_none());
    assert_eq!(
        compare("a", "<", "5"),
        (
            false,
            "< needs numeric operands, got \"a\" and \"5\"".to_string()
        )
    );
    assert_eq!(
        compare("a", "~", "5"),
        (false, "unknown operator ~".to_string())
    );
    assert!(compare("a", "matches", "(").1.starts_with("bad regex: "));
}

/// A selector with spaces inside a bracket is one selector.
#[test]
fn parse_filter_selector() {
    let e = parse(r#"body.$.items[?(@.name == "a b")].length == 2"#).unwrap();
    assert!(
        e.selector == r#"body.$.items[?(@.name == "a b")].length"#
            && e.op == "=="
            && e.value == "2",
        "{e:?}"
    );
    let e = parse("body.$.items[?(@.x =~ /a ]b/)] exists").unwrap();
    assert!(
        e.selector == "body.$.items[?(@.x =~ /a ]b/)]" && e.op == "exists",
        "{e:?}"
    );
}

#[test]
fn predicates_and_length() {
    let resp = Response {
        status: 200,
        status_text: "OK".to_string(),
        body: r#"{"id": 7, "ratio": 0.5, "big": 1e3, "name": "zoë", "blank": "", "ok": true, "none": null, "items": [1, 2, 3], "empty": [], "obj": {"a": 1}, "nothing": {}}"#.as_bytes().to_vec(),
        ..Response::default()
    };
    let cases = [
        ("body.$.id isInteger", true, "", "number"),
        ("body.$.id isNumber", true, "", "number"),
        ("body.$.ratio isInteger", false, "", "number"),
        ("body.$.ratio not isInteger", true, "", "number"),
        ("body.$.big isInteger", true, "", "number"),
        ("body.$.name isString", true, "", "string"),
        ("body.$.id isString", false, "", "number"),
        ("body.$.ok isBoolean", true, "", "boolean"),
        ("body.$.none isNull", true, "", "null"),
        ("body.$.none not isNull", false, "", "null"),
        ("body.$.items isArray", true, "", "array"),
        ("body.$.obj isObject", true, "", "object"),
        ("body.$.items not isObject", true, "", "array"),
        ("status isNumber", true, "", "number"),
        ("statusText isString", true, "", "string"),
        ("body.$.blank isEmpty", true, "", ""),
        ("body.$.empty isEmpty", true, "", "[]"),
        ("body.$.nothing isEmpty", true, "", "{}"),
        ("body.$.items not isEmpty", true, "", "[1, 2, 3]"),
        (
            "body.$.id isEmpty",
            false,
            "isEmpty needs a string, an array or an object, got number 7",
            "7",
        ),
        (
            "body.$.missing isString",
            false,
            "no value at body.$.missing",
            "",
        ),
        ("body.$.items length == 3", true, "", "3"),
        ("body.$.items length >= 4", false, "", "3"),
        ("body.$.name length == 3", true, "", "3"),
        ("body.$.obj length == 1", true, "", "1"),
        ("body.$.empty length == 0", true, "", "0"),
        (
            "body.$.id length == 1",
            false,
            "length needs a string, an array or an object, got number 7",
            "7",
        ),
        ("body.$.items[?(@ > 1)] length == 2", true, "", "2"),
    ];
    for (expr, pass, err_has, actual) in cases {
        let e = parse(expr).unwrap_or_else(|err| panic!("{expr}: {err}"));
        let r = eval(&e, &e.value, &resp);
        let err_ok = if err_has.is_empty() {
            r.error.is_empty()
        } else {
            r.error.contains(err_has)
        };
        assert!(
            r.pass == pass && err_ok && r.actual == actual,
            "{expr}: pass={} err={:?} actual={:?}; want pass={pass} err~{err_has:?} actual={actual:?}",
            r.pass,
            r.error,
            r.actual
        );
        assert_eq!(r.expr, expr, "{expr}: displayed as {:?}", r.expr);
    }
    let bad: BTreeMap<&str, &str> = [
        ("body.$.id isString 1", "does not take a value"),
        ("body.$.id not isString x", "does not take a value"),
        ("body.$.items length", "length <op> <number>"),
        ("body.$.items length ~= 3", "length takes one of"),
        ("body.$.items isnt", "unknown operator"),
        ("body.$ matchesSchema", "expected `<selector> <op> <value>`"),
    ]
    .into();
    for (expr, want) in bad {
        let err = parse(expr).expect_err(expr);
        assert!(err.contains(want), "{expr}: err = {err}; want {want:?}");
    }
    assert_eq!(
        parse("body.$.items isnt").unwrap_err(),
        "assert \"body.$.items isnt\": unknown operator \"isnt\" (one of ==, !=, <=, >=, <, >, contains, matches, startsWith, endsWith, matchesSchema, exists, not exists, length <op>, or a predicate: isString, isNumber, isInteger, isBoolean, isArray, isObject, isNull, isEmpty)"
    );
    // Redaction keeps the operator words and a filter whole.
    let redactions: BTreeMap<&str, &str> = [
        ("body.$.items length == 3", "body.$.items length == ***"),
        ("body.$.id not isString", "body.$.id not isString"),
        (
            r#"body.$.items[?(@.a == "x y")].id == abc"#,
            r#"body.$.items[?(@.a == "x y")].id == ***"#,
        ),
        ("body.$ matchesSchema ./s.json", "body.$ matchesSchema ***"),
        // What parse cannot read is masked after two words, never shown.
        ("body.$.user name == s3cret", "body.$.user name ***"),
        ("body.$.items[0 == s3cret", "body.$.items[0 == ***"),
        ("body.$.x s3cret", "body.$.x ***"),
        ("body.$.x", "body.$.x"),
    ]
    .into();
    for (expr, want) in redactions {
        assert_eq!(redact(expr, "***"), want, "redact({expr})");
    }
}

#[test]
fn matches_schema() {
    let schema = r##"{
	  "$schema": "https://json-schema.org/draft/2020-12/schema",
	  "type": "object",
	  "required": ["id", "name"],
	  "properties": {
	    "id": {"type": "integer"},
	    "name": {"type": "string", "minLength": 1},
	    "tags": {"type": "array", "items": {"$ref": "#/$defs/tag"}}
	  },
	  "$defs": {"tag": {"type": "string"}}
	}"##;
    let load = |path: &str| -> std::result::Result<Vec<u8>, String> {
        match path {
            "user.json" => Ok(schema.as_bytes().to_vec()),
            "broken.json" => Ok(b"{nope".to_vec()),
            _ => Err(format!("{path}: no such file")),
        }
    };
    let reads = Cell::new(0);
    let loader = schemas(|path: &str| {
        reads.set(reads.get() + 1);
        load(path)
    });
    let opts = Options {
        schema: Some(&loader),
    };
    let run = |body: &str, expr: &str| -> Result {
        let e = parse(expr).unwrap();
        let resp = Response {
            body: body.as_bytes().to_vec(),
            ..Response::default()
        };
        eval_with(&e, &e.value, &resp, opts)
    };
    let r = run(
        r#"{"id": 1, "name": "a", "tags": ["x"]}"#,
        "body.$ matchesSchema user.json",
    );
    assert!(r.pass && r.error.is_empty(), "valid: {r:?}");
    let r = run(
        r#"{"id": "1", "name": "a"}"#,
        "body.$ matchesSchema user.json",
    );
    assert!(
        !r.pass && r.error.is_empty() && r.actual.contains("id"),
        "wrong type: {r:?}"
    );
    assert_eq!(
        r.actual,
        "validating root: validating /properties/id: type: 1 has type \"string\", want \"integer\""
    );
    let r = run(
        r#"{"id": 1, "name": "a", "tags": [3]}"#,
        "body.$ matchesSchema user.json",
    );
    assert!(!r.pass && r.actual.contains("tags"), "$ref: {r:?}");
    let r = run(
        r#"{"user": {"id": 1, "name": "a"}}"#,
        "body.$.user matchesSchema user.json",
    );
    assert!(r.pass, "a path into the body: {r:?}");
    let r = run(r#"{"id": 1, "name": "a"}"#, "body matchesSchema user.json");
    assert!(r.pass, "the raw body is read as JSON: {r:?}");
    assert_eq!(reads.get(), 1, "a loader resolves each file once");
    let r = run("{}", "body.$ matchesSchema missing.json");
    assert!(r.error.contains("no such file"), "missing: {r:?}");
    for _ in 0..2 {
        let r = run("{}", "body.$ matchesSchema broken.json");
        assert_eq!(
            r.error,
            "schema broken.json: invalid character 'n' looking for beginning of object key string",
            "broken: {r:?}"
        );
    }
    assert_eq!(reads.get(), 3, "a file that fails is not read again either");
    let e = parse("body.$ matchesSchema user.json").unwrap();
    let r = eval(
        &e,
        &e.value,
        &Response {
            body: b"{}".to_vec(),
            ..Response::default()
        },
    );
    assert_eq!(
        r.error, "matchesSchema is not available here",
        "without a loader the operator refuses"
    );
    let r = run("not json", "body matchesSchema user.json");
    assert_eq!(
        r.error,
        "matchesSchema needs JSON, and body is not: invalid character 'o' in literal null (expecting 'u')"
    );
}

#[test]
fn results_serialise_without_their_empty_fields() {
    let r = Result {
        expr: "status == 200".to_string(),
        pass: true,
        actual: "200".to_string(),
        expected: "200".to_string(),
        error: String::new(),
    };
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"expr":"status == 200","pass":true,"actual":"200","expected":"200"}"#
    );
    assert_eq!(
        serde_json::to_string(&Result::default()).unwrap(),
        r#"{"expr":"","pass":false}"#
    );
}
