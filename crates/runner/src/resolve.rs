//! Turning a request as written into the request to send: placeholders
//! substituted, the body assembled, the auth spec rendered.

use std::cell::RefCell;
use std::sync::Arc;

use apic_gocompat::quote;
use apic_httpc::canonical_header_key;
use apic_httpfile::{Header, Multipart, Request, GRAPHQL_HEADER};
use apic_project::path;
use apic_template::RenderError;

use crate::errors::{uerr, Code, Error};
use crate::result::{FormPart, Resolved, Result};
use crate::runner::{dedupe, plural, uctx};
use crate::Runner;

/// Renders the texts of one request, collecting the placeholders that
/// have no value instead of failing on them.
struct Renderer<'a> {
    r: &'a Runner,
    req: &'a Arc<Request>,
    missing: RefCell<Vec<String>>,
}

impl Renderer<'_> {
    fn render(&self, s: &str) -> std::result::Result<String, Error> {
        match apic_template::render(s, |e| self.r.resolve_expr(Some(self.req), e)) {
            Ok(out) => Ok(out),
            Err(RenderError::Missing(me)) => {
                self.missing.borrow_mut().extend(me.exprs);
                Ok(me.output)
            }
            Err(RenderError::Resolve {
                expr,
                source,
                output,
            }) => {
                // A file variable whose own value names an unknown one.
                if let Some(me) = source.missing() {
                    self.missing.borrow_mut().extend(me.exprs.iter().cloned());
                    return Ok(output);
                }
                Err(Error::context(format!("{{{{{expr}}}}}"), source))
            }
        }
    }
}

fn valid_boundary(b: &str) -> std::result::Result<(), &'static str> {
    if b.is_empty() || b.len() > 70 {
        return Err("mime: invalid boundary length");
    }
    let last = b.len() - 1;
    for (i, c) in b.bytes().enumerate() {
        let ok = c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'\'' | b'(' | b')' | b'+' | b'_' | b',' | b'-' | b'.' | b'/' | b':' | b'=' | b'?'
            )
            || (c == b' ' && i != last);
        if !ok {
            return Err("mime: invalid boundary character");
        }
    }
    Ok(())
}

impl Runner {
    /// Resolves a request as `run` would and stops there: no dependency
    /// runs, no auth is applied, nothing is sent and nothing is captured.
    /// A variable a `# @ref` dependency would capture is left as its
    /// placeholder with a warning saying so, since the run would supply
    /// it; any other missing variable is the error the run would give.
    /// The result carries the request and `dry_run` set, for an agent (or
    /// a person) to look at before a call that changes something.
    pub fn dry_run(&self, req: &Arc<Request>) -> std::result::Result<Result, Error> {
        let resolved = self.resolve(req)?;
        let unresolved = resolved.missing.clone();
        let mut result = Result {
            request: resolved,
            ok: true,
            redact: self.opts.redact,
            dry_run: true,
            ..Result::for_request(req)
        };
        if unresolved.is_empty() {
            return Ok(result);
        }
        let description = self.describe(req);
        let mut missing = Vec::new();
        for e in unresolved {
            let Some(v) = description
                .variables
                .iter()
                .rev()
                .find(|v| v.ref_runs && v.name == e)
            else {
                missing.push(e);
                continue;
            };
            let mut by = v.captured_by.as_str();
            if by.is_empty() {
                // A `login.response.body.$.token` reference.
                by = e
                    .split_once(".response.")
                    .map_or(e.as_str(), |(name, _)| name);
            }
            result.warnings.push(format!(
                "{{{{{e}}}}} is not set; a run would send {by} first (# @ref) and capture it"
            ));
        }
        if !missing.is_empty() {
            return Err(self.missing_error(req, &missing));
        }
        Ok(result)
    }

