//! Following redirects, with the caller's policy.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::client::{Request, Response, RoundTrip};
use crate::error::{Error, ErrorKind};
use crate::headers::Headers;
use crate::target::parse_target;

/// What a redirect check decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectAction {
    /// Send the next request.
    Follow,
    /// Do not follow: return the redirect response itself.
    Stop,
}

/// Cookies kept between requests. The redirect helper asks the jar for
/// the cookies of every request it sends and hands it the `Set-Cookie`
/// headers of every response it reads, redirects included.
pub trait CookieJar {
    /// The `(name, value)` pairs to send to `url`, in order. They are
    /// joined as `name=value` onto the request's `Cookie` header.
    fn cookies(&self, url: &str) -> Vec<(String, String)>;
    /// Stores what a response from `url` set: the raw values of its
    /// `Set-Cookie` headers. Not called for a response that sets none.
    fn set_cookies(&self, url: &str, set_cookie: &[String]);
}

/// The check run before each redirect is followed. It is given the
/// request about to be sent, which it may change (to drop a header, say),
/// and the requests already sent, oldest first, their bodies left out.
/// `Err` stops with that message as the error.
pub type CheckRedirect<'a> =
    dyn Fn(&mut Request, &[Request]) -> Result<RedirectAction, String> + 'a;

/// How `follow_redirects` behaves. The default follows up to ten
/// redirects and keeps no cookies.
#[derive(Clone, Copy, Default)]
pub struct RedirectPolicy<'a> {
    /// Decides whether each redirect is followed. `None` follows until
    /// ten requests have been sent, then fails with `stopped after 10
    /// redirects`. A check of the caller's own replaces that limit, so it
    /// must count `via` itself.
    pub check: Option<&'a CheckRedirect<'a>>,
    /// Where cookies are kept across the hops.
    pub jar: Option<&'a dyn CookieJar>,
}

/// The headers that are not carried to another host.
const SENSITIVE: [&str; 6] = [
    "Authorization",
    "Www-Authenticate",
    "Cookie",
    "Cookie2",
    "Proxy-Authorization",
    "Proxy-Authenticate",
];

