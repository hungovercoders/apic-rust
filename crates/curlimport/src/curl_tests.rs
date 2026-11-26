use super::*;

struct Case {
    name: &'static str,
    command: &'static str,
    /// The rendered block, or "" when only warnings matter.
    want: &'static str,
    /// A substring one warning must contain.
    warn: &'static str,
}

#[test]
fn parse_and_render() {
    let cases = [
        Case {
            name: "json post with quotes and continuations",
            command: "curl -X POST https://api.example.com/todos \\\n  -H \"Content-Type: application/json\" \\\n  -H 'Authorization: Bearer abc' \\\n  -d '{\"title\": \"it'\\''s\", \"done\": false}'",
            want: "### POST /todos\n# @name post-todos\n# @assert status == 200\nPOST {{baseUrl}}/todos\nContent-Type: application/json\nAuthorization: Bearer abc\n\n{\"title\": \"it's\", \"done\": false}\n",
            warn: "",
        },
        Case {
            name: "data implies POST and a form content type; bundled flags; basic auth",
            command: "curl -sSL -u alice:s3cret -d user=alice -d pass=x https://api.example.com/login",
            want: "### POST /login\n# @name post-login\n# @auth basic alice s3cret\n# @assert status == 200\nPOST {{baseUrl}}/login\nContent-Type: application/x-www-form-urlencoded\n\nuser=alice&pass=x\n",
            warn: "",
        },
        Case {
            name: "get with data goes to the query; attached short values; --url",
            command: "curl -G --url 'https://api.example.com/search?x=1' --data-urlencode 'q=a b&c' --data-urlencode '=raw' -XGET -A apic-test -e https://ref -b 'sid=1; theme=dark'",
            want: "### GET /search\n# @name get-search\n# @assert status == 200\nGET {{baseUrl}}/search?x=1&q=a+b%26c&raw\nUser-Agent: apic-test\nReferer: https://ref\nCookie: sid=1; theme=dark\n",
            warn: "",
        },
        Case {
            name: "multipart form with a file, a type and a literal string",
            command: "curl https://api.example.com/upload -F title=Quarterly -F 'file=@./docs/report.pdf;type=application/pdf' -F 'meta=@x.json;filename=renamed.json' --form-string 'note=@not-a-file'",
            want: "### POST /upload\n# @name post-upload\n# @assert status == 200\nPOST {{baseUrl}}/upload\nContent-Type: multipart/form-data; boundary=WebAppBoundary\n\n--WebAppBoundary\nContent-Disposition: form-data; name=\"title\"\n\nQuarterly\n--WebAppBoundary\nContent-Disposition: form-data; name=\"file\"; filename=\"report.pdf\"\nContent-Type: application/pdf\n\n< ./docs/report.pdf\n--WebAppBoundary\nContent-Disposition: form-data; name=\"meta\"; filename=\"renamed.json\"\n\n< ./x.json\n--WebAppBoundary\nContent-Disposition: form-data; name=\"note\"\n\n@not-a-file\n--WebAppBoundary--\n",
            warn: "",
        },
        Case {
            name: "body from a file, insecure, head, unknown flags warn",
            command: "curl -k -I --data-binary @payload.json --bogus --retry 3 -o out.json --cacert ca.pem https://other.example.com/v1/import?dry=1",
            want: "### HEAD /v1/import\n# @name head-v1-import\n# curl was run with --insecure; run this with `apic --insecure` or set tls.verifyHost in apic.yaml\n# @assert status == 200\nHEAD https://other.example.com/v1/import?dry=1\nContent-Type: application/x-www-form-urlencoded\n\n< ./payload.json\n",
            warn: "unknown flag --bogus",
        },
        Case {
            name: "ansi quoting and a bare host",
            command: "curl example.com/a $'-H' $'X-Note: line1\\nline2' --compressed",
            want: "### GET /a\n# @name get-a\n# @assert status == 200\nGET http://example.com/a\nX-Note: line1 line2\n",
            warn: "",
        },
        Case {
            name: "the HTTP version flags go on the request line",
            command: "curl --http2 https://api.example.com/h2",
            want: "### GET /h2\n# @name get-h2\n# @assert status == 200\nGET {{baseUrl}}/h2 HTTP/2\n",
            warn: "",
        },
        Case {
            name: "http1.1",
            command: "curl -s --http1.1 https://api.example.com/h1",
            want: "### GET /h1\n# @name get-h1\n# @assert status == 200\nGET {{baseUrl}}/h1 HTTP/1.1\n",
            warn: "",
        },
        Case {
            name: "long option with =, empty header and a cookie file",
            command: "curl --request=PUT https://api.example.com/x -H 'X-Empty;' -b cookies.txt --data-raw @literal",
            want: "### PUT /x\n# @name put-x\n# @assert status == 200\nPUT {{baseUrl}}/x\nX-Empty: \nContent-Type: application/x-www-form-urlencoded\n\n@literal\n",
            warn: "cookie file",
        },
    ];
    for c in cases {
        let r = parse(c.command).unwrap_or_else(|e| panic!("{}: {e}", c.name));
        let got = render(&r, &name(&r), "https://api.example.com/");
        if !c.want.is_empty() {
            assert_eq!(got, c.want, "{}", c.name);
        }
        if !c.warn.is_empty() {
            assert!(
                r.warnings.join("\n").contains(c.warn),
                "{}: want a warning containing {:?}, got {:?}",
                c.name,
                c.warn,
                r.warnings
            );
        }
        if c.warn.is_empty() && !c.name.contains("warn") {
            assert!(
                r.warnings.is_empty(),
                "{}: unexpected warnings: {:?}",
                c.name,
                r.warnings
            );
        }
    }
}

#[test]
fn parse_errors() {
    for bad in [
        "",
        "curl -H 'x: y'",
        "curl 'https://x",
        "curl \"https://x",
        "curl -X",
        "curl https://x \\",
    ] {
        assert!(parse(bad).is_err(), "{bad:?}: want an error");
    }
    let toks = tokenize(r#"a "b c" 'd e' f\ g "h\"i" $'j\tk'"#).unwrap();
    assert_eq!(toks.join("|"), "a|b c|d e|f g|h\"i|j\tk");
}

#[test]
fn unknown_value_flags_do_not_eat_the_url() {
    let r =
        parse("curl --oauth2-bearer eyJ --max-redirs 5 --something odd https://api.example.com/me")
            .unwrap();
    assert_eq!(r.url, "https://api.example.com/me", "request = {r:?}");
    assert_eq!(r.headers.len(), 1, "request = {r:?}");
    assert_eq!(r.headers[0].value, "Bearer eyJ", "request = {r:?}");
    assert_eq!(
        r.warnings.join("\n"),
        "--max-redirs is about curl's own output or transport and has no place in a request file (ignored)\nunknown flag --something (ignored)\nextra argument \"odd\" (ignored)"
    );
    let r = parse(r#"curl --json '{"a":1}' https://x/j -F 'cv=@résumé.pdf'"#).unwrap();
    assert_eq!(r.method, "POST", "--json with a form: {r:?}");
    assert_eq!(r.parts.len(), 1, "--json with a form: {r:?}");
    let got = render(&r, "j", "");
    assert!(
        got.contains(r#"filename="résumé.pdf""#) && !got.contains(r"\u"),
        "non-ASCII filename: {got}"
    );
    assert!(splits_block("### heading\ntext"), "splits_block");
    assert!(splits_block("< 5 items"), "splits_block");
    assert!(!splits_block("plain\nbody"), "splits_block");
}
