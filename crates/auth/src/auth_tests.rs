use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apic_gocompat::Time;
use apic_httpc::{CancelToken, Client, Headers, Request};
use apic_httpd::{Handler, Mux, Response, TestServer};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::json;

use crate::apply::{apply, hash_key, Cache, Clock, Env, Stderr};
use crate::awscreds::resolve_aws;
use crate::oauth2::{cache_key, describe_cached};
use crate::sigv4::{sign_sig_v4, sign_without, AwsCredentials};
use crate::spec::{parse, Spec};
use crate::Error;

pub(crate) fn get(url: &str) -> Request {
    Request::get(url)
}

pub(crate) fn authz(req: &Request) -> String {
    req.headers.get("Authorization").unwrap_or("").to_string()
}

/// A cache kept in memory.
#[derive(Default)]
pub(crate) struct MemCache(pub Mutex<BTreeMap<String, String>>);

impl Cache for MemCache {
    fn get(&self, key: &str) -> Option<String> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
}

/// A cache that cannot be saved.
struct ErrCache(&'static str);

impl Cache for ErrCache {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }
    fn set(&self, _key: &str, _value: &str) -> Result<(), String> {
        Err(self.0.to_string())
    }
}

/// A clock the test moves.
pub(crate) fn clock(start: Time) -> (Arc<Mutex<Time>>, Option<Clock>) {
    let now = Arc::new(Mutex::new(start));
    let read = now.clone();
    (now, Some(Box::new(move || *read.lock().unwrap())))
}

/// A prompt writer the test reads back.
pub(crate) fn prompt() -> (Arc<Mutex<Vec<u8>>>, Option<Stderr>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let stderr: Stderr = buf.clone();
    (buf, Some(stderr))
}

pub(crate) fn text(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
}

/// An environment the test sets, in place of the process's own.
type Vars = Arc<Mutex<HashMap<String, String>>>;

fn vars(pairs: &[(&str, &str)]) -> Vars {
    Arc::new(Mutex::new(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    ))
}

fn setenv(vars: &Vars, name: &str, value: &str) {
    vars.lock()
        .unwrap()
        .insert(name.to_string(), value.to_string());
}

fn env_with(vars: &Vars) -> Env {
    let vars = vars.clone();
    Env {
        getenv: Some(Box::new(move |name| {
            vars.lock().unwrap().get(name).cloned()
        })),
        ..Env::default()
    }
}

fn bg() -> CancelToken {
    CancelToken::new()
}

