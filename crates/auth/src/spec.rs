use std::collections::BTreeMap;

use apic_gocompat::quote;

use crate::authcode::redirect_target;
use crate::error::Error;

/// A parsed `# @auth` value. `args` holds positional arguments and
/// `options` holds key=value arguments; both may still contain
/// {{placeholders}} until the runner renders them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    /// The auth type, lower case: one of [`TYPES`].
    pub r#type: String,
    pub args: Vec<String>,
    pub options: BTreeMap<String, String>,
    pub raw: String,
}

/// The supported auth types with a one-line description, by name.
pub const TYPES: [(&str, &str); 8] = [
    ("apikey", "apikey <key> [header=X-Api-Key] [query=name] [prefix=..]: send the key as a header (default X-Api-Key, no prefix) or a query parameter"),
    ("aws", "aws [service=execute-api] [region=..] [profile=..]: AWS Signature V4 using the SDK credential chain"),
    ("basic", "basic <user> <password>: HTTP basic auth"),
    ("bearer", "bearer <token>: Authorization: Bearer <token>"),
    ("digest", "digest <user> <password>: HTTP digest auth (RFC 7616), answering the server's challenge"),
    ("exec", "exec <command> [args..] [header=Authorization] [prefix=Bearer] [ttl=10m]: use a command's stdout as the token (needs auth.allowExec in apic.yaml)"),
    ("none", "send no credentials (overrides a project default)"),
    ("oauth2", "oauth2 tokenUrl=.. clientId=.. [clientSecret=..] [grant=client_credentials|password|device_code|authorization_code] [scope=..] [username=..] [password=..] [audience=..] [deviceUrl=..] [authUrl=..] [redirectPort=..] [redirectUrl=http://localhost:<port>/<path>] [clientAuth=body|basic]"),
];

/// The description of an auth type, or `None` when there is no such type.
pub fn type_description(name: &str) -> Option<&'static str> {
    TYPES.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// The names of the supported auth types, sorted.
pub fn type_names() -> Vec<&'static str> {
    TYPES.iter().map(|(n, _)| *n).collect()
}

