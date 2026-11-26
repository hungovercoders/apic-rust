//! A request router with method, host and wildcard patterns.

use std::collections::BTreeSet;

use crate::request::{path_unescape, Request};
use crate::response::Response;

/// A boxed handler: what a `Mux` routes to and what a server runs.
pub type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync + 'static>;

/// Boxes a closure as a `Handler`.
pub fn handler(f: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Handler {
    Box::new(f)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    /// A literal, a wildcard's name, or `/` for `{$}`.
    s: String,
    wild: bool,
    /// A `{name...}` wildcard, or the nameless one a trailing slash is.
    multi: bool,
}

#[derive(Clone, Debug)]
struct Pattern {
    text: String,
    method: String,
    host: String,
    segments: Vec<Segment>,
}

/// How the requests two patterns match relate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Relationship {
    Equivalent,
    MoreGeneral,
    MoreSpecific,
    Disjoint,
    Overlaps,
}
use Relationship::{Disjoint, Equivalent, MoreGeneral, MoreSpecific, Overlaps};

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn valid_wildcard_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .enumerate()
            .all(|(i, c)| c.is_alphabetic() || c == '_' || (i > 0 && c.is_numeric()))
}

impl Pattern {
    /// Parses `[METHOD ][HOST]/[PATH]`.
    fn parse(s: &str) -> Result<Pattern, String> {
        if s.is_empty() {
            return Err("empty pattern".to_string());
        }
        let at = |off: usize, msg: String| format!("at offset {off}: {msg}");
        let (method, mut rest, found) = match s.find([' ', '\t']) {
            Some(i) => (&s[..i], s[i + 1..].trim_start_matches([' ', '\t']), true),
            None => ("", s, false),
        };
        if !method.is_empty() && !is_token(method) {
            return Err(at(0, format!("invalid method {method:?}")));
        }
        let mut off = if found { method.len() + 1 } else { 0 };
        let Some(i) = rest.find('/') else {
            return Err(at(off, "host/path missing /".to_string()));
        };
        let host = &rest[..i];
        rest = &rest[i..];
        if let Some(j) = host.find('{') {
            return Err(at(
                off + j,
                "host contains '{' (missing initial '/'?)".to_string(),
            ));
        }
        off += i;
        if !method.is_empty() && method != "CONNECT" && rest != clean_path(rest) {
            return Err(at(
                off,
                "non-CONNECT pattern with unclean path can never match".to_string(),
            ));
        }
        let mut segments = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        while !rest.is_empty() {
            rest = &rest[1..];
            off = s.len() - rest.len();
            if rest.is_empty() {
                // A trailing slash matches everything beneath it.
                segments.push(Segment {
                    s: String::new(),
                    wild: true,
                    multi: true,
                });
                break;
            }
            let i = rest.find('/').unwrap_or(rest.len());
            let seg = &rest[..i];
            rest = &rest[i..];
            match seg.find('{') {
                None => segments.push(Segment {
                    s: path_unescape(seg),
                    wild: false,
                    multi: false,
                }),
                Some(i) => {
                    if i != 0 {
                        return Err(at(
                            off,
                            "bad wildcard segment (must start with '{')".to_string(),
                        ));
                    }
                    if !seg.ends_with('}') {
                        return Err(at(
                            off,
                            "bad wildcard segment (must end with '}')".to_string(),
                        ));
                    }
                    let name = &seg[1..seg.len() - 1];
                    if name == "$" {
                        if !rest.is_empty() {
                            return Err(at(off, "{$} not at end".to_string()));
                        }
                        segments.push(Segment {
                            s: "/".to_string(),
                            wild: false,
                            multi: false,
                        });
                        break;
                    }
                    let (name, multi) = match name.strip_suffix("...") {
                        Some(n) => (n, true),
                        None => (name, false),
                    };
                    if multi && !rest.is_empty() {
                        return Err(at(off, "{...} wildcard not at end".to_string()));
                    }
                    if name.is_empty() {
                        return Err(at(off, "empty wildcard".to_string()));
                    }
                    if !valid_wildcard_name(name) {
                        return Err(at(off, format!("bad wildcard name {name:?}")));
                    }
                    if seen.iter().any(|n| n == name) {
                        return Err(at(off, format!("duplicate wildcard name {name:?}")));
                    }
                    seen.push(name.to_string());
                    segments.push(Segment {
                        s: name.to_string(),
                        wild: true,
                        multi,
                    });
                }
            }
        }
        Ok(Pattern {
            text: s.to_string(),
            method: method.to_string(),
            host: host.to_string(),
            segments,
        })
    }

