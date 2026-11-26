use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::{compare, Change, Error, RangeError, Store, Time};

fn result(status: i64, body: &str) -> String {
    format!(
        r#"{{"ok":true,"request":{{"method":"GET","url":"http://x/"}},"response":{{"status":{status},"status_text":"OK","headers":{{}},"body":{body},"duration_ms":12,"size":3}}}}"#
    )
}

fn t0() -> Time {
    Time::parse_rfc3339("2026-10-06T09:00:00Z").unwrap()
}

const SECOND: Duration = Duration::from_secs(1);
const MINUTE: Duration = Duration::from_secs(60);

fn compact(raw: &Option<String>) -> String {
    apic_gocompat::compact_json(raw.as_deref().unwrap_or(""), false)
}

#[test]
fn record_keeps_the_newest_n() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 3);
    for i in 0..5 {
        s.record(
            "dev",
            "login",
            t0() + MINUTE * i,
            &result(200 + i64::from(i), &format!(r#"{{"n":{i}}}"#)),
        )
        .unwrap();
    }
    let list = s.list("dev", "login").unwrap();
    assert_eq!(list.len(), 3, "kept {} entries, want 3", list.len());
    for (i, e) in list.iter().enumerate() {
        assert!(
            e.index == i + 1 && e.status == 204 - i as i64,
            "entry {i} = index {} status {}",
            e.index,
            e.status
        );
        assert!(
            e.result.is_none(),
            "list returned a result for entry {}",
            e.index
        );
    }
    assert!(
        list[0].duration_ms == 12 && list[0].size == 3 && list[0].ok && list[0].status_text == "OK",
        "summary = {:?}",
        list[0]
    );
    assert_eq!(list[0].time, t0() + MINUTE * 4);
    assert_eq!(
        list[0].file,
        ".apic/history/dev/login/20261006T090400.000000000Z.json"
    );
    let e = s.get("dev", "login", 3).unwrap();
    assert!(
        e.status == 202 && compact(&e.result).contains(r#""n":2"#),
        "entry 3 = {} {:?}",
        e.status,
        e.result
    );
    assert!(
        root.path().join(".apic").join(".gitignore").exists(),
        ".apic/.gitignore not written"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(root.path().join(&e.file))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "entry mode");
    }
    assert_eq!(s.keep(), 3);
    assert_eq!(
        serde_json::to_string(&list[0]).unwrap(),
        r#"{"index":1,"time":"2026-10-06T09:04:00Z","ok":true,"status":204,"status_text":"OK","duration_ms":12,"size":3,"file":".apic/history/dev/login/20261006T090400.000000000Z.json"}"#
    );
}

/// The file an entry is kept in, byte for byte.
#[test]
fn entry_file_shape() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 3);
    let at = Time::parse_rfc3339("2026-10-06T09:00:00.123+01:00").unwrap();
    s.record(
        "dev",
        "Get <user>/1",
        at,
        r#"{"ok":true,"response":{"status":200,"body":{"a":[1,2,{}],"b":"<x>","e":[]},"duration_ms":1,"size":2}}"#,
    )
    .unwrap();
    let list = s.list("dev", "Get <user>/1").unwrap();
    assert_eq!(
        list[0].file,
        ".apic/history/dev/Get%20%3Cuser%3E%2F1/20261006T080000.123000000Z.json"
    );
    assert_eq!(
        list[0].time.format_rfc3339_nano(),
        "2026-10-06T08:00:00.123Z"
    );
    assert_eq!(
        fs::read_to_string(root.path().join(&list[0].file)).unwrap(),
        "{\n  \"time\": \"2026-10-06T08:00:00.123Z\",\n  \"env\": \"dev\",\n  \"request\": \"Get \\u003cuser\\u003e/1\",\n  \"result\": {\n    \"ok\": true,\n    \"response\": {\n      \"status\": 200,\n      \"body\": {\n        \"a\": [\n          1,\n          2,\n          {}\n        ],\n        \"b\": \"\\u003cx\\u003e\",\n        \"e\": []\n      },\n      \"duration_ms\": 1,\n      \"size\": 2\n    }\n  }\n}\n"
    );
    assert!(matches!(
        s.record("dev", "x", at, "{nope"),
        Err(Error::Other(_))
    ));
}

