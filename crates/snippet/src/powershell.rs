use apic_httpfile::{HTTP1, HTTP2};

use crate::snippet::{Kind, Request, Secret};

/// Renders Invoke-RestMethod for PowerShell 7, whose -Form,
/// -SkipCertificateCheck and -HttpVersion the snippet uses.
pub(crate) fn powershell(m: &Request) -> String {
    let mut b = String::new();
    // -SkipCertificateCheck covers --insecure; a CA file or client
    // certificate is not something Invoke-RestMethod takes as a path.
    let tls_done = m
        .tls
        .as_ref()
        .is_none_or(|t| t.ca_file.is_empty() && t.cert_file.is_empty());
    // -Credential answers digest
    for n in m.notes(tls_done, true, true) {
        b.push_str(&format!("# {n}\n"));
    }
    if let Some(t) = m.tls.as_ref().filter(|t| !t.cert_file.is_empty()) {
        b.push_str(&format!(
            "# present the client certificate with -Certificate (Get-PfxCertificate {})\n",
            ps_quote(&t.cert_file)
        ));
    }
    let c = m.cred.as_ref();
    let mut uri = ps_quote(&m.url);
    if let Some(c) = c.filter(|c| c.kind == Kind::Query) {
        let sep = if m.url.contains('?') { "&" } else { "?" };
        if !c.value.env.is_empty() {
            uri = format!(
                "{} + [uri]::EscapeDataString({})",
                ps_quote(&format!(
                    "{}{}{}={}",
                    m.url,
                    sep,
                    query_escape(&c.name),
                    query_escape(&c.prefix)
                )),
                ps_secret(&c.value)
            );
        } else {
            uri = ps_quote(&format!(
                "{}{}{}={}",
                m.url,
                sep,
                query_escape(&c.name),
                query_escape(&format!("{}{}", c.prefix, c.value.literal))
            ));
        }
    }
    let mut content_type = "";
    let mut headers = Vec::new();
    for h in &m.headers {
        if h.name.eq_ignore_ascii_case("Content-Type") {
            content_type = &h.value;
            continue;
        }
        headers.push(format!("  {} = {}", ps_quote(&h.name), ps_quote(&h.value)));
    }
    if let Some(c) = c {
        match c.kind {
            Kind::Header => headers.push(format!(
                "  {} = {}",
                ps_quote(&c.name),
                ps_concat(&c.prefix, &c.value)
            )),
            Kind::Basic => headers.push(format!(
                "  'Authorization' = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes({} + ':' + {}))",
                ps_secret(&c.user),
                ps_secret(&c.pass)
            )),
            _ => {}
        }
    }
    let mut args = vec![ps_method(&m.method), format!("-Uri {uri}")];
    if !headers.is_empty() {
        b.push_str(&format!("$headers = @{{\n{}\n}}\n", headers.join("\n")));
        args.push("-Headers $headers".to_string());
    }
    if let Some(c) = c.filter(|c| c.kind == Kind::Digest) {
        b.push_str(&format!(
            "$credential = [pscredential]::new({}, (ConvertTo-SecureString {} -AsPlainText -Force))\n",
            ps_secret(&c.user),
            ps_secret(&c.pass)
        ));
        args.push("-Credential $credential".to_string());
        args.push("-AllowUnencryptedAuthentication".to_string());
    }
    if !m.parts.is_empty() {
        let form: Vec<String> = m
            .parts
            .iter()
            .map(|p| {
                if !p.file.is_empty() {
                    format!("  {} = Get-Item {}", ps_quote(&p.name), ps_quote(&p.file))
                } else {
                    format!("  {} = {}", ps_quote(&p.name), ps_quote(m.part_value(p)))
                }
            })
            .collect();
        b.push_str(&format!("$form = @{{\n{}\n}}\n", form.join("\n")));
        args.push("-Form $form".to_string());
    }
    if !m.body.is_empty() {
        b.push_str(&format!("$body = {}\n", ps_quote(&m.body)));
        args.push("-Body $body".to_string());
    }
    if !content_type.is_empty() {
        args.push(format!("-ContentType {}", ps_quote(content_type)));
    }
    if m.http == HTTP1 {
        args.push("-HttpVersion 1.1".to_string());
    } else if m.http == HTTP2 {
        args.push("-HttpVersion 2.0".to_string());
    }
    if m.tls.as_ref().is_some_and(|t| t.insecure) {
        args.push("-SkipCertificateCheck".to_string());
    }
    if !m.proxy.is_empty() {
        args.push(format!("-Proxy {}", ps_quote(&m.proxy)));
    }
    b.push_str(&format!("Invoke-RestMethod {}", args.join(" `\n  ")));
    b
}

/// A single-quoted PowerShell string: nothing inside expands, and a quote
/// is doubled.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn ps_secret(v: &Secret) -> String {
    if !v.env.is_empty() {
        return format!("$env:{}", v.env);
    }
    ps_quote(&v.literal)
}

fn ps_concat(prefix: &str, v: &Secret) -> String {
    if !v.env.is_empty() {
        if prefix.is_empty() {
            return format!("$env:{}", v.env);
        }
        return format!("{} + $env:{}", ps_quote(prefix), v.env);
    }
    ps_quote(&format!("{}{}", prefix, v.literal))
}

/// -Method Get, Post, … for the methods PowerShell names, and
/// -CustomMethod for any other verb.
fn ps_method(m: &str) -> String {
    match m {
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" | "TRACE" => {
            format!("-Method {}{}", &m[..1], m[1..].to_lowercase())
        }
        _ => format!("-CustomMethod {}", ps_quote(m)),
    }
}

/// Escapes text for a URL query: a space is `+`, and every byte but the
/// unreserved ones (letters, digits, `-`, `_`, `.`, `~`) is `%XX`.
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_escape_matches_form_encoding() {
        assert_eq!(query_escape("a b&c=d/é~"), "a+b%26c%3Dd%2F%C3%A9~");
        assert_eq!(ps_method("PURGE"), "-CustomMethod 'PURGE'");
        assert_eq!(ps_method("DELETE"), "-Method Delete");
    }
}
