use apic_httpd::Response;

use super::{execute, join, must_write, server, tmp};

/// A file run as a flow skips a # @disabled request and counts it; naming
/// it sends it.
#[test]
fn run_skips_disabled_in_a_flow() {
    let srv = server(|_| Response::new(200));
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "api.http"),
        "### one\n# @name one\nGET {{baseUrl}}/one\n\n### off\n# @name off\n# @disabled\nGET {{baseUrl}}/off\n",
    );
    must_write(
        join(dir, "http-client.env.json"),
        &format!(r#"{{"dev":{{"baseUrl":"{}"}}}}"#, srv.url()),
    );
    let args = ["-C", dir, "--env", "dev", "--no-session", "--no-color"];
    let (code, out, errb) = execute(&[&["run", "api.http"], &args[..]].concat());
    assert!(
        code == 0 && out.contains("skipped (disabled)") && out.contains("1 passed, 1 skipped"),
        "code={code} out={out} err={errb}"
    );
    let (code, out, _) = execute(&[&["run", "api.http", "--json"], &args[..]].concat());
    assert!(
        code == 0 && out.contains(r#""skipped":"disabled""#),
        "json: code={code} out={out}"
    );
    let (code, out, _) = execute(&[&["run", "off"], &args[..]].concat());
    assert!(
        code == 0 && out.contains("200 OK") && !out.contains("skipped"),
        "by name: code={code} out={out}"
    );
    let (code, out, _) = execute(&[&["list", "--json"], &args[..2]].concat());
    assert!(
        code == 0 && out.matches(r#""disabled": true"#).count() == 1,
        "list: code={code} out={out}"
    );
}
