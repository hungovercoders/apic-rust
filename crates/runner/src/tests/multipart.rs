use std::collections::BTreeMap;

use apic_httpd::{Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;
use crate::{EXIT_USAGE, MASKED};

const UPLOAD_HTTP: &str = r#"
### Upload a report with a title
# @name upload
# @assert status == 201
# @assert body.$.fields.title == Quarterly report for {{user}}
POST {{baseUrl}}/upload
Content-Type: multipart/form-data; boundary=WebAppBoundary

--WebAppBoundary
Content-Disposition: form-data; name="title"

Quarterly report for {{user}}
--WebAppBoundary
Content-Disposition: form-data; name="file"; filename="report.pdf"
Content-Type: application/pdf

< ./files/report.pdf
--WebAppBoundary
Content-Disposition: form-data; name="meta"; filename="meta.json"
Content-Type: application/json

<@ ./files/meta.json
--WebAppBoundary--

### A part that needs a value nobody has
# @name upload-missing
POST {{baseUrl}}/upload
Content-Type: multipart/form-data; boundary=b

--b
Content-Disposition: form-data; name="who"

{{nobody}}
--b--

### A part file outside the project
# @name upload-escape
POST {{baseUrl}}/upload
Content-Type: multipart/form-data; boundary=b

--b
Content-Disposition: form-data; name="f"; filename="x"

< ../../etc/passwd
--b--
"#;

/// Binary-looking content with a NUL, a CR LF pair and a line that looks
/// like a delimiter, all of which must go over the wire untouched. It is
/// ASCII, so that the server can echo it inside JSON.
const PDF: &[u8] = b"%PDF-1.4\x00\r\n--NotTheBoundary\nlooks like one\n%%EOF";

fn upload_server() -> TestServer {
    TestServer::new(|r: &HttpRequest| {
        let length: i64 = r
            .header("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if length <= 0 {
            return HttpResponse::error(411, "no Content-Length");
        }
        let parts = match r.multipart() {
            Ok(p) => p,
            Err(e) => return HttpResponse::error(400, &format!("not multipart: {e}")),
        };
        let mut fields = BTreeMap::new();
        let mut files = BTreeMap::new();
        for p in parts {
            let data = String::from_utf8_lossy(&p.data).into_owned();
            match p.filename.as_deref().filter(|f| !f.is_empty()) {
                None => {
                    fields.insert(p.name, data);
                }
                Some(filename) => {
                    files.insert(
                        p.name.clone(),
                        serde_json::json!({
                            "filename": filename,
                            "type": p.headers.get("Content-Type").unwrap_or(""),
                            "content": data,
                        }),
                    );
                }
            }
        }
        HttpResponse::json(
            201,
            serde_json::json!({"fields": fields, "files": files}).to_string(),
        )
    })
}

#[test]
fn multipart_body_is_assembled_from_parts_and_files() {
    let srv = upload_server();
    let dir = write_project(&[
        ("api.http", UPLOAD_HTTP),
        (
            "http-client.env.json",
            &format!(
                r#"{{"dev": {{"baseUrl": "{}", "user": "alice"}}}}"#,
                srv.url()
            ),
        ),
        ("files/meta.json", r#"{"owner": "{{user}}"}"#),
    ]);
    std::fs::write(dir.path().join("files").join("report.pdf"), PDF).unwrap();
    let r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            no_session: true,
            ..Options::default()
        },
    );
    let mut res = run_ok(&r, "upload");
    assert!(res.ok, "not ok: {res:?}");
    let got: serde_json::Value = serde_json::from_slice(&res.raw().unwrap().body).unwrap();
    assert_eq!(got["fields"]["title"], "Quarterly report for alice");
    let f = &got["files"]["file"];
    assert!(
        f["filename"] == "report.pdf"
            && f["type"] == "application/pdf"
            && f["content"] == String::from_utf8_lossy(PDF).as_ref(),
        "file part = {f}, want the bytes of report.pdf as they are"
    );
    assert_eq!(got["files"]["meta"]["content"], r#"{"owner": "alice"}"#);
    // The request view summarises the body rather than dumping bytes.
    assert_eq!(res.request.body, "<multipart: 3 parts, 2 files>");
    let parts = &res.request.parts;
    assert!(
        parts.len() == 3
            && parts[0].value == "Quarterly report for alice"
            && parts[0].name == "title"
            && parts[1].file == "files/report.pdf"
            && parts[1].filename == "report.pdf"
            && parts[1].content_type == "application/pdf"
            && parts[1].size == PDF.len(),
        "parts = {parts:?}"
    );
    let view: serde_json::Value = serde_json::from_str(&res.to_json()).unwrap();
    assert_eq!(view["request"]["body"], "<multipart: 3 parts, 2 files>");
    // Redacted, the summary is masked like any other body.
    res.redact = true;
    assert_eq!(res.request.display_body(true), MASKED);
    let wire = res.request.body_bytes();
    assert!(
        wire.starts_with(b"--WebAppBoundary\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nQuarterly report for alice\r\n--WebAppBoundary\r\n"),
        "wire body = {:?}",
        String::from_utf8_lossy(&wire)
    );
    assert!(wire.ends_with(b"\r\n--WebAppBoundary--\r\n"));

    // A missing variable inside a part is the usual missing-variable error.
    let err = run_err(&r, "upload-missing");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("{{nobody}}"),
        "missing variable in a part: {err}"
    );
    // A part file is confined to the project like a whole-body file.
    let err = run_err(&r, "upload-escape");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("outside project root"),
        "part file escape: {err}"
    );
}

#[test]
fn multipart_body_that_does_not_parse_is_a_usage_error() {
    let dir = write_project(&[(
        "api.http",
        "### x\n# @name x\nPOST https://example.com/upload\nContent-Type: multipart/form-data\n\n--a\nContent-Disposition: form-data; name=\"a\"\n\n1\n--a--\n",
    )]);
    let r = new_runner(
        dir.path(),
        Options {
            no_session: true,
            ..Options::default()
        },
    );
    let err = run_err(&r, "x");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("boundary"),
        "want a boundary error, got {err}"
    );
}