    fn last_multi(&self) -> bool {
        self.segments.last().is_some_and(|s| s.multi)
    }

    fn conflicts_with(&self, other: &Pattern) -> bool {
        if self.host != other.host {
            return false;
        }
        matches!(self.compare_paths_and_methods(other), Equivalent | Overlaps)
    }

    fn compare_paths_and_methods(&self, other: &Pattern) -> Relationship {
        let methods = self.compare_methods(other);
        if methods == Disjoint {
            return Disjoint;
        }
        combine(methods, self.compare_paths(other))
    }

    /// A pattern without a method matches every method, and GET matches
    /// HEAD too.
    fn compare_methods(&self, other: &Pattern) -> Relationship {
        if self.method == other.method {
            Equivalent
        } else if self.method.is_empty() {
            MoreGeneral
        } else if other.method.is_empty() {
            MoreSpecific
        } else if self.method == "GET" && other.method == "HEAD" {
            MoreGeneral
        } else if other.method == "GET" && self.method == "HEAD" {
            MoreSpecific
        } else {
            Disjoint
        }
    }

    fn compare_paths(&self, other: &Pattern) -> Relationship {
        let (a, b) = (&self.segments, &other.segments);
        if a.len() != b.len() && !self.last_multi() && !other.last_multi() {
            return Disjoint;
        }
        let mut rel = Equivalent;
        for (s1, s2) in a.iter().zip(b.iter()) {
            rel = combine(rel, compare_segments(s1, s2));
            if rel == Disjoint {
                return rel;
            }
        }
        if a.len() == b.len() {
            return rel;
        }
        if a.len() < b.len() && self.last_multi() {
            return combine(rel, MoreGeneral);
        }
        if b.len() < a.len() && other.last_multi() {
            return combine(rel, MoreSpecific);
        }
        Disjoint
    }

    /// Matches a path against the pattern's segments and returns what the
    /// named wildcards took.
    fn match_path(&self, path: &str) -> Option<Vec<(String, String)>> {
        let mut values = Vec::new();
        let mut rest = path;
        for seg in &self.segments {
            if rest.is_empty() {
                return None;
            }
            if seg.multi {
                if !seg.s.is_empty() {
                    values.push((seg.s.clone(), path_unescape(&rest[1..])));
                }
                return Some(values);
            }
            let current = if rest == "/" {
                rest = "";
                "/".to_string()
            } else {
                let tail = &rest[1..];
                let i = tail.find('/').unwrap_or(tail.len());
                rest = &tail[i..];
                path_unescape(&tail[..i])
            };
            if seg.wild {
                // A wildcard takes a segment, never the trailing slash.
                if current == "/" {
                    return None;
                }
                values.push((seg.s.clone(), current));
            } else if seg.s != current {
                return None;
            }
        }
        rest.is_empty().then_some(values)
    }

    /// How well the pattern's method fits a request's: 2 for the method
    /// itself, 1 for GET answering HEAD, 0 for a pattern without one.
    fn method_rank(&self, method: &str) -> Option<u8> {
        if self.method == method {
            Some(2)
        } else if self.method == "GET" && method == "HEAD" {
            Some(1)
        } else if self.method.is_empty() {
            Some(0)
        } else {
            None
        }
    }
}

