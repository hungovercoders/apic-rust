//! What the tests build requests from.

use std::collections::BTreeSet;

use apic_auth::Spec;
use apic_httpfile::Header;
use apic_runner::{FormPart, ProxyInfo, Resolved, TlsInfo};

/// The fields of a [`Resolved`] a snippet reads, so a test can write one
/// as a literal.
#[derive(Clone, Default)]
pub(crate) struct Fixture {
    pub method: String,
    pub url: String,
    pub headers: Vec<Header>,
    pub body: String,
    pub http_version: String,
    pub tls: Option<TlsInfo>,
    pub proxy: Option<ProxyInfo>,
    pub auth_spec: Option<Spec>,
    pub secret_headers: BTreeSet<String>,
    pub parts: Vec<FormPart>,
}

impl Fixture {
    pub fn new(method: &str, url: &str) -> Fixture {
        Fixture {
            method: method.to_string(),
            url: url.to_string(),
            ..Fixture::default()
        }
    }

    pub fn resolved(&self) -> Resolved {
        let f = self.clone();
        let mut r = Resolved::default();
        r.method = f.method;
        r.url = f.url;
        r.headers = f.headers;
        r.body = f.body;
        r.http_version = f.http_version;
        r.tls = f.tls;
        r.proxy = f.proxy;
        r.auth_spec = f.auth_spec;
        r.secret_headers = f.secret_headers;
        r.parts = f.parts;
        r
    }
}

pub(crate) fn header(name: &str, value: &str) -> Header {
    Header {
        name: name.to_string(),
        value: value.to_string(),
    }
}

pub(crate) fn proxy(url: &str, source: &str, off: bool) -> ProxyInfo {
    let mut p = ProxyInfo::default();
    p.url = url.to_string();
    p.source = source.to_string();
    p.off = off;
    p
}
