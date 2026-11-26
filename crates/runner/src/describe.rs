//! What `apic describe` and `apic env` show: a request's variables and
//! where each comes from.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use apic_gocompat::json_string;
use apic_httpfile::{Request, GRAPHQL_HEADER};
use serde::{Deserialize, Serialize};

use crate::proxy::ProxyInfo;
use crate::result::header_map;
use crate::runner::lock;
use crate::tls::TlsInfo;
use crate::vars::VarInfo;
use crate::Runner;

/// A list that is written as `null` when it is empty.
fn null_when_empty<S: serde::Serializer>(v: &[VarInfo], s: S) -> Result<S::Ok, S::Error> {
    if v.is_empty() {
        s.serialize_none()
    } else {
        v.serialize(s)
    }
}

fn list_or_null<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<VarInfo>, D::Error> {
    Ok(Option::<Vec<VarInfo>>::deserialize(d)?.unwrap_or_default())
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// What `apic describe` shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Description {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub id: String,
    pub file: String,
    pub line: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub method: String,
    pub url_template: String,
    /// Resolved as far as possible.
    pub url: String,
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body_file: String,
    /// `>> file` after the body, relative to the .http file.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub save_to: String,
    #[serde(serialize_with = "null_when_empty", deserialize_with = "list_or_null")]
    pub variables: Vec<VarInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captures: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asserts: Vec<String>,
    /// `# @step` phrases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    /// `# @ref` and `# @forceRef` targets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    /// `# @sleep`: the wait before sending.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sleep: String,
    /// `# @disabled`: skipped by flows.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    /// HTTP/1.1 or HTTP/2 from the request line.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub http_version: String,
    /// The auth spec template.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth: String,
    /// "request" or "apic.yaml".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_source: String,
    /// The non-default TLS setup for the request's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsInfo>,
    /// The proxy in effect, when one is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyInfo>,
    /// Every variable resolves, or a `# @ref` supplies it.
    pub ready: bool,
}

/// Shows what goes on the wire for a GraphQL request: a POST with the
/// JSON envelope, the placeholders still to be filled in.
fn describe_graphql(d: &mut Description, req: &Request) {
    d.method = "POST".to_string();
    d.headers
        .retain(|k, _| !k.eq_ignore_ascii_case(GRAPHQL_HEADER));
    if req.header("Content-Type").is_none() {
        d.headers
            .insert("Content-Type".to_string(), "application/json".to_string());
    }
    if !req.body_file.is_empty() {
        return;
    }
    if let Ok((q, v)) = req.graphql() {
        d.body = format!("{{\"query\": {}", json_string(&q, true));
        if !v.is_empty() {
            d.body.push_str(&format!(", \"variables\": {v}"));
        }
        d.body.push('}');
    }
}

impl Runner {
    /// Reports a request's variables and where each comes from.
    pub fn describe(&self, req: &Arc<Request>) -> Description {
        let mut d = Description {
            name: req.name.clone(),
            id: req.id(),
            file: req.file.path.clone(),
            line: req.line,
            description: req.description.clone(),
            method: req.method.clone(),
            url_template: req.url.clone(),
            headers: header_map(&req.headers),
            body: req.body.clone(),
            body_file: req.body_file.clone(),
            ready: true,
            disabled: req.disabled(),
            http_version: req.protocol().unwrap_or_default().to_string(),
            ..Description::default()
        };
        if let Some(v) = req.directive("sleep") {
            d.sleep = v.trim().to_string();
        }
        if let Some(save) = &req.save_to {
            d.save_to = save.path.clone();
            if save.overwrite {
                d.save_to.push_str(" (overwrite)");
            }
        }
        if req.is_graphql() {
            describe_graphql(&mut d, req);
        }
        let mut texts = vec![req.url.clone()];
        texts.extend(req.headers.iter().map(|h| h.value.clone()));
        texts.push(req.body.clone());
        for a in &req.asserts {
            d.asserts.push(a.expr.clone());
            texts.push(a.expr.clone());
        }
        for c in &req.captures {
            d.captures.push(format!("{} = {}", c.name, c.selector));
        }
        d.steps = req.steps();
        let mut ref_runs = HashSet::new();
        for r in req.refs() {
            if let Ok(targets) = self.project.resolve(&r.id) {
                if targets.len() == 1 {
                    ref_runs.insert(targets[0].id());
                }
            }
            d.refs.push(r.id);
        }
        let (raw, src) = self.auth_source(req);
        if !raw.is_empty() {
            if let Ok(spec) = apic_auth::parse(&raw) {
                texts.extend(spec.texts());
            }
            d.auth = raw;
            d.auth_source = src;
        }
        let mut seen = HashSet::new();
        for t in &texts {
            for e in apic_template::exprs(t) {
                if !seen.insert(e.clone()) {
                    continue;
                }
                let (info, ready) = self.describe_var(req, &e, &ref_runs);
                d.ready = d.ready && ready;
                d.variables.push(info);
            }
        }
        d.variables.sort_by_key(|v| !v.missing);
        d.url = req.url.clone();
        // Only the host decides the TLS settings, so render the URL alone
        // (as far as it resolves) rather than the whole request.
        let rendered =
            match apic_template::render(req.url.trim(), |e| self.resolve_expr(Some(req), e)) {
                Ok(s) => s,
                Err(err) => err.output().to_string(),
            };
        d.tls = self.tls_info_for_url(&rendered);
        d.proxy = self.proxy_info(&rendered);
        d
    }

