use std::collections::BTreeMap;
use std::path::Path;

use apic_gocompat::Time;
use apic_session::{RequestUrl, SetCookie};
use serde_json::Value;

use super::{execute, join, must_write, tmp};

#[test]
fn session_json_masks_cached_tokens() {
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(join(dir, "api.http"), "GET http://example.com\n");
    let expires = Time::now()
        .add_nanos(3_600_000_000_000)
        .utc()
        .format_rfc3339_nano();
    must_write(
        join(dir, ".apic/session.json"),
        &format!("{{\n  \"envs\": {{\n    \"dev\": {{\n      \"$oauth2:test\": \"{{\\\"access_token\\\":\\\"secret-token\\\",\\\"refresh_token\\\":\\\"secret-refresh\\\",\\\"expires_at\\\":\\\"{expires}\\\"}}\",\n      \"$meta\": \"value-with-dollar-prefix\",\n      \"plain\": \"value\"\n    }}\n  }}\n}}\n"),
    );
    let (code, stdout, stderr) = execute(&["--json", "-C", dir, "session"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let got: BTreeMap<String, BTreeMap<String, String>> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(got["dev"]["plain"], "value", "plain value missing: {got:?}");
    assert_eq!(
        got["dev"]["$meta"], "value-with-dollar-prefix",
        "non-auth $ key should not be rewritten: {got:?}"
    );
    let cached = &got["dev"]["$oauth2:test"];
    assert!(
        !cached.contains("secret-token") && !cached.contains("secret-refresh"),
        "cached token leaked: {cached:?}"
    );
    assert!(
        cached.contains("token"),
        "cached token metadata missing: {cached:?}"
    );
}

#[test]
fn session_lists_and_clears_cookies() {
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(join(dir, "api.http"), "GET http://example.com\n");
    must_write(
        join(dir, ".apic/session.json"),
        r#"{"envs": {"dev": {"token": "t-1"}}}"#,
    );
    must_write(
        join(dir, "http-client.env.json"),
        r#"{"dev": {}, "staging": {}}"#,
    );
    let jar = apic_session::open_jar(Path::new(dir)).unwrap();
    let u = RequestUrl::parse("https://api.example.com/auth/login").unwrap();
    jar.http("dev").set_cookies(
        &u,
        &[SetCookie {
            name: "sid".into(),
            value: "secret-cookie".into(),
            path: "/".into(),
            http_only: true,
            ..SetCookie::default()
        }],
    );
    jar.http("staging").set_cookies(
        &u,
        &[SetCookie {
            name: "sid".into(),
            value: "other".into(),
            path: "/".into(),
            ..SetCookie::default()
        }],
    );
    jar.save().unwrap();
    let exec = |args: &[&str]| {
        let (code, stdout, stderr) = execute(&[&["-C", dir], args].concat());
        (stdout + &stderr, code)
    };
    let (out, code) = exec(&["session"]);
    assert!(
        code == 0
            && out.contains("token = t-1")
            && out.contains("cookie sid = ***")
            && out.contains("api.example.com/")
            && out.contains("until cleared")
            && out.contains("staging"),
        "session:\n{out}"
    );
    assert!(!out.contains("secret-cookie"), "cookie value printed");
    let (out, code) = exec(&["session", "cookies", "--json"]);
    let listed: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("session cookies --json: {e}\n{out}"));
    assert!(
        code == 0 && !out.contains("secret-cookie"),
        "session cookies --json:\n{out}"
    );
    let c = listed["dev"].as_array().unwrap();
    assert!(
        c.len() == 1
            && c[0]["name"] == "sid"
            && c[0]["domain"] == "api.example.com"
            && c[0]["path"] == "/"
            && c[0]["http_only"] == true
            && c[0].get("value").is_none(),
        "dev cookies = {c:?}"
    );
    // clear takes the current environment's cookies with its captures.
    let (out, code) = exec(&["session", "clear", "--env", "dev"]);
    assert_eq!(code, 0, "clear: {out}");
    let (out, _) = exec(&["session"]);
    assert!(
        !out.contains("token") && out.contains("staging"),
        "after clear dev:\n{out}"
    );
    let (out, code) = exec(&["session", "clear", "--all"]);
    assert_eq!(code, 0, "clear --all: {out}");
    let (out, _) = exec(&["session", "cookies"]);
    assert!(
        out.contains("no cookies stored"),
        "after clear --all:\n{out}"
    );
}