#[test]
fn parse_specs() {
    let good = [
        ("none", "none"),
        ("bearer {{token}}", "bearer"),
        ("basic {{user}} {{password}}", "basic"),
        ("aws", "aws"),
        ("aws service=s3 region=eu-west-2 profile=dev", "aws"),
        (
            "oauth2 tokenUrl={{tokenUrl}} clientId=a clientSecret=b scope=\"read write\"",
            "oauth2",
        ),
        (
            "oauth2 grant=password tokenUrl=u clientId=a username=x password=y",
            "oauth2",
        ),
        (
            "oauth2 grant=device_code tokenUrl=u deviceUrl=d clientId=a",
            "oauth2",
        ),
        ("exec gcloud auth print-access-token ttl=5m", "exec"),
        (
            "exec az account get-access-token --query=accessToken header=X-Token prefix=",
            "exec",
        ),
    ];
    for (raw, typ) in good {
        match parse(raw) {
            Ok(s) => assert_eq!(s.r#type, typ, "{raw}"),
            Err(e) => panic!("{raw:?}: {e}"),
        }
    }
    let s = parse("exec az account get-access-token --query=accessToken header=X-Token prefix=")
        .unwrap();
    assert!(
        s.args.len() == 4
            && s.args[3] == "--query=accessToken"
            && s.option("header") == "X-Token"
            && s.options.get("prefix").map(String::as_str) == Some(""),
        "exec parse: {s:?}"
    );
    let s = parse(r#"oauth2 tokenUrl=u clientId=a scope="read write""#).unwrap();
    assert_eq!(s.option("scope"), "read write", "quoted option: {s:?}");
    let bad = [
        "",
        "magic",
        "bearer",
        "basic user",
        "aws sigv4",
        "aws foo=1",
        "oauth2 clientId=a",
        "oauth2 tokenUrl=u",
        "oauth2 tokenUrl=u clientId=a grant=implicit",
        "exec",
        "none x",
        r#"bearer "unterminated"#,
    ];
    for raw in bad {
        assert!(parse(raw).is_err(), "{raw:?} should fail");
    }
}

#[test]
fn parse_error_messages() {
    let msg = |raw: &str| parse(raw).unwrap_err().to_string();
    assert_eq!(
        msg(""),
        "@auth needs a type: one of apikey, aws, basic, bearer, digest, exec, none, oauth2"
    );
    assert_eq!(
        msg("Magic x"),
        "@auth: unknown type \"Magic\" (one of apikey, aws, basic, bearer, digest, exec, none, oauth2)"
    );
    assert_eq!(
        msg("bearer \"unterminated"),
        "@auth \"bearer \\\"unterminated\": unterminated quote"
    );
    assert_eq!(
        msg("oauth2 tokenUrl=u clientId=a grant=implicit"),
        "@auth oauth2: unknown grant \"implicit\" (client_credentials, password, device_code or authorization_code)"
    );
}

// Nothing a user sees from a spec's options may change from run to run.
#[test]
fn spec_options_in_a_stable_order() {
    let spec = parse("oauth2 tokenUrl={{tok}} clientId={{id}} clientSecret={{secret}} scope={{scope}} audience={{aud}}").unwrap();
    assert_eq!(
        spec.texts().join(" "),
        "{{aud}} {{id}} {{secret}} {{scope}} {{tok}}",
        "texts() should list the options by name"
    );

    let err = spec
        .render(|v| {
            if v == "{{id}}" || v == "{{tok}}" {
                return Err(format!("{v}: missing"));
            }
            Ok(v.to_string())
        })
        .unwrap_err();
    assert!(
        err.starts_with("{{id}}"),
        "render should fail on clientId, the first failing option by name, got {err}"
    );

    let err = parse("aws service=s3 zone=a bucket=b").unwrap_err();
    assert!(err.to_string().contains("\"bucket\""), "{err}");
    let err = parse("oauth2 tokenUrl=t clientId=c zeta=1 alpha=2").unwrap_err();
    assert!(err.to_string().contains("\"alpha\""), "{err}");
}

#[test]
fn bearer_and_basic() {
    let mut req = get("http://x/");
    let s = parse("bearer abc").unwrap();
    apply(&bg(), &s, &mut req, b"", &Env::default()).unwrap();
    assert_eq!(authz(&req), "Bearer abc");
    let s = parse("basic alice s3cret").unwrap();
    apply(&bg(), &s, &mut req, b"", &Env::default()).unwrap();
    assert_eq!(
        authz(&req),
        format!("Basic {}", STANDARD.encode("alice:s3cret"))
    );
}

fn sign_no_content_hash(
    req: &mut Request,
    creds: &AwsCredentials,
    service: &str,
    region: &str,
    at: Time,
) {
    sign_sig_v4(req, b"", creds, service, region, at);
    // Re-sign without x-amz-content-sha256 to match the official vectors.
    req.headers.del("X-Amz-Content-Sha256");
    req.headers.del("Authorization");
    sign_without(req, creds, service, region, at);
}

// Vectors from the AWS SigV4 test suite (aws-sig-v4-test-suite, 2015-08-30).
#[test]
fn sig_v4_test_vectors() {
    let creds = AwsCredentials {
        access_key_id: "AKIDEXAMPLE".to_string(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
        session_token: String::new(),
    };
    let at = Time::from_civil(2015, 8, 30, 12, 36, 0, 0, 0);
    let cases = [
        (
            "get-vanilla",
            "GET",
            "https://example.amazonaws.com/",
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31",
        ),
        (
            "get-vanilla-query-order-key-case",
            "GET",
            "https://example.amazonaws.com/?Param2=value2&Param1=value1",
            "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500",
        ),
        (
            "get-vanilla-empty-query-key",
            "GET",
            "https://example.amazonaws.com/?Param1=value1",
            "a67d582fa61cc504c4bae71f336f98b97f1ea3c7a6bfe1b6e45aec72011b9aeb",
        ),
    ];
    for (name, method, url, want) in cases {
        let mut req = Request::new(method, url);
        // The suite signs only host and x-amz-date; our signer also adds
        // x-amz-content-sha256, so strip it to compare against the vectors.
        sign_no_content_hash(&mut req, &creds, "service", "us-east-1", at);
        let got = authz(&req);
        assert!(got.ends_with(&format!("Signature={want}")), "{name}: {got}");
        assert!(
            got.contains("SignedHeaders=host;x-amz-date,"),
            "{name}: signed headers {got}"
        );
    }
}

#[test]
fn aws_signs_through_apply() {
    let vars = vars(&[
        ("AWS_ACCESS_KEY_ID", "AKIAEXAMPLE"),
        ("AWS_SECRET_ACCESS_KEY", "secretkey"),
        ("AWS_SESSION_TOKEN", "sess"),
        ("AWS_REGION", "eu-west-2"),
        ("AWS_PROFILE", ""),
        ("AWS_CONFIG_FILE", "/nonexistent"),
        ("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent"),
    ]);

    let seen: Arc<Mutex<(Headers, Vec<u8>)>> = Arc::default();
    let record = seen.clone();
    let srv = TestServer::new(move |r: &apic_httpd::Request| {
        *record.lock().unwrap() = (r.headers.clone(), r.body.to_vec());
        Response::new(200)
    });

    let body = br#"{"a":1}"#;
    let mut req = Request::post(&format!("{}/orders?x=1", srv.url()), body.to_vec())
        .header("Content-Type", "application/json");
    let at = Time::from_civil(2026, 9, 12, 8, 0, 0, 0, 0);
    let s = parse("aws service=execute-api").unwrap();
    let mut env = env_with(&vars);
    env.now = Some(Box::new(move || at));
    apply(&bg(), &s, &mut req, body, &env).unwrap();
    Client::new().send(&req).unwrap();
    let (got, got_body) = seen.lock().unwrap().clone();
    let authz = got.get("Authorization").unwrap_or("");
    assert!(
        authz.starts_with("AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE/20260912/eu-west-2/execute-api/aws4_request, SignedHeaders=content-length;content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token, Signature="),
        "authorization: {authz}"
    );
    assert_eq!(got.get("X-Amz-Date"), Some("20260912T080000Z"));
    assert_eq!(got.get("X-Amz-Security-Token"), Some("sess"));
    assert_eq!(got_body, body);
}

#[test]
fn aws_profile_files_and_cli() {
    let home = tempfile::tempdir().unwrap();
    let credentials = home.path().join("credentials");
    let config = home.path().join("config");
    let vars = vars(&[
        ("AWS_ACCESS_KEY_ID", ""),
        ("AWS_SECRET_ACCESS_KEY", ""),
        ("AWS_REGION", ""),
        ("AWS_DEFAULT_REGION", ""),
        ("AWS_PROFILE", ""),
        ("AWS_SHARED_CREDENTIALS_FILE", credentials.to_str().unwrap()),
        ("AWS_CONFIG_FILE", config.to_str().unwrap()),
        ("PATH", ""),
    ]);
    let env = env_with(&vars);
    std::fs::write(&credentials, "[default]\naws_access_key_id = DEF\naws_secret_access_key = defsecret\n\n[static]\naws_access_key_id = STAT\naws_secret_access_key = statsecret\naws_session_token = stattok\n").unwrap();
    std::fs::write(&config, "[default]\nregion = us-east-1\n\n[profile static]\nregion = eu-west-2\ns3 =\n    max_concurrent_requests = 20\n\n[profile sso]\nsso_start_url = https://x\nregion = ap-southeast-2\n").unwrap();

    let s = parse("aws").unwrap();
    let got = resolve_aws(&bg(), &s, &env).unwrap();
    assert!(
        got.creds.access_key_id == "DEF"
            && got.region == "us-east-1"
            && got.source == "profile default",
        "default: {got:?}"
    );
    let s = parse("aws profile=static").unwrap();
    let got = resolve_aws(&bg(), &s, &env).unwrap();
    assert!(
        got.creds.access_key_id == "STAT"
            && got.creds.session_token == "stattok"
            && got.region == "eu-west-2",
        "static: {got:?}"
    );
    // An explicit profile wins over environment keys.
    setenv(&vars, "AWS_ACCESS_KEY_ID", "ENV");
    setenv(&vars, "AWS_SECRET_ACCESS_KEY", "envsecret");
    let got = resolve_aws(&bg(), &s, &env).unwrap();
    assert_eq!(
        got.creds.access_key_id, "STAT",
        "explicit profile should win"
    );
    let s = parse("aws").unwrap();
    let got = resolve_aws(&bg(), &s, &env).unwrap();
    assert!(
        got.creds.access_key_id == "ENV" && got.source == "environment",
        "env: {got:?}"
    );
    setenv(&vars, "AWS_ACCESS_KEY_ID", "");
    setenv(&vars, "AWS_SECRET_ACCESS_KEY", "");

    // A profile without static keys falls back to the AWS CLI.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        let cli = bin.path().join("aws");
        let install = |script: &str| {
            std::fs::write(&cli, script).unwrap();
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        install("#!/bin/sh\n[ \"$1 $2 $4\" = \"configure export-credentials sso\" ] || { echo wrong args >&2; exit 2; }\necho '{\"Version\":1,\"AccessKeyId\":\"CLI\",\"SecretAccessKey\":\"clisecret\",\"SessionToken\":\"clitok\"}'\n");
        setenv(&vars, "PATH", bin.path().to_str().unwrap());
        let s = parse("aws profile=sso").unwrap();
        // A script written a moment ago can be "busy" while another
        // thread of the test binary is starting a process of its own.
        let resolve = || loop {
            match resolve_aws(&bg(), &s, &env) {
                Err(e) if e.to_string().contains("text file busy") => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                other => break other,
            }
        };
        let got = resolve().unwrap();
        assert!(
            got.creds.access_key_id == "CLI"
                && got.creds.session_token == "clitok"
                && got.region == "ap-southeast-2"
                && got.source.starts_with("aws cli"),
            "cli: {got:?}"
        );
        // And the CLI's error is surfaced.
        install("#!/bin/sh\necho 'Error loading SSO Token: run aws sso login' >&2; exit 1\n");
        let err = resolve().unwrap_err();
        assert_eq!(
            err.to_string(),
            "aws: `aws configure export-credentials --profile sso` failed: Error loading SSO Token: run aws sso login"
        );
        // No CLI and no keys: a clear error.
        let empty = tempfile::tempdir().unwrap();
        setenv(&vars, "PATH", empty.path().to_str().unwrap());
        let err = resolve_aws(&bg(), &s, &env).unwrap_err();
        assert!(err.to_string().contains("profile \"sso\""), "{err}");
    }
}

#[test]
fn aws_needs_region() {
    let vars = vars(&[
        ("AWS_ACCESS_KEY_ID", "a"),
        ("AWS_SECRET_ACCESS_KEY", "b"),
        ("AWS_REGION", ""),
        ("AWS_DEFAULT_REGION", ""),
        ("AWS_CONFIG_FILE", "/nonexistent"),
        ("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent"),
    ]);
    let mut req = get("http://x/");
    let s = parse("aws").unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &env_with(&vars)).unwrap_err();
    assert!(err.to_string().contains("region"), "{err}");
}

fn json_response(status: u16, value: serde_json::Value) -> Response {
    Response::json(status, value.to_string())
}

#[test]
fn oauth2_client_credentials_caching_and_refresh() {
    let calls = Arc::new(AtomicUsize::new(0));
    let last_form: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let (count, record) = (calls.clone(), last_form.clone());
    let srv = TestServer::new(move |r: &apic_httpd::Request| {
        count.fetch_add(1, Ordering::SeqCst);
        *record.lock().unwrap() = r.form().into_iter().collect();
        match r.form_value("grant_type").as_str() {
            "client_credentials" => json_response(
                200,
                json!({"access_token": "tok-1", "refresh_token": "ref-1", "expires_in": 3600}),
            ),
            "refresh_token" => {
                json_response(200, json!({"access_token": "tok-2", "expires_in": 3600}))
            }
            _ => json_response(400, json!({"error": "unsupported_grant_type"})),
        }
    });
    let form = |name: &str| {
        last_form
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default()
    };

    let start = Time::from_civil(2026, 9, 12, 8, 0, 0, 0, 0);
    let (now, read_now) = clock(start);
    let cache = Arc::new(MemCache::default());
    let env = Env {
        cache: Some(cache.clone()),
        now: read_now,
        ..Env::default()
    };
    let s = parse(&format!(
        "oauth2 tokenUrl={} clientId=cid clientSecret=csec scope=read audience=aud",
        srv.url()
    ))
    .unwrap();

    let mut req = get("http://x/");
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(authz(&req), "Bearer tok-1");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        form("client_id") == "cid"
            && form("client_secret") == "csec"
            && form("scope") == "read"
            && form("audience") == "aud",
        "form: {:?}",
        last_form.lock().unwrap()
    );

    // Second call within the lifetime: served from cache.
    let mut req = get("http://x/");
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cache miss");
    assert_eq!(authz(&req), "Bearer tok-1");

    // Near expiry: refreshed with the refresh token.
    let later = start + Duration::from_secs(3600 - 30);
    *now.lock().unwrap() = later;
    let mut req = get("http://x/");
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(authz(&req), "Bearer tok-2");
    assert_eq!(form("grant_type"), "refresh_token");
    let entry = cache.get(&cache_key(&s)).unwrap();
    assert_eq!(describe_cached(&entry, later), "token, expires in 1h0m0s");
    // The entry keeps the refresh token the refresh did not renew.
    assert_eq!(
        entry,
        r#"{"access_token":"tok-2","refresh_token":"ref-1","expires_at":"2026-09-12T09:59:30Z"}"#
    );
}

#[test]
fn describe_cached_entries() {
    let now = Time::from_civil(2026, 9, 12, 8, 0, 0, 0, 0);
    assert_eq!(describe_cached("not json", now), "(unreadable)");
    assert_eq!(
        describe_cached(r#"{"access_token":7}"#, now),
        "(unreadable)"
    );
    assert_eq!(describe_cached("{}", now), "expired");
    assert_eq!(
        describe_cached(
            r#"{"access_token":"t","expires_at":"2026-09-12T08:00:00.4Z"}"#,
            now
        ),
        "expired"
    );
    assert_eq!(
        describe_cached(
            r#"{"access_token":"t","expires_at":"2026-09-12T09:01:30.5+01:00"}"#,
            now
        ),
        "token, expires in 1m31s"
    );
}

#[test]
fn oauth2_basic_client_auth_and_errors() {
    let srv = TestServer::new(|r: &apic_httpd::Request| match r.basic_auth() {
        Some((u, p)) if u == "cid" && p == "csec" => {
            json_response(200, json!({"access_token": "tok", "expires_in": 60}))
        }
        _ => json_response(
            401,
            json!({"error": "invalid_client", "error_description": "bad secret"}),
        ),
    });
    let mut req = get("http://x/");
    let s = parse(&format!(
        "oauth2 tokenUrl={} clientId=cid clientSecret=csec clientAuth=basic",
        srv.url()
    ))
    .unwrap();
    apply(&bg(), &s, &mut req, b"", &Env::default()).unwrap();
    assert_eq!(authz(&req), "Bearer tok");
    let s = parse(&format!(
        "oauth2 tokenUrl={} clientId=cid clientSecret=wrong clientAuth=basic",
        srv.url()
    ))
    .unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &Env::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("oauth2: {}: invalid_client bad secret", srv.url())
    );
}

#[test]
fn oauth2_token_endpoint_failures() {
    let srv = TestServer::new(|r: &apic_httpd::Request| match r.path.as_str() {
        "/html" => Response::html(502, "<h1>Bad gateway</h1>"),
        _ => json_response(200, json!({"token_type": "Bearer"})),
    });
    let mut req = get("http://x/");
    let s = parse(&format!("oauth2 tokenUrl={}/html clientId=cid", srv.url())).unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &Env::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "oauth2: {}/html returned 502 with a non-JSON body: <h1>Bad gateway</h1>",
            srv.url()
        )
    );
    let s = parse(&format!("oauth2 tokenUrl={}/empty clientId=cid", srv.url())).unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &Env::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "oauth2: {}/empty returned 200 without an access_token: {{\"token_type\":\"Bearer\"}}",
            srv.url()
        )
    );
    let s = parse("oauth2 tokenUrl=http://127.0.0.1:1/token clientId=cid").unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &Env::default()).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("oauth2: token request: Post \"http://127.0.0.1:1/token\": "),
        "{err}"
    );
}

