use std::fs;
use std::time::Duration;

use crate::cookiejar::default_path;
use crate::{Cookie, Jar, RequestUrl, SetCookie, Time, COOKIE_FILE, DIR};

fn must_url(s: &str) -> RequestUrl {
    RequestUrl::parse(s).unwrap()
}

fn set(name: &str, value: &str) -> SetCookie {
    SetCookie {
        name: name.to_string(),
        value: value.to_string(),
        ..SetCookie::default()
    }
}

const HOUR: Duration = Duration::from_secs(3600);

#[test]
fn jar_records_what_the_server_sets_and_replays_it_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let j = Jar::open(root.path()).unwrap();
    let now = Time::now();
    j.set_now(move || now);
    let e = j.http("dev");
    let login = must_url("https://api.example.com/auth/login");
    e.set_cookies(
        &login,
        &[
            set("sid", "abc"), // host-only, default path /auth, session cookie
            SetCookie {
                domain: "example.com".into(),
                path: "/".into(),
                max_age: 3600,
                ..set("pref", "dark")
            }, // domain cookie with a lifetime
            SetCookie {
                expires: Some(now - HOUR),
                ..set("old", "x")
            }, // already expired: dropped
            SetCookie {
                domain: "evil.com".into(),
                ..set("other", "y")
            }, // rejected by the jar: not recorded
            SetCookie {
                max_age: -1,
                ..set("gone", "z")
            }, // deletion
        ],
    );
    let got = j.cookies("dev");
    assert_eq!(got.len(), 2, "cookies = {got:?}");
    assert_eq!(
        got[0],
        Cookie {
            name: "sid".into(),
            value: "abc".into(),
            domain: "api.example.com".into(),
            path: "/auth".into(),
            host_only: true,
            ..Cookie::default()
        },
        "host-only cookie"
    );
    assert_eq!(
        got[1],
        Cookie {
            name: "pref".into(),
            value: "dark".into(),
            domain: "example.com".into(),
            path: "/".into(),
            expires: Some(now + HOUR),
            ..Cookie::default()
        },
        "domain cookie"
    );
    // Sent where they apply, and not elsewhere.
    let names = |u: &str| -> Vec<String> {
        e.cookies(&must_url(u))
            .into_iter()
            .map(|c| c.name)
            .collect()
    };
    assert_eq!(
        names("https://api.example.com/auth/me").len(),
        2,
        "same host and path"
    );
    assert_eq!(
        names("https://api.example.com/todos"),
        ["pref"],
        "other path"
    );
    assert_eq!(
        names("https://www.example.com/"),
        ["pref"],
        "sibling host gets the domain cookie only"
    );
    assert_eq!(
        e.cookie_header(&must_url("https://api.example.com/auth/me"), None)
            .as_deref(),
        Some("sid=abc; pref=dark")
    );
    assert_eq!(
        e.cookie_header(&must_url("https://api.example.com/todos"), Some("mine=1"))
            .as_deref(),
        Some("mine=1; pref=dark")
    );
    assert_eq!(
        e.cookie_header(&must_url("https://other.test/"), Some("mine=1")),
        None
    );
    j.save().unwrap();
    let file = root.path().join(DIR).join(COOKIE_FILE);
    // File modes are not meaningful on Windows.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600,
            "cookie file"
        );
    }
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        format!(
            "{{\n  \"envs\": {{\n    \"dev\": [\n      {{\n        \"name\": \"sid\",\n        \"value\": \"abc\",\n        \"domain\": \"api.example.com\",\n        \"path\": \"/auth\",\n        \"host_only\": true\n      }},\n      {{\n        \"name\": \"pref\",\n        \"value\": \"dark\",\n        \"domain\": \"example.com\",\n        \"path\": \"/\",\n        \"expires\": \"{}\"\n      }}\n    ]\n  }}\n}}\n",
            (now + HOUR).format_rfc3339_nano()
        )
    );

    // A new process sees the same cookies, minus the ones that expired.
    let j2 = Jar::open(root.path()).unwrap();
    j2.set_now(move || now + HOUR / 2);
    let e2 = j2.http("dev");
    assert_eq!(
        e2.cookies(&must_url("https://api.example.com/auth/me"))
            .len(),
        2,
        "after reopen"
    );
    j2.set_now(move || now + 2 * HOUR);
    let got = j2.cookies("dev");
    assert!(
        got.len() == 1 && got[0].name == "sid",
        "after expiry: {got:?}"
    );
    assert_eq!(
        e2.cookies(&must_url("https://api.example.com/auth/me"))
            .len(),
        1,
        "the lifetime ran out"
    );
    // Environments are isolated.
    assert!(j2.cookies("staging").is_empty());
    assert_eq!(j2.env_names(), ["dev"]);

    // Clearing one environment leaves the file for the others, clearing
    // all removes it.
    j2.http("staging")
        .set_cookies(&must_url("http://s.example.com/"), &[set("a", "1")]);
    j2.clear("dev");
    j2.save().unwrap();
    let j3 = Jar::open(root.path()).unwrap();
    assert!(
        j3.cookies("dev").is_empty() && j3.cookies("staging").len() == 1,
        "after clear dev: dev={:?} staging={:?}",
        j3.cookies("dev"),
        j3.cookies("staging")
    );
    j3.clear("*");
    j3.save().unwrap();
    assert!(!file.exists(), "an empty jar leaves no file");
}

