//! Which proxy a request goes through: --proxy, apic.yaml's `proxy`, or
//! the HTTP_PROXY, HTTPS_PROXY and NO_PROXY variables.

use std::net::IpAddr;

use apic_gocompat::quote;
use serde::{Deserialize, Serialize};

use crate::errors::{uerr, Code, Error};
use crate::gourl::UrlParts;
use crate::result::MASKED;
use crate::Runner;

/// The proxy in effect for one request, for `describe`, `env --json`, the
/// verbose run output and `apic curl`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyInfo {
    /// The proxy with any userinfo replaced by ***. Empty when `off`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Where the setting came from: "--proxy", "apic.yaml", the
    /// environment variable that named it, "--no-proxy", or "noProxy" when
    /// the host is excluded by apic.yaml's noProxy list (or NO_PROXY for a
    /// proxy from the environment).
    pub source: String,
    /// No proxy will be used although one is configured.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
    /// The setting could not be used; the request is refused with it
    /// rather than sent directly.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// The proxy as it will be dialled; `None` when off.
    #[serde(skip)]
    raw: Option<String>,
}

impl ProxyInfo {
    /// The proxy URL as it will be dialled, credentials included, for a
    /// curl export that must stay runnable. It is `None` when off.
    pub fn raw(&self) -> Option<&str> {
        self.raw.as_deref()
    }

    fn off(source: &str) -> ProxyInfo {
        ProxyInfo {
            source: source.to_string(),
            off: true,
            ..ProxyInfo::default()
        }
    }
}

/// Renders the proxy the way describe and -v show it.
impl std::fmt::Display for ProxyInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.error.is_empty() {
            return write!(f, "invalid ({}): {}", self.source, self.error);
        }
        if self.off {
            return write!(f, "none ({})", self.source);
        }
        write!(f, "{} ({})", self.url, self.source)
    }
}

/// The explicit proxy from --proxy or apic.yaml, parsed once when the
/// runner is built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProxySettings {
    pub url: String,
    pub source: String,
    pub no_proxy: Vec<String>,
}

/// Everything that decides a request's proxy, apart from the request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProxyConfig {
    /// --no-proxy: every request goes directly.
    pub disabled: bool,
    /// `None` means the environment decides.
    pub settings: Option<ProxySettings>,
}

/// Validates a --proxy or apic.yaml proxy URL. A bare host:port is taken
/// as an HTTP proxy, the way curl reads -x.
pub(crate) fn parse_proxy(raw: &str, source: &str) -> Result<String, Error> {
    let mut raw = raw.trim().to_string();
    if !raw.contains("://") {
        raw = format!("http://{raw}");
    }
    let parts = match UrlParts::parse(&raw) {
        Ok(p) if !p.host.is_empty() => p,
        _ => {
            return Err(uerr(
                Code::Proxy,
                format!("{source}: bad proxy URL {}", quote(&raw)),
            ))
        }
    };
    match parts.scheme.as_str() {
        "http" | "https" | "socks5" | "socks5h" => Ok(raw),
        other => Err(uerr(
            Code::Proxy,
            format!(
                "{source}: proxy scheme {} not supported (http, https, socks5 or socks5h)",
                quote(other)
            ),
        )),
    }
}

impl ProxyConfig {
    /// The proxy in effect for a request to `u`; `None` when no proxy is
    /// configured anywhere.
    pub fn info(&self, u: &UrlParts) -> Option<ProxyInfo> {
        if self.disabled {
            return Some(ProxyInfo::off("--no-proxy"));
        }
        if let Some(s) = &self.settings {
            if no_proxy_match(&s.no_proxy, u) {
                return Some(ProxyInfo::off("noProxy"));
            }
            return Some(ProxyInfo {
                url: mask_userinfo(&s.url),
                source: s.source.clone(),
                raw: Some(s.url.clone()),
                ..ProxyInfo::default()
            });
        }
        env_proxy(u)
    }

    /// The proxy to dial for a request to `url`: `Ok(None)` for a direct
    /// connection, `Err` with the reason a setting cannot be used.
    pub fn dial(&self, url: &str) -> Result<Option<apic_httpc::Proxy>, String> {
        let parts = UrlParts::parse(url).unwrap_or_default();
        match self.info(&parts) {
            None => Ok(None),
            Some(info) if info.off => Ok(None),
            Some(info) if !info.error.is_empty() => Err(info.error),
            Some(info) => match info.raw() {
                Some(raw) => apic_httpc::Proxy::parse(raw).map(Some),
                None => Ok(None),
            },
        }
    }
}

impl Runner {
    /// Reports the proxy in effect for a request URL. It is `None` when
    /// no proxy is configured anywhere, so the field stays out of the
    /// output in the common case. An empty or unparsable URL reports the
    /// setting for an https request, which is what `apic env` wants.
    pub fn proxy_info(&self, raw_url: &str) -> Option<ProxyInfo> {
        let u = match UrlParts::parse(raw_url) {
            Ok(u) if !u.host.is_empty() => u,
            _ => UrlParts {
                scheme: "https".to_string(),
                host: "example.com".to_string(),
            },
        };
        self.proxy_config().info(&u)
    }