#[test]
fn environments_and_requests_are_separate() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 5);
    s.record("", "login", t0(), &result(200, "1")).unwrap();
    s.record("staging", "login", t0(), &result(500, "2"))
        .unwrap();
    s.record("", "whoami", t0(), &result(401, "3")).unwrap();
    for (env, name, status) in [
        ("", "login", 200),
        ("default", "login", 200),
        ("staging", "login", 500),
        ("", "whoami", 401),
    ] {
        let list = s.list(env, name).unwrap();
        assert!(
            list.len() == 1 && list[0].status == status,
            "{env:?}/{name} = {list:?}, want one entry with {status}"
        );
    }
    assert!(s.list("staging", "whoami").unwrap().is_empty());
    s.record("", "Get user/1", t0(), &result(200, "4")).unwrap();
    let reqs = s.requests("").unwrap();
    let got: Vec<(&str, usize)> = reqs
        .iter()
        .map(|r| (r.request.as_str(), r.entries))
        .collect();
    assert_eq!(got, [("Get user/1", 1), ("login", 1), ("whoami", 1)]);
    assert_eq!(
        serde_json::to_string(&reqs[0]).unwrap(),
        r#"{"request":"Get user/1","entries":1}"#
    );
    assert!(s.requests("nowhere").unwrap().is_empty());
}

#[test]
fn same_instant_entries_keep_their_order() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 20);
    for i in 0..12 {
        s.record("", "poll", t0(), &result(200, &i.to_string()))
            .unwrap();
    }
    let e = s.get("", "poll", 1).unwrap();
    assert!(
        compact(&e.result).contains(r#""body":11"#),
        "newest = {:?}, want body 11",
        e.result
    );
    let e = s.get("", "poll", 12).unwrap();
    assert!(
        compact(&e.result).contains(r#""body":0,"#),
        "oldest = {:?}, want body 0",
        e.result
    );
}

#[test]
fn pruning_never_drops_the_newest_of_one_instant() {
    // Four entries in one clock tick with room for two: once the bare
    // name is pruned it must not be reused, or the newest entry would
    // sort as the oldest and be pruned on the spot.
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 2);
    for i in 0..4 {
        s.record("", "poll", t0(), &result(200, &i.to_string()))
            .unwrap();
    }
    for (index, body) in [(1, r#""body":3"#), (2, r#""body":2"#)] {
        let e = s.get("", "poll", index).unwrap();
        assert!(
            compact(&e.result).contains(body),
            "entry {index} = {}, want {body}",
            compact(&e.result)
        );
    }
}

#[test]
fn a_clock_that_goes_back_still_records_the_newest_last() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 5);
    s.record("", "login", t0(), &result(200, "1")).unwrap();
    s.record(
        "",
        "login",
        t0() - Duration::from_secs(3600),
        &result(201, "2"),
    )
    .unwrap();
    assert_eq!(
        s.get("", "login", 1).unwrap().status,
        201,
        "want the entry recorded last (201)"
    );
}

#[test]
fn a_damaged_entry_is_passed_over() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 5);
    s.record("", "login", t0(), &result(200, "1")).unwrap();
    s.record("", "login", t0() + SECOND, &result(201, "2"))
        .unwrap();
    let list = s.list("", "login").unwrap();
    // A write cut short by a killed process.
    fs::write(root.path().join(&list[0].file), r#"{"time":"#).unwrap();
    let list = s.list("", "login").unwrap();
    assert!(
        list.len() == 1 && list[0].status == 200 && list[0].index == 1,
        "list = {list:?}; want the one whole entry"
    );
    assert_eq!(s.get("", "login", 1).unwrap().status, 200);
    s.record("", "login", t0() + SECOND * 2, &result(202, "3"))
        .unwrap();
    let list = s.list("", "login").unwrap();
    assert!(
        list.len() == 2 && list[0].status == 202,
        "after another run = {list:?}"
    );
}