#[test]
fn oauth2_client_auth_validation() {
    let err = parse("oauth2 tokenUrl=https://idp/token clientId=cid clientAuth=basci").unwrap_err();
    assert!(
        err.to_string().contains("clientAuth must be body or basic"),
        "{err}"
    );
}

#[test]
fn oauth2_cache_key_includes_credential_inputs() {
    let base = parse("oauth2 tokenUrl=https://idp/token clientId=cid clientSecret=secret clientAuth=basic scope=read audience=aud").unwrap();
    let diff_secret = parse("oauth2 tokenUrl=https://idp/token clientId=cid clientSecret=other clientAuth=basic scope=read audience=aud").unwrap();
    let diff_client_auth = parse("oauth2 tokenUrl=https://idp/token clientId=cid clientSecret=secret clientAuth=body scope=read audience=aud").unwrap();
    let password = |pw: &str| Spec {
        r#type: "oauth2".to_string(),
        options: [
            ("tokenUrl", "https://idp/token"),
            ("clientId", "cid"),
            ("grant", "password"),
            ("username", "alice"),
            ("password", pw),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
        ..Spec::default()
    };
    assert_ne!(
        cache_key(&base),
        cache_key(&diff_secret),
        "clientSecret must affect cache key"
    );
    assert_ne!(
        cache_key(&base),
        cache_key(&diff_client_auth),
        "clientAuth must affect cache key"
    );
    assert_ne!(
        cache_key(&password("pw-one")),
        cache_key(&password("pw-two")),
        "password must affect cache key"
    );
}

#[test]
fn oauth2_cache_save_error_returned() {
    let srv = TestServer::new(|_: &apic_httpd::Request| {
        json_response(200, json!({"access_token": "tok", "expires_in": 60}))
    });
    let s = parse(&format!("oauth2 tokenUrl={} clientId=cid", srv.url())).unwrap();
    let mut req = get("http://x/");
    let env = Env {
        cache: Some(Arc::new(ErrCache("save failed"))),
        ..Env::default()
    };
    let err = apply(&bg(), &s, &mut req, b"", &env).unwrap_err();
    assert_eq!(err, Error::Cache("save failed".to_string()));
    assert_eq!(err.to_string(), "save failed");
}

#[test]
fn oauth2_device_code() {
    let polls = Arc::new(AtomicUsize::new(0));
    let count = polls.clone();
    let mut mux: Mux<Handler> = Mux::new();
    mux.handle_fn("/device", |_| {
        json_response(
            200,
            json!({"device_code": "dev", "user_code": "ABCD-1234", "verification_uri": "https://idp/activate", "expires_in": 60, "interval": 1}),
        )
    })
    .unwrap();
    mux.handle_fn("/token", move |r| {
        if r.form_value("device_code") != "dev" {
            return Response::new(400);
        }
        if count.fetch_add(1, Ordering::SeqCst) + 1 < 2 {
            return json_response(400, json!({"error": "authorization_pending"}));
        }
        json_response(200, json!({"access_token": "dtok", "expires_in": 60}))
    })
    .unwrap();
    let srv = TestServer::new(mux.into_handler());
    let (buf, stderr) = prompt();
    let mut req = get("http://x/");
    let s = parse(&format!(
        "oauth2 grant=device_code tokenUrl={0}/token deviceUrl={0}/device clientId=cid",
        srv.url()
    ))
    .unwrap();
    let env = Env {
        stderr,
        ..Env::default()
    };
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(authz(&req), "Bearer dtok");
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    assert_eq!(
        text(&buf),
        "\nTo sign in, open https://idp/activate and enter the code ABCD-1234\nWaiting for approval. approved.\n"
    );
}

#[test]
fn oauth2_device_code_is_cancelled() {
    let srv = TestServer::new(|_: &apic_httpd::Request| {
        json_response(200, json!({"device_code": "dev", "interval": 30}))
    });
    let s = parse(&format!(
        "oauth2 grant=device_code tokenUrl={0}/token deviceUrl={0}/device clientId=cid",
        srv.url()
    ))
    .unwrap();
    let cancel = CancelToken::new();
    let stop = cancel.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        stop.cancel();
    });
    let mut req = get("http://x/");
    let err = apply(&cancel, &s, &mut req, b"", &Env::default()).unwrap_err();
    t.join().unwrap();
    assert_eq!(err, Error::Cancelled);
    assert_eq!(err.to_string(), "context canceled");
}

