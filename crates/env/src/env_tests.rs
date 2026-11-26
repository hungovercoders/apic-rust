use std::fs;
use std::path::Path;

use serde_json::json;

use crate::{load, DOT_ENV_FILE, PRIVATE_FILE, PUBLIC_FILE};

fn write(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

/// A project with no env files at all still loads: every file here is
/// optional.
#[test]
fn load_missing_files_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let e = load(dir.path()).expect("missing env files should not be an error");
    assert!(
        e.names().is_empty() && e.found.is_empty(),
        "names={:?} found={:?}",
        e.names(),
        e.found
    );
    // Reading an absent environment is a lookup miss, not a panic.
    assert!(e.public_vars("dev").is_empty() && e.private_vars("dev").is_empty());
}

#[test]
fn load_reads_all_three_files() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        PUBLIC_FILE,
        r#"{"dev": {"baseUrl": "https://dev.example.com"}}"#,
    );
    write(
        dir.path(),
        PRIVATE_FILE,
        r#"{"dev": {"token": "secret-token"}}"#,
    );
    write(dir.path(), DOT_ENV_FILE, "API_KEY=from-dotenv\n");

    let e = load(dir.path()).unwrap();
    assert_eq!(e.public_vars("dev")["baseUrl"], "https://dev.example.com");
    assert_eq!(e.private_vars("dev")["token"], "secret-token");
    assert_eq!(e.dot_env["API_KEY"], "from-dotenv");
    assert_eq!(e.found, [PUBLIC_FILE, PRIVATE_FILE, DOT_ENV_FILE]);
}

/// `$shared` is the convention inherited from JetBrains: per-environment
/// values win over shared ones, and `$shared` is never itself an
/// environment.
#[test]
fn shared_merging() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        PUBLIC_FILE,
        r#"{
		"$shared": {"baseUrl": "https://shared.example.com", "common": "yes"},
		"dev":     {"baseUrl": "https://dev.example.com"},
		"prod":    {}
	}"#,
    );
    write(
        dir.path(),
        PRIVATE_FILE,
        r#"{
		"$shared": {"token": "shared-token"},
		"dev":     {"token": "dev-token"}
	}"#,
    );

    let e = load(dir.path()).unwrap();
    let dev = e.public_vars("dev");
    assert_eq!(
        dev["baseUrl"], "https://dev.example.com",
        "dev should win over $shared"
    );
    assert_eq!(dev["common"], "yes", "dev should inherit $shared");
    assert_eq!(
        e.public_vars("prod")["baseUrl"],
        "https://shared.example.com"
    );
    assert_eq!(e.private_vars("dev")["token"], "dev-token");
    assert_eq!(e.private_vars("prod")["token"], "shared-token");

    // $shared is a merge source, not an environment anyone can select.
    assert!(!e.names().iter().any(|n| n == "$shared"), "{:?}", e.names());
}

/// Callers cannot mutate the loaded state through the map they are handed.
#[test]
fn merged_vars_are_copies() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        PUBLIC_FILE,
        r#"{"dev": {"baseUrl": "https://dev.example.com"}}"#,
    );
    let e = load(dir.path()).unwrap();
    let mut vars = e.public_vars("dev");
    vars.insert("baseUrl".to_string(), "mutated".to_string());
    assert_eq!(e.public_vars("dev")["baseUrl"], "https://dev.example.com");
}

#[test]
fn names_and_has() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        PUBLIC_FILE,
        r#"{"prod": {}, "dev": {}, "$shared": {}}"#,
    );
    write(dir.path(), PRIVATE_FILE, r#"{"dev": {}, "staging": {}}"#);

    let e = load(dir.path()).unwrap();
    let want = ["dev", "prod", "staging"];
    assert_eq!(e.names(), want, "sorted, deduplicated, no $shared");
    for n in want {
        assert!(e.has(n), "has({n:?}) = false");
    }
    assert!(!e.has("nope"));
}

/// The JSON types a hand-written env file can legally contain; apic
/// substitutes strings, so each has to render sensibly.
#[test]
fn non_string_values() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        PUBLIC_FILE,
        r#"{"dev": {
		"str":   "s",
		"int":   8080,
		"float": 1.5,
		"bool":  true,
		"null":  null,
		"list":  [1, 2],
		"obj":   {"a": 1},
		"mixed": [1.0, 1e21, "<", {"b":null}],
		"big": 9007199254740993, "f": 1e-7, "g": 1e21, "h": -0.0
	}, "n": null}"#,
    );
    let e = load(dir.path()).unwrap();
    let vars = e.public_vars("dev");
    for (name, want) in [
        ("str", "s"),
        ("int", "8080"), // not 8080.000000, and not 8.08e+03
        ("float", "1.5"),
        ("bool", "true"),
        ("null", ""),
        ("list", "[1,2]"),
        ("obj", r#"{"a":1}"#),
        ("mixed", "[1,1e+21,\"\\u003c\",{\"b\":null}]"),
        ("big", "9007199254740992"),
        ("f", "0.0000001"),
        ("g", "1000000000000000000000"),
        ("h", "-0"),
    ] {
        assert_eq!(vars[name], want, "{name}");
    }
    assert!(e.has("n") && e.public_vars("n").is_empty());
}