#[test]
fn memory_jar_writes_nothing() {
    let j = Jar::new_memory();
    j.http("")
        .set_cookies(&must_url("http://x/"), &[set("a", "1")]);
    j.save().unwrap();
    let got = j.cookies("");
    assert!(
        got.len() == 1
            && got[0].domain == "x"
            && got[0].expiry_text(Time::now()) == "until cleared",
        "cookies = {got:?}"
    );
    assert!(
        default_path("") == "/"
            && default_path("/a") == "/"
            && default_path("/a/b") == "/a"
            && default_path("x") == "/",
        "default path"
    );
}

#[test]
fn save_is_a_no_op_when_nothing_changed() {
    let root = tempfile::tempdir().unwrap();
    let j = Jar::open(root.path()).unwrap();
    j.http("dev")
        .set_cookies(&must_url("http://x/"), &[set("a", "1")]);
    j.save().unwrap();
    let path = root.path().join(DIR).join(COOKIE_FILE);
    let earlier = fs::metadata(&path).unwrap().modified().unwrap() - HOUR;
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(earlier)
        .unwrap();
    j.save().unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        earlier,
        "a clean jar was rewritten"
    );
    j.clear("dev");
    j.save().unwrap();
    assert!(!path.exists(), "clearing marks the jar dirty");
}

#[test]
fn expiry_reads_as_a_duration() {
    let now = Time::parse_rfc3339("2026-10-06T09:00:00Z").unwrap();
    let c = |expires: Option<Time>| Cookie {
        expires,
        ..Cookie::default()
    };
    assert_eq!(c(None).expiry_text(now), "until cleared");
    assert_eq!(c(Some(now + HOUR)).expiry_text(now), "expires in 1h0m0s");
    assert_eq!(
        c(Some(now + Duration::from_millis(89_500))).expiry_text(now),
        "expires in 1m30s"
    );
    assert_eq!(
        c(Some(now + Duration::from_millis(400))).expiry_text(now),
        "expired"
    );
    assert_eq!(c(Some(now - HOUR)).expiry_text(now), "expired");
    assert_eq!(
        c(Some(Time::parse_rfc3339("9999-12-31T23:59:59Z").unwrap())).expiry_text(now),
        "expires in 2562047h47m16.854775807s"
    );
}

#[test]
fn headers_are_read_and_written_as_a_client_does() {
    let j = Jar::new_memory();
    let now = Time::parse_rfc3339("2026-10-06T09:00:00Z").unwrap();
    j.set_now(move || now);
    let e = j.http("");
    let u = must_url("http://localhost:8080/app/login?next=1");
    e.set_cookie_headers(
        &u,
        &[
            "sid=abc; Path=/; HttpOnly",
            "q=\"a b\"; Secure",
            "theme=dark; Max-Age=60",
            "late=1; Expires=Wed, 07 Oct 2026 09:00:00 GMT",
            "stale=1; Expires=Mon, 05 Oct 2026 09:00:00 GMT",
            "not a cookie",
        ],
    );
    // A secure cookie still goes to this machine over plain HTTP.
    assert_eq!(
        e.cookie_header(&must_url("http://localhost:8080/app/x"), None)
            .as_deref(),
        Some("q=\"a b\"; theme=dark; late=1; sid=abc")
    );
    assert_eq!(
        e.cookie_header(&must_url("http://localhost:9999/"), None)
            .as_deref(),
        Some("sid=abc"),
        "the port is not part of a cookie's scope, the path is"
    );
    let stored = j.cookies("");
    let by_name = |n: &str| {
        stored
            .iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n} in {stored:?}"))
    };
    assert_eq!(stored.len(), 4);
    assert!(
        by_name("sid").http_only
            && by_name("sid").host_only
            && by_name("sid").domain == "localhost"
    );
    assert!(by_name("q").secure && by_name("q").path == "/app" && by_name("q").value == "a b");
    assert_eq!(
        by_name("theme").expires,
        Some(now + Duration::from_secs(60))
    );
    assert_eq!(
        by_name("late").expires.unwrap().format_rfc3339_nano(),
        "2026-10-07T09:00:00Z"
    );
    // Setting a cookie again replaces it; an expired one deletes it.
    e.set_cookie_headers(&u, &["sid=new; Path=/", "theme=x; Max-Age=0"]);
    assert_eq!(
        e.cookie_header(&must_url("http://localhost:8080/app/x"), None)
            .as_deref(),
        Some("q=\"a b\"; late=1; sid=new")
    );
    assert_eq!(j.cookies("").len(), 3);
    // A response without cookies stores nothing and leaves the jar clean.
    let clean = Jar::new_memory();
    clean.http("").set_cookie_headers(&u, &["nope"]);
    assert!(clean.cookies("").is_empty());
}

