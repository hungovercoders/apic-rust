use apic_runner::{FormPart, TlsInfo};

use crate::testutil::{header, proxy, Fixture as Resolved};

fn curl(r: &Resolved, redact: bool) -> String {
    crate::curl(&r.resolved(), redact)
}

fn request(method: &str, url: &str) -> Resolved {
    Resolved::new(method, url)
}

fn with_auth(spec: &str) -> Resolved {
    Resolved {
        auth_spec: Some(apic_auth::parse(spec).unwrap()),
        ..request("GET", "https://a.b")
    }
}

#[test]
fn curl_command() {
    let got = curl(
        &Resolved {
            headers: vec![header("Authorization", "******'s")],
            body: r#"{"a":1}"#.to_string(),
            ..request("POST", "https://a.b/c?x=1")
        },
        false,
    );
    let want = "curl -sS \\\n  -H 'Authorization: ******'\\''s' \\\n  --data-raw '{\"a\":1}' \\\n  'https://a.b/c?x=1'";
    assert_eq!(got, want);
    assert_eq!(
        curl(&request("DELETE", "https://a.b"), false),
        "curl -sS \\\n  -X DELETE \\\n  'https://a.b'"
    );
    let get = Resolved {
        body: "x=1".to_string(),
        ..request("GET", "https://a.b")
    };
    assert_eq!(
        curl(&get, false),
        "curl -sS \\\n  -X GET \\\n  --data-raw 'x=1' \\\n  'https://a.b'"
    );
}

