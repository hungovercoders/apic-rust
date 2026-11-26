use super::{execute, join, must_write, tmp};

/// apic env lists the Security.Auth configurations with secrets masked,
/// and validate reports one it cannot use and fields it ignores.
#[test]
fn env_and_validate_show_security_auth() {
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "api.http"),
        "GET https://x/me\nAuthorization: Bearer {{$auth.token(\"api\")}}\n",
    );
    must_write(
        join(dir, "http-client.env.json"),
        r#"{"dev": {"Security": {"Auth": {
  "api": {"Type": "OAuth2", "Grant Type": "Client Credentials", "Token URL": "https://idp/token", "Client ID": "cli", "Revoke URL": "r"},
  "old": {"Type": "OAuth2", "Grant Type": "Implicit", "Auth URL": "https://idp/auth", "Client ID": "cli"}}}}}"#,
    );
    must_write(
        join(dir, "http-client.private.env.json"),
        r#"{"dev": {"Security": {"Auth": {"api": {"Client Secret": "s3cret"}}}}}"#,
    );
    let (code, out, errb) = execute(&["env", "-C", dir, "--env", "dev", "--json"]);
    assert!(
        code == 0
            && !out.contains("s3cret")
            && out.contains(r#""name": "api""#)
            && out.contains("clientSecret=***")
            && out.contains("Implicit grant is not supported"),
        "env --json: code={code} out={out} err={errb}"
    );
    let (code, out, _) = execute(&["env", "-C", dir, "--env", "dev", "--no-color"]);
    assert!(
        code == 0 && !out.contains("s3cret") && out.contains("api oauth2 clientId=cli clientSecret=***"),
        "env: code={code} out={out}"
    );
    let (code, out, _) = execute(&["validate", "-C", dir, "--no-color"]);
    assert!(
        code == 2
            && out.contains(r#"Security.Auth "old": the Implicit grant is not supported"#)
            && out.contains(r#""Revoke URL" is not used by apic"#)
            && out.contains("(bad-auth-config)"),
        "validate: code={code} out={out}"
    );
}