    /// Substitutes variables in a request without sending it.
    pub fn resolve(&self, req: &Arc<Request>) -> std::result::Result<Resolved, Error> {
        let at = format!("{}:{}", req.file.path, req.line);
        let mut res = Resolved {
            name: req.name.clone(),
            file: req.file.path.clone(),
            line: req.line,
            method: req.method.clone(),
            // An unsupported version is refused when sending.
            http_version: req.protocol().unwrap_or_default().to_string(),
            ..Resolved::default()
        };
        let rr = Renderer {
            r: self,
            req,
            missing: RefCell::new(Vec::new()),
        };
        res.url = rr
            .render(req.url.trim())
            .map_err(|e| uctx(Code::Build, at.clone(), &e))?;
        res.tls = self.tls_info_for_url(&res.url);
        res.proxy = self.proxy_info(&res.url);
        let uses_secret = |text: &str| {
            apic_template::exprs(text)
                .iter()
                .any(|e| self.resolve_expr_meta(Some(req), e, 0).secret)
        };
        res.secret = uses_secret(&req.url);
        let graphql = req.is_graphql();
        for h in &req.headers {
            if graphql && h.name.eq_ignore_ascii_case(GRAPHQL_HEADER) {
                continue; // an editor marker, never sent
            }
            let v = rr
                .render(&h.value)
                .map_err(|e| uctx(Code::Build, format!("{at}: header {}", h.name), &e))?;
            res.headers.push(Header {
                name: h.name.clone(),
                value: v,
            });
            if uses_secret(&h.value) {
                res.secret_headers.insert(h.name.clone());
                res.secret = true;
            }
        }
        let mut body = req.body.clone();
        let mut templated = true;
        let mut file_bytes = None;
        if !req.body_file.is_empty() {
            let data = self.read_body_file(req, &req.body_file, "body file")?;
            body = String::from_utf8_lossy(&data).into_owned();
            templated = req.body_file_templated;
            file_bytes = Some(data);
        }
        if templated && uses_secret(&body) {
            res.secret = true; // a password in a login body, a token in a multipart part
        }
        if graphql {
            // The query and the variables become the JSON envelope a
            // GraphQL server reads, sent as a POST whatever the request
            // line said.
            res.body = self.graphql_body(req, &body, templated, &rr)?;
            res.method = "POST".to_string();
            if req.header("Content-Type").is_none() {
                res.headers.push(Header {
                    name: "Content-Type".to_string(),
                    value: "application/json".to_string(),
                });
            }
            body.clear();
        } else if !req.body_file.is_empty() && !templated {
            // The file goes out byte for byte, whatever it holds.
            res.body = std::mem::take(&mut body);
            res.raw_body = file_bytes;
        }
        match req.multipart() {
            Err(e) => return Err(uerr(Code::InvalidFile, format!("{at}: {e}"))),
            Ok(Some(m)) => {
                let (raw, parts) = self.multipart_body(req, &m, &rr)?;
                res.raw_body = Some(raw);
                res.parts = parts;
                res.body = format!(
                    "<multipart: {} part{}, {} file{}>",
                    m.parts.len(),
                    plural(m.parts.len()),
                    m.files(),
                    plural(m.files())
                );
                body.clear();
            }
            Ok(None) => {}
        }
        if !body.is_empty() {
            res.body = rr
                .render(&body)
                .map_err(|e| uctx(Code::Build, format!("{at}: body"), &e))?;
        }
        if let Some(spec) = self.auth_spec(req)? {
            let rendered = spec
                .render(|s| rr.render(s))
                .map_err(|e| uctx(Code::Auth, format!("{at}: @auth"), &e))?;
            res.auth = spec.r#type.clone();
            res.auth_spec = Some(rendered);
            res.secret = true;
        }
        res.missing = dedupe(rr.missing.into_inner());
        Ok(res)
    }

    /// Builds the `{"query", "variables"}` body of a GraphQL request from
    /// the text of its body (inline or from a file), rendering the
    /// placeholders in both halves when templated.
    fn graphql_body(
        &self,
        req: &Request,
        body: &str,
        templated: bool,
        rr: &Renderer<'_>,
    ) -> std::result::Result<String, Error> {
        let at = format!("{}:{}", req.file.path, req.line);
        let (mut query, mut variables) = apic_httpfile::split_graphql(body);
        if query.is_empty() {
            return Err(uerr(
                Code::InvalidFile,
                format!("{at}: a GraphQL request needs a query in its body"),
            ));
        }
        if templated {
            query = rr
                .render(&query)
                .map_err(|e| uctx(Code::Build, format!("{at}: body"), &e))?;
            variables = rr
                .render(&variables)
                .map_err(|e| uctx(Code::Build, format!("{at}: variables"), &e))?;
        }
        apic_httpfile::graphql_envelope(&query, &variables)
            .map_err(|e| uerr(Code::Build, format!("{at}: {e}")))
    }