#[test]
fn auth_flags() {
    let cases = [
        ("basic u p", "--user 'u:p'"),
        ("bearer t", "Authorization:"),
        (
            "aws region=eu-west-2 service=s3",
            "--aws-sigv4 'aws:amz:eu-west-2:s3'",
        ),
        ("aws", r#"--aws-sigv4 "aws:amz:$AWS_REGION:execute-api""#),
        (
            "aws service=s3",
            "${AWS_SESSION_TOKEN:+x-amz-security-token:$AWS_SESSION_TOKEN}",
        ),
        (
            "exec gcloud auth print-access-token",
            "$('gcloud' 'auth' 'print-access-token')",
        ),
        ("oauth2 tokenUrl=https://idp/t clientId=c", "$TOKEN"),
    ];
    for (spec, want) in cases {
        let got = curl(&with_auth(spec), false);
        assert!(got.contains(want), "{spec}:\n{got}\nmissing {want}");
    }
}

#[test]
fn oauth2_export_has_no_inline_comment() {
    let got = curl(
        &with_auth("oauth2 tokenUrl=https://idp/t clientId=c"),
        false,
    );
    assert!(
        !got.contains('#'),
        "unexpected inline comment in curl export:\n{got}"
    );
}

#[test]
fn exec_export_quotes_arguments() {
    let got = curl(
        &with_auth(r#"exec cmd "arg with space" "quo'te" header=X-Api-Key prefix="pre;fix""#),
        false,
    );
    let want = r"-H 'X-Api-Key: pre;fix '$('cmd' 'arg with space' 'quo'\''te')";
    assert!(got.contains(want), "got\n{got}\nmissing\n{want}");
}

/// Pins that `apic curl --redact` prints nothing runnable against a real
/// API: bearer and basic join the aws/oauth2/exec branches in emitting
/// shell placeholders rather than live credentials.
#[test]
fn command_redacts() {
    let mk = |spec: &str| Resolved {
        headers: vec![
            header("X-Api-Key", "live-key"),
            header("Accept", "application/json"),
        ],
        secret_headers: ["X-Api-Key".to_string()].into(),
        body: r#"{"password":"live-password"}"#.to_string(),
        auth_spec: Some(apic_auth::parse(spec).unwrap()),
        ..request("POST", "https://a.b/c?token=live-token&id=1")
    };
    let live = [
        "live-token",
        "live-key",
        "live-password",
        "bearer-secret",
        "basic-user",
        "basic-password",
    ];
    for spec in ["bearer bearer-secret", "basic basic-user basic-password"] {
        let got = curl(&mk(spec), true);
        for bad in live {
            assert!(
                !got.contains(bad),
                "{spec}: redacted command leaks {bad:?}:\n{got}"
            );
        }
    }
    let got = curl(&mk("bearer bearer-secret"), true);
    assert!(
        got.contains("$TOKEN"),
        "redacted bearer should use a placeholder:\n{got}"
    );
    let got = curl(&mk("basic basic-user basic-password"), true);
    assert!(
        got.contains("$APIC_USER:$APIC_PASSWORD"),
        "redacted basic should use placeholders:\n{got}"
    );
    // Without redact the command stays runnable as printed.
    let got = curl(&mk("bearer bearer-secret"), false);
    assert!(
        got.contains("bearer-secret"),
        "without redact the command should be runnable:\n{got}"
    );
}

#[test]
fn command_maps_multipart_parts_onto_form() {
    let r = Resolved {
        headers: vec![
            header(
                "Content-Type",
                "multipart/form-data; boundary=WebAppBoundary",
            ),
            header("X-Trace", "1"),
        ],
        body: "<multipart: 3 parts, 2 files>".to_string(),
        parts: vec![
            FormPart {
                name: "title".to_string(),
                value: "Quarterly report for alice".to_string(),
                ..FormPart::default()
            },
            FormPart {
                name: "file".to_string(),
                filename: "report.pdf".to_string(),
                content_type: "application/pdf".to_string(),
                file: "files/report.pdf".to_string(),
                ..FormPart::default()
            },
            FormPart {
                name: "meta".to_string(),
                filename: "renamed.json".to_string(),
                file: "files/meta.json".to_string(),
                ..FormPart::default()
            },
        ],
        ..request("POST", "https://a.b/upload")
    };
    let want = "curl -sS \\\n  -H 'X-Trace: 1' \\\n  --form-string 'title=Quarterly report for alice' \\\n  -F 'file=@files/report.pdf;type=application/pdf' \\\n  -F 'meta=@files/meta.json;filename=renamed.json' \\\n  'https://a.b/upload'";
    assert_eq!(curl(&r, false), want);
    // Redacted: values masked, paths kept, no body summary leaks in.
    let got = curl(&r, true);
    assert!(
        got.contains("--form-string 'title=***'")
            && got.contains("-F 'file=@files/report.pdf;type=application/pdf'")
            && !got.contains("data-raw"),
        "redacted: {got}"
    );
}

#[test]
fn command_maps_tls_onto_curl_flags() {
    let mut r = Resolved {
        tls: Some(TlsInfo {
            ca_file: "certs/ca.pem".to_string(),
            cert_file: "certs/client.pem".to_string(),
            key_file: "certs/client-key.pem".to_string(),
            insecure: true,
        }),
        ..request("GET", "https://api.internal/me")
    };
    let want = "curl -sS \\\n  --cacert 'certs/ca.pem' \\\n  --cert 'certs/client.pem' \\\n  --key 'certs/client-key.pem' \\\n  --insecure \\\n  'https://api.internal/me'";
    assert_eq!(curl(&r, false), want);
    // One file for both: no --key.
    r.tls = Some(TlsInfo {
        cert_file: "certs/client.pem".to_string(),
        key_file: "certs/client.pem".to_string(),
        ..TlsInfo::default()
    });
    let got = curl(&r, true);
    assert!(
        !got.contains("--key") && got.contains("--cert 'certs/client.pem'"),
        "combined file: {got}"
    );
}

/// The version a request line pins is curl's --http1.1 or --http2.
#[test]
fn command_keeps_the_http_version() {
    for (version, flag) in [("HTTP/1.1", "--http1.1"), ("HTTP/2", "--http2")] {
        let r = Resolved {
            http_version: version.to_string(),
            ..request("GET", "https://x/")
        };
        assert_eq!(
            curl(&r, false),
            format!("curl -sS \\\n  {flag} \\\n  'https://x/'"),
            "{version}"
        );
    }
    let got = curl(&request("GET", "https://x/"), false);
    assert!(!got.contains("--http"), "no version, no flag: {got}");
}

#[test]
fn proxy_flags() {
    let mut base = request("GET", "https://a.b");
    let got = curl(&base, false);
    assert!(!got.contains("proxy"), "no proxy configured, got {got:?}");
    base.proxy = Some(proxy("http://***@proxy.internal:3128", "apic.yaml", false));
    let got = curl(&base, true);
    assert!(
        got.contains("--proxy 'http://***@proxy.internal:3128' \\\n  'https://a.b'"),
        "redacted proxy: {got:?}"
    );
    base.proxy = Some(proxy("", "--no-proxy", true));
    let got = curl(&base, false);
    assert!(
        got.contains("--noproxy '*' \\\n  'https://a.b'"),
        "off: {got:?}"
    );
}

#[test]
fn api_key_and_digest_flags() {
    let cases = [
        (
            "apikey k1",
            "-H 'X-Api-Key: k1'",
            r#"-H "X-Api-Key: $APIC_API_KEY""#,
        ),
        (
            "apikey k1 header=X-Auth-Token",
            "-H 'X-Auth-Token: k1'",
            r#"-H "X-Auth-Token: $APIC_API_KEY""#,
        ),
        (
            r#"apikey k1 header=Authorization prefix="Token ""#,
            "-H 'Authorization: Token k1'",
            r#"-H "Authorization: Token $APIC_API_KEY""#,
        ),
        (
            "apikey k1 query=api_key",
            "--url-query 'api_key=k1'",
            r#"--url-query "api_key=$APIC_API_KEY""#,
        ),
        (
            "digest u p",
            "--digest --user 'u:p'",
            r#"--digest --user "$APIC_USER:$APIC_PASSWORD""#,
        ),
    ];
    for (spec, plain, redacted) in cases {
        let got = curl(&with_auth(spec), false);
        assert!(got.contains(plain), "{spec}:\n{got}\nmissing {plain}");
        let got = curl(&with_auth(spec), true);
        assert!(
            got.contains(redacted) && !got.contains("k1") && !got.contains("'u:p'"),
            "{spec} redacted:\n{got}\nmissing {redacted}"
        );
    }
}