#[test]
fn a_damaged_cookie_file_names_itself() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join(DIR)).unwrap();
    let file = root.path().join(DIR).join(COOKIE_FILE);
    fs::write(&file, "{nope").unwrap();
    let err = Jar::open(root.path()).err().unwrap().to_string();
    assert_eq!(
        err,
        format!(
            "{}: invalid character 'n' looking for beginning of object key string",
            file.display()
        )
    );
    fs::write(
        &file,
        r#"{"envs":{"dev":[{"name":"a","value":"1","domain":"x","path":"/","expires":"0001-01-01T00:00:00Z"},{"name":"b","value":"2","domain":"x","path":"/","expires":"2000-01-01T00:00:00+01:00","secure":true}],"none":null}}"#,
    )
    .unwrap();
    let j = Jar::open(root.path()).unwrap();
    let got = j.cookies("dev");
    assert!(
        got.len() == 1 && got[0].name == "a" && got[0].expires.is_none(),
        "{got:?}"
    );
    assert!(j.cookies("none").is_empty());
}

/// Scenarios with what was recorded for each: the `Cookie` header every
/// request gets and the cookies the jar keeps.
#[test]
fn reference_scenarios() {
    let data = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/cookie_reference.json"
    ))
    .unwrap();
    let scenarios: serde_json::Value = serde_json::from_str(&data).unwrap();
    let mut failures = Vec::new();
    for (n, sc) in scenarios.as_array().unwrap().iter().enumerate() {
        let j = Jar::new_memory();
        let e = j.http("dev");
        for step in sc["Steps"].as_array().unwrap() {
            let lines: Vec<&str> = step["Lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap())
                .collect();
            e.set_cookie_headers(&must_url(step["URL"].as_str().unwrap()), &lines);
        }
        for (q, want) in sc["Queries"]
            .as_array()
            .unwrap()
            .iter()
            .zip(sc["Headers"].as_array().unwrap())
        {
            let (q, want) = (q.as_str().unwrap(), want.as_str().unwrap());
            if want.starts_with('!') {
                continue; // not a URL a request can be made to
            }
            let got = e.cookie_header(&must_url(q), None).unwrap_or_default();
            if got != want {
                failures.push(format!(
                    "scenario {n}, {q}:\n  got  {got:?}\n  want {want:?}"
                ));
            }
        }
        let stored: Vec<String> = j
            .cookies("dev")
            .iter()
            .map(|c| {
                format!(
                    "{}={} {}{} expires={} secure={} http_only={} host_only={}",
                    c.name,
                    c.value,
                    c.domain,
                    c.path,
                    c.expires
                        .map(|t| t.format_rfc3339_nano())
                        .unwrap_or_default(),
                    c.secure,
                    c.http_only,
                    c.host_only
                )
            })
            .collect();
        let want: Vec<String> = sc["Stored"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let flag = |k: &str| c[k].as_bool().unwrap_or(false);
                format!(
                    "{}={} {}{} expires={} secure={} http_only={} host_only={}",
                    c["name"].as_str().unwrap(),
                    c["value"].as_str().unwrap(),
                    c["domain"].as_str().unwrap(),
                    c["path"].as_str().unwrap(),
                    c["expires"].as_str().unwrap_or(""),
                    flag("secure"),
                    flag("http_only"),
                    flag("host_only")
                )
            })
            .collect();
        if stored != want {
            failures.push(format!(
                "scenario {n} stored:\n  got  {stored:#?}\n  want {want:#?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn jars_cross_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Jar>();
    assert_send_sync::<crate::EnvJar>();
    assert_send_sync::<crate::Store>();
}