    /// Says where one placeholder's value comes from, and whether the
    /// request is ready as far as it goes. `ref_runs` holds the ids of
    /// the requests a `# @ref` runs first.
    fn describe_var(
        &self,
        req: &Arc<Request>,
        e: &str,
        ref_runs: &HashSet<String>,
    ) -> (VarInfo, bool) {
        if e.starts_with("$auth.") {
            // Say which configuration runs; fetching a token is for
            // sending, not describing.
            let info = self.describe_auth(e);
            let ready = !info.missing;
            return (info, ready);
        }
        if e.starts_with('$') || e.contains(".response.") {
            return self.describe_builtin(req, e, ref_runs);
        }
        let (mut info, ok, err) = self.lookup(Some(req), e, 0);
        if let Some(err) = &err {
            info.source = format!("{}: {err}", info.source);
            info.missing = true;
        }
        if info.missing && err.is_none() && ref_runs.contains(&info.captured_by) {
            // A `# @ref` supplies it before the request goes out, so it
            // does not make the request unready.
            info.ref_runs = true;
            return (info, true);
        }
        (info, ok && err.is_none())
    }

    /// Describes a built-in ($uuid, $env, …) or a response reference.
    fn describe_builtin(
        &self,
        req: &Arc<Request>,
        e: &str,
        ref_runs: &HashSet<String>,
    ) -> (VarInfo, bool) {
        let r = self.resolve_expr_meta(Some(req), e, 0);
        let mut info = VarInfo {
            name: e.to_string(),
            source: "built-in".to_string(),
            value: r.value,
            secret: r.secret,
            missing: !r.ok || r.err.is_some(),
            ..VarInfo::default()
        };
        if let Some((name, _)) = e.split_once(".response.") {
            info.source = "response reference (flow only)".to_string();
            if !r.ok && r.err.is_none() && ref_runs.contains(name) {
                // `# @ref` runs the request first, as for a captured
                // variable.
                info.ref_runs = true;
            }
        }
        if let Some(err) = &r.err {
            info.source.push_str(&format!(": {err}"));
        }
        let ready = (r.ok && r.err.is_none()) || info.ref_runs;
        (info, ready)
    }

    /// Lists the effective variables for the current environment, for
    /// `apic env`.
    pub fn env_vars(&self) -> Vec<VarInfo> {
        let env = &self.opts.env;
        let mut names: BTreeSet<String> = BTreeSet::new();
        names.extend(self.envs.public_vars(env).into_keys());
        names.extend(self.envs.private_vars(env).into_keys());
        names.extend(self.envs.dot_env.keys().cloned());
        if let Some(session) = &self.session {
            names.extend(
                lock(session)
                    .vars(env)
                    .into_keys()
                    .filter(|k| !k.starts_with('$')),
            );
        }
        names.extend(self.vars().into_keys());
        names.iter().map(|n| self.lookup(None, n, 0).0).collect()
    }
}