/// Splits an auth spec such as `basic {{user}} {{password}}` or
/// `aws service=execute-api region=eu-west-2`.
pub fn parse(raw: &str) -> Result<Spec, Error> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::msg(format!(
            "@auth needs a type: one of {}",
            type_names().join(", ")
        )));
    }
    let fields = split(raw).map_err(|e| Error::msg(format!("@auth {}: {}", quote(raw), e)))?;
    let mut s = Spec {
        r#type: fields[0].to_lowercase(),
        args: Vec::new(),
        options: BTreeMap::new(),
        raw: raw.to_string(),
    };
    if type_description(&s.r#type).is_none() {
        return Err(Error::msg(format!(
            "@auth: unknown type {} (one of {})",
            quote(&fields[0]),
            type_names().join(", ")
        )));
    }
    // Only aws, oauth2, exec and apikey take key=value options: a bearer
    // token or a basic password containing '=' (base64 padding, say) is a
    // value. exec and apikey keep an unknown k=v positional for the same
    // reason: a command word or an API key may contain '='.
    let takes_options = matches!(s.r#type.as_str(), "aws" | "oauth2" | "exec" | "apikey");
    for f in &fields[1..] {
        if let Some((k, v)) = f.split_once('=') {
            if takes_options
                && (s.r#type != "exec" || is_exec_option(k))
                && (s.r#type != "apikey" || is_api_key_option(k))
                && is_ident(k)
            {
                s.options.insert(k.to_string(), v.to_string());
                continue;
            }
        }
        s.args.push(f.clone());
    }
    s.check()?;
    Ok(s)
}

impl Spec {
    /// The value of an option, empty when it is not set.
    pub fn option(&self, key: &str) -> &str {
        self.options.get(key).map(String::as_str).unwrap_or("")
    }

    pub(crate) fn check(&self) -> Result<(), Error> {
        match self.r#type.as_str() {
            "none" => {
                if !self.args.is_empty() || !self.options.is_empty() {
                    return Err(Error::msg("@auth none takes no arguments"));
                }
            }
            "bearer" => {
                if self.args.len() != 1 {
                    return Err(Error::msg(
                        "@auth bearer needs exactly one argument: the token",
                    ));
                }
            }
            "basic" | "digest" => {
                if self.args.len() != 2 {
                    return Err(Error::msg(format!(
                        "@auth {} needs two arguments: user and password",
                        self.r#type
                    )));
                }
            }
            "apikey" => return self.check_api_key(),
            "aws" => return self.check_aws(),
            "oauth2" => return self.check_oauth2(),
            "exec" if self.args.is_empty() => {
                return Err(Error::msg("@auth exec needs a command"));
            }
            _ => {}
        }
        Ok(())
    }

    fn check_api_key(&self) -> Result<(), Error> {
        if self.args.len() != 1 {
            return Err(Error::msg("@auth apikey needs exactly one argument: the key (then header=, query= or prefix=)"));
        }
        if !self.option("header").is_empty() && !self.option("query").is_empty() {
            return Err(Error::msg(
                "@auth apikey: header= and query= are alternatives; give one",
            ));
        }
        Ok(())
    }

    fn check_aws(&self) -> Result<(), Error> {
        if !self.args.is_empty() {
            return Err(Error::msg(
                "@auth aws takes only key=value options (service, region, profile)",
            ));
        }
        for k in self.options.keys() {
            if k != "service" && k != "region" && k != "profile" {
                return Err(Error::msg(format!(
                    "@auth aws: unknown option {}",
                    quote(k)
                )));
            }
        }
        Ok(())
    }

    fn check_oauth2(&self) -> Result<(), Error> {
        if !self.args.is_empty() {
            return Err(Error::msg("@auth oauth2 takes only key=value options"));
        }
        if self.option("tokenUrl").is_empty() {
            return Err(Error::msg("@auth oauth2 needs tokenUrl="));
        }
        if self.option("clientId").is_empty() {
            return Err(Error::msg("@auth oauth2 needs clientId="));
        }
        self.check_grant()?;
        for k in self.options.keys() {
            if !OAUTH2_OPTIONS.contains(&k.as_str()) {
                return Err(Error::msg(format!(
                    "@auth oauth2: unknown option {}",
                    quote(k)
                )));
            }
        }
        let ca = self.option("clientAuth");
        if !ca.is_empty() && ca != "body" && ca != "basic" {
            return Err(Error::msg("@auth oauth2: clientAuth must be body or basic"));
        }
        Ok(())
    }

    /// Checks the options the oauth2 grant needs.
    fn check_grant(&self) -> Result<(), Error> {
        match self.grant() {
            "client_credentials" => {}
            "password" => {
                if self.option("username").is_empty() || self.option("password").is_empty() {
                    return Err(Error::msg(
                        "@auth oauth2 grant=password needs username= and password=",
                    ));
                }
            }
            "device_code" => {
                if self.option("deviceUrl").is_empty() {
                    return Err(Error::msg(
                        "@auth oauth2 grant=device_code needs deviceUrl=",
                    ));
                }
            }
            "authorization_code" => return self.check_authorization_code(),
            g => {
                return Err(Error::msg(format!(
                    "@auth oauth2: unknown grant {} (client_credentials, password, device_code or authorization_code)",
                    quote(g)
                )))
            }
        }
        Ok(())
    }

    fn check_authorization_code(&self) -> Result<(), Error> {
        if self.option("authUrl").is_empty() {
            return Err(Error::msg(
                "@auth oauth2 grant=authorization_code needs authUrl=",
            ));
        }
        let p = self.option("redirectPort");
        if !p.is_empty() && !matches!(atoi(p), Some(0..=65535)) {
            return Err(Error::msg(format!(
                "@auth oauth2: redirectPort must be a port number, not {}",
                quote(p)
            )));
        }
        let u = self.option("redirectUrl");
        if !u.is_empty() {
            redirect_target(u).map_err(|e| Error::msg(format!("@auth oauth2: {e}")))?;
            if !self.option("redirectPort").is_empty() {
                return Err(Error::msg(
                    "@auth oauth2: redirectUrl and redirectPort are alternatives; give one",
                ));
            }
        }
        Ok(())
    }

    /// The oauth2 grant: the `grant` option, or `client_credentials`.
    pub(crate) fn grant(&self) -> &str {
        match self.option("grant") {
            "" => "client_credentials",
            g => g,
        }
    }

    /// Reports where an apikey spec puts the key: the header name (default
    /// X-Api-Key) or the query parameter, for describe and the curl
    /// export. One of the two is empty.
    pub fn api_key_placement(&self) -> (String, String) {
        let q = self.option("query");
        if !q.is_empty() {
            return (String::new(), q.to_string());
        }
        let h = self.option("header");
        if !h.is_empty() {
            return (h.to_string(), String::new());
        }
        ("X-Api-Key".to_string(), String::new())
    }

    /// Returns a copy of the spec with `f` applied to every argument and
    /// option value (used to substitute {{placeholders}}): the arguments
    /// in order, then the options by name, so which placeholder a render
    /// fails on does not change from run to run.
    pub fn render<E>(&self, mut f: impl FnMut(&str) -> Result<String, E>) -> Result<Spec, E> {
        let mut out = Spec {
            r#type: self.r#type.clone(),
            args: Vec::with_capacity(self.args.len()),
            options: BTreeMap::new(),
            raw: self.raw.clone(),
        };
        for a in &self.args {
            out.args.push(f(a)?);
        }
        for (k, v) in &self.options {
            out.options.insert(k.clone(), f(v)?);
        }
        Ok(out)
    }

    /// Returns every string in the spec that may contain placeholders:
    /// the arguments in order, then the options by name, so describe lists
    /// their variables the same way every run.
    pub fn texts(&self) -> Vec<String> {
        self.args
            .iter()
            .chain(self.options.values())
            .cloned()
            .collect()
    }
}

/// The keys `@auth oauth2` accepts.
const OAUTH2_OPTIONS: [&str; 13] = [
    "tokenUrl",
    "clientId",
    "clientSecret",
    "grant",
    "scope",
    "username",
    "password",
    "audience",
    "deviceUrl",
    "authUrl",
    "redirectPort",
    "redirectUrl",
    "clientAuth",
];

fn is_exec_option(k: &str) -> bool {
    k == "header" || k == "prefix" || k == "ttl"
}

fn is_api_key_option(k: &str) -> bool {
    k == "header" || k == "query" || k == "prefix"
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Reads a decimal integer with an optional sign, as a port option is
/// written.
pub(crate) fn atoi(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok()
}

/// Tokenises on whitespace, honouring single and double quotes so that
/// values with spaces can be written as key="a b".
fn split(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut in_word = false;
    let mut quote_byte = 0u8;
    let mut flush = |cur: &mut Vec<u8>| {
        out.push(String::from_utf8_lossy(cur).into_owned());
        cur.clear();
    };
    for &c in s.as_bytes() {
        if quote_byte != 0 {
            if c == quote_byte {
                quote_byte = 0;
            } else {
                cur.push(c);
            }
        } else if c == b'"' || c == b'\'' {
            quote_byte = c;
            in_word = true;
        } else if c == b' ' || c == b'\t' {
            if in_word {
                flush(&mut cur);
                in_word = false;
            }
        } else {
            cur.push(c);
            in_word = true;
        }
    }
    if quote_byte != 0 {
        return Err("unterminated quote".to_string());
    }
    if in_word {
        flush(&mut cur);
    }
    Ok(out)
}
