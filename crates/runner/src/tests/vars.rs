use apic_gocompat::Time;
use regex::Regex;

use super::*;
use crate::EXIT_USAGE;

/// The built-ins, with the clock pinned: REST Client's offsets and
/// $localDatetime, JetBrains' $random family, and $projectRoot.
#[test]
fn builtins() {
    let dir = write_project(&[("a.http", "GET http://x\n")]);
    let opts = || Options {
        no_session: true,
        ..Options::default()
    };
    let mut r = new_runner(dir.path(), opts());
    // A fixed instant in a zone that is not UTC, so local and UTC differ.
    let fixed = Time::from_civil(2026, 3, 31, 10, 30, 0, 0, 2 * 3600);
    r.now = Some(Box::new(move || fixed));
    r.zone = Some("test".to_string());
    let root = r.project.root.clone();
    let exact = [
        ("$timestamp", "1774945800"),
        ("$timestamp -1 d", "1774859400"),
        ("$timestamp 2 h", "1774953000"),
        ("$timestamp 30 m", "1774947600"),
        ("$timestamp -10 s", "1774945790"),
        ("$timestamp 1 w", "1775550600"),
        ("$timestamp 1 M", "1777624200"), // 31 March + 1 month rolls to 1 May
        ("$timestamp 1 y", "1806481800"),
        ("$timestamp 500 ms", "1774945800"),
        ("$isoTimestamp", "2026-03-31T08:30:00Z"),
        ("$datetime", "2026-03-31T08:30:00Z"),
        ("$datetime iso8601", "2026-03-31T08:30:00Z"),
        ("$datetime rfc1123", "Tue, 31 Mar 2026 08:30:00 UTC"),
        ("$datetime iso8601 1 h", "2026-03-31T09:30:00Z"),
        (r#"$datetime "2006-01-02""#, "2026-03-31"),
        (r#"$datetime "2006-01-02 15:04" -1 d"#, "2026-03-30 08:30"),
        ("$datetime '2006-01-02' 1 Q", "2026-07-01"), // 31 March + 3 months rolls too
        ("$localDatetime", "2026-03-31T10:30:00+02:00"),
        ("$localDatetime rfc1123", "Tue, 31 Mar 2026 10:30:00 test"),
        (r#"$localDatetime "15:04" 2 h"#, "12:30"),
        ("$projectRoot", root.as_str()),
        // An unquoted layout with spaces, as files have long written it.
        ("$datetime 2006-01-02 15:04", "2026-03-31 08:30"),
        ("$datetime 2006-01-02 15:04 1 h", "2026-03-31 09:30"),
    ];
    for (expr, want) in exact {
        let got = r.builtin(expr);
        assert!(
            got.err.is_none() && got.ok && got.value == want,
            "{{{{{expr}}}}} = {:?}, {}, {:?}; want {want:?}",
            got.value,
            got.ok,
            got.err
        );
    }
    let shaped = [
        ("$random.integer", r"^\d{1,3}$"),
        ("$random.integer(1, 3)", r"^[12]$"),
        ("$random.integer(5,6)", r"^5$"),
        ("$random.float", r"^\d{1,3}\.\d{3}$"),
        ("$random.float(1.5, 1.6)", r"^1\.(5\d\d|600)$"), // the top of the range rounds to 1.600
        ("$random.alphabetic", r"^[A-Za-z]{10}$"),
        ("$random.alphabetic(3)", r"^[A-Za-z]{3}$"),
        ("$random.alphanumeric(12)", r"^[A-Za-z0-9]{12}$"),
        ("$random.hexadecimal(8)", r"^[0-9a-f]{8}$"),
        ("$random.email", r"^[a-z]{8}@example\.com$"),
        ("$random.uuid", r"^[0-9a-f-]{36}$"),
        ("$uuid", r"^[0-9a-f-]{36}$"),
        ("$randomInt 1 3", r"^[12]$"),
        ("$randomInt", r"^\d{1,3}$"),
    ];
    for (expr, pattern) in shaped {
        let re = Regex::new(pattern).unwrap();
        for _ in 0..20 {
            let got = r.builtin(expr);
            assert!(
                got.err.is_none() && got.ok && re.is_match(&got.value),
                "{{{{{expr}}}}} = {:?}, {}, {:?}; want {pattern}",
                got.value,
                got.ok,
                got.err
            );
        }
    }
    let bad = [
        ("$timestamp -1", "offset"),
        ("$timestamp 1 fortnight", "unknown offset unit"),
        ("$timestamp x d", "not an integer"),
        ("$datetime rfc1123 x d", "not an integer"),
        ("$random.integer(3, 1)", "max greater than min"),
        ("$random.integer(1)", "takes (min, max)"),
        ("$random.integer(0, 0.5)", "two integers"),
        (
            "$random.integer(-9223372036854775808, 9223372036854775807)",
            "too wide",
        ),
        ("$random.float(0, inf)", "finite"),
        ("$random.float(1.8, 1.2)", "max greater than min"),
        ("$random.alphabetic(x)", "one length"),
        ("$random.email(1)", "no arguments"),
        ("$random.nope", "unknown built-in $random.nope"),
        ("$nope", "unknown built-in $nope"),
    ];
    for (expr, want) in bad {
        let err = r.builtin(expr).err.map(|e| e.to_string());
        assert!(
            err.as_deref().is_some_and(|e| e.contains(want)),
            "{{{{{expr}}}}}: err = {err:?}; want it to mention {want:?}"
        );
    }
    // The system clock reads in the machine's zone, whatever that is.
    r.now = None;
    let local = r.builtin("$localDatetime");
    assert!(
        local.ok && Time::parse_rfc3339(&local.value).is_ok(),
        "{local:?}"
    );
    assert_eq!(
        r.builtin("$timestamp").value.len(),
        r.builtin("$timestamp 1 M").value.len()
    );
    // A bad offset is a usage error that names the placeholder and the
    // line.
    std::fs::write(
        dir.path().join("a.http"),
        "GET http://x/{{$timestamp 1 fortnight}}\n",
    )
    .unwrap();
    let r = new_runner(dir.path(), opts());
    let (_, err) = r.run(&ctx(), &r.project.requests()[0]);
    let err = err.expect("an error");
    let text = err.to_string();
    assert!(
        text.contains("a.http:1")
            && text.contains("{{$timestamp 1 fortnight}}")
            && exit(Some(&err)) == EXIT_USAGE,
        "err = {text}"
    );
    assert_eq!(crate::code_of(&err), crate::Code::Build);
}

/// The layers a plain variable is looked for in, the first that has it
/// winning.
#[test]
fn variable_precedence() {
    let dir = write_project(&[
        (
            "a.http",
            "@v = file\n@only = {{v}}-var\nGET http://x/{{v}}\n",
        ),
        (".env", "v=dotenv\nd=1\n"),
        (
            "http-client.env.json",
            r#"{"$shared": {"v": "shared", "s": "1"}, "dev": {"v": "public"}}"#,
        ),
        (
            "http-client.private.env.json",
            r#"{"dev": {"v": "private"}}"#,
        ),
    ]);
    let session = memory_session();
    let opts = |env: &str| Options {
        env: env.into(),
        session: Some(session.clone()),
        ..Options::default()
    };
    let r = new_runner(dir.path(), opts("dev"));
    let req = r.project.requests()[0].clone();
    let source = |r: &Runner| {
        let (info, ok, err) = r.lookup(Some(&req), "v", 0);
        assert!(ok && err.is_none());
        (info.value, info.source, info.secret)
    };
    let s = |a: &str, b: &str, c: bool| (a.to_string(), b.to_string(), c);
    assert_eq!(
        source(&r),
        s("private", "http-client.private.env.json [dev]", true)
    );
    session.lock().unwrap().set("dev", [("v", "sess")]);
    assert_eq!(source(&r), s("sess", "session", true));
    r.capture("v", "cap");
    assert_eq!(source(&r), s("cap", "captured this run", true));
    assert_eq!(r.captured()["v"], "cap");
    r.set_var("v", "flag");
    assert_eq!(source(&r), s("flag", "--var", true));
    assert_eq!(r.vars()["v"], "flag");
    assert_eq!(r.render("{{v}}/{{s}}/{{d}}").unwrap(), "flag/1/1");
    let err = r.render("{{v}} {{nope}}").unwrap_err();
    assert_eq!(
        err.missing().map(|m| m.exprs.clone()),
        Some(vec!["nope".to_string()])
    );
    let names: Vec<String> = r.env_vars().into_iter().map(|v| v.name).collect();
    assert_eq!(names, ["d", "s", "v"]);

    // No environment: the shared block, then .env, then the file.
    let r = new_runner(dir.path(), opts(""));
    assert_eq!(
        source(&r),
        s("shared", "http-client.env.json ($shared)", false)
    );
    let (info, ok, _) = r.lookup(Some(&req), "d", 0);
    assert!(ok && info.source == ".env" && info.secret);
    let (info, ok, _) = r.lookup(Some(&req), "only", 0);
    assert!(
        ok && info.value == "shared-var" && info.source == "a.http:2 @only",
        "{info:?}"
    );
    let (info, ok, _) = r.lookup(Some(&req), "nothing", 0);
    assert!(!ok && info.missing && info.source == "missing");
}
