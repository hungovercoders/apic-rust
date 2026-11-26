use apic_auth::Spec;
use apic_gocompat::quote as go_quote;
use apic_httpfile::{Header, HTTP2};
use apic_runner::{FormPart, Resolved, TlsInfo};

use crate::curl::curl;
use crate::gocode::go_code;
use crate::httpie::httpie;
use crate::js::js;
use crate::powershell::powershell;
use crate::python::python;

/// The snippet languages, in the order the UI cycles them.
pub const LANGUAGES: [&str; 6] = ["curl", "httpie", "powershell", "python", "js", "go"];

/// Why a snippet could not be rendered: the language is not one of
/// [`LANGUAGES`]. It displays as the message a command prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Returns the snippet for `lang`.
pub fn render(lang: &str, r: &Resolved, redact: bool) -> Result<String, Error> {
    let m = model(r, redact);
    match lang {
        "curl" => Ok(curl(r, redact)),
        "httpie" => Ok(httpie(&m)),
        "powershell" => Ok(powershell(&m)),
        "python" => Ok(python(&m)),
        "js" => Ok(js(&m)),
        "go" => Ok(go_code(&m)),
        _ => Err(Error {
            message: format!(
                "unknown language {}: one of {}",
                go_quote(lang),
                LANGUAGES.join(", ")
            ),
        }),
    }
}

/// What every renderer but curl's works from: the request as it goes out,
/// with its credential described rather than applied.
pub(crate) struct Request {
    pub method: String,
    pub url: String,
    /// Without Content-Type when the parts set it.
    pub headers: Vec<Header>,
    pub body: String,
    pub parts: Vec<FormPart>,
    pub redact: bool,
    pub cred: Option<Credential>,
    pub tls: Option<TlsInfo>,
    /// The proxy URL, "" for none.
    pub proxy: String,
    /// `apic_httpfile::HTTP1`, `apic_httpfile::HTTP2` or "".
    pub http: String,
}

/// A credential value: written literally, or read from an environment
/// variable.
#[derive(Clone, Debug, Default)]
pub(crate) struct Secret {
    pub literal: String,
    pub env: String,
}

/// How a credential is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Header,
    Query,
    Basic,
    Digest,
    Note,
}

/// How a request authenticates, in terms each language can express.
#[derive(Clone, Debug)]
pub(crate) struct Credential {
    pub kind: Kind,
    /// Header or query parameter name.
    pub name: String,
    /// Text before the value, e.g. "Bearer ".
    pub prefix: String,
    pub value: Secret,
    /// Basic and digest.
    pub user: Secret,
    pub pass: Secret,
    /// Why the language cannot do it, for a note and alongside others.
    pub note: String,
}

impl Credential {
    fn new(kind: Kind) -> Credential {
        Credential {
            kind,
            name: String::new(),
            prefix: String::new(),
            value: Secret::default(),
            user: Secret::default(),
            pass: Secret::default(),
            note: String::new(),
        }
    }

    /// Reports whether the credential is read from the environment.
    pub fn uses_env(&self) -> bool {
        !self.value.env.is_empty() || !self.user.env.is_empty()
    }
}

fn model(r: &Resolved, redact: bool) -> Request {
    let headers = if redact {
        r.display_headers(true)
    } else {
        r.headers.clone()
    };
    let headers = headers
        .into_iter()
        // each language writes its own boundary
        .filter(|h| r.parts.is_empty() || !h.name.eq_ignore_ascii_case("Content-Type"))
        .collect();
    let body = if r.parts.is_empty() {
        r.display_body(redact)
    } else {
        String::new()
    };
    let mut proxy = String::new();
    if let Some(p) = r.proxy.as_ref().filter(|p| !p.off) {
        proxy = p.url.clone();
        if !redact {
            if let Some(raw) = p.raw() {
                proxy = raw.to_string();
            }
        }
    }
    Request {
        method: r.method.clone(),
        url: r.display_url(redact),
        headers,
        body,
        parts: r.parts.clone(),
        redact,
        cred: credential_of(r.auth_spec.as_ref(), redact),
        tls: r.tls.clone(),
        proxy,
        http: r.http_version.clone(),
    }
}

fn lit(v: &str, redact: bool, env: &str) -> Secret {
    if redact {
        Secret {
            literal: String::new(),
            env: env.to_string(),
        }
    } else {
        Secret {
            literal: v.to_string(),
            env: String::new(),
        }
    }
}

