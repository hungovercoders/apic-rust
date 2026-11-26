use std::collections::BTreeMap;

use crate::{check, leading, select, select_value, Headers, Kind, Response};

const DOC: &str = r#"{
  "id": 7,
  "name": "shop",
  "length": 99,
  "items": [
    {"id": "a", "name": "apple", "done": true, "price": 1.5, "tags": ["red", "fruit"]},
    {"id": "b", "name": "banana", "done": false, "price": 0.25, "tags": []},
    {"id": "c", "name": "Avocado", "done": true, "price": 2, "owner": {"id": "u1"}}
  ],
  "meta": {"k.y": "dot", "a*b": "star", "empty": [], "nothing": null, "count": 3},
  "big": 9007199254740993
}"#;

fn doc_response() -> Response {
    Response {
        body: DOC.as_bytes().to_vec(),
        ..Response::default()
    }
}

/// Every form the language has, with the value it selects on one document.
#[test]
fn paths() {
    let resp = doc_response();
    let cases: &[(&str, &str, Kind, bool)] = &[
        // What worked before keeps working, byte for byte.
        ("body.$.id", "7", Kind::Number, true),
        ("body.$.name", "shop", Kind::String, true),
        ("body.$.items[1].id", "b", Kind::String, true),
        ("body.$.items.#", "3", Kind::Number, true),
        (r#"body.$.meta["k.y"]"#, "dot", Kind::String, true),
        ("body.$.meta['a*b']", "star", Kind::String, true),
        (
            "body.$.items[0].tags",
            r#"["red", "fruit"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[2].owner",
            r#"{"id": "u1"}"#,
            Kind::Object,
            true,
        ),
        ("body.$.items.#.id", r#"["a","b","c"]"#, Kind::Array, true),
        ("body.$.big", "9007199254740993", Kind::Number, true),
        ("body.$.meta.nothing", "null", Kind::Null, true),
        ("body.$.items[0].done", "true", Kind::Bool, true),
        ("body.$.nope", "", Kind::String, false),
        // Negative indexes and slices.
        ("body.$.items[-1].id", "c", Kind::String, true),
        ("body.$.items[-3].id", "a", Kind::String, true),
        ("body.$.items[-4].id", "", Kind::String, false),
        ("body.$.items[1:3].id", r#"["b","c"]"#, Kind::Array, true),
        ("body.$.items[:2].id", r#"["a","b"]"#, Kind::Array, true),
        ("body.$.items[-2:].id", r#"["b","c"]"#, Kind::Array, true),
        ("body.$.items[5:9]", "", Kind::String, false),
        ("body.$.items[1:99].id", r#"["b","c"]"#, Kind::Array, true),
        ("body.$.items[-99:1].id", r#"["a"]"#, Kind::Array, true),
        // A numeric key on an array is an index, and a # in the middle
        // maps, so a count after it is each item's own.
        ("body.$.items.0.id", "a", Kind::String, true),
        ("body.$.items.2.owner.id", "u1", Kind::String, true),
        ("body.$.items.3.id", "", Kind::String, false),
        ("body.$.meta.0", "", Kind::String, false),
        ("body.$.items.#.tags.#", "[2,0]", Kind::Array, true),
        ("body.$.items.#.name.length", "[5,6,7]", Kind::Array, true),
        ("body.$.items.#.nope.#", "", Kind::String, false),
        // Wildcards.
        (
            "body.$.items[*].name",
            r#"["apple","banana","Avocado"]"#,
            Kind::Array,
            true,
        ),
        ("body.$.items.*.id", r#"["a","b","c"]"#, Kind::Array, true),
        // Recursive descent, document order.
        ("body.$..id", r#"[7,"a","b","c","u1"]"#, Kind::Array, true),
        (
            "body.$.items..id",
            r#"["a","b","c","u1"]"#,
            Kind::Array,
            true,
        ),
        ("body.$..tags[0]", r#"["red"]"#, Kind::Array, true),
        ("body.$..nope", "", Kind::String, false),
        // Filters.
        (
            "body.$.items[?(@.done == true)].id",
            r#"["a","c"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.done==false)].name",
            r#"["banana"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.price > 1)].id",
            r#"["a","c"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.price <= 0.25)].id",
            r#"["b"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.name =~ /^a/)].id",
            r#"["a"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.name =~ /^a/i)].id",
            r#"["a","c"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.name =~ /[bc]an/)].id",
            r#"["b"]"#,
            Kind::Array,
            true,
        ),
        (
            r#"body.$.items[?(@.id == "b" || @.id == 'c')].id"#,
            r#"["b","c"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.done == true && @.price < 2)].id",
            r#"["a"]"#,
            Kind::Array,
            true,
        ),
        ("body.$.items[?(@.owner)].id", r#"["c"]"#, Kind::Array, true),
        (
            "body.$.items[?(!@.owner)].id",
            r#"["a","b"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.owner.id == 'u1')].name",
            r#"["Avocado"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.tags.length == 2)].id",
            r#"["a"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?@.done == true].id",
            r#"["a","c"]"#,
            Kind::Array,
            true,
        ),
        ("body.$.items[?(@.price == 9)]", "", Kind::String, false),
        // Brackets inside a filter nest, and a ] in quotes is text.
        (
            r#"body.$.items[?(@.tags[0] == "red")].id"#,
            r#"["a"]"#,
            Kind::Array,
            true,
        ),
        (
            r#"body.$.items[?(@['id'] == "b")].name"#,
            r#"["banana"]"#,
            Kind::Array,
            true,
        ),
        (
            r#"body.$.items[?(@.tags[?(@ == "fruit")])].id"#,
            r#"["a"]"#,
            Kind::Array,
            true,
        ),
        (
            r#"body.$.items[?(@.name == "a]")]"#,
            "",
            Kind::String,
            false,
        ),
        // != holds where the key is missing (RFC 9535); nothing else does.
        (
            r#"body.$.items[?(@.owner.id != "u1")].id"#,
            r#"["a","b"]"#,
            Kind::Array,
            true,
        ),
        (
            r#"body.$.items[?(@.owner.id == "u2")]"#,
            "",
            Kind::String,
            false,
        ),
        (
            r#"body.$.items[?(@.owner.id < "z")].id"#,
            r#"["c"]"#,
            Kind::Array,
            true,
        ),
        // In a filter @.length on an object is its key, never a key count.
        ("body.$.items[?(@.length)]", "", Kind::String, false),
        (
            "body.$.items[?(!@.length)].id",
            r#"["a","b","c"]"#,
            Kind::Array,
            true,
        ),
        (
            "body.$.items[?(@.name.length > 5)].id",
            r#"["b","c"]"#,
            Kind::Array,
            true,
        ),
        // Counting: an array, an object, a string, and a set of matches.
        ("body.$.items.length", "3", Kind::Number, true),
        ("body.$.meta.#", "5", Kind::Number, true),
        ("body.$.items[0].name.length", "5", Kind::Number, true),
        (
            "body.$.items[?(@.done == true)].length",
            "2",
            Kind::Number,
            true,
        ),
        ("body.$.items[?(@.price == 9)].#", "0", Kind::Number, true),
        ("body.$..id.#", "5", Kind::Number, true),
        ("body.$.meta.empty.length", "0", Kind::Number, true),
        // A number, boolean or null has no count.
        ("body.$.id.#", "", Kind::String, false),
        ("body.$.items[0].done.length", "", Kind::String, false),
        ("body.$.meta.nothing.#", "", Kind::String, false),
        // An object with a real "length" key keeps it.
        ("body.$.length", "99", Kind::Number, true),
        ("body.$.nope.length", "", Kind::String, false),
        // The whole body.
        ("body.$[0]", "", Kind::String, false),
    ];
    for (sel, want, kind, ok) in cases {
        match select_value(&resp, sel) {
            Ok(Some(v)) => assert!(
                *ok && v.text == *want && v.kind == *kind,
                "{sel}: got {:?} {}; want {want:?} {kind} ok={ok}",
                v.text,
                v.kind
            ),
            Ok(None) => assert!(!ok, "{sel}: nothing there; want {want:?}"),
            Err(e) => panic!("{sel}: {e}"),
        }
        if let Err(e) = check(sel) {
            panic!("check({sel}) = {e}");
        }
    }
    let v = select_value(&resp, "body.$").unwrap().unwrap();
    assert!(
        v.kind == Kind::Object && v.text.starts_with('{'),
        "body.$ = {v:?}"
    );
}

/// What cannot be read says what it was.
#[test]
fn path_errors() {
    let cases: BTreeMap<&str, &str> = [
        ("body.$.items[0,1]", "unions are not supported"),
        ("body.$.items[0:4:2]", "slice steps are not supported"),
        ("body.$.items[x]", "expected an index"),
        ("body.$.items[0", "no closing ]"),
        ("body.$.items.", "cannot end with a dot"),
        ("body.$.a..", "cannot end with a dot"),
        ("body.$..#", ".. must be followed by"),
        ("body.$.items[?(@.a ~= 1)]", "unsupported filter"),
        (
            "body.$.items[?(@.a == nope)]",
            "expected a number, a quoted string",
        ),
        ("body.$.items[?((@.a == 1))]", "parentheses"),
        ("body.$.items[?(@.a =~ abc)]", "needs a /regex/"),
        ("body.$.items[?(@.a =~ /x/g)]", "the only regex flag is i"),
        ("body.$.items[?(@.a =~ /(/)]", "bad regex"),
        ("body.$.items[?(a == 1)]", "must start with @"),
        ("body.$.items[?(!@.a == 1)]", "only negates a presence test"),
        ("body.$.items[?()]", "empty"),
        ("body.$.items[\"a]", "no closing ]"),
        ("body.$.items[?(@.a[0 == 1)]", "no closing ]"),
        (
            r#"body.$.items.#(id=="a").name"#,
            "gjson queries are not supported",
        ),
        ("header.x[1", ""),
        ("header.", "needs a name"),
        ("cookie.", "needs a name"),
        ("bogus", "unknown selector"),
    ]
    .into_iter()
    .collect();
    let resp = doc_response();
    for (sel, want) in cases {
        let err = check(sel);
        let serr = select_value(&resp, sel);
        if want.is_empty() {
            continue;
        }
        let err = err.expect_err(sel);
        assert!(
            err.to_string().contains(want),
            "check({sel}) = {err}; want {want:?}"
        );
        assert_eq!(
            serr.expect_err(sel).to_string(),
            err.to_string(),
            "select({sel}): want the same error as check"
        );
    }
    assert!(check("bogus").unwrap_err().is_unknown());
    assert!(!check("cookie.").unwrap_err().is_unknown());
    assert_eq!(
        check(" bogus ").unwrap_err().to_string(),
        "unknown selector \"bogus\" (expected status, statusText, header.<name>, cookie.<name>, body, body.$.<path> or duration)"
    );
    assert_eq!(
        check("body.$.items[0,1]").unwrap_err().to_string(),
        "unsupported selector part \"[0,1]\": unions are not supported; select each value on its own"
    );
}

#[test]
fn multi_value_headers() {
    let mut h = Headers::new();
    for c in ["a=1", "b=2", "c=3"] {
        h.add("Set-Cookie", c);
    }
    h.add("Link", "<https://x/2>; rel=next");
    h.add("X.Correlation-ID", "abc"); // a dot is a token character
    let resp = Response {
        headers: h,
        ..Response::default()
    };
    let cases: &[(&str, &str, bool)] = &[
        ("header.set-cookie", "a=1", true),
        ("header.set-cookie.#", "3", true),
        ("header.set-cookie[1]", "b=2", true),
        ("header.set-cookie[-1]", "c=3", true),
        ("header.set-cookie[3]", "", false),
        ("header.link.#", "1", true),
        ("header.missing.#", "0", true),
        ("header.missing[0]", "", false),
        ("header.X.Correlation-Id", "abc", true),
        ("header.x.correlation-id.#", "1", true),
        ("header.set-cookie[99999999999999999999]", "", false),
    ];
    for (sel, want, ok) in cases {
        let got = select(&resp, sel).unwrap_or_else(|e| panic!("{sel}: {e}"));
        assert_eq!(got.is_some(), *ok, "{sel}");
        assert_eq!(got.unwrap_or_default(), *want, "{sel}");
    }
}

/// The selector of an assertion runs to the first space outside a bracket,
/// however deep, so a filter keeps its spaces and nested brackets.
#[test]
fn leading_selector() {
    let cases = [
        ("body.$.id == 2", "body.$.id", "== 2"),
        (
            r#"body.$.items[?(@.tags[0] == "a b")].id == 1"#,
            r#"body.$.items[?(@.tags[0] == "a b")].id"#,
            "== 1",
        ),
        (
            "body.$.items[?(@['x y'] == 1)] exists",
            "body.$.items[?(@['x y'] == 1)]",
            "exists",
        ),
        (
            "body.$.items[?(@.n =~ /] x/)] exists",
            "body.$.items[?(@.n =~ /] x/)]",
            "exists",
        ),
        ("body.$.it's == 1", "body.$.it's", "== 1"),
        ("body.$.items[0 == 1", "body.$.items[0 == 1", ""),
        ("  status   ==  200", "status", "==  200"),
    ];
    for (input, sel, rest) in cases {
        assert_eq!(leading(input), (sel, rest), "leading({input:?})");
    }
}

/// A single index or slice stops walking the array where it can.
#[test]
fn index_stops_early() {
    let mut body = String::from(r#"{"items":["#);
    for i in 0..100_000 {
        if i > 0 {
            body.push(',');
        }
        body.push_str(r#"{"id":1}"#);
    }
    body.push_str("]}");
    let resp = Response {
        body: body.into_bytes(),
        ..Response::default()
    };
    for _ in 0..20 {
        assert!(
            select(&resp, "body.$.items[0].id").unwrap().is_some(),
            "nothing at items[0].id"
        );
    }
}

/// The text of what is selected is the server's, white space and all.
#[test]
fn values_keep_their_text() {
    let resp = Response {
        body: b" { \"a\" : [ 1 , 2 ] , \"s\": \"x\\u00e9\\n\", \"n\": -1.50e+2 }\n".to_vec(),
        ..Response::default()
    };
    let get = |sel: &str| select(&resp, sel).unwrap().unwrap();
    assert_eq!(
        get("body.$"),
        "{ \"a\" : [ 1 , 2 ] , \"s\": \"x\\u00e9\\n\", \"n\": -1.50e+2 }\n"
    );
    assert_eq!(get("body.$.a"), "[ 1 , 2 ]");
    assert_eq!(get("body.$.s"), "xé\n");
    assert_eq!(get("body.$.n"), "-1.50e+2");
    assert_eq!(get("body.$.a[*]"), "[1,2]");
    assert_eq!(get("body.$[?(@ == -150)]"), "[-1.50e+2]");
    assert_eq!(get("body.$.a[?(@ == 0x2)]"), "[2]");
    let scalar = Response {
        body: b" 12 ".to_vec(),
        ..Response::default()
    };
    assert_eq!(select(&scalar, "body.$").unwrap().unwrap(), "12");
}
