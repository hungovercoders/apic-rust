use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use apic_gocompat::{check_json, format_float_json, json_string, path_error, quote};
use serde_json::Value;

use crate::dotenv;

pub const PUBLIC_FILE: &str = "http-client.env.json";
pub const PRIVATE_FILE: &str = "http-client.private.env.json";
pub const DOT_ENV_FILE: &str = ".env";
const SHARED_KEY: &str = "$shared";

/// The fields of one `Security.Auth` entry, as written.
pub type AuthFields = serde_json::Map<String, Value>;

type VarsByEnv = BTreeMap<String, BTreeMap<String, String>>;
type AuthByEnv = BTreeMap<String, BTreeMap<String, AuthFields>>;

/// An env file that could not be read; the message names the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Everything loaded from the env files of a project.
#[derive(Clone, Debug, Default)]
pub struct Environments {
    /// Environment name to variables.
    pub public: VarsByEnv,
    pub private: VarsByEnv,
    pub dot_env: BTreeMap<String, String>,
    /// The files that were present.
    pub found: Vec<String>,
    ssl: BTreeMap<String, SslConfig>,
    /// Each file's Security.Auth blocks: file, then environment, then name.
    auth: BTreeMap<String, AuthByEnv>,
}

/// One entry of a JetBrains `Security.Auth` block:
///
/// ```json
/// "Security": {
///   "Auth": {
///     "my-api": {"Type": "OAuth2", "Grant Type": "Client Credentials",
///                "Token URL": "https://idp/token", "Client ID": "{{clientId}}"}
///   }
/// }
/// ```
///
/// Requests use it as `{{$auth.token("my-api")}}`. Fields are as written,
/// so strings may hold {{placeholders}}; the private file's fields win
/// over the public file's, and an environment's over `$shared`'s.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuthConfig {
    pub name: String,
    pub fields: AuthFields,
    /// The env files that declare it.
    pub files: Vec<String>,
}

/// The JetBrains `SSLConfiguration` block of an environment:
///
/// ```json
/// "SSLConfiguration": {
///   "clientCertificate": {"path": "certs/client.pem", "keyPath": "certs/client-key.pem"},
///   "hasCertificatePassphrase": false,
///   "verifyHostCertificate": true
/// }
/// ```
///
/// It is read from either env file (the private one wins) and is not a
/// variable. Paths are relative to the project root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SslConfig {
    pub cert_file: String,
    pub key_file: String,
    pub has_passphrase: bool,
    pub verify_host: Option<bool>,
}

/// Reads the env files in `root`. Missing files are fine.
pub fn load(root: &Path) -> Result<Environments, Error> {
    let mut e = Environments::default();
    for file in [PUBLIC_FILE, PRIVATE_FILE] {
        let mut auth = AuthByEnv::new();
        let vars = load_json(&root.join(file), file, &mut e.found, &mut e.ssl, &mut auth)?;
        if file == PUBLIC_FILE {
            e.public = vars;
        } else {
            e.private = vars;
        }
        e.auth.insert(file.to_string(), auth);
    }
    let dot = root.join(DOT_ENV_FILE);
    let read = |path: &Path| -> Result<Option<Vec<u8>>, String> {
        let mut f = match fs::File::open(path) {
            Ok(f) => f,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(path_error("open", path, &err)),
        };
        let mut data = Vec::new();
        f.read_to_end(&mut data)
            .map_err(|err| path_error("read", path, &err))?;
        Ok(Some(data))
    };
    let wrap = |msg: String| Error(format!("{DOT_ENV_FILE}: {msg}"));
    if let Some(data) = read(&dot).map_err(wrap)? {
        e.dot_env = dotenv::parse(&data).map_err(wrap)?;
        e.found.push(DOT_ENV_FILE.to_string());
    }
    Ok(e)
}

impl Environments {
    /// The SSLConfiguration block for `env`, or `$shared`'s when the
    /// environment has none; None when neither file declares one.
    pub fn ssl(&self, env: &str) -> Option<&SslConfig> {
        if !env.is_empty() {
            if let Some(c) = self.ssl.get(env) {
                return Some(c);
            }
        }
        self.ssl.get(SHARED_KEY)
    }