fn env_secret(env: &str) -> Secret {
    Secret {
        literal: String::new(),
        env: env.to_string(),
    }
}

fn credential_of(s: Option<&Spec>, redact: bool) -> Option<Credential> {
    let s = s?;
    match s.r#type.as_str() {
        "bearer" => Some(Credential {
            name: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
            value: lit(&s.args[0], redact, "TOKEN"),
            ..Credential::new(Kind::Header)
        }),
        "basic" | "digest" => Some(Credential {
            user: lit(&s.args[0], redact, "APIC_USER"),
            pass: lit(&s.args[1], redact, "APIC_PASSWORD"),
            ..Credential::new(if s.r#type == "basic" {
                Kind::Basic
            } else {
                Kind::Digest
            })
        }),
        "apikey" => {
            let (header, query) = s.api_key_placement();
            let mut c = Credential {
                name: header,
                prefix: s.option("prefix").to_string(),
                value: lit(&s.args[0], redact, "APIC_API_KEY"),
                ..Credential::new(Kind::Header)
            };
            if !query.is_empty() {
                c.kind = Kind::Query;
                c.name = query;
            }
            Some(c)
        }
        "oauth2" => Some(Credential {
            name: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
            value: env_secret("TOKEN"),
            note: format!(
                "TOKEN is an OAuth2 access token from {}",
                s.option("tokenUrl")
            ),
            ..Credential::new(Kind::Header)
        }),
        "exec" => {
            let (header, prefix) = exec_header(s);
            Some(Credential {
                name: header,
                prefix,
                value: env_secret("TOKEN"),
                note: format!("TOKEN is the output of: {}", s.args.join(" ")),
                ..Credential::new(Kind::Header)
            })
        }
        "aws" => {
            let (mut service, mut region) = (s.option("service"), s.option("region"));
            if service.is_empty() {
                service = "execute-api";
            }
            if region.is_empty() {
                region = "$AWS_REGION";
            }
            Some(Credential {
                note: format!("sign this request with AWS Signature V4 (service {service}, region {region}); apic does it with the AWS credential chain"),
                ..Credential::new(Kind::Note)
            })
        }
        _ => None,
    }
}

/// The header an exec credential goes in and the text before it:
/// Authorization and "Bearer " unless the spec says otherwise; a prefix
/// set to nothing stays nothing.
pub(crate) fn exec_header(s: &Spec) -> (String, String) {
    let mut header = s.option("header");
    if header.is_empty() {
        header = "Authorization";
    }
    let mut prefix = match s.options.get("prefix") {
        Some(p) => p.clone(),
        None => "Bearer".to_string(),
    };
    if !prefix.is_empty() {
        prefix.push(' ');
    }
    (header.to_string(), prefix)
}

impl Request {
    /// Lists what the snippet does not carry over, as comment lines. Each
    /// flag says the language handles that itself.
    pub fn notes(&self, tls: bool, proxy: bool, digest: bool) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(c) = &self.cred {
            if !c.note.is_empty() {
                out.push(c.note.clone());
            }
            if c.kind == Kind::Digest && !digest {
                out.push(
                    "the API uses HTTP digest auth, which this snippet does not answer; apic does"
                        .to_string(),
                );
            }
        }
        if let Some(t) = &self.tls {
            if !tls {
                out.push(format!("apic's TLS settings are not carried over: {t}"));
            }
        }
        if !self.proxy.is_empty() && !proxy {
            out.push(format!("apic sends this through the proxy {}", self.proxy));
        }
        if self.http == HTTP2 {
            out.push(
                "the request line asks for HTTP/2, which this client does not require".to_string(),
            );
        }
        out
    }

    /// The value of a text part as the snippet shows it.
    pub fn part_value<'a>(&self, p: &'a FormPart) -> &'a str {
        if self.redact {
            "***"
        } else {
            &p.value
        }
    }
}

/// The last element of a slash-separated path, trailing slashes aside:
/// "." for an empty path and "/" for one that is only slashes.
pub(crate) fn path_base(p: &str) -> &str {
    if p.is_empty() {
        return ".";
    }
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/";
    }
    match trimmed.rfind('/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}

/// The file name of a path written with either separator.
pub(crate) fn base_name(p: &str) -> &str {
    match p.rfind(['/', '\\']) {
        Some(i) => &p[i + 1..],
        None => p,
    }
}