    /// Returns the parsed auth spec for a request: its own `# @auth`
    /// directive, else auth.default from apic.yaml, else `None`.
    fn auth_spec(&self, req: &Request) -> std::result::Result<Option<apic_auth::Spec>, Error> {
        let (raw, place) = match req.directive("auth") {
            Some(raw) => (raw, format!("{}:{}", req.file.path, req.line)),
            None => {
                let raw = self.project.config.auth.default.as_str();
                if raw.trim().is_empty() {
                    return Ok(None);
                }
                (raw, format!("{} auth.default", apic_project::CONFIG_FILE))
            }
        };
        let spec = apic_auth::parse(raw).map_err(|e| uerr(Code::Auth, format!("{place}: {e}")))?;
        Ok((spec.r#type != "none").then_some(spec))
    }

    /// Reports the auth spec template for describe: the raw text and
    /// where it was declared; both empty when there is none.
    pub fn auth_source(&self, req: &Request) -> (String, String) {
        if let Some(v) = req.directive("auth") {
            return (v.to_string(), "request".to_string());
        }
        let default = &self.project.config.auth.default;
        if !default.is_empty() {
            return (default.clone(), apic_project::CONFIG_FILE.to_string());
        }
        (String::new(), String::new())
    }

    /// Assembles a multipart/form-data body from its parts: text parts
    /// are rendered as templates, `< file` parts read the file (rendered
    /// too for `<@`). The result is binary-safe and uses the boundary the
    /// file declares, so the Content-Type header written in the file is
    /// right.
    fn multipart_body(
        &self,
        req: &Request,
        m: &Multipart,
        rr: &Renderer<'_>,
    ) -> std::result::Result<(Vec<u8>, Vec<FormPart>), Error> {
        let file = &req.file.path;
        if let Err(e) = valid_boundary(&m.boundary) {
            return Err(uerr(
                Code::InvalidFile,
                format!(
                    "{file}:{}: multipart boundary {}: {e}",
                    req.line,
                    quote(&m.boundary)
                ),
            ));
        }
        let mut buf: Vec<u8> = Vec::new();
        let mut parts = Vec::with_capacity(m.parts.len());
        for (n, p) in m.parts.iter().enumerate() {
            let mut hdr: Vec<(String, String)> = Vec::new();
            let mut fp = FormPart {
                name: p.name.clone(),
                filename: p.filename.clone(),
                content_type: p.content_type.clone(),
                ..FormPart::default()
            };
            for h in &p.headers {
                let v = rr.render(&h.value).map_err(|e| {
                    uctx(
                        Code::Build,
                        format!("{file}:{}: part header {}", p.line, h.name),
                        &e,
                    )
                })?;
                if h.name.eq_ignore_ascii_case("Content-Disposition") {
                    if let Ok((_, params)) = apic_httpfile::parse_media_type(&v) {
                        fp.name = params.get("name").cloned().unwrap_or_default();
                        fp.filename = params.get("filename").cloned().unwrap_or_default();
                    }
                } else if h.name.eq_ignore_ascii_case("Content-Type") {
                    fp.content_type = v.clone();
                }
                hdr.push((canonical_header_key(&h.name), v));
            }
            let data = if !p.file.is_empty() {
                let mut data = self.read_body_file(req, &p.file, "part file")?;
                if p.file_templated {
                    data = rr
                        .render(&String::from_utf8_lossy(&data))
                        .map_err(|e| {
                            uctx(
                                Code::BodyFile,
                                format!("{file}:{}: part file {}", p.file_line, p.file),
                                &e,
                            )
                        })?
                        .into_bytes();
                }
                fp.file = path::to_slash(&path::join(&path::dir(file), &p.file));
                data
            } else {
                let s = rr.render(&p.body).map_err(|e| {
                    uctx(
                        Code::Build,
                        format!("{file}:{}: part {}", p.line, p.name),
                        &e,
                    )
                })?;
                fp.value = s.clone();
                s.into_bytes()
            };
            fp.size = data.len();
            if n > 0 {
                buf.extend_from_slice(b"\r\n");
            }
            buf.extend_from_slice(format!("--{}\r\n", m.boundary).as_bytes());
            // Headers by name, the values of one name in the order written.
            hdr.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in &hdr {
                buf.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
            }
            buf.extend_from_slice(b"\r\n");
            buf.extend_from_slice(&data);
            parts.push(fp);
        }
        buf.extend_from_slice(format!("\r\n--{}--\r\n", m.boundary).as_bytes());
        Ok((buf, parts))
    }
}