/// A typo in an env file produces an error the user can act on, naming the
/// file rather than just "unexpected token".
#[test]
fn bad_json_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), PUBLIC_FILE, r#"{"dev": {"baseUrl": }}"#);
    let err = load(dir.path()).expect_err("malformed env JSON should be an error");
    assert_eq!(
        err.to_string(),
        "http-client.env.json: invalid character '}' looking for beginning of value"
    );
    write(dir.path(), PUBLIC_FILE, r#"{"dev": "x"}"#);
    assert_eq!(
        load(dir.path()).unwrap_err().to_string(),
        "http-client.env.json: json: cannot unmarshal string into Go value of type map[string]interface {}"
    );
    write(dir.path(), PUBLIC_FILE, "[1]");
    assert_eq!(
        load(dir.path()).unwrap_err().to_string(),
        "http-client.env.json: json: cannot unmarshal array into Go value of type map[string]map[string]interface {}"
    );
}

#[test]
fn bad_private_json_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), PRIVATE_FILE, "not json at all");
    let err = load(dir.path()).expect_err("malformed private env JSON should be an error");
    assert!(
        err.to_string().contains(PRIVATE_FILE),
        "error should name the file, got {err}"
    );
}

#[test]
fn bad_dotenv_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), DOT_ENV_FILE, "A=1\nB='x\n");
    assert_eq!(
        load(dir.path()).unwrap_err().to_string(),
        ".env: unterminated quoted value 'x"
    );
    fs::remove_file(dir.path().join(DOT_ENV_FILE)).unwrap();
    fs::create_dir(dir.path().join(DOT_ENV_FILE)).unwrap();
    let err = load(dir.path()).unwrap_err().to_string();
    assert!(
        err.starts_with(".env: read ") && err.ends_with(".env: is a directory"),
        "{err}"
    );
}

#[test]
fn ssl_configuration_is_read_not_a_variable() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        PUBLIC_FILE,
        r#"{"$shared": {"SSLConfiguration": {"clientCertificate": "certs/shared.pem"}}, "dev": {"baseUrl": "https://dev", "SSLConfiguration": {"clientCertificate": {"path": "certs/dev.pem", "keyPath": "certs/dev-key.pem"}, "verifyHostCertificate": false}}, "prod": {"baseUrl": "https://prod"}}"#,
    );
    write(
        dir.path(),
        PRIVATE_FILE,
        r#"{"prod": {"SSLConfiguration": {"clientCertificate": {"path": "certs/prod.pem"}, "hasCertificatePassphrase": true}}}"#,
    );
    let e = load(dir.path()).unwrap();
    assert!(
        !e.public_vars("dev").contains_key("SSLConfiguration"),
        "SSLConfiguration leaked into the variables"
    );
    let dev = e.ssl("dev").unwrap();
    assert!(
        dev.cert_file == "certs/dev.pem"
            && dev.key_file == "certs/dev-key.pem"
            && dev.verify_host == Some(false),
        "dev = {dev:?}"
    );
    let prod = e.ssl("prod").unwrap();
    assert!(
        prod.cert_file == "certs/prod.pem" && prod.has_passphrase && prod.verify_host.is_none(),
        "prod (private file) = {prod:?}"
    );
    let other = e.ssl("staging").unwrap();
    assert_eq!(
        other.cert_file, "certs/shared.pem",
        "$shared fallback = {other:?}"
    );
    assert_eq!(e.ssl("").unwrap().cert_file, "certs/shared.pem");
    write(
        dir.path(),
        PRIVATE_FILE,
        r#"{"prod": {"SSLConfiguration": {"clientCertificate": 42}}}"#,
    );
    assert_eq!(
        load(dir.path()).unwrap_err().to_string(),
        "http-client.private.env.json: prod: SSLConfiguration: clientCertificate must be a path or an object with path and keyPath"
    );
}

/// A Security.Auth block is lifted out of the variables; the private file's
/// fields win over the public file's, an environment's over $shared's.
#[test]
fn security_auth_is_read_not_a_variable() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        PUBLIC_FILE,
        r#"{
  "$shared": {"Security": {"Auth": {"api": {"Type": "OAuth2", "Grant Type": "Client Credentials", "Token URL": "https://idp/token", "Client ID": "shared-id"}}}},
  "dev": {"baseUrl": "https://dev", "Security": {"Auth": {"api": {"Client ID": "dev-id"}, "other": {"Type": "OAuth2"}}}}
}"#,
    );
    write(
        dir.path(),
        PRIVATE_FILE,
        r#"{"dev": {"Security": {"Auth": {"api": {"Client Secret": "s3cret"}}}}}"#,
    );
    let e = load(dir.path()).unwrap();
    assert!(
        !e.public_vars("dev").contains_key("Security"),
        "Security is not a variable"
    );
    let dev = e.auth("dev");
    let api = &dev["api"];
    assert!(
        api.fields["Client ID"] == json!("dev-id")
            && api.fields["Client Secret"] == json!("s3cret")
            && api.fields["Token URL"] == json!("https://idp/token"),
        "dev api: {api:?}"
    );
    assert_eq!(api.files, [PUBLIC_FILE, PRIVATE_FILE]);
    assert!(dev.contains_key("other"));
    let prod = e.auth("prod");
    let shared = &prod["api"];
    assert!(
        shared.fields["Client ID"] == json!("shared-id")
            && !shared.fields.contains_key("Client Secret"),
        "prod sees only $shared: {shared:?}"
    );
    assert_eq!(e.auth_names(), ["api", "other"]);
    assert_eq!(
        e.all_auth()[PUBLIC_FILE]["dev"]["other"]["Type"],
        json!("OAuth2")
    );
    write(
        dir.path(),
        PUBLIC_FILE,
        r#"{"dev": {"Security": {"Auth": {"api": "nope"}}}}"#,
    );
    let err = load(dir.path()).unwrap_err().to_string();
    assert!(
        err.contains(r#"auth configuration "api": expected an object"#),
        "bad block: {err}"
    );
}
