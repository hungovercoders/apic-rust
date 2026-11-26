use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use apic_auth::Spec;
use apic_runner::{FormPart, TlsInfo};

use crate::testutil::{header, proxy, Fixture as Resolved};
use crate::{Error, LANGUAGES};

fn render(lang: &str, r: &Resolved, redact: bool) -> Result<String, Error> {
    crate::render(lang, &r.resolved(), redact)
}

/// Set to rewrite the golden files instead of comparing against them.
const UPDATE_ENV: &str = "APIC_UPDATE_GOLDEN";

fn spec(raw: &str) -> Option<Spec> {
    Some(apic_auth::parse(raw).unwrap())
}

/// The fixtures cover a JSON body with a bearer token, a multipart upload
/// with basic auth, an API key in the query with TLS, a proxy and
/// HTTP/1.1, and the credentials a language cannot express itself.
fn fixtures() -> Vec<(&'static str, Resolved)> {
    let request = Resolved::new;
    vec![
        (
            "json",
            Resolved {
                headers: vec![
                    header("Content-Type", "application/json"),
                    header("Accept", "application/json"),
                    header("X-Note", r#"say "hi" $HOME"#),
                ],
                body: "{\"title\": \"café \\\"x\\\"\",\n \"done\": false}".to_string(),
                auth_spec: spec("bearer s3cret-token"),
                ..request("POST", "https://api.example.com/todos?list=home&q=it's")
            },
        ),
        (
            "multipart",
            Resolved {
                headers: vec![header("Content-Type", "multipart/form-data; boundary=X")],
                parts: vec![
                    FormPart {
                        name: "title".to_string(),
                        value: "Q3 report".to_string(),
                        ..FormPart::default()
                    },
                    FormPart {
                        name: "file".to_string(),
                        file: "docs/report.pdf".to_string(),
                        content_type: "application/pdf".to_string(),
                        filename: "q3.pdf".to_string(),
                        ..FormPart::default()
                    },
                ],
                auth_spec: Some(Spec {
                    r#type: "basic".to_string(),
                    args: vec!["alice".to_string(), "pa'ss".to_string()],
                    ..Spec::default()
                }),
                ..request("POST", "http://localhost:8080/upload")
            },
        ),
        (
            "apikey",
            Resolved {
                http_version: "HTTP/1.1".to_string(),
                auth_spec: spec("apikey k-123 query=api_key"),
                tls: Some(TlsInfo {
                    insecure: true,
                    ..TlsInfo::default()
                }),
                proxy: Some(proxy("http://proxy.internal:3128", "--proxy", false)),
                ..request("GET", "https://internal.example.com/v1/items")
            },
        ),
        (
            "notes",
            Resolved {
                http_version: "HTTP/2".to_string(),
                auth_spec: spec("oauth2 tokenUrl=https://idp/token clientId=c"),
                tls: Some(TlsInfo {
                    ca_file: "certs/ca.pem".to_string(),
                    cert_file: "certs/client.pem".to_string(),
                    ..TlsInfo::default()
                }),
                ..request("PURGE", "https://cdn.example.com/x")
            },
        ),
        (
            "digest",
            Resolved {
                auth_spec: spec("digest bob hunter2"),
                ..request("DELETE", "https://api.example.com/items/7")
            },
        ),
        (
            "aws",
            Resolved {
                auth_spec: spec("aws region=eu-west-2"),
                ..request(
                    "GET",
                    "https://abc.execute-api.eu-west-2.amazonaws.com/prod/orders",
                )
            },
        ),
        (
            "exec",
            Resolved {
                auth_spec: spec("exec gcloud auth print-access-token header=X-Token prefix="),
                ..request("GET", "https://api.example.com/me")
            },
        ),
    ]
}

/// Each language's snippet for each fixture, with and without --redact,
/// against a golden file; running the tests with `APIC_UPDATE_GOLDEN=1`
/// rewrites them.
#[test]
fn golden() {
    let update = std::env::var_os(UPDATE_ENV).is_some();
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata"));
    let mut failures = Vec::new();
    for (name, r) in fixtures() {
        for redact in [false, true] {
            for lang in LANGUAGES {
                let got = render(lang, &r, redact).unwrap_or_else(|e| panic!("{name} {lang}: {e}"));
                let mut file = name.to_string();
                if redact {
                    file.push_str(".redacted");
                }
                let path = dir.join(format!("{file}.{lang}"));
                if update {
                    std::fs::create_dir_all(dir).unwrap();
                    std::fs::write(&path, format!("{got}\n")).unwrap();
                    continue;
                }
                let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("{}: {e} (run with {UPDATE_ENV}=1)", path.display())
                });
                if format!("{got}\n") != want.replace("\r\n", "\n") {
                    failures.push(format!("{} differs:\n{got}", path.display()));
                }
                if redact
                    && (got.contains("s3cret")
                        || got.contains("hunter2")
                        || got.contains("k-123")
                        || got.contains("pa'ss"))
                {
                    failures.push(format!("{} leaks a credential:\n{got}", path.display()));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

fn installed(bin: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(bin).is_file())
}

/// Runs `cmd` with `src` on its standard input; the error is its output.
fn check(mut cmd: Command, src: &str) -> Result<String, String> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    // A checker that reads a file instead may exit before the write.
    let _ = child.stdin.take().unwrap().write_all(src.as_bytes());
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "{}\n{stdout}{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

/// The snippets parse in their own languages, where the interpreter is
/// installed: Python's ast, node --check on an ES module, bash -n; the Go
/// one is exactly what gofmt would write.
#[test]
fn snippets_parse() {
    let mut failures = Vec::new();
    if installed("gofmt") {
        for (name, r) in fixtures() {
            for redact in [false, true] {
                let src = render("go", &r, redact).unwrap();
                match check(Command::new("gofmt"), &format!("{src}\n")) {
                    Ok(formatted) if formatted == format!("{src}\n") => {}
                    other => failures.push(format!(
                        "go {name} (redact {redact}) is not gofmt'd: {other:?}\n{src}"
                    )),
                }
            }
        }
    } else {
        eprintln!("go: gofmt not installed, not checked");
    }
    for (lang, bin) in [
        ("python", "python3"),
        ("js", "node"),
        ("curl", "bash"),
        ("httpie", "bash"),
    ] {
        if !installed(bin) {
            eprintln!("{lang}: {bin} not installed, not checked");
            continue;
        }
        for (name, r) in fixtures() {
            for redact in [false, true] {
                let src = render(lang, &r, redact).unwrap();
                let dir = tempfile::tempdir().unwrap();
                let mut cmd = Command::new(bin);
                match lang {
                    "python" => {
                        cmd.args(["-c", "import ast,sys; ast.parse(sys.stdin.read())"]);
                    }
                    "js" => {
                        let f = dir.path().join("snippet.mjs");
                        std::fs::write(&f, &src).unwrap();
                        cmd.arg("--check").arg(f);
                    }
                    _ => {
                        cmd.arg("-n");
                    }
                }
                if let Err(e) = check(cmd, &src) {
                    failures.push(format!(
                        "{lang} {name} (redact {redact}) does not parse: {e}\n{src}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    let err = render("cobol", &fixtures()[0].1, false).unwrap_err();
    assert!(
        err.to_string().contains("curl, httpie"),
        "unknown language: {err}"
    );
    assert_eq!(
        err.to_string(),
        "unknown language \"cobol\": one of curl, httpie, powershell, python, js, go"
    );
}