#[test]
fn names_cannot_leave_the_directory() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 2);
    for name in ["../escape", "..", "a/b", "a\\b", "Get User"] {
        s.record("../env", name, t0(), &result(200, "1")).unwrap();
        let list = s.list("../env", name).unwrap();
        assert_eq!(list.len(), 1, "{name:?}");
        let rel = &list[0].file;
        assert!(
            rel.starts_with(".apic/history/") && rel.matches('/').count() == 4,
            "{name:?} stored at {rel}"
        );
        assert!(root.path().join(rel).exists());
    }
    assert!(
        !root.path().join("escape").exists(),
        "a name climbed out of the history directory"
    );
}

#[test]
fn get_out_of_range() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 2);
    let err = s.get("", "login", 1).unwrap_err();
    assert!(
        matches!(&err, Error::Range(RangeError { have: 0, .. }))
            && err.to_string() == "no history for login",
        "err = {err}"
    );
    s.record("", "login", t0(), &result(200, "1")).unwrap();
    assert_eq!(
        s.get("", "login", 2).unwrap_err().to_string(),
        "login has 1 history entry, no #2"
    );
    s.record("", "login", t0() + SECOND, &result(200, "1"))
        .unwrap();
    assert_eq!(
        s.get("", "login", 0).unwrap_err().to_string(),
        "login has 2 history entries, no #0"
    );
}

#[test]
fn clear() {
    let root = tempfile::tempdir().unwrap();
    let s = Store::new(root.path(), 5);
    s.record("", "login", t0(), &result(200, "1")).unwrap();
    s.record("", "login", t0() + SECOND, &result(200, "1"))
        .unwrap();
    s.record("", "whoami", t0(), &result(200, "1")).unwrap();
    s.record("staging", "login", t0(), &result(200, "1"))
        .unwrap();

    assert_eq!(s.clear("", "login").unwrap(), 2, "clear one request");
    assert_eq!(
        s.list("", "whoami").unwrap().len(),
        1,
        "clearing login cleared whoami"
    );
    assert_eq!(s.clear("default", "").unwrap(), 1, "clear the environment");
    assert_eq!(
        s.list("staging", "login").unwrap().len(),
        1,
        "clearing default cleared staging"
    );
    assert_eq!(s.clear("*", "").unwrap(), 1, "clear everything");
    assert_eq!(s.clear("*", "").unwrap(), 0, "clear nothing");
}

#[test]
fn keep_zero_records_nothing() {
    let root = tempfile::tempdir().unwrap();
    Store::new(root.path(), 0)
        .record("", "login", t0(), &result(200, "1"))
        .unwrap();
    assert!(
        !Path::new(&root.path().join(".apic")).exists(),
        "history 0 wrote .apic"
    );
}

fn render(cs: &[Change]) -> Vec<String> {
    cs.iter()
        .map(|c| {
            format!(
                "{} {} {} → {}",
                c.op,
                c.path,
                c.from.as_deref().unwrap_or(""),
                c.to.as_deref().unwrap_or("")
            )
        })
        .collect()
}