fn compare_segments(s1: &Segment, s2: &Segment) -> Relationship {
    if s1.multi && s2.multi {
        Equivalent
    } else if s1.multi {
        MoreGeneral
    } else if s2.multi {
        MoreSpecific
    } else if s1.wild && s2.wild {
        Equivalent
    } else if s1.wild {
        if s2.s == "/" {
            Disjoint
        } else {
            MoreGeneral
        }
    } else if s2.wild {
        if s1.s == "/" {
            Disjoint
        } else {
            MoreSpecific
        }
    } else if s1.s == s2.s {
        Equivalent
    } else {
        Disjoint
    }
}

fn inverse(r: Relationship) -> Relationship {
    match r {
        MoreSpecific => MoreGeneral,
        MoreGeneral => MoreSpecific,
        other => other,
    }
}

fn combine(r1: Relationship, r2: Relationship) -> Relationship {
    match r1 {
        Equivalent => r2,
        Disjoint => Disjoint,
        Overlaps => {
            if r2 == Disjoint {
                Disjoint
            } else {
                Overlaps
            }
        }
        MoreGeneral | MoreSpecific => {
            if r2 == Equivalent {
                r1
            } else if r2 == inverse(r1) {
                Overlaps
            } else {
                r2
            }
        }
    }
}

fn describe_conflict(p1: &Pattern, p2: &Pattern) -> String {
    let methods = p1.compare_methods(p2);
    let paths = p1.compare_paths(p2);
    let (a, b) = (&p1.text, &p2.text);
    if combine(methods, paths) == Equivalent {
        return format!("{a} matches the same requests as {b}");
    }
    if paths == Overlaps {
        return format!(
            "{a} and {b} both match some paths.\nBut neither is more specific than the other."
        );
    }
    if methods == MoreGeneral && paths == MoreSpecific {
        return format!("{a} matches more methods than {b}, but has a more specific path pattern");
    }
    if methods == MoreSpecific && paths == MoreGeneral {
        return format!("{a} matches fewer methods than {b}, but has a more general path pattern");
    }
    format!("{a} and {b} conflict")
}

/// Cleans a path the lexical way: `.` and `..` resolved, doubled slashes
/// removed, no trailing slash except on the root.
pub(crate) fn path_clean(p: &str) -> String {
    if p.is_empty() {
        return ".".to_string();
    }
    let rooted = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if !rooted {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// The canonical form of a request path: rooted, cleaned, its trailing
/// slash kept.
fn clean_path(p: &str) -> String {
    if p.is_empty() {
        return "/".to_string();
    }
    let rooted = if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    };
    let mut clean = path_clean(&rooted);
    if rooted.ends_with('/') && clean != "/" {
        clean.push('/');
    }
    clean
}

fn strip_host_port(host: &str) -> &str {
    if !host.contains(':') {
        return host;
    }
    if let Some(rest) = host.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((inner, tail)) if tail.is_empty() || tail.starts_with(':') => inner,
            _ => host,
        };
    }
    match host.rsplit_once(':') {
        Some((name, _)) if !name.contains(':') => name,
        _ => host,
    }
}

/// The values a pattern's named wildcards matched.
type PathValues = Vec<(String, String)>;

/// What a `Mux` would do with a request.
#[derive(Debug)]
pub enum Lookup<'a, H> {
    /// A pattern matches.
    Found {
        /// The handler registered for the pattern.
        handler: &'a H,
        /// The pattern as registered.
        pattern: &'a str,
        /// What the pattern's named wildcards matched, in order.
        path_values: Vec<(String, String)>,
    },
    /// No pattern matches the path as written, but one matches it
    /// cleaned or with a trailing slash: the mux answers 301 to there.
    Redirect {
        /// The path (and query) to redirect to.
        location: String,
    },
    /// The path matches under other methods: the mux answers 405.
    MethodNotAllowed {
        /// The methods that would match, sorted.
        allow: Vec<String>,
    },
    /// Nothing matches: the mux answers 404.
    NotFound,
}

