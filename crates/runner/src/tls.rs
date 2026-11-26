//! The TLS settings of a request: apic.yaml's `tls:`, the environment's
//! SSLConfiguration block, per-host overrides and the command-line flags.

use std::collections::BTreeMap;
use std::path::Path;

use apic_gocompat::path_error;
use apic_httpc::{Client, Protocols, TlsConfig, TlsConfigError};
use apic_httpfile::Request;
use apic_project::{confine, TlsHost};
use serde::{Deserialize, Serialize};

use crate::errors::{uerr, Code, Error};
use crate::gourl::UrlParts;
use crate::Runner;

/// What applies to one host once apic.yaml's `tls:`, the environment's
/// JetBrains SSLConfiguration block, a per-host override and the
/// command-line flags are merged, in that order. Paths from the flags are
/// absolute (the user named them); the others are relative to the project
/// root and confined to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TlsSettings {
    pub ca_file: String,
    pub cert_file: String,
    pub key_file: String,
    pub verify: bool,
    /// Marks a key the env file says is encrypted, which apic cannot use.
    pub passphrase: bool,
    /// `ca_from_flag` and `cert_from_flag` mark paths the user typed on
    /// the command line, the only ones not confined to the project root.
    pub ca_from_flag: bool,
    pub cert_from_flag: bool,
}

impl TlsSettings {
    fn is_default(&self) -> bool {
        self.ca_file.is_empty() && self.cert_file.is_empty() && self.verify
    }

    /// Reports the settings for output, or `None` when they are the
    /// default.
    pub fn info(&self) -> Option<TlsInfo> {
        if self.is_default() {
            return None;
        }
        Some(TlsInfo {
            ca_file: self.ca_file.clone(),
            cert_file: self.cert_file.clone(),
            key_file: self.key_file.clone(),
            insecure: !self.verify,
        })
    }

    /// Identifies a set of settings for the caches.
    fn key(&self) -> String {
        if self.is_default() {
            return "default".to_string();
        }
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.ca_file,
            self.cert_file,
            self.key_file,
            self.verify,
            self.ca_from_flag,
            self.cert_from_flag
        )
    }
}

/// The TLS setup of a request as shown in output and mapped by `apic
/// curl`. Paths are as configured: relative to the project root, or
/// absolute when they came from a flag.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsInfo {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ca_file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cert_file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_file: String,
    /// The server certificate is not verified.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
}

/// The one-line description `describe` and verbose runs print.
impl std::fmt::Display for TlsInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if !self.cert_file.is_empty() {
            let mut s = format!("client cert {}", self.cert_file);
            if !self.key_file.is_empty() && self.key_file != self.cert_file {
                s.push_str(&format!(" (key {})", self.key_file));
            }
            parts.push(s);
        }
        if !self.ca_file.is_empty() {
            parts.push(format!("ca {}", self.ca_file));
        }
        if self.insecure {
            parts.push("server certificate not verified".to_string());
        }
        f.write_str(&parts.join(" · "))
    }
}

/// Finds the `tls.hosts` entry for host: an exact name, a name with the
/// port, or a `*.suffix` wildcard; the most specific wins.
pub(crate) fn host_override<'a>(
    hosts: &'a BTreeMap<String, TlsHost>,
    host: &str,
) -> Option<&'a TlsHost> {
    if host.is_empty() || hosts.is_empty() {
        return None;
    }
    let host = host.to_lowercase();
    let mut bare = host.as_str();
    if let Some(i) = host.rfind(':') {
        if i > 0 && !host[i..].contains(']') {
            bare = &host[..i];
        }
    }
    for candidate in [host.as_str(), bare] {
        if let Some(h) = hosts.get(candidate) {
            return Some(h);
        }
    }
    let mut best = None;
    let mut longest = 0;
    for (name, h) in hosts {
        let name = name.to_lowercase();
        if !name.starts_with("*.") {
            continue;
        }
        let suffix = &name[1..];
        if bare.ends_with(suffix)
            && bare.len() > suffix.len()
            && (best.is_none() || suffix.len() > longest)
        {
            best = Some(h);
            longest = suffix.len();
        }
    }
    best
}

