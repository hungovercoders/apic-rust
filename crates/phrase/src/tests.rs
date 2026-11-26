use super::*;

fn must(text: &str) -> Phrase {
    parse(text).unwrap_or_else(|err| panic!("{text:?}: {err}"))
}

#[test]
fn parse_and_match() {
    let p = must("a user named {name} with role {role} exists");
    assert_eq!(p.params, ["name", "role"]);
    let re = p.compile().unwrap();
    let cases = [
        (
            r#"a user named "alice smith" with role admin exists"#,
            ["alice smith", "admin"],
        ),
        (
            r#"a user named bob with role "read only" exists"#,
            ["bob", "read only"],
        ),
        (r#"a user named "" with role x exists"#, ["", "x"]),
    ];
    for (text, want) in cases {
        let m = re
            .captures(text)
            .unwrap_or_else(|| panic!("no match: {text}"));
        let v = p.values(&[&m[1], &m[2]]);
        assert!(
            v["name"] == want[0] && v["role"] == want[1],
            "{text}: {v:?}"
        );
    }
    assert!(
        !re.is_match("a user named alice exists"),
        "should not match with a missing parameter"
    );
    let dashed = must("a user {user-id} with {a.b}");
    assert_eq!(
        dashed.params,
        ["user-id", "a.b"],
        "parameter names follow the variable grammar"
    );
    let quoted = must(r#"I say "{value}" loudly"#);
    let qre = quoted.compile().unwrap();
    let qm = qre
        .captures(r#"I say "hello world" loudly"#)
        .expect("a quoted placeholder must match quoted text with spaces");
    assert_eq!(quoted.values(&[&qm[1]])["value"], "hello world");
    assert!(
        !qre.is_match("I say hello loudly"),
        "a quoted placeholder must not match a bare word"
    );
    let plain = must("I fetch the user");
    assert!(
        plain.regex == "^I fetch the user$" && plain.params.is_empty(),
        "{plain:?}"
    );
    assert_eq!(
        p.regex,
        r#"^a user named ("[^"]*"|\S+) with role ("[^"]*"|\S+) exists$"#
    );
    assert_eq!(
        must("is it (really) 1.5?").regex,
        r"^is it \(really\) 1\.5\?$"
    );
    for bad in [
        "",
        "a {name} and {name}",
        "unbalanced {",
        "bad {1x}",
        "a user {name} }",
        "{a} {",
        "I use {login.response.body}",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} should fail");
    }
    assert_eq!(parse("  ").unwrap_err(), "@step needs a phrase");
    assert_eq!(
        parse("a {name} and {name}").unwrap_err(),
        "@step \"a {name} and {name}\": parameter {name} appears twice"
    );
    assert_eq!(
        parse("unbalanced {").unwrap_err(),
        "@step \"unbalanced {\": braces must wrap a parameter name like {userId}"
    );
    assert_eq!(
        parse("I use {login.response.body}").unwrap_err(),
        "@step \"I use {login.response.body}\": parameter {login.response.body} uses \".response.\", which is reserved for response references"
    );
    assert_eq!(unquote("\"a\""), "a");
    assert_eq!(unquote("\""), "\"");
    assert_eq!(unquote("a\""), "a\"");
}

#[test]
fn conflicts() {
    let seven = parse("{a} {b} {c} {d} {e} {f} {g}");
    assert_eq!(
        seven.unwrap_err(),
        "@step \"{a} {b} {c} {d} {e} {f} {g}\": more than 6 parameters"
    );
    for text in [
        "I run {req}",
        "the response status is {code}",
        r#"I run "login""#,
        "the variables:",
        r#"the response {what} "{sel}" is "{v}""#,
        "I capture the response {a} {b} as {c}",
    ] {
        assert!(
            must(text).conflicts_with_builtin().is_err(),
            "{text:?} should conflict with a built-in step"
        );
    }
    assert_eq!(
        must("I run {req}").conflicts_with_builtin().unwrap_err(),
        "@step \"I run {req}\" could match the same text as the built-in step run (I run \"([^\"]*)\")"
    );
    // Built-in captures keep their constraints: a bare word cannot match a
    // quoted capture and a word cannot match a numeric one.
    for text in [
        "I log in as {user}",
        "I run locally",
        "the response status is pending",
        r#"the response body "$.x" is ready"#,
        "the response time is under budget ms",
        "the response time is under pending",
        "I run {id}foo",
        "the response status is v{n}",
        "I run pre{id}",
    ] {
        if let Err(err) = must(text).conflicts_with_builtin() {
            panic!("unexpected conflict for {text:?}: {err}");
        }
    }
    for text in [
        "the response status is 200",
        r#"I run "{x}""#,
        "the response time is under {n} ms",
        "the response time is under 500ms",
        "the response time is under {t}",
        "the response status is {n}0",
        "the response time is under {n}ms",
    ] {
        assert!(
            must(text).conflicts_with_builtin().is_err(),
            "{text:?} should conflict with a built-in step"
        );
    }
    let affixed = must("I say {a}x");
    let lit = must("I say helloworldx");
    let nolit = must("I say hello");
    assert!(
        affixed.conflicts_with(&lit) && !affixed.conflicts_with(&nolit),
        "a placeholder with a suffix only meets words carrying that suffix"
    );
    let foo = must("I say {x}foo");
    let bar = must("I say {y}bar");
    let ofoo = must("I say {y}ofoo");
    let free_y = must("I say {y}");
    assert!(
        !foo.conflicts_with(&bar),
        "{{x}}foo and {{y}}bar cannot match the same word"
    );
    assert!(
        foo.conflicts_with(&ofoo) && foo.conflicts_with(&free_y),
        "compatible affixes still overlap"
    );
    let quoted_only = must(r#"I say "{a}""#);
    let bare = must("I say hello");
    let free = must("I say {b}");
    assert!(
        !quoted_only.conflicts_with(&bare),
        "a quoted-only parameter cannot match a bare word"
    );
    assert!(
        quoted_only.conflicts_with(&free),
        "a free parameter can match quoted text"
    );
    let a = must("a user named {name} exists");
    let b = must("a {role} named {name} exists");
    let c = must("a {role} called {name} exists");
    assert!(
        a.conflicts_with(&b) && b.conflicts_with(&a),
        "overlapping phrases should conflict"
    );
    assert!(
        !a.conflicts_with(&c),
        "distinct phrases should not conflict"
    );
    // `I do foo` matches both of these, which fixed samples cannot detect.
    let d = must("I do {x}");
    let e = must("I {x} foo");
    assert!(
        d.conflicts_with(&e),
        "`I do {{x}}` and `I {{x}} foo` both match `I do foo`"
    );
    let f = must("I log in as {user}");
    let g = must("I log out");
    assert!(
        !f.conflicts_with(&g),
        "different literals must not conflict"
    );
    // A quoted span with spaces is one token, so this collides with
    // `I run {x}`.
    let q = must(r#"I run "foo bar""#);
    assert!(
        q.conflicts_with_builtin().is_err(),
        r#"I run "foo bar" should conflict with the built-in run step"#
    );
    let h = must(r#"I say "hello there" to {who}"#);
    let i = must("I say {what} to {who}");
    assert!(
        h.conflicts_with(&i),
        "quoted literal must unify with a parameter"
    );
}

#[test]
fn conflicts_with_several_placeholders_in_one_word() {
    // A word with several placeholders constrains only its ends: the
    // literal between them is absorbed by the neighbouring captures. The
    // conflict check must therefore weigh the prefix and suffix only.
    // Each pair comes with a text both accept, or "" when they cannot
    // conflict.
    let pairs = [
        ("I use {a}-{b}", "I use {x}-{y}", "I use 1-2"),
        ("I use {a}-{b}", "I use {c}", "I use 1-2"),
        ("I use {a}x{b}", "I use {c}y{d}", "I use axbyc"),
        ("I use {a}-{b}", "I use -{c}", "I use -1-2"),
        ("I use {a}-{b}bar", "I use {c}baz", ""), // ends bar vs baz
        ("I use foo{a}-{b}", "I use fob{c}", ""), // starts foo vs fob
    ];
    for (ta, tb, sample) in pairs {
        let (a, b) = (must(ta), must(tb));
        let want = !sample.is_empty();
        assert_eq!(a.conflicts_with(&b), want, "{ta:?} vs {tb:?}");
        assert_eq!(b.conflicts_with(&a), want, "{tb:?} vs {ta:?} (reversed)");
        if want {
            // Ground truth: both compiled patterns accept the sample text.
            assert!(
                a.compile().unwrap().is_match(sample) && b.compile().unwrap().is_match(sample),
                "{sample:?} should match both {ta:?} and {tb:?}"
            );
        }
    }
}

/// Every built-in pattern compiles, and each of its shapes is a text the
/// pattern accepts once the captures are filled in.
#[test]
fn builtin_shapes_match_their_patterns() {
    assert_eq!(builtin().len(), 19);
    assert_eq!(BUILTIN[0].name, "environment");
    let fill = |shape: &str| {
        let mut text = shape.replace("{#ms}", "5ms").replace("{#}", "5");
        while let (Some(open), Some(close)) = (text.find('{'), text.find('}')) {
            text.replace_range(open..=close, "x");
        }
        text
    };
    for b in builtin() {
        let re = compile(b.regex).unwrap();
        assert!(!b.shapes.is_empty(), "{}", b.name);
        for shape in &b.shapes {
            let text = fill(shape);
            assert!(re.is_match(&text), "{}: {text:?} !~ {}", b.name, b.regex);
        }
    }
    let compare = builtin().iter().find(|b| b.name == "compare").unwrap();
    assert_eq!(compare.shapes.len(), 21);
    // The digit class is the plain one.
    let status = compile(r"^the response status is (\d+)$").unwrap();
    assert!(status.is_match("the response status is 200"));
    assert!(!status.is_match("the response status is ٢٠٠"));
    assert!(compile(r"^a\\d$").unwrap().is_match(r"a\d"));
}