/// A request router. Patterns are `[METHOD ][HOST]/[PATH]`:
///
/// - a method restricts the pattern to it (`GET` also takes HEAD);
/// - `{name}` matches one path segment and `{name...}` the rest of the
///   path, both read back with `Request::path_value`;
/// - a path ending in `/` matches everything under it, and `{$}` at the
///   end matches the path with its trailing slash and nothing more;
/// - when several patterns match, the most specific wins, and two
///   patterns where neither is more specific cannot both be registered.
///
/// A path that only matches with a trailing slash added, or once it has
/// been cleaned of `.`, `..` and doubled slashes, is redirected with a
/// 301; a path that matches under other methods only is a 405 with an
/// `Allow` header; anything else is `404 page not found`.
///
/// `H` is what a pattern routes to: a `Handler` for a mux that serves
/// requests itself, or any other value for a caller that only wants
/// `lookup`.
pub struct Mux<H = Handler> {
    routes: Vec<(Pattern, H)>,
}

impl<H> Default for Mux<H> {
    fn default() -> Mux<H> {
        Mux { routes: Vec::new() }
    }
}

impl<H> Mux<H> {
    /// An empty mux.
    pub fn new() -> Mux<H> {
        Mux::default()
    }

    /// Registers a value for a pattern. The error says why a pattern
    /// cannot be held: `parsing "GET x": at offset 4: host/path missing /`
    /// for one that is malformed, `pattern "GET /a/{x}" conflicts with
    /// pattern "GET /a/{y}": ...` for one that clashes with an earlier one.
    pub fn handle(&mut self, pattern: &str, handler: H) -> Result<(), String> {
        if pattern.is_empty() {
            return Err("http: invalid pattern".to_string());
        }
        let parsed = Pattern::parse(pattern).map_err(|e| format!("parsing {pattern:?}: {e}"))?;
        for (existing, _) in &self.routes {
            if parsed.conflicts_with(existing) {
                return Err(format!(
                    "pattern {:?} conflicts with pattern {:?}:\n{}",
                    parsed.text,
                    existing.text,
                    describe_conflict(&parsed, existing)
                ));
            }
        }
        self.routes.push((parsed, handler));
        Ok(())
    }

    /// The patterns registered, in order.
    pub fn patterns(&self) -> Vec<&str> {
        self.routes.iter().map(|(p, _)| p.text.as_str()).collect()
    }

    /// Finds the most specific pattern that matches. A pattern with a
    /// host beats one without; then the method's own pattern beats GET
    /// standing in for HEAD, which beats a pattern without a method; then
    /// the more specific path wins.
    fn find(&self, host: &str, method: &str, path: &str) -> Option<(usize, PathValues)> {
        let mut best: Option<(usize, (bool, u8), PathValues)> = None;
        for (i, (pattern, _)) in self.routes.iter().enumerate() {
            if !pattern.host.is_empty() && pattern.host != host {
                continue;
            }
            let Some(rank) = pattern.method_rank(method) else {
                continue;
            };
            let Some(values) = pattern.match_path(path) else {
                continue;
            };
            let class = (!pattern.host.is_empty(), rank);
            let better = match &best {
                None => true,
                Some((j, best_class, _)) => {
                    class > *best_class
                        || (class == *best_class
                            && pattern.compare_paths(&self.routes[*j].0) == MoreSpecific)
                }
            };
            if better {
                best = Some((i, class, values));
            }
        }
        best.map(|(i, _, values)| (i, values))
    }

    /// Reports whether the pattern at `found` matches `path` itself and
    /// not merely something under it.
    fn exact(&self, found: &Option<(usize, PathValues)>, path: &str) -> bool {
        let Some((i, _)) = found else { return false };
        let pattern = &self.routes[*i].0;
        if !pattern.last_multi() {
            return true;
        }
        path.ends_with('/') && pattern.segments.len() == path.matches('/').count()
    }