/// What the HTTP version on a request line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProtoWant {
    /// No version: HTTP/2 when TLS negotiates it, else HTTP/1.1.
    Any,
    /// HTTP/1.1 (or 1.0): never HTTP/2.
    Http1,
    /// HTTP/2: required, over TLS.
    Http2,
}

impl ProtoWant {
    fn as_str(self) -> &'static str {
        match self {
            ProtoWant::Any => "any",
            ProtoWant::Http1 => "HTTP/1.1",
            ProtoWant::Http2 => "HTTP/2",
        }
    }
}

/// Maps a request's HTTP version to what the client must do.
pub(crate) fn want_for(req: &Request) -> Result<ProtoWant, String> {
    Ok(match req.protocol()? {
        v if v == apic_httpfile::HTTP1 => ProtoWant::Http1,
        v if v == apic_httpfile::HTTP2 => ProtoWant::Http2,
        _ => ProtoWant::Any,
    })
}

fn read_file(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| path_error("open", Path::new(path), &e))
}

impl Runner {
    /// Merges the TLS settings that apply to host (empty for none in
    /// particular, as for a token endpoint).
    pub(crate) fn tls_for(&self, host: &str) -> TlsSettings {
        let cfg = &self.project.config.tls;
        let mut s = TlsSettings {
            ca_file: cfg.ca_file.clone(),
            cert_file: cfg.cert_file.clone(),
            key_file: cfg.key_file.clone(),
            verify: cfg.verify_host.unwrap_or(true),
            ..TlsSettings::default()
        };
        if let Some(ssl) = self.envs.ssl(&self.opts.env) {
            if !ssl.cert_file.is_empty() {
                s.cert_file = ssl.cert_file.clone();
                s.key_file = ssl.key_file.clone();
                s.passphrase = ssl.has_passphrase;
            }
            if let Some(v) = ssl.verify_host {
                s.verify = v;
            }
        }
        if let Some(h) = host_override(&cfg.hosts, host) {
            if !h.ca_file.is_empty() {
                s.ca_file = h.ca_file.clone();
            }
            if !h.cert_file.is_empty() {
                s.cert_file = h.cert_file.clone();
                s.key_file = h.key_file.clone();
                s.passphrase = false;
            }
            if let Some(v) = h.verify_host {
                s.verify = v;
            }
        }
        if !self.opts.ca_cert.is_empty() {
            s.ca_file = self.opts.ca_cert.clone();
            s.ca_from_flag = true;
        }
        if !self.opts.cert.is_empty() {
            s.cert_file = self.opts.cert.clone();
            s.key_file = self.opts.key.clone();
            s.passphrase = false;
            s.cert_from_flag = true;
        }
        if !s.cert_file.is_empty() && s.key_file.is_empty() {
            s.key_file = s.cert_file.clone();
        }
        if self.opts.insecure {
            s.verify = false;
        }
        s
    }

    /// Reports the non-default TLS setup for a request URL's host.
    pub(crate) fn tls_info_for_url(&self, raw_url: &str) -> Option<TlsInfo> {
        match UrlParts::parse(raw_url) {
            Ok(u) => self.tls_for(&u.host).info(),
            Err(_) => self.tls_for("").info(),
        }
    }