#[cfg(unix)]
const SEARCH_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

#[cfg(unix)]
#[test]
fn exec() {
    let path = vars(&[("PATH", SEARCH_PATH)]);
    let mut req = get("http://x/");
    let s = parse("exec echo tok-1").unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &env_with(&path)).unwrap_err();
    assert_eq!(
        err.to_string(),
        "@auth exec is disabled: set `auth:\n  allowExec: true` in apic.yaml to let request files run \"echo tok-1\""
    );
    let allowed = || Env {
        allow_exec: true,
        ..env_with(&path)
    };
    apply(&bg(), &s, &mut req, b"", &allowed()).unwrap();
    assert_eq!(authz(&req), "Bearer tok-1");

    let s = parse("exec echo tok-2 header=X-Api-Key prefix= ttl=1h").unwrap();
    let cache = Arc::new(MemCache::default());
    let start = Time::from_civil(2026, 9, 12, 8, 0, 0, 0, 0);
    let (now, read_now) = clock(start);
    let env = Env {
        cache: Some(cache.clone()),
        now: read_now,
        ..allowed()
    };
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(req.headers.get("X-Api-Key"), Some("tok-2"));
    let entries = cache.0.lock().unwrap().clone();
    assert_eq!(entries.len(), 1, "ttl should cache");
    let (key, entry) = entries.iter().next().unwrap();
    assert_eq!(key, &format!("$exec:{}", hash_key("echo\x00tok-2")));
    assert_eq!(
        entry,
        r#"{"access_token":"tok-2","expires_at":"2026-09-12T09:00:00Z"}"#
    );
    // Within the ttl the cached token is used, whatever the command
    // would print now; after it the command runs again.
    cache
        .set(key, &entry.replace("tok-2", "tok-cached"))
        .unwrap();
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(req.headers.get("X-Api-Key"), Some("tok-cached"));
    *now.lock().unwrap() = start + Duration::from_secs(2 * 3600);
    apply(&bg(), &s, &mut req, b"", &env).unwrap();
    assert_eq!(req.headers.get("X-Api-Key"), Some("tok-2"));

    let s = parse("exec definitely-not-a-command-xyz").unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &allowed()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "@auth exec definitely-not-a-command-xyz: exec: \"definitely-not-a-command-xyz\": executable file not found in $PATH: "
    );
    let s = parse("exec sh -c \"echo denied >&2; exit 3\"").unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &allowed()).unwrap_err();
    assert_eq!(err.to_string(), "@auth exec sh: exit status 3: denied");
    let s = parse("exec true").unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &allowed()).unwrap_err();
    assert_eq!(err.to_string(), "@auth exec true: command printed nothing");
    let s = parse("exec echo x ttl=soon").unwrap();
    let err = apply(&bg(), &s, &mut req, b"", &allowed()).unwrap_err();
    assert_eq!(err.to_string(), "@auth exec: bad ttl \"soon\"");
}

