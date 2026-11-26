use apic_httpd::Response;

use super::{execute, join, must_write, server, tmp};

/// `apic run --output` saves one request's body from the working
/// directory; a flow is refused, and so is a project input as the target.
#[test]
fn run_output_flag() {
    let srv = server(|_| Response::new(200).with_header("Content-Type", "text/csv").with_body("date,total\n2026-01-01,3\n"));
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "api.http"),
        "### Report\n# @name report\nGET {{baseUrl}}/report\n\n### Other\n# @name other\nGET {{baseUrl}}/report\n",
    );
    must_write(
        join(dir, "http-client.env.json"),
        &format!(r#"{{"dev":{{"baseUrl":"{}"}}}}"#, srv.url()),
    );
    let elsewhere = super::tmp();
    let out = join(elsewhere.path(), "daily.csv");
    let (code, stdout, stderr) = execute(&[
        "run", "report", "-C", dir, "--env", "dev", "--no-session", "--no-color", "--output", &out,
    ]);
    assert!(
        code == 0 && stdout.contains(&format!("saved to {out}")),
        "code={code} out={stdout} err={stderr}"
    );
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "date,total\n2026-01-01,3\n");
    let (code, _, stderr) = execute(&[
        "run", "report", "other", "-C", dir, "--env", "dev", "--no-session", "--output", &out,
    ]);
    assert!(code == 2 && stderr.contains("--output saves one response"), "flow: code={code} err={stderr}");
    let (code, _, stderr) = execute(&[
        "run", "report", "-C", dir, "--env", "dev", "--no-session", "--output", &join(dir, "api.http"),
    ]);
    assert!(
        code == 2 && stderr.contains("would overwrite a project file"),
        "input: code={code} err={stderr}"
    );
    let (code, stdout, stderr) = execute(&[
        "run", "report", "-C", dir, "--env", "dev", "--no-session", "--json", "--output", &out,
    ]);
    assert!(
        code == 0 && stdout.contains(r#""saved_to":"#),
        "json: code={code} out={stdout} err={stderr}"
    );
}
