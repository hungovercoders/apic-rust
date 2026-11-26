//! Replays `testdata/yaml_reference.json`: `apic.yaml` contents with the
//! configuration, or the error, each one has always produced.

use apic_project::{Config, TlsHost};
use serde_json::{json, Value};

fn host(h: &TlsHost) -> Value {
    json!({
        "CAFile": h.ca_file,
        "CertFile": h.cert_file,
        "KeyFile": h.key_file,
        "VerifyHost": h.verify_host,
    })
}

/// The configuration in the shape the reference file records it.
fn shape(c: &Config) -> Value {
    let hosts: serde_json::Map<String, Value> = c
        .tls
        .hosts
        .iter()
        .map(|(name, h)| (name.clone(), host(h)))
        .collect();
    json!({
        "Env": c.env,
        "Dir": c.dir,
        "Timeout": c.timeout,
        "Retry": c.retry,
        "Cookies": c.cookies,
        "History": c.history,
        "Proxy": c.proxy,
        "NoProxy": c.no_proxy,
        "MaxBodyBytes": c.max_body_bytes,
        "Auth": {"Default": c.auth.default, "AllowExec": c.auth.allow_exec},
        "Test": {"Paths": c.test.paths},
        "TLS": {
            "CAFile": c.tls.ca_file,
            "CertFile": c.tls.cert_file,
            "KeyFile": c.tls.key_file,
            "VerifyHost": c.tls.verify_host,
            "Hosts": hosts,
        },
    })
}

/// The reference tells a list or mapping that was never set (null) from
/// an empty one; a `Config` does not.
fn normalise(mut want: Value) -> Value {
    for (section, key, empty) in [
        ("", "NoProxy", json!([])),
        ("Test", "Paths", json!([])),
        ("TLS", "Hosts", json!({})),
    ] {
        let slot = if section.is_empty() {
            &mut want[key]
        } else {
            &mut want[section][key]
        };
        if slot.is_null() {
            *slot = empty;
        }
    }
    want
}

#[test]
fn yaml_reference() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/yaml_reference.json");
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(cases.len() > 250, "only {} cases", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let input = case["input"].as_str().unwrap();
        let got = Config::from_yaml(input.as_bytes());
        let ok = match (&got, case.get("error")) {
            (Err(got), Some(want)) => Some(got.as_str()) == want.as_str(),
            (Ok(got), None) => shape(got) == normalise(case["config"].clone()),
            _ => false,
        };
        if !ok {
            let got = match &got {
                Ok(c) => shape(c).to_string(),
                Err(e) => format!("error {e:?}"),
            };
            let want = match case.get("error") {
                Some(e) => format!("error {e}"),
                None => normalise(case["config"].clone()).to_string(),
            };
            failures.push(format!("{input:?}\n   got: {got}\n  want: {want}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