#[cfg(unix)]
#[test]
fn exec_cache_save_error_returned() {
    let s = parse("exec echo tok ttl=1h").unwrap();
    let mut req = get("http://x/");
    let env = Env {
        allow_exec: true,
        cache: Some(Arc::new(ErrCache("save failed"))),
        ..env_with(&vars(&[("PATH", SEARCH_PATH)]))
    };
    let err = apply(&bg(), &s, &mut req, b"", &env).unwrap_err();
    assert_eq!(err, Error::Cache("save failed".to_string()));
}

#[test]
fn parse_keeps_equals_in_bearer_and_basic_values() {
    let s = parse("bearer abc==").unwrap();
    assert!(
        s.args == ["abc=="] && s.options.is_empty(),
        "bearer with padding: {s:?}"
    );
    let s = parse("basic admin p=ss").unwrap();
    assert!(
        s.args.len() == 2 && s.args[1] == "p=ss",
        "basic with = in the password: {s:?}"
    );
    parse("aws region=eu-west-2").expect("aws still takes options");
}

// Signatures, cache entries and messages pinned to known-good values.
#[test]
fn pinned_outputs() {
    use crate::digest::{digest_response, parse_digest_challenges};
    use crate::oauth2::{encode_token, CachedToken};

    let creds = AwsCredentials {
        access_key_id: "AKID".to_string(),
        secret_access_key: "secret".to_string(),
        session_token: "tok".to_string(),
    };
    let at = Time::from_civil(2026, 1, 2, 3, 4, 5, 0, 0);
    let prefix = |svc: &str| {
        format!("AWS4-HMAC-SHA256 Credential=AKID/20260102/eu-west-2/{svc}/aws4_request, SignedHeaders=content-length;host;x-amz-content-sha256;x-amz-date;x-amz-security-token;x-custom, Signature=")
    };
    let cases = [
        (
            "https://example.amazonaws.com:8443/a%20b/c!d?z=1&a=b%20c&a=a+a&k",
            "d705e27065b82bf6e078c4e12bb5ea06945d8965cb48905ac9f27d4523a20e5d",
            "0822b742800846e91f92fb7641a61b74a717eea5d96ceb10124fd7c3189f06b0",
        ),
        (
            "https://example.amazonaws.com/x/y~z/%E2%82%AC?X-Amz-Signature=q&b=%zz&c=1;2&d==",
            "705e1fe43b605755c90ce3c2a864c3fff98143ee7fa8a65aaa0490a6e0de3581",
            "3ef124b3c3d3282e0b67f104ea762352171885a014b13ee3d801507bee651393",
        ),
        (
            "https://example.amazonaws.com",
            "50a5f56c966c9933832cb1c60aa7a1c8ef807397c029efd2cb5fbc2fd09c34b2",
            "72ae632711daa7121c96f8cbcfea6fd448eecf54affe50a6365e892392cf88db",
        ),
    ];
    for (url, execute_api, s3) in cases {
        for (svc, want) in [("execute-api", execute_api), ("s3", s3)] {
            let mut req = Request::post(url, "hi")
                .header("X-Custom", "  a   b ")
                .header("X-Custom", "c")
                .header("User-Agent", "apic/1");
            sign_sig_v4(&mut req, b"hi", &creds, svc, "eu-west-2", at);
            assert_eq!(authz(&req), format!("{}{}", prefix(svc), want), "{url}");
        }
    }

    let entry = CachedToken {
        access_token: "a<b>&\"c".to_string(),
        refresh_token: "r".to_string(),
        expires_at: at + Duration::from_millis(1500),
        ..CachedToken::default()
    };
    assert_eq!(
        encode_token(&entry),
        r#"{"access_token":"a\u003cb\u003e\u0026\"c","refresh_token":"r","expires_at":"2026-01-02T03:04:06.5Z"}"#
    );
    assert_eq!(
        describe_cached(
            r#"{"ACCESS_TOKEN":"x","expires_at":"2026-01-02T05:04:05Z"}"#,
            at
        ),
        "token, expires in 2h0m0s"
    );
    assert_eq!(
        describe_cached(r#"{"access_token":"x","expires_at":null}"#, at),
        "expired"
    );
    assert_eq!(describe_cached("[]", at), "(unreadable)");
    assert_eq!(describe_cached("null", at), "expired");

    let c = parse_digest_challenges(&[
        r#"Digest realm="r\"q", nonce="n", qop="auth-int, auth", algorithm=sha-256-sess, opaque="o""#,
    ])
    .remove(0);
    assert_eq!(
        digest_response(&c, "ü", "p", "POST", "/a%20b?x=1", b"body", "cn", 255).unwrap(),
        r#"Digest username="ü", realm="r\"q", nonce="n", uri="/a%20b?x=1", response="a8a10ef76f7f3810c32c44107307de3d772079af10d177b0d4b31e8b7073c0bc", algorithm=SHA-256-SESS, qop=auth-int, nc=000000ff, cnonce="cn", opaque="o""#
    );

    let msg = |raw: &str| parse(raw).unwrap_err().to_string();
    assert_eq!(
        msg("oauth2 tokenUrl=t clientId=c grant=authorization_code authUrl=a redirectUrl=http://LOCALHOST:1/x"),
        "@auth oauth2: redirectUrl \"http://LOCALHOST:1/x\" must point at localhost, where apic listens for the sign-in"
    );
    assert_eq!(
        msg("apikey k header=A query=b"),
        "@auth apikey: header= and query= are alternatives; give one"
    );
    assert_eq!(msg("exec"), "@auth exec needs a command");
    assert_eq!(msg("none x"), "@auth none takes no arguments");
    assert_eq!(
        msg("aws x"),
        "@auth aws takes only key=value options (service, region, profile)"
    );
    assert_eq!(msg("oauth2 x"), "@auth oauth2 takes only key=value options");
    parse("oauth2 tokenUrl=t clientId=c grant=authorization_code authUrl=a redirectPort=+80")
        .unwrap();
    assert_eq!(
        msg("oauth2 tokenUrl=t clientId=c grant=authorization_code authUrl=a redirectUrl=http://127.0.0.1:99999/"),
        "@auth oauth2: redirectUrl \"http://127.0.0.1:99999/\" needs a port"
    );
}
