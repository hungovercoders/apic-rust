use apic_httpd::{Request as HttpRequest, Response as HttpResponse, TestServer};
use base64::Engine;

use super::*;
use crate::{Body, BASE64, MASKED};

fn pngish() -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0, 0xff, 0xfe];
    for _ in 0..40 {
        b.extend_from_slice(&[0x00, 0x80, 0xff]);
    }
    b
}

#[cfg(unix)]
fn mode(p: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// A `>> file` line saves the response bytes as they came, confined to
/// the project, created 0600 when a secret went into the request; `>>`
/// refuses an existing file and `>>!` overwrites it.
#[test]
fn save_response_to_file() {
    let srv = TestServer::new(|r: &HttpRequest| match r.path.as_str() {
        "/image" => HttpResponse::new(200)
            .with_header("Content-Type", "image/png")
            .with_body(pngish()),
        _ => HttpResponse::json(200, r#"{"ok": true}"#),
    });
    let api = r#"
### Public image
# @name image
GET SRV/image

>> ../out/logo.png

### Image again, refusing to overwrite
# @name again
GET SRV/image

>> ../out/logo.png

### Overwrite with a secret in play
# @name secret
GET SRV/data
X-Api-Key: {{apiKey}}

>>! ../out/data.json

### A secret in the body only
# @name bodysecret
POST SRV/data
Content-Type: application/json

{"key": "{{apiKey}}"}

>>! ../out/body.json

### Escapes the project
# @name escape
GET SRV/data

>> ../../../escape.json
"#
    .replace("SRV", &srv.url());
    let tmp = write_project(&[
        (
            "http-client.private.env.json",
            r#"{"dev": {"apiKey": "s3cret"}}"#,
        ),
        ("sub/api.http", &api),
    ]);
    let dir = tmp.path();
    let mut r = new_runner(
        dir,
        Options {
            env: "dev".into(),
            no_session: true,
            ..Options::default()
        },
    );
    let mut res = run_ok(&r, "image");
    assert!(res.ok && res.saved_to == "out/logo.png", "image: {res:?}");
    assert_eq!(std::fs::read(dir.join("out/logo.png")).unwrap(), pngish());
    // The JSON view carries the bytes as base64 and says so; the text
    // output summarises instead of printing them.
    let obj: serde_json::Value = serde_json::from_str(&res.to_json()).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(pngish());
    assert!(
        obj["response"]["body_encoding"] == "base64"
            && obj["response"]["body"] == b64.as_str()
            && obj["saved_to"] == "out/logo.png",
        "json: {obj}"
    );
    assert_eq!(res.response.as_ref().unwrap().body_encoding, BASE64);
    // Under --redact the body is the mask, which is text, so no encoding.
    res.redact = true;
    let d = res.display_response().unwrap();
    assert!(
        d.body == Body::text(MASKED) && d.body_encoding.is_empty(),
        "redacted: {d:?}"
    );
    res.redact = false;
    // A bounded body is the base64 of its first bytes, marked truncated.
    res.body_limit = 6;
    let d = res.display_response().unwrap();
    assert!(
        d.body_truncated
            && d.body
                == Body::text(base64::engine::general_purpose::STANDARD.encode(&pngish()[..6]))
            && d.size == pngish().len(),
        "limited: {d:?}"
    );
    assert_eq!(res.display_raw_body(), &pngish()[..6]);
    #[cfg(unix)]
    assert_eq!(mode(&dir.join("out/logo.png")), 0o644, "no secret went in");
    // The same file again without `!` is refused, and the request still
    // counts as failed so CI notices.
    let res = run_ok(&r, "again");
    assert!(
        !res.ok
            && res.errors.len() == 1
            && res.errors[0].contains("use >>! to overwrite")
            && res.saved_to.is_empty(),
        "again: {res:?}"
    );
    // Overwrite, with a secret header: 0600.
    std::fs::write(dir.join("out/data.json"), "old").unwrap();
    let res = run_ok(&r, "secret");
    assert!(res.ok && res.saved_to == "out/data.json", "secret: {res:?}");
    assert_eq!(
        std::fs::read(dir.join("out/data.json")).unwrap(),
        br#"{"ok": true}"#
    );
    #[cfg(unix)]
    assert_eq!(mode(&dir.join("out/data.json")), 0o600, "a secret went in");
    // A secret placeholder in the body tightens the file too.
    let res = run_ok(&r, "bodysecret");
    assert!(
        res.ok && res.saved_to == "out/body.json",
        "bodysecret: {res:?}"
    );
    #[cfg(unix)]
    assert_eq!(
        mode(&dir.join("out/body.json")),
        0o600,
        "a secret went into the body"
    );
    // A path outside the project is refused before anything is written.
    let res = run_ok(&r, "escape");
    assert!(
        !res.ok && res.errors.len() == 1 && res.errors[0].contains("outside project root"),
        "escape: {res:?}"
    );
    assert!(
        !dir.join("..").join("escape.json").exists(),
        "escaped the project"
    );
    let diags = r.project.validate();
    assert!(
        diags.len() == 1
            && diags[0].code == "bad-save-path"
            && diags[0].line == 34
            && diags[0].column == 4,
        "validate: {diags:?}"
    );
    let d = r.describe(&r.project.requests()[2]);
    assert_eq!(d.save_to, "../out/data.json (overwrite)");

    // --output: a path from the working directory, overwriting, and in
    // place of the request's own `>>` line, so "again" (whose target
    // exists) succeeds and writes only there.
    let out_dir = tempfile::tempdir().unwrap();
    let target = out_dir.path().join("deep").join("logo.png");
    let target_text = target.to_str().unwrap().to_string();
    r.opts.output = target_text.clone();
    let before = std::fs::metadata(dir.join("out/logo.png"))
        .unwrap()
        .modified()
        .unwrap();
    let res = run_ok(&r, "again");
    assert!(
        res.ok && res.saved_to == target_text && res.errors.is_empty(),
        "--output: {res:?}"
    );
    assert_eq!(std::fs::read(&target).unwrap(), pngish());
    let after = std::fs::metadata(dir.join("out/logo.png"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(after, before, "the request's own >> target was touched");
}

/// A project reached through a symlink (macOS keeps temp dirs under /var,
/// which is /private/var) still saves into directories that do not exist
/// yet: the confinement resolves the nearest existing ancestor.
#[cfg(unix)]
#[test]
fn save_response_through_symlinked_root() {
    let srv = TestServer::new(|_: &HttpRequest| HttpResponse::new(200).with_body("csv,data\n"));
    // The project is a real directory whose path goes through a symlink,
    // as /var/folders/... does on macOS.
    let real_parent = tempfile::tempdir().unwrap();
    let link_holder = tempfile::tempdir().unwrap();
    let link_parent = link_holder.path().join("link");
    if std::os::unix::fs::symlink(real_parent.path(), &link_parent).is_err() {
        return; // no symlinks here
    }
    let real = real_parent.path().join("proj");
    let link = link_parent.join("proj");
    std::fs::create_dir(&real).unwrap();
    let file = |save: &str| format!("### r\n# @name r\nGET {}/x\n\n>> {save}\n", srv.url());
    std::fs::write(real.join("api.http"), file("./new/deeper/out.csv")).unwrap();
    let opts = || Options {
        no_session: true,
        ..Options::default()
    };
    let r = new_runner(&link, opts());
    let res = run_ok(&r, "r");
    assert!(res.ok && res.saved_to == "new/deeper/out.csv", "{res:?}");
    assert_eq!(
        std::fs::read(real.join("new/deeper/out.csv")).unwrap(),
        b"csv,data\n"
    );
    // And an escape through the link is still an escape.
    std::fs::write(real.join("api.http"), file("../../nope/out.csv")).unwrap();
    let r = new_runner(&link, opts());
    let res = run_ok(&r, "r");
    assert!(
        !res.ok && res.errors[0].contains("outside project root"),
        "{res:?}"
    );
}
