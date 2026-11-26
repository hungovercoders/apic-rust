use serde_json::{json, Map, Value};

use crate::jetbrains::from_jetbrains;

fn identity(s: &str) -> Result<String, String> {
    Ok(s.to_string())
}

fn fields(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

// Each JetBrains grant maps onto the oauth2 options `# @auth oauth2` takes.
#[test]
fn from_jetbrains_maps_each_grant() {
    let cases = [
        (
            json!({"Type": "OAuth2", "Grant Type": "Client Credentials", "Token URL": "https://idp/token", "Client ID": "{{clientId}}", "Client Secret": "{{secret}}", "Scope": "read write", "Client Credentials": "basic"}),
            "oauth2 clientAuth=basic clientId={{clientId}} clientSecret=*** grant=client_credentials scope=read write tokenUrl=https://idp/token",
        ),
        (
            json!({"Type": "OAuth2", "Grant Type": "Password", "Token URL": "t", "Client ID": "c", "Username": "u", "Password": "p", "Client Credentials": "in body"}),
            "oauth2 clientId=c grant=password password=*** tokenUrl=t username=u",
        ),
        (
            json!({"Type": "OAuth2", "Grant Type": "Device Authorization", "Token URL": "t", "Client ID": "c", "Device Auth URL": "d"}),
            "oauth2 clientId=c deviceUrl=d grant=device_code tokenUrl=t",
        ),
        (
            json!({"type": "oauth2", "grant type": "authorization code", "token url": "t", "client id": "c", "Auth URL": "a", "Redirect URL": "http://localhost:9876/cb", "PKCE": true,
                "Custom Request Parameters": {"audience": {"Value": "api://x", "Use": "Everywhere"}}}),
            "oauth2 audience=api://x authUrl=a clientId=c grant=authorization_code redirectUrl=http://localhost:9876/cb tokenUrl=t",
        ),
    ];
    for (input, want) in cases {
        let jb = from_jetbrains(&fields(input.clone()), identity)
            .unwrap_or_else(|e| panic!("{input}: {e}"));
        assert_eq!(jb.spec.raw, want);
        assert!(jb.ignored.is_empty(), "ignored {:?}", jb.ignored);
    }
}

#[test]
fn from_jetbrains_refuses_and_reports() {
    let cases = [
        (
            json!({"Type": "OAuth2", "Grant Type": "Implicit", "Auth URL": "a", "Client ID": "c"}),
            "Implicit grant is not supported",
        ),
        (json!({"Type": "Basic"}), r#"Type "Basic" is not supported"#),
        (json!({"Type": "OAuth2"}), r#""Grant Type" is missing"#),
        (
            json!({"Type": "OAuth2", "Grant Type": "Client Credentials", "Client ID": "c"}),
            r#"needs "Token URL""#,
        ),
        (
            json!({"Type": "OAuth2", "Grant Type": "Password", "Token URL": "t", "Client ID": "c"}),
            r#"needs "Username" and "Password""#,
        ),
        (
            json!({"Type": "OAuth2", "Grant Type": "Authorization Code", "Token URL": "t", "Client ID": "c", "Auth URL": "a", "Redirect URL": "https://example.com/cb"}),
            "must be an http:// URL on this machine",
        ),
        (
            json!({"Type": "OAuth2", "Grant Type": "Client Credentials", "Token URL": "t", "Client ID": "c", "Client Credentials": "sideways"}),
            "use basic or in body",
        ),
        (
            json!({"Type": "OAuth2", "Grant Type": "Client Credentials", "Token URL": 7, "Client ID": "c"}),
            r#""Token URL" must be a string"#,
        ),
    ];
    for (input, want) in cases {
        match from_jetbrains(&fields(input.clone()), identity) {
            Err(e) if e.to_string().contains(want) => {}
            other => panic!("{input}: got {other:?}, want {want:?}"),
        }
    }
    // The whole message is in the file's own words.
    let err = from_jetbrains(
        &fields(
            json!({"Type": "OAuth2", "Grant Type": "Password", "Token URL": "t", "Client ID": "c"}),
        ),
        identity,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"the configuration "Grant Type" password needs "Username" and "Password""#
    );

    let jb = from_jetbrains(
        &fields(json!({"Type": "OAuth2", "Grant Type": "Client Credentials", "Token URL": "t", "Client ID": "c", "Use ID Token": true,
            "Revoke URL": "r", "Custom Request Parameters": {"resource": "x"}})),
        identity,
    )
    .unwrap();
    assert!(jb.use_id_token);
    assert_eq!(
        jb.ignored.join(","),
        "Custom Request Parameters.resource,Revoke URL"
    );
    // What render refuses is the error.
    let err = from_jetbrains(
        &fields(json!({"Type": "OAuth2", "Grant Type": "Client Credentials", "Token URL": "{{t}}", "Client ID": "c"})),
        |s| {
            if s.contains("{{") {
                Err(format!("{s}: not defined"))
            } else {
                Ok(s.to_string())
            }
        },
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "{{t}}: not defined");
}
