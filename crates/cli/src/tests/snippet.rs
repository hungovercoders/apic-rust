use super::{execute, join, must_write, tmp};

/// apic snippet renders each language; curl stays the alias with its own
/// JSON shape; a missing variable or an unknown language is refused.
#[test]
fn snippet() {
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "api.http"),
        "### get\n# @name get-user\n# @auth bearer {{token}}\nGET {{baseUrl}}/users/1\n\n### gap\n# @name gap\nGET {{baseUrl}}/{{nope}}\n",
    );
    must_write(
        join(dir, "http-client.env.json"),
        r#"{"dev": {"baseUrl": "https://api.example.com", "token": "t-1"}}"#,
    );
    let args = ["-C", dir, "--env", "dev", "--no-session"];
    let run = |first: &[&str]| execute(&[first, &args[..]].concat());
    let (code, out, errb) = run(&["snippet", "get-user", "--lang", "python"]);
    assert!(
        code == 0 && out.contains(r#""Authorization": "Bearer t-1""#) && out.contains("requests.request("),
        "python: code={code} out={out} err={errb}"
    );
    let (code, out, _) = run(&["snippet", "get-user", "-l", "js", "--redact", "--json"]);
    assert!(
        code == 0 && out.contains(r#""lang": "js""#) && out.contains("process.env.TOKEN") && !out.contains("t-1"),
        "js --json --redact: code={code} out={out}"
    );
    let (code, curl_out, _) = run(&["curl", "get-user"]);
    let (_, snip_out, _) = run(&["snippet", "get-user"]);
    assert!(
        code == 0 && curl_out == snip_out && curl_out.starts_with("curl -sS"),
        "curl alias: {curl_out:?} vs {snip_out:?}"
    );
    let (code, out, _) = run(&["curl", "get-user", "--json"]);
    assert!(
        code == 0 && out.contains(r#""command": "curl -sS"#),
        "curl --json keeps its shape: {out}"
    );
    let (code, _, errb) = run(&["snippet", "get-user", "--lang", "cobol"]);
    assert!(
        code == 2 && errb.contains("curl, httpie, powershell, python, js, go"),
        "unknown language: code={code} err={errb}"
    );
    let (code, _, errb) = run(&["snippet", "gap", "--lang", "go"]);
    assert!(code == 2 && errb.contains("nope"), "missing variable: code={code} err={errb}");
}