    pub(crate) fn proxy_config(&self) -> ProxyConfig {
        ProxyConfig {
            disabled: self.opts.no_proxy,
            settings: self.proxy.clone(),
        }
    }
}

fn getenv(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Reads the proxy variables the way curl does, by request scheme, afresh
/// each time so a change in the environment is seen (and so tests can set
/// them).
pub(crate) fn env_proxy(u: &UrlParts) -> Option<ProxyInfo> {
    let names = if u.scheme.eq_ignore_ascii_case("https") {
        ["HTTPS_PROXY", "https_proxy"]
    } else {
        ["HTTP_PROXY", "http_proxy"]
    };
    for name in names {
        let v = getenv(name);
        if v.is_empty() {
            continue;
        }
        let p = match parse_proxy(&v, name) {
            Ok(p) => p,
            Err(err) => {
                return Some(ProxyInfo {
                    source: name.to_string(),
                    error: err.to_string(),
                    ..ProxyInfo::default()
                })
            }
        };
        // The environment's proxy never applies to the local machine,
        // which is what every other client does with these variables.
        let host = u.hostname();
        if host == "localhost" || host.ends_with(".localhost") {
            return Some(ProxyInfo::off("noProxy"));
        }
        if parse_ip(host).is_some_and(|ip| ip.is_loopback()) {
            return Some(ProxyInfo::off("noProxy"));
        }
        let mut no_proxy = getenv("NO_PROXY");
        if no_proxy.is_empty() {
            no_proxy = getenv("no_proxy");
        }
        let list: Vec<String> = no_proxy.split(',').map(str::to_string).collect();
        if no_proxy_match(&list, u) {
            return Some(ProxyInfo::off("noProxy"));
        }
        return Some(ProxyInfo {
            url: mask_userinfo(&p),
            source: name.to_string(),
            raw: Some(p),
            ..ProxyInfo::default()
        });
    }
    None
}

fn parse_ip(host: &str) -> Option<IpAddr> {
    let ip: IpAddr = host.parse().ok()?;
    // An IPv4 address written as IPv6 is the IPv4 address.
    Some(match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    })
}

/// Reports whether `ip` lies in the network `cidr` names (`10.0.0.0/8`).
fn cidr_contains(cidr: &str, ip: IpAddr) -> bool {
    let Some((addr, bits)) = cidr.split_once('/') else {
        return false;
    };
    let (Some(net), Ok(bits)) = (parse_ip(addr), bits.parse::<u32>()) else {
        return false;
    };
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) if bits <= 32 => {
            let mask = if bits == 0 {
                0
            } else {
                u32::MAX << (32 - bits)
            };
            u32::from(n) & mask == u32::from(i) & mask
        }
        (IpAddr::V6(n), IpAddr::V6(i)) if bits <= 128 => {
            let mask = if bits == 0 {
                0
            } else {
                u128::MAX << (128 - bits)
            };
            u128::from(n) & mask == u128::from(i) & mask
        }
        _ => false,
    }
}

/// Splits `host:port` (`[v6]:port`); `None` for anything else.
fn split_host_port(s: &str) -> Option<(&str, &str)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        return (!port.contains(':')).then_some((host, port));
    }
    let (host, port) = s.split_once(':')?;
    (!port.contains(':') && !host.contains(['[', ']'])).then_some((host, port))
}

/// Reports whether a request host is excluded by a noProxy list: `*` for
/// everything, `host`, `host:port`, `.suffix` or `*.suffix` for a domain
/// and everything under it, an IP, or a CIDR.
pub(crate) fn no_proxy_match<S: AsRef<str>>(list: &[S], u: &UrlParts) -> bool {
    let host = u.hostname().to_lowercase();
    let port = match u.port() {
        "" if u.scheme.eq_ignore_ascii_case("https") => "443",
        "" => "80",
        p => p,
    };
    let ip = parse_ip(&host);
    for entry in list {
        let entry = entry.as_ref().trim().to_lowercase();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return true;
        }
        if ip.is_some_and(|ip| cidr_contains(&entry, ip)) {
            return true;
        }
        if let Some((h, p)) = split_host_port(&entry) {
            if h == host && p == port {
                return true;
            }
            continue;
        }
        let suffix = entry.strip_prefix('*').unwrap_or(&entry);
        let suffix = suffix.strip_prefix('.').unwrap_or(suffix);
        if entry == host
            || (!suffix.is_empty() && (host == suffix || host.ends_with(&format!(".{suffix}"))))
        {
            return true;
        }
    }
    false
}

/// Renders a proxy URL with its credentials hidden, so the proxy can be
/// shown wherever the request is.
fn mask_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{MASKED}@{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// Names where an explicit proxy came from, for error messages.
pub(crate) fn proxy_source(flag: bool) -> &'static str {
    if flag {
        "--proxy"
    } else {
        "apic.yaml"
    }
}