    /// Says what the mux would do with the request, without doing it.
    pub fn lookup(&self, req: &Request) -> Lookup<'_, H> {
        let with_query = |path: &str| {
            if req.raw_query.is_empty() {
                path.to_string()
            } else {
                format!("{path}?{}", req.raw_query)
            }
        };
        let (host, path, found) = if req.method == "CONNECT" {
            let found = self.find(&req.host, &req.method, &req.raw_path);
            (req.host.as_str(), req.raw_path.clone(), found)
        } else {
            let host = strip_host_port(&req.host);
            let path = clean_path(&req.raw_path);
            let found = self.find(host, &req.method, &path);
            if !self.exact(&found, &path) && !path.ends_with('/') {
                let slashed = format!("{path}/");
                if self.exact(&self.find(host, &req.method, &slashed), &slashed) {
                    return Lookup::Redirect {
                        location: with_query(&slashed),
                    };
                }
            }
            if path != req.raw_path {
                return Lookup::Redirect {
                    location: with_query(&path),
                };
            }
            (host, path, found)
        };
        match found {
            Some((i, path_values)) => Lookup::Found {
                handler: &self.routes[i].1,
                pattern: &self.routes[i].0.text,
                path_values,
            },
            None => {
                let allow = self.matching_methods(host, &path);
                if allow.is_empty() {
                    Lookup::NotFound
                } else {
                    Lookup::MethodNotAllowed { allow }
                }
            }
        }
    }

    /// The methods under which some pattern matches the path, with or
    /// without a trailing slash.
    fn matching_methods(&self, host: &str, path: &str) -> Vec<String> {
        let mut set = BTreeSet::new();
        let mut paths = vec![path.to_string()];
        if !path.ends_with('/') {
            paths.push(format!("{path}/"));
        }
        for (pattern, _) in &self.routes {
            if pattern.method.is_empty() || (!pattern.host.is_empty() && pattern.host != host) {
                continue;
            }
            if paths.iter().any(|p| pattern.match_path(p).is_some()) {
                set.insert(pattern.method.clone());
            }
        }
        if set.contains("GET") {
            set.insert("HEAD".to_string());
        }
        set.into_iter().collect()
    }
}

impl Mux<Handler> {
    /// Registers a closure for a pattern; see `handle`.
    pub fn handle_fn(
        &mut self,
        pattern: &str,
        f: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) -> Result<(), String> {
        self.handle(pattern, Box::new(f))
    }

    /// Routes a request to its handler and returns the answer. The
    /// handler sees the request with `pattern` and the path values set.
    pub fn serve(&self, req: &Request) -> Response {
        match self.lookup(req) {
            Lookup::Found {
                handler,
                pattern,
                path_values,
            } => {
                let mut routed = req.clone();
                routed.pattern = pattern.to_string();
                routed.path_values = path_values;
                handler(&routed)
            }
            Lookup::Redirect { location } => Response::redirect(req, &location, 301),
            Lookup::MethodNotAllowed { allow } => {
                Response::error(405, "Method Not Allowed").with_header("Allow", &allow.join(", "))
            }
            Lookup::NotFound => Response::not_found(),
        }
    }