#[test]
fn compare_json() {
    let from = result(
        200,
        r#"{"user":{"name":"alice","roles":["read"],"tags":{}},"token_type":"bearer","odd key":1,"n":1.50}"#,
    );
    let to = result(
        201,
        r#"{"user":{"name":"bob","roles":["read","admin"],"tags":{}},"odd key":2,"n":1.50,"new":null}"#,
    );
    let changes = compare(&from, &to).unwrap();
    let want = [
        "changed status 200 → 201",
        "added $.new  → null",
        "changed $['odd key'] 1 → 2",
        "removed $.token_type \"bearer\" → ",
        "changed $.user.name \"alice\" → \"bob\"",
        "added $.user.roles[1]  → \"admin\"",
    ];
    assert_eq!(render(&changes), want);
    assert!(
        compare(&from, &from).unwrap().is_empty(),
        "identical results differ"
    );
    assert_eq!(
        serde_json::to_string(&changes[..2]).unwrap(),
        r#"[{"path":"status","op":"changed","from":200,"to":201},{"path":"$.new","op":"added","to":null}]"#
    );
    // Numbers are compared as they were written.
    let changes = compare(
        &result(200, r#"{"n":1.50,"k'\\":[1e2]}"#),
        &result(200, r#"{"n":1.5,"k'\\":[100]}"#),
    )
    .unwrap();
    assert_eq!(
        render(&changes),
        [
            "changed $['k\\'\\\\'][0] 1e2 → 100",
            "changed $.n 1.50 → 1.5"
        ]
    );
    assert_eq!(
        serde_json::to_string(&changes[1]).unwrap(),
        r#"{"path":"$.n","op":"changed","from":1.50,"to":1.5}"#
    );
}

#[test]
fn compare_shape_change() {
    let changes = compare(
        &result(200, r#"{"items":{"a":1}}"#),
        &result(200, r#"{"items":[1]}"#),
    )
    .unwrap();
    assert_eq!(render(&changes), [r#"changed $.items {"a":1} → [1]"#]);
}

#[test]
fn compare_text() {
    let changes = compare(
        &result(200, r#""one\ntwo\nthree""#),
        &result(200, r#""one\n2\nthree\nfour""#),
    )
    .unwrap();
    let want = [
        r#"removed line 2 "two" → "#,
        r#"added line 2  → "2""#,
        r#"added line 4  → "four""#,
    ];
    assert_eq!(render(&changes), want);
}

#[test]
fn compare_long_texts_that_differ_in_one_line() {
    let (mut a, mut b) = (String::new(), String::new());
    for i in 0..5000 {
        let line = format!("line {i}");
        a.push_str(&line);
        a.push('\n');
        b.push_str(if i == 4321 { "changed" } else { &line });
        b.push('\n');
    }
    let quoted = |s: &str| serde_json::to_string(s).unwrap();
    let changes = compare(&result(200, &quoted(&a)), &result(200, &quoted(&b))).unwrap();
    assert_eq!(
        render(&changes),
        [
            r#"removed line 4322 "line 4321" → "#,
            r#"added line 4322  → "changed""#
        ]
    );
}

#[test]
fn compare_binary() {
    let bin = |b: &str| {
        format!(
            r#"{{"ok":true,"response":{{"status":200,"body":"{b}","body_encoding":"base64"}}}}"#
        )
    };
    let changes = compare(&bin("AAE="), &bin("AAI=")).unwrap();
    assert_eq!(render(&changes), ["changed body  → "]);
    assert!(
        compare(&bin("AAE="), &bin("AAE=")).unwrap().is_empty(),
        "same bytes differ"
    );
    assert_eq!(
        serde_json::to_string(&changes[0]).unwrap(),
        r#"{"path":"body","op":"changed"}"#
    );
}

#[test]
fn compare_needs_two_responses() {
    assert_eq!(
        compare(r#"{"ok":false}"#, &result(200, "1")).unwrap_err(),
        "an entry has no response"
    );
    assert_eq!(
        compare("{nope", &result(200, "1")).unwrap_err(),
        "invalid character 'n' looking for beginning of object key string"
    );
    assert_eq!(
        compare("[1]", &result(200, "1")).unwrap_err(),
        "json: cannot unmarshal array into Go value of type history.stored"
    );
    assert_eq!(
        compare("null", &result(200, "1")).unwrap_err(),
        "an entry has no response"
    );
    // An absent body is null.
    let none = r#"{"response":{"status":204}}"#;
    assert_eq!(
        render(&compare(none, &result(204, "null")).unwrap()),
        Vec::<String>::new()
    );
    assert_eq!(
        render(&compare(none, &result(204, r#"{"a":1}"#)).unwrap()),
        [r#"changed $ null → {"a":1}"#]
    );
}
