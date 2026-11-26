use std::time::Duration;

use crate::{select, Headers, Response};

#[test]
fn select_forms() {
    let resp = Response {
        status: 201,
        status_text: "Created".to_string(),
        headers: Headers::from_iter([
            ("Content-Type", "application/json; charset=utf-8"),
            ("Set-Cookie", "sid=s-1; Path=/; HttpOnly"),
            ("Set-Cookie", "theme=dark"),
        ]),
        body: br#"{"id": 7, "name": "x", "items": [{"id": "a"}, {"id": "b"}], "nested": {"k.y": true}}"#.to_vec(),
        duration: Duration::from_millis(1500),
    };
    let cases: &[(&str, &str, bool, bool)] = &[
        ("status", "201", true, false),
        ("statusText", "Created", true, false),
        ("duration", "1500", true, false),
        (
            "header.content-type",
            "application/json; charset=utf-8",
            true,
            false,
        ),
        (
            "headers.Content-Type",
            "application/json; charset=utf-8",
            true,
            false,
        ),
        ("header.x-missing", "", false, false),
        ("cookie.sid", "s-1", true, false),
        ("cookie.theme", "dark", true, false),
        ("cookie.SID", "", false, false),
        ("cookie.nope", "", false, false),
        ("cookie.", "", false, true),
        ("body.$.id", "7", true, false),
        ("body.$.name", "x", true, false),
        ("body.$.items[1].id", "b", true, false),
        ("body.$.items.#", "2", true, false),
        ("body.$.nested[\"k.y\"]", "true", true, false),
        ("body.$.nope", "", false, false),
        ("body.$.items[0]", r#"{"id": "a"}"#, true, false),
        ("bogus", "", false, true),
    ];
    for (sel, want, ok, err) in cases {
        let got = select(&resp, sel);
        assert_eq!(got.is_err(), *err, "{sel}: {got:?}");
        let got = got.unwrap_or_default();
        assert_eq!(got.is_some(), *ok, "{sel}");
        assert_eq!(got.unwrap_or_default(), *want, "{sel}");
    }
    let not_json = Response {
        body: b"nope".to_vec(),
        ..Response::default()
    };
    assert_eq!(
        select(&not_json, "body.$.x").unwrap_err().to_string(),
        "body is not valid JSON"
    );
}

#[test]
fn cookies_are_read_as_the_client_reads_them() {
    let resp = Response {
        headers: Headers::from_iter([
            ("set-cookie", " quoted=\"a b\" ; Path=/"),
            ("Set-Cookie", "bad name=1"),
            ("Set-Cookie", "ctl=a\u{1}b"),
            ("Set-Cookie", "novalue"),
            ("Set-Cookie", "empty="),
            ("Set-Cookie", "eq=a=b"),
        ]),
        ..Response::default()
    };
    assert_eq!(
        select(&resp, "cookie.quoted").unwrap().as_deref(),
        Some("a b")
    );
    assert_eq!(select(&resp, "cookie.bad name").unwrap(), None);
    assert_eq!(select(&resp, "cookie.ctl").unwrap(), None);
    assert_eq!(select(&resp, "cookie.novalue").unwrap(), None);
    assert_eq!(select(&resp, "cookie.empty").unwrap().as_deref(), Some(""));
    assert_eq!(select(&resp, "cookie.eq").unwrap().as_deref(), Some("a=b"));
}
