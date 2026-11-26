//! JetBrains' `{{$auth.token("name")}}`: the Security.Auth configurations
//! of the env files.

use std::sync::{Arc, LazyLock};

use apic_gocompat::quote;
use apic_httpc::CancelToken;
use apic_httpfile::Request;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::errors::{uerr, uwrap, Code, Error};
use crate::vars::{render_error, Resolution, VarInfo};
use crate::Runner;

/// JetBrains' {{$auth.token("name")}} and {{$auth.idToken("name")}}.
static RE_AUTH_BUILTIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\$auth\.(token|idToken)\(\s*"([^"]+)"\s*\)$"#).expect("a valid pattern")
});

/// One Security.Auth configuration, for apic env: secrets masked,
/// placeholders as written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfigInfo {
    pub name: String,
    /// The oauth2 spec it maps to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spec: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub use_id_token: bool,
    pub files: Vec<String>,
    /// Fields apic does not act on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored: Vec<String>,
    /// Why it cannot be used.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl Runner {
    /// Resolves {{$auth.token("name")}} by running the grant of the env
    /// files' Security.Auth configuration "name", through the same token
    /// cache as `# @auth oauth2`. The value is a credential, so it is
    /// secret.
    pub(crate) fn auth_builtin(&self, req: Option<&Arc<Request>>, expr: &str) -> Resolution {
        match self.auth_token(req, expr) {
            Ok(v) => Resolution {
                value: v,
                ok: true,
                secret: true,
                err: None,
            },
            Err(err) => Resolution::fail(err).secret(),
        }
    }

    fn auth_token(&self, req: Option<&Arc<Request>>, expr: &str) -> Result<String, Error> {
        let Some(m) = RE_AUTH_BUILTIN.captures(expr) else {
            return Err(uerr(Code::Auth, format!("{expr}: write $auth.token(\"name\") or $auth.idToken(\"name\"), the name of a Security.Auth configuration in {}", apic_env::PUBLIC_FILE)));
        };
        let (kind, name) = (&m[1], &m[2]);
        let jb = self.jetbrains_auth(req, name)?;
        let auth_env = self.auth_env(None)?;
        let want_id = kind == "idToken" || jb.use_id_token;
        // Built-ins resolve without a caller to cancel them; the token
        // client has the request timeout, and a device or browser sign-in
        // its own.
        let toks = apic_auth::oauth2_tokens(&CancelToken::new(), &jb.spec, &auth_env, want_id)
            .map_err(|e| uerr(Code::Auth, format!("$auth {}: {e}", quote(name))))?;
        Ok(if want_id { toks.id } else { toks.access })
    }

    /// Maps the Security.Auth configuration `name`, for the current
    /// environment, with its placeholders resolved.
    fn jetbrains_auth(
        &self,
        req: Option<&Arc<Request>>,
        name: &str,
    ) -> Result<apic_auth::JetBrains, Error> {
        let auths = self.envs.auth(&self.opts.env);
        let Some(c) = auths.get(name) else {
            return Err(uerr(
                Code::Auth,
                format!(
                    "$auth {}: no Security.Auth configuration of that name for environment {}{}",
                    quote(name),
                    quote(&self.opts.env),
                    self.auth_names_hint()
                ),
            ));
        };
        // The first coded error a placeholder gave, whose code the
        // message built around it keeps.
        let inner: std::cell::RefCell<Option<Error>> = std::cell::RefCell::new(None);
        let jb = apic_auth::from_jetbrains(&c.fields, |s: &str| {
            apic_template::render(s, |e| {
                if e.starts_with("$auth.") {
                    return Err(Error::Other(format!(
                        "$auth {}: a configuration cannot use $auth itself",
                        quote(name)
                    )));
                }
                self.resolve_expr(req, e)
            })
            .map_err(|err| {
                let err = render_error(err);
                inner.borrow_mut().get_or_insert_with(|| err.clone());
                err
            })
        });
        jb.map_err(|err| {
            let msg = format!(
                "$auth {} (Security.Auth in {}): {err}",
                quote(name),
                c.files.join(", ")
            );
            match inner.borrow().as_ref() {
                Some(inner) => uwrap(Code::Auth, msg, inner),
                None => uerr(Code::Auth, msg),
            }
        })
    }

    fn auth_names_hint(&self) -> String {
        let names: Vec<String> = self.envs.auth(&self.opts.env).into_keys().collect();
        if names.is_empty() {
            return " (the env files declare none)".to_string();
        }
        format!(" (declared: {})", names.join(", "))
    }

    /// What describe says about an $auth placeholder: which configuration
    /// it runs, without fetching a token.
    pub(crate) fn describe_auth(&self, expr: &str) -> VarInfo {
        let mut info = VarInfo {
            name: expr.to_string(),
            secret: true,
            ..VarInfo::default()
        };
        let Some(m) = RE_AUTH_BUILTIN.captures(expr) else {
            let err = self.auth_token(None, expr).err();
            info.source = err.map(|e| e.to_string()).unwrap_or_default();
            info.missing = true;
            return info;
        };
        let name = &m[2];
        match self.auth_config(name) {
            None => {
                info.source = format!(
                    "no Security.Auth configuration {} for environment {}{}",
                    quote(name),
                    quote(&self.opts.env),
                    self.auth_names_hint()
                );
                info.missing = true;
            }
            Some(cfg) if !cfg.error.is_empty() => {
                info.source = format!("Security.Auth {}: {}", quote(name), cfg.error);
                info.missing = true;
            }
            Some(cfg) => {
                info.source = format!(
                    "Security.Auth {} in {}: {} (fetched when sent)",
                    quote(name),
                    cfg.files.join(", "),
                    cfg.spec
                );
            }
        }
        info
    }

    /// Lists the Security.Auth configurations of the current environment,
    /// by name.
    pub fn auth_configs(&self) -> Vec<AuthConfigInfo> {
        self.envs
            .auth(&self.opts.env)
            .keys()
            .filter_map(|name| self.auth_config(name))
            .collect()
    }

    /// Describes one configuration without resolving anything; `None`
    /// when the environment declares none of that name.
    pub fn auth_config(&self, name: &str) -> Option<AuthConfigInfo> {
        let auths = self.envs.auth(&self.opts.env);
        let c = auths.get(name)?;
        let mut info = AuthConfigInfo {
            name: name.to_string(),
            files: c.files.clone(),
            ..AuthConfigInfo::default()
        };
        match apic_auth::from_jetbrains(&c.fields, |s: &str| Ok::<_, String>(s.to_string())) {
            Ok(jb) => {
                info.spec = jb.spec.raw;
                info.use_id_token = jb.use_id_token;
                info.ignored = jb.ignored;
            }
            Err(err) => info.error = err.to_string(),
        }
        Some(info)
    }
}