    /// The environment names found in the JSON files, sorted.
    pub fn names(&self) -> Vec<String> {
        let set: BTreeSet<&String> = self.public.keys().chain(self.private.keys()).collect();
        set.into_iter()
            .filter(|n| *n != SHARED_KEY)
            .cloned()
            .collect()
    }

    /// Reports whether an environment is declared in either JSON file.
    pub fn has(&self, name: &str) -> bool {
        self.public.contains_key(name) || self.private.contains_key(name)
    }

    /// The public vars for `env` merged over `$shared`.
    pub fn public_vars(&self, env: &str) -> BTreeMap<String, String> {
        merge(self.public.get(SHARED_KEY), self.public.get(env))
    }

    /// The private vars for `env` merged over `$shared`.
    pub fn private_vars(&self, env: &str) -> BTreeMap<String, String> {
        merge(self.private.get(SHARED_KEY), self.private.get(env))
    }

    /// The Security.Auth configurations in effect for `env`, by name:
    /// `$shared`'s and the environment's from the public file, then the
    /// private file's over them field by field.
    pub fn auth(&self, env: &str) -> BTreeMap<String, AuthConfig> {
        let mut out: BTreeMap<String, AuthConfig> = BTreeMap::new();
        for file in [PUBLIC_FILE, PRIVATE_FILE] {
            let mut layers = vec![SHARED_KEY];
            if !env.is_empty() && env != SHARED_KEY {
                layers.push(env);
            }
            for layer in layers {
                let Some(entries) = self.auth.get(file).and_then(|envs| envs.get(layer)) else {
                    continue;
                };
                for (name, fields) in entries {
                    let c = out.entry(name.clone()).or_insert_with(|| AuthConfig {
                        name: name.clone(),
                        ..AuthConfig::default()
                    });
                    for (k, v) in fields {
                        c.fields.insert(k.clone(), v.clone());
                    }
                    if c.files.last().map(String::as_str) != Some(file) {
                        c.files.push(file.to_string());
                    }
                }
            }
        }
        out
    }

    /// Every Security.Auth name any environment declares, sorted.
    pub fn auth_names(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for envs in self.auth.values() {
            for by_name in envs.values() {
                set.extend(by_name.keys().cloned());
            }
        }
        set.into_iter().collect()
    }

    /// Every Security.Auth entry as each file and environment declares it,
    /// for validation: file, then environment, then name, then fields.
    pub fn all_auth(&self) -> &BTreeMap<String, AuthByEnv> {
        &self.auth
    }
}

