use std::collections::HashSet;

use crate::*;

/// Pins the documented exit codes against wrapping. A transport error
/// with words put in front of it must still exit 3, not 2.
#[test]
fn exit_code_unwraps() {
    let transport = || Error::Transport(TransportError::new("dial tcp: refused"));
    let usage_err = |m: &str| Error::Usage(UsageError::new(Code::Other, m));
    let cases = [
        ("transport", transport(), EXIT_TRANSPORT),
        (
            "wrapped transport",
            Error::context("running login", transport()),
            EXIT_TRANSPORT,
        ),
        (
            "twice-wrapped transport",
            Error::context("flow", Error::context("running login", transport())),
            EXIT_TRANSPORT,
        ),
        ("usage", usage_err("unknown request"), EXIT_USAGE),
        (
            "wrapped usage",
            Error::context("resolving", usage_err("missing variable")),
            EXIT_USAGE,
        ),
        ("plain", Error::Other("something else".into()), EXIT_USAGE),
    ];
    assert_eq!(exit_code(None), EXIT_OK);
    for (name, err, want) in cases {
        assert_eq!(exit_code(Some(&err)), want, "{name}");
    }
    assert_eq!(
        Error::context("flow", Error::context("running login", transport())).to_string(),
        "flow: running login: request failed: dial tcp: refused"
    );
}

/// Codes are unique, grouped by their first digit, and every entry has
/// what the errors page and the --json error object need.
#[test]
fn catalogue_is_well_formed() {
    let mut seen = HashSet::new();
    for e in CATALOGUE {
        let code = e.code.as_str();
        assert!(seen.insert(e.code), "{code} listed twice");
        assert!(
            code.len() == 4 && code.starts_with('E'),
            "{code}: codes look like E101"
        );
        let want = if code.as_bytes()[1] == b'3' {
            EXIT_TRANSPORT
        } else {
            EXIT_USAGE
        };
        assert_eq!(e.exit, want, "{code}");
        assert!(
            !e.title.is_empty() && !e.hint.is_empty() && !e.about.is_empty(),
            "{code}: needs a title, a hint and an explanation"
        );
        assert_eq!(lookup(e.code), Some(*e));
        assert_eq!(Code::parse(code), Some(e.code));
    }
    assert_eq!(CATALOGUE.len(), 29);
    assert_eq!(catalogue()[0].code, CODE_MISSING_VARIABLE);
    assert_eq!(CODE_TLS.url(), "https://apic.sh/errors/#e303");
}

/// The exit status a code documents is the one `exit_code` gives.
#[test]
fn info_matches_exit_code() {
    for err in [
        Error::Usage(usage(CODE_FLAG, "x")),
        Error::Transport(TransportError::new("boom")),
        Error::context(
            "wrapped",
            Error::Transport(TransportError::with_code(CODE_TIMEOUT, "slow")),
        ),
        Error::Other("plain".into()),
    ] {
        assert_eq!(info(&err).exit, exit_code(Some(&err)), "{err}");
    }
    let inner = Error::Usage(usage(CODE_MISSING_VARIABLE, "token"));
    let outer = Error::context(
        "outer",
        Error::Usage(UsageError::wrapping(
            CODE_FLAG,
            format!("inner: {inner}"),
            &inner,
        )),
    );
    assert_eq!(
        code_of(&outer),
        CODE_MISSING_VARIABLE,
        "an inner code should win"
    );
    assert_eq!(outer.to_string(), "outer: inner: token");
}

#[test]
fn info_carries_the_entry_and_the_suggestions() {
    let mut ue = usage(CODE_ENVIRONMENT, "environment \"prd\" not found");
    ue.suggestions = vec!["prod".to_string()];
    let got = info(&Error::context("x", ue.into()));
    assert_eq!(
        serde_json::to_string(&got).unwrap(),
        r#"{"code":"E204","title":"unknown environment","message":"x: environment \"prd\" not found","hint":"Use one of the environments the message lists, or add it to http-client.env.json.","exit":2,"url":"https://apic.sh/errors/#e204","suggestions":["prod"]}"#
    );
    let te = TransportError::new("remote error: tls: bad certificate");
    assert_eq!(te.error_code(), CODE_TLS);
    assert_eq!(TransportError::cancelled().error_code(), CODE_CANCELLED);
    assert!(Error::from(TransportError::cancelled()).is_cancelled());
}