/// Sends a request and follows the redirects it meets.
///
/// A 301, 302 or 303 is followed with GET (HEAD stays HEAD) and no body;
/// a 307 or 308 repeats the method and the body. A redirect without a
/// `Location` is returned as it is. Each new request carries the first
/// request's headers, minus `Authorization`, `Cookie` and their kin once
/// a redirect has left the first host for one that is not a subdomain of
/// it, plus a `Referer` (never from https to http). A `Host` override
/// survives a relative redirect only. The request's timeout covers the
/// whole chain, and the timings of the hops are folded together with
/// `Timings::then`.
pub fn follow_redirects(
    transport: &dyn RoundTrip,
    req: &Request,
    policy: &RedirectPolicy<'_>,
) -> Result<Response, Error> {
    let started = Instant::now();
    let deadline = [req.deadline, req.timeout.map(|t| started + t)]
        .into_iter()
        .flatten()
        .min();
    let first_method = req.method.clone();
    let initial_headers = req.headers.clone();
    // The cookies the caller wrote by hand; a server that sets one of
    // them on the way takes it over from then on.
    let mut initial_cookies: Option<BTreeMap<String, Vec<String>>> =
        match (policy.jar, req.headers.get("Cookie")) {
            (Some(_), Some(_)) => Some(parse_cookie_header(&req.headers.values("Cookie"))),
            _ => None,
        };
    let mut copied_headers = initial_headers.clone();
    let mut strip_sensitive = false;
    let mut include_body = true;

    let mut current = req.clone();
    current.timeout = None;
    current.deadline = deadline;
    let mut via: Vec<Request> = Vec::new();
    let mut timings = None;

    loop {
        if let Some(jar) = policy.jar {
            for (name, value) in jar.cookies(&current.url) {
                let pair = format!("{name}={value}");
                match current.headers.get("Cookie").filter(|c| !c.is_empty()) {
                    Some(have) => {
                        let joined = format!("{have}; {pair}");
                        current.headers.set("Cookie", &joined);
                    }
                    None => current.headers.set("Cookie", &pair),
                }
            }
        }
        let mut resp = transport.round_trip(&current).map_err(|mut e| {
            e.op = Error::new(e.kind, &first_method, "", "").op;
            e
        })?;
        if let Some(jar) = policy.jar {
            let set: Vec<String> = resp
                .headers
                .values("Set-Cookie")
                .into_iter()
                .map(str::to_string)
                .collect();
            if !set.is_empty() {
                jar.set_cookies(&current.url, &set);
            }
        }
        resp.timings = match &timings {
            Some(earlier) => crate::client::Timings::then(earlier, &resp.timings),
            None => resp.timings,
        };
        timings = Some(resp.timings);

        let (next_method, hop_keeps_body) = match resp.status_code {
            301..=303 => {
                let method = if current.method == "GET" || current.method == "HEAD" {
                    current.method.clone()
                } else {
                    "GET".to_string()
                };
                (method, false)
            }
            307 | 308 => (current.method.clone(), true),
            _ => return Ok(resp),
        };
        // Once a hop drops the body it is never sent again.
        include_body &= hop_keeps_body;
        let Some(location) = resp.headers.get("Location").filter(|l| !l.is_empty()) else {
            return Ok(resp);
        };
        let location = location.to_string();
        let fail =
            |url: &str, message: String| Error::new(ErrorKind::Other, &first_method, url, message);
        let next_url = match url::Url::parse(&current.url).and_then(|base| base.join(&location)) {
            Ok(u) => u.to_string(),
            Err(e) => {
                return Err(fail(
                    &current.url,
                    format!("failed to parse Location header {location:?}: {e}"),
                ))
            }
        };
        // A Host override follows a relative redirect and no other.
        let relative = url::Url::parse(&location) == Err(url::ParseError::RelativeUrlWithoutBase);
        let host = current
            .host
            .clone()
            .or_else(|| current.headers.get("Host").map(str::to_string))
            .filter(|_| relative);

        let mut sent = current.clone();
        sent.body = Vec::new();
        via.push(sent);

        let mut next = Request {
            method: next_method,
            url: next_url,
            headers: Headers::new(),
            host,
            body: if include_body {
                req.body.clone()
            } else {
                Vec::new()
            },
            timeout: None,
            deadline,
            cancel: req.cancel.clone(),
            proxy: req.proxy.clone(),
            max_body: req.max_body,
        };

        if !strip_sensitive
            && !same_host(&via[0].url, &next.url)
            && !is_subdomain(&next.url, &via[0].url)
        {
            strip_sensitive = true;
        }
        if let Some(cookies) = &mut initial_cookies {
            let mut changed = false;
            for set in resp.headers.values("Set-Cookie") {
                let name = set
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .split('=')
                    .next()
                    .unwrap_or("")
                    .trim();
                changed |= cookies.remove(name).is_some();
            }
            if changed {
                let mut pairs: Vec<String> = cookies
                    .iter()
                    .flat_map(|(name, values)| values.iter().map(move |v| format!("{name}={v}")))
                    .collect();
                pairs.sort();
                copied_headers.set("Cookie", &pairs.join("; "));
            }
        }
        for (name, value) in copied_headers.iter() {
            if name == "Host" || (strip_sensitive && SENSITIVE.contains(&name)) {
                continue;
            }
            next.headers.add(name, value);
        }
        let last_url = &via[via.len() - 1].url;
        if !(last_url.starts_with("https://") && next.url.starts_with("http://"))
            && !next.headers.contains("Referer")
        {
            next.headers.set("Referer", &without_userinfo(last_url));
        }

        let action = match policy.check {
            Some(check) => check(&mut next, &via),
            None if via.len() >= 10 => Err("stopped after 10 redirects".to_string()),
            None => Ok(RedirectAction::Follow),
        };
        match action {
            Ok(RedirectAction::Stop) => return Ok(resp),
            Ok(RedirectAction::Follow) => current = next,
            Err(message) => return Err(fail(&location, message)),
        }
    }
}

/// Reports whether two URLs name the same `host[:port]`.
fn same_host(a: &str, b: &str) -> bool {
    match (parse_target(a), parse_target(b)) {
        (Ok(a), Ok(b)) => a.authority == b.authority,
        _ => false,
    }
}

/// Reports whether the host of `sub` is the host of `parent` or a
/// subdomain of it, ports aside.
fn is_subdomain(sub: &str, parent: &str) -> bool {
    let (Ok(sub), Ok(parent)) = (parse_target(sub), parse_target(parent)) else {
        return false;
    };
    let (sub, parent) = (sub.host, parent.host);
    if sub == parent {
        return true;
    }
    // An IP address is never a subdomain.
    if sub.contains(':') || sub.contains('%') || sub.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    sub.strip_suffix(&parent)
        .is_some_and(|rest| rest.ends_with('.'))
}

/// The URL without `user:password@`, for the Referer header.
fn without_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// Reads `Cookie` header values into name to values.
fn parse_cookie_header(lines: &[&str]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in lines {
        for part in line.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            out.entry(name.trim().to_string())
                .or_default()
                .push(value.trim().to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert!(same_host("http://a.test:80/x", "http://A.test:80/y"));
        assert!(!same_host("http://a.test/x", "http://a.test:81/y"));
        assert!(is_subdomain("http://api.a.test/", "https://a.test:8443/"));
        assert!(!is_subdomain("http://xa.test/", "http://a.test/"));
        assert!(!is_subdomain("http://127.0.0.1/", "http://0.0.1/"));
        assert_eq!(without_userinfo("http://u:p@h/x@y"), "http://h/x@y");
    }
}