    /// Builds (and caches) the client TLS configuration for a set of
    /// settings; `None` is the default. Every problem is a usage error:
    /// it is the project's configuration, not the network, that is wrong.
    pub(crate) fn tls_config(&self, s: &TlsSettings) -> Result<Option<TlsConfig>, Error> {
        if s.is_default() {
            return Ok(None);
        }
        if !s.cert_file.is_empty() && s.passphrase {
            return Err(uerr(Code::TlsConfig, format!("tls: the client key for {} needs a passphrase (hasCertificatePassphrase), which apic cannot supply; store a decrypted key instead", s.cert_file)));
        }
        let key = s.key();
        let mut cache = self.tls_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cache.get(&key) {
            return Ok(Some(c.clone()));
        }
        let mut c = TlsConfig {
            insecure: !s.verify,
            ..TlsConfig::default()
        };
        if !s.ca_file.is_empty() {
            let path = self.tls_path(&s.ca_file, "ca file", s.ca_from_flag)?;
            let pem = read_file(&path)
                .map_err(|e| uerr(Code::TlsConfig, format!("tls: ca file {}: {e}", s.ca_file)))?;
            c.ca_pem = Some(pem);
        }
        if !s.cert_file.is_empty() {
            let cert_path = self.tls_path(&s.cert_file, "client certificate", s.cert_from_flag)?;
            let key_path = self.tls_path(&s.key_file, "client key", s.cert_from_flag)?;
            refuse_world_readable(&key_path, &s.key_file)?;
            let bad = |e: String| {
                uerr(
                    Code::TlsConfig,
                    format!("tls: client certificate {}: {e}", s.cert_file),
                )
            };
            c.cert_pem = Some(read_file(&cert_path).map_err(bad)?);
            c.key_pem = Some(read_file(&key_path).map_err(bad)?);
        }
        // Building a client is what checks the PEM.
        match Client::builder().tls(c.clone()).build() {
            Ok(_) => {}
            Err(TlsConfigError::NoCaCertificates) => {
                return Err(uerr(
                    Code::TlsConfig,
                    format!("tls: ca file {}: no PEM certificates found", s.ca_file),
                ))
            }
            Err(TlsConfigError::ClientCertificate(e)) => {
                return Err(uerr(
                    Code::TlsConfig,
                    format!("tls: client certificate {}: {e}", s.cert_file),
                ))
            }
        }
        cache.insert(key, c.clone());
        Ok(Some(c))
    }

    /// Resolves a certificate path: one from a flag is the user's own,
    /// taken as it is; one from apic.yaml or an env file is relative to
    /// the project root and confined to it, absolute or not.
    fn tls_path(&self, p: &str, what: &str, from_flag: bool) -> Result<String, Error> {
        if from_flag {
            return Ok(p.to_string());
        }
        let root = &self.project.root;
        confine(root, root, p).map_err(|err| {
            if err.is_outside_root() {
                uerr(
                    Code::TlsConfig,
                    format!("tls: {what} {p}: resolves outside the project root"),
                )
            } else {
                uerr(Code::TlsConfig, format!("tls: {what} {p}: {err}"))
            }
        })
    }

    /// Returns the HTTP client for a request to host, with the TLS
    /// settings that apply to it.
    ///
    /// One client serves every request with the same TLS settings in an
    /// invocation, so a flow's requests reuse their connections (which
    /// the timings report as reused).
    pub(crate) fn client_for(&self, host: &str, want: ProtoWant) -> Result<Client, Error> {
        let s = self.tls_for(host);
        let cfg = self.tls_config(&s)?;
        let key = format!("{}|{}", s.key(), want.as_str());
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = clients.get(&key) {
            return Ok(c.clone());
        }
        // A version on the request line pins the protocol; without one,
        // TLS negotiates HTTP/2 when the server offers it.
        let protocols = match want {
            ProtoWant::Any => Protocols::Any,
            ProtoWant::Http1 => Protocols::Http1,
            ProtoWant::Http2 => Protocols::Http2,
        };
        let client = Client::builder()
            .tls(cfg.unwrap_or_default())
            .protocols(protocols)
            .build()
            .map_err(|e| uerr(Code::TlsConfig, format!("tls: {e}")))?;
        clients.insert(key, client.clone());
        Ok(client)
    }
}

/// Keeps a private key that anyone on the machine can read from being
/// used silently, the way ssh does. File modes are not meaningful on
/// Windows.
fn refuse_world_readable(path: &str, shown: &str) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| {
            uerr(
                Code::TlsConfig,
                format!(
                    "tls: client key {shown}: {}",
                    path_error("stat", Path::new(path), &e)
                ),
            )
        })?;
        let perm = meta.permissions().mode() & 0o777;
        if perm & 0o004 != 0 {
            return Err(uerr(Code::TlsConfig, format!("tls: client key {shown} is world-readable (mode {perm:04o}); run `chmod 600 {shown}`")));
        }
    }
    #[cfg(not(unix))]
    let _ = (path, shown);
    Ok(())
}