    /// Turns the mux into a handler a server can run.
    pub fn into_handler(self) -> Handler {
        Box::new(move |req| self.serve(req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &'static str) -> Handler {
        Box::new(move |r| {
            Response::text(
                200,
                format!(
                    "{name} id={} rest={}",
                    r.path_value("id"),
                    r.path_value("rest")
                ),
            )
        })
    }

    fn mux() -> Mux {
        let mut m = Mux::new();
        for (pattern, name) in [
            ("/", "root"),
            ("GET /todos", "list"),
            ("POST /todos", "create"),
            ("GET /todos/{id}", "get"),
            ("GET /todos/latest", "latest"),
            ("DELETE /todos/{id}", "delete"),
            ("/files/", "files"),
            ("/files/{rest...}", "never"),
            ("/static/{rest...}", "static"),
            ("GET /exact/{$}", "exact"),
            ("api.example.com/", "api-host"),
            ("/any", "any-method"),
        ] {
            let r = m.handle(pattern, named(name));
            if name == "never" {
                assert!(r.unwrap_err().contains("conflicts with"), "{pattern}");
            } else {
                r.unwrap_or_else(|e| panic!("{pattern}: {e}"));
            }
        }
        m
    }

    fn body(m: &Mux, method: &str, target: &str) -> (u16, String) {
        let r = m.serve(&Request::new(method, target));
        (r.status, String::from_utf8(r.body).unwrap())
    }

    #[test]
    fn precedence_and_wildcards() {
        let m = mux();
        assert_eq!(body(&m, "GET", "/todos"), (200, "list id= rest=".into()));
        assert_eq!(body(&m, "POST", "/todos"), (200, "create id= rest=".into()));
        assert_eq!(
            body(&m, "GET", "/todos/42"),
            (200, "get id=42 rest=".into())
        );
        assert_eq!(
            body(&m, "GET", "/todos/a%2Fb"),
            (200, "get id=a/b rest=".into())
        );
        assert_eq!(
            body(&m, "GET", "/todos/latest"),
            (200, "latest id= rest=".into())
        );
        assert_eq!(body(&m, "HEAD", "/todos/7"), (200, "get id=7 rest=".into()));
        assert_eq!(
            body(&m, "DELETE", "/todos/7"),
            (200, "delete id=7 rest=".into())
        );
        assert_eq!(
            body(&m, "GET", "/static/css/site.css"),
            (200, "static id= rest=css/site.css".into())
        );
        assert_eq!(
            body(&m, "GET", "/files/a/b"),
            (200, "files id= rest=".into())
        );
        assert_eq!(body(&m, "GET", "/exact/"), (200, "exact id= rest=".into()));
        assert_eq!(
            body(&m, "GET", "/exact/more"),
            (200, "root id= rest=".into())
        );
        assert_eq!(
            body(&m, "PATCH", "/any"),
            (200, "any-method id= rest=".into())
        );
        assert_eq!(
            body(&m, "GET", "/nothing/here"),
            (200, "root id= rest=".into())
        );
        let r = m.serve(&Request::new("GET", "/x").with_host("api.example.com:8443"));
        assert_eq!(r.body, b"api-host id= rest=");
    }

    #[test]
    fn method_not_allowed_and_not_found() {
        let mut m = Mux::new();
        m.handle_fn("GET /todos/{id}", |_| Response::new(200))
            .unwrap();
        m.handle_fn("DELETE /todos/{id}", |_| Response::new(204))
            .unwrap();
        m.handle_fn("POST /todos", |_| Response::new(201)).unwrap();
        let r = m.serve(&Request::new("PUT", "/todos/1"));
        assert_eq!(r.status, 405);
        assert_eq!(r.headers.get("Allow"), Some("DELETE, GET, HEAD"));
        assert_eq!(r.body, b"Method Not Allowed\n");
        let r = m.serve(&Request::new("GET", "/todos"));
        assert_eq!((r.status, r.headers.get("Allow")), (405, Some("POST")));
        let r = m.serve(&Request::new("GET", "/nope"));
        assert_eq!(r.status, 404);
        assert_eq!(r.body, b"404 page not found\n");
        assert_eq!(
            r.headers.get("Content-Type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(r.headers.get("X-Content-Type-Options"), Some("nosniff"));
        assert!(matches!(
            m.lookup(&Request::new("GET", "/nope")),
            Lookup::NotFound
        ));
    }

    #[test]
    fn redirects() {
        let m = mux();
        let r = m.serve(&Request::new("GET", "/files?x=1"));
        assert_eq!(
            (r.status, r.headers.get("Location")),
            (301, Some("/files/?x=1"))
        );
        assert_eq!(
            r.body,
            b"<a href=\"/files/?x=1\">Moved Permanently</a>.\n\n"
        );
        let r = m.serve(&Request::new("GET", "/todos/../todos//7"));
        assert_eq!(
            (r.status, r.headers.get("Location")),
            (301, Some("/todos/7"))
        );
        assert!(matches!(
            m.lookup(&Request::new("GET", "/exact")),
            Lookup::Redirect { location } if location == "/exact/"
        ));
    }

    #[test]
    fn rejects_bad_patterns() {
        let mut m: Mux<u8> = Mux::new();
        for (pattern, want) in [
            ("", "http: invalid pattern"),
            ("GET x", "parsing \"GET x\": at offset 4: host/path missing /"),
            ("/a/{x", "parsing \"/a/{x\": at offset 3: bad wildcard segment (must end with '}')"),
            ("/a/b{x}", "parsing \"/a/b{x}\": at offset 3: bad wildcard segment (must start with '{')"),
            ("/{$}/a", "parsing \"/{$}/a\": at offset 1: {$} not at end"),
            ("/{a...}/b", "parsing \"/{a...}/b\": at offset 1: {...} wildcard not at end"),
            ("/{}", "parsing \"/{}\": at offset 1: empty wildcard"),
            ("/{a}/{a}", "parsing \"/{a}/{a}\": at offset 5: duplicate wildcard name \"a\""),
            ("/{1a}", "parsing \"/{1a}\": at offset 1: bad wildcard name \"1a\""),
            ("G=T /", "parsing \"G=T /\": at offset 0: invalid method \"G=T\""),
            ("GET /a//b", "parsing \"GET /a//b\": at offset 4: non-CONNECT pattern with unclean path can never match"),
        ] {
            assert_eq!(m.handle(pattern, 0).unwrap_err(), want);
        }
        m.handle("GET /a/{x}", 1).unwrap();
        let e = m.handle("GET /a/{y}", 2).unwrap_err();
        assert!(
            e.starts_with("pattern \"GET /a/{y}\" conflicts with pattern \"GET /a/{x}\":\n"),
            "{e}"
        );
        assert!(m.handle("/a/{x}/b", 3).is_ok());
        assert!(m
            .handle("/{x}/a/b", 4)
            .unwrap_err()
            .contains("conflicts with"));
        assert!(m
            .handle("/a/b", 5)
            .unwrap_err()
            .contains("matches more methods"));
        assert_eq!(m.patterns().len(), 2);
    }

    #[test]
    fn lookup_with_values() {
        let mut m: Mux<&str> = Mux::new();
        m.handle("GET /pets/{p0}", "pets").unwrap();
        match m.lookup(&Request::new("GET", "/pets/rex")) {
            Lookup::Found {
                handler,
                pattern,
                path_values,
            } => {
                assert_eq!((*handler, pattern), ("pets", "GET /pets/{p0}"));
                assert_eq!(path_values, [("p0".to_string(), "rex".to_string())]);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            m.lookup(&Request::new("POST", "/pets/rex")),
            Lookup::MethodNotAllowed { allow } if allow == ["GET", "HEAD"]
        ));
    }

    #[test]
    fn cleaning() {
        for (give, want) in [
            ("", "/"),
            ("a", "/a"),
            ("/a/", "/a/"),
            ("//a/./b/../c", "/a/c"),
            ("/..", "/"),
            ("/a/b/..", "/a"),
        ] {
            assert_eq!(clean_path(give), want);
        }
        assert_eq!(strip_host_port("h:80"), "h");
        assert_eq!(strip_host_port("[::1]:80"), "::1");
        assert_eq!(strip_host_port("h"), "h");
    }
}
