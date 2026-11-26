use std::collections::BTreeMap;
use std::fmt::Display;

use apic_gocompat::quote;
use serde_json::{Map, Value};

use crate::error::Error;
use crate::spec::Spec;

/// A JetBrains HTTP Client `Security.Auth` configuration mapped onto an
/// oauth2 spec, so {{$auth.token("name")}} runs the same grant and shares
/// the same token cache as `# @auth oauth2`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JetBrains {
    pub spec: Spec,
    /// The configuration's "Use ID Token": $auth.token then gives the ID
    /// token rather than the access token.
    pub use_id_token: bool,
    /// Names the fields apic does not act on, sorted; validate warns about
    /// each.
    pub ignored: Vec<String>,
}

/// Maps each JetBrains field onto an oauth2 option.
const JETBRAINS_OPTIONS: [(&str, &str); 9] = [
    ("token url", "tokenUrl"),
    ("auth url", "authUrl"),
    ("device auth url", "deviceUrl"),
    ("client id", "clientId"),
    ("client secret", "clientSecret"),
    ("scope", "scope"),
    ("username", "username"),
    ("password", "password"),
    ("redirect url", "redirectUrl"),
];

/// The JetBrains "Grant Type" values and apic's names.
const JETBRAINS_GRANTS: [(&str, &str); 4] = [
    ("client credentials", "client_credentials"),
    ("password", "password"),
    ("device authorization", "device_code"),
    ("authorization code", "authorization_code"),
];

/// Fields apic reads but that need no option: the type and grant, and
/// behaviour apic has anyway (it always uses PKCE, and fetches a token
/// when a request needs one).
const JETBRAINS_KNOWN: [&str; 7] = [
    "type",
    "grant type",
    "client credentials",
    "custom request parameters",
    "use id token",
    "pkce",
    "acquire automatically",
];

fn lookup(table: &[(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// A field by name, whatever its case.
fn get<'a>(fields: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    let want = key.to_lowercase();
    fields
        .iter()
        .find(|(k, _)| k.to_lowercase() == want)
        .map(|(_, v)| v)
}

/// Maps a Security.Auth entry. `render` substitutes the {{placeholders}} a
/// string field may hold; the result is checked like a written `# @auth
/// oauth2`. The Implicit grant is refused: it is deprecated by OAuth 2.1,
/// and Authorization Code with PKCE replaces it.
pub fn from_jetbrains<E: Display>(
    fields: &Map<String, Value>,
    render: impl Fn(&str) -> Result<String, E>,
) -> Result<JetBrains, Error> {
    let render = |s: &str| render(s).map_err(|e| Error::msg(e.to_string()));
    let str_field = |key: &str| -> Result<String, Error> {
        match get(fields, key) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(s)) => render(s),
            Some(_) => Err(Error::msg(format!("{} must be a string", quote(key)))),
        }
    };
    let mut options: BTreeMap<String, String> = BTreeMap::new();
    let mut ignored = Vec::new();

    let typ = str_field("Type")?;
    if !typ.eq_ignore_ascii_case("OAuth2") {
        return Err(Error::msg(format!(
            "the Type {} is not supported: apic reads OAuth2 configurations",
            quote(&typ)
        )));
    }
    let grant = str_field("Grant Type")?;
    match grant.to_lowercase().as_str() {
        "implicit" => {
            return Err(Error::msg("the Implicit grant is not supported (OAuth 2.1 drops it); use Authorization Code, which apic runs with PKCE"))
        }
        "" => {
            return Err(Error::msg("\"Grant Type\" is missing: Client Credentials, Password, Device Authorization or Authorization Code"))
        }
        g => match lookup(&JETBRAINS_GRANTS, g) {
            Some(name) => {
                options.insert("grant".to_string(), name.to_string());
            }
            None => {
                return Err(Error::msg(format!(
                    "the Grant Type {} is not supported: Client Credentials, Password, Device Authorization or Authorization Code",
                    quote(&grant)
                )))
            }
        },
    }
    for k in fields.keys() {
        let lower = k.to_lowercase();
        let Some(opt) = lookup(&JETBRAINS_OPTIONS, &lower) else {
            if !JETBRAINS_KNOWN.contains(&lower.as_str()) {
                ignored.push(k.clone());
            }
            continue;
        };
        let v = str_field(k)?;
        if !v.is_empty() {
            options.insert(opt.to_string(), v);
        }
    }
    let ca = str_field("Client Credentials")?;
    if ca.eq_ignore_ascii_case("basic") {
        options.insert("clientAuth".to_string(), "basic".to_string());
    } else if !(ca.is_empty()
        || ca.eq_ignore_ascii_case("in body")
        || ca.eq_ignore_ascii_case("none"))
    {
        return Err(Error::msg(format!(
            "\"Client Credentials\" {}: use basic or in body",
            quote(&ca)
        )));
    }
    let mut use_id_token = false;
    if let Some(v) = get(fields, "Use ID Token") {
        let Value::Bool(b) = v else {
            return Err(Error::msg("\"Use ID Token\" must be true or false"));
        };
        use_id_token = *b;
    }
    if let Some(v) = get(fields, "Custom Request Parameters") {
        let Value::Object(params) = v else {
            return Err(Error::msg(
                "\"Custom Request Parameters\" must be an object",
            ));
        };
        for (name, p) in params {
            // A parameter is a value, or {"Value": ..., "Use": ...}.
            let p = match p {
                Value::Object(obj) => obj.get("Value").unwrap_or(&Value::Null),
                p => p,
            };
            match p {
                Value::String(s) if name.eq_ignore_ascii_case("audience") => {
                    options.insert("audience".to_string(), render(s)?);
                }
                _ => ignored.push(format!("Custom Request Parameters.{name}")),
            }
        }
    }
    ignored.sort();
    let mut spec = Spec {
        r#type: "oauth2".to_string(),
        args: Vec::new(),
        options,
        raw: String::new(),
    };
    spec.raw = describe(&spec);
    if let Err(err) = spec.check() {
        // Say it in the file's own words: "Token URL", not tokenUrl=.
        let mut msg = err
            .to_string()
            .replacen("@auth oauth2", "the configuration", 1);
        for (field, opt) in JETBRAINS_OPTIONS {
            msg = msg.replace(&format!("{opt}="), &quote(&jetbrains_field(field)));
        }
        return Err(Error::msg(msg.replace("grant=", "\"Grant Type\" ")));
    }
    Ok(JetBrains {
        spec,
        use_id_token,
        ignored,
    })
}

/// Renders an oauth2 spec as `oauth2 key=value ...` without its secrets,
/// for describe and apic env.
fn describe(s: &Spec) -> String {
    let mut parts = vec![s.r#type.clone()];
    for (k, v) in &s.options {
        let v = if k == "clientSecret" || k == "password" {
            "***"
        } else {
            v.as_str()
        };
        parts.push(format!("{k}={v}"));
    }
    parts.join(" ")
}

/// A lower-cased field name in JetBrains' own spelling.
fn jetbrains_field(lower: &str) -> String {
    lower
        .split_whitespace()
        .map(|w| match w {
            "url" | "id" => w.to_uppercase(),
            w => {
                let mut chars = w.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