fn merge(
    base: Option<&BTreeMap<String, String>>,
    over: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut out = base.cloned().unwrap_or_default();
    if let Some(over) = over {
        out.extend(over.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    out
}

const SSL_KEY: &str = "SSLConfiguration";
const SECURITY_KEY: &str = "Security";

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Reads one env file. An SSLConfiguration block is lifted out of the
/// variables into `ssl`, and a Security block's Auth entries into `auth`,
/// later files overriding earlier ones.
fn load_json(
    path: &Path,
    base: &str,
    found: &mut Vec<String>,
    ssl: &mut BTreeMap<String, SslConfig>,
    auth: &mut AuthByEnv,
) -> Result<VarsByEnv, Error> {
    let mut out = VarsByEnv::new();
    let data = match fs::File::open(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(err) => return Err(Error(path_error("open", path, &err))),
        Ok(mut f) => {
            let mut data = Vec::new();
            f.read_to_end(&mut data)
                .map_err(|err| Error(path_error("read", path, &err)))?;
            data
        }
    };
    let fail = |msg: String| Error(format!("{base}: {msg}"));
    check_json(&data).map_err(fail)?;
    let raw: Value = serde_json::from_slice(&data).map_err(|e| fail(format!("json: {e}")))?;
    let envs = match raw {
        Value::Null => serde_json::Map::new(),
        Value::Object(envs) => envs,
        other => {
            return Err(fail(format!(
            "json: cannot unmarshal {} into Go value of type map[string]map[string]interface {{}}",
            json_type(&other)
        )))
        }
    };
    for (env_name, vars) in envs {
        let vars = match vars {
            Value::Null => serde_json::Map::new(),
            Value::Object(vars) => vars,
            other => {
                return Err(fail(format!(
                    "json: cannot unmarshal {} into Go value of type map[string]interface {{}}",
                    json_type(&other)
                )))
            }
        };
        let mut m = BTreeMap::new();
        for (k, v) in vars {
            if k == SSL_KEY {
                let c = parse_ssl(&v).map_err(|e| fail(format!("{env_name}: {SSL_KEY}: {e}")))?;
                ssl.insert(env_name.clone(), c);
                continue;
            }
            if k == SECURITY_KEY {
                let entries = parse_security(v)
                    .map_err(|e| fail(format!("{env_name}: {SECURITY_KEY}: {e}")))?;
                auth.insert(env_name.clone(), entries);
                continue;
            }
            m.insert(k, stringify(&v));
        }
        out.insert(env_name, m);
    }
    found.push(base.to_string());
    Ok(out)
}

/// Reads a JetBrains SSLConfiguration value. clientCertificate is an
/// object with path and keyPath, or a bare path string.
fn parse_ssl(v: &Value) -> Result<SslConfig, String> {
    let Value::Object(obj) = v else {
        return Err("expected an object".to_string());
    };
    let mut c = SslConfig::default();
    let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    match obj.get("clientCertificate") {
        None | Some(Value::Null) => {}
        Some(Value::String(cert)) => c.cert_file = cert.clone(),
        Some(Value::Object(cert)) => {
            c.cert_file = text(cert.get("path"));
            c.key_file = text(cert.get("keyPath"));
            if c.cert_file.is_empty() {
                return Err("clientCertificate needs a path".to_string());
            }
        }
        Some(_) => {
            return Err(
                "clientCertificate must be a path or an object with path and keyPath".to_string(),
            )
        }
    }
    if let Some(Value::Bool(b)) = obj.get("hasCertificatePassphrase") {
        c.has_passphrase = *b;
    }
    if let Some(Value::Bool(b)) = obj.get("verifyHostCertificate") {
        c.verify_host = Some(*b);
    }
    Ok(c)
}

/// Reads a JetBrains Security block: `{"Auth": {"<name>": {...}}}`. Only
/// Auth is read.
fn parse_security(v: Value) -> Result<BTreeMap<String, AuthFields>, String> {
    let Value::Object(mut obj) = v else {
        return Err("expected an object".to_string());
    };
    let mut out = BTreeMap::new();
    match obj.remove("Auth") {
        None | Some(Value::Null) => {}
        Some(Value::Object(a)) => {
            for (name, fields) in a {
                let Value::Object(f) = fields else {
                    return Err(format!(
                        "auth configuration {}: expected an object",
                        quote(&name)
                    ));
                };
                out.insert(name, f);
            }
        }
        Some(_) => return Err("Auth: expected an object of named configurations".to_string()),
    }
    Ok(out)
}

/// A variable's value as the text that is substituted: a string as it is,
/// a number without an exponent, a boolean as its word, null as nothing,
/// and an array or object as compact JSON.
fn stringify(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        // Display never uses an exponent and prints the shortest digits
        // that read back the same: 8080, 1.5, 1000000000000000000000.
        Value::Number(n) => format!("{}", n.as_f64().unwrap_or(0.0)),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => {
            let mut out = String::new();
            marshal(other, &mut out);
            out
        }
    }
}

/// Writes a value as compact JSON: keys sorted, numbers as 64-bit floats,
/// and `<`, `>` and `&` escaped in strings.
fn marshal(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&format_float_json(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => out.push_str(&json_string(s, true)),
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                marshal(e, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            for (i, (k, e)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&json_string(k, true));
                out.push(':');
                marshal(e, out);
            }
            out.push('}');
        }
    }
}
