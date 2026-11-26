use apic_auth::Spec;
use apic_httpfile::{HTTP1, HTTP2};
use apic_runner::{FormPart, ProxyInfo, Resolved, TlsInfo, MASKED};

use crate::snippet::{exec_header, path_base};

/// Returns a POSIX-shell curl command for the request. With redact set
/// the header values, body and query values are masked and the bearer and basic
/// credentials become shell placeholders, so the command can go into a stored
/// log; without it the command is runnable as printed.
pub fn curl(r: &Resolved, redact: bool) -> String {
    let mut parts = vec!["curl -sS".to_string()];
    let body = r.display_body(redact);
    if (r.method != "GET" && (r.method != "POST" || body.is_empty()))
        || (r.method == "GET" && !body.is_empty())
    {
        parts.push(format!("-X {}", r.method));
    }
    // Without redact the command must stay runnable as printed, so headers go
    // out verbatim — including Authorization, which display_headers masks
    // unconditionally for ordinary output. Redacting swaps in that masking.
    let headers = if redact {
        r.display_headers(true)
    } else {
        r.headers.clone()
    };
    for h in &headers {
        if !r.parts.is_empty() && h.name.eq_ignore_ascii_case("Content-Type") {
            continue; // -F sets multipart/form-data with curl's own boundary
        }
        parts.push(format!("-H {}", quote(&format!("{}: {}", h.name, h.value))));
    }
    if !r.parts.is_empty() {
        parts.extend(form_flags(&r.parts, redact));
    } else if !body.is_empty() {
        parts.push(format!("--data-raw {}", quote(&body)));
    }
    parts.extend(auth_flags(r.auth_spec.as_ref(), redact));
    if r.http_version == HTTP1 {
        parts.push("--http1.1".to_string());
    } else if r.http_version == HTTP2 {
        parts.push("--http2".to_string());
    }
    parts.extend(tls_flags(r.tls.as_ref()));
    parts.extend(proxy_flags(r.proxy.as_ref(), redact));
    parts.push(quote(&r.display_url(redact)));
    parts.join(" \\\n  ")
}

/// Maps the parts of a multipart body onto curl's form options: a text
/// part as --form-string name=value (which, unlike -F, never reads a
/// value starting with @ or < as a file), a file part as -F name=@path with
/// curl's own ;type= and ;filename= suffixes where the file declares them.
/// Under redact the text values are masked; a path is not a secret.
fn form_flags(parts: &[FormPart], redact: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        if p.file.is_empty() {
            let v = if redact { MASKED } else { &p.value };
            out.push(format!(
                "--form-string {}",
                quote(&format!("{}={}", p.name, v))
            ));
            continue;
        }
        let mut spec = format!("{}=@{}", p.name, p.file);
        if !p.content_type.is_empty() {
            spec.push_str(";type=");
            spec.push_str(&p.content_type);
        }
        if !p.filename.is_empty() && p.filename != path_base(&p.file) {
            spec.push_str(";filename=");
            spec.push_str(&p.filename);
        }
        out.push(format!("-F {}", quote(&spec)));
    }
    out
}

/// Maps the request's TLS setup onto curl's own options.
fn tls_flags(t: Option<&TlsInfo>) -> Vec<String> {
    let Some(t) = t else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if !t.ca_file.is_empty() {
        out.push(format!("--cacert {}", quote(&t.ca_file)));
    }
    if !t.cert_file.is_empty() {
        out.push(format!("--cert {}", quote(&t.cert_file)));
        if !t.key_file.is_empty() && t.key_file != t.cert_file {
            out.push(format!("--key {}", quote(&t.key_file)));
        }
    }
    if t.insecure {
        out.push("--insecure".to_string());
    }
    out
}

/// Maps the proxy in effect onto curl: --noproxy '*' when apic would send
/// directly although one is configured, --proxy otherwise. Under redact
/// the proxy's own credentials are masked with the rest.
fn proxy_flags(p: Option<&ProxyInfo>, redact: bool) -> Vec<String> {
    let Some(p) = p else {
        return Vec::new();
    };
    if p.off {
        return vec!["--noproxy '*'".to_string()];
    }
    match p.raw() {
        Some(raw) if !redact => vec![format!("--proxy {}", quote(raw))],
        _ => vec![format!("--proxy {}", quote(&p.url))],
    }
}

/// A single-quoted POSIX-shell word.
pub(crate) fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Maps an auth spec onto curl's own options where curl has them. The
/// aws, oauth2 and exec branches never embed a credential; under redact
/// bearer and basic join them, so every branch is placeholder-only.
fn auth_flags(s: Option<&Spec>, redact: bool) -> Vec<String> {
    let Some(s) = s else {
        return Vec::new();
    };
    match s.r#type.as_str() {
        "bearer" => {
            if redact {
                return vec![r#"-H "Authorization: Bearer $TOKEN""#.to_string()];
            }
            vec![format!(
                "-H {}",
                quote(&format!("Authorization: Bearer {}", s.args[0]))
            )]
        }
        "basic" => {
            if redact {
                return vec![r#"--user "$APIC_USER:$APIC_PASSWORD""#.to_string()];
            }
            vec![format!(
                "--user {}",
                quote(&format!("{}:{}", s.args[0], s.args[1]))
            )]
        }
        "apikey" => {
            let (header, query) = s.api_key_placement();
            let prefix = s.option("prefix");
            if !query.is_empty() {
                if redact {
                    return vec![format!(
                        "--url-query \"{}={}$APIC_API_KEY\"",
                        escape_double(&query),
                        escape_double(prefix)
                    )];
                }
                return vec![format!(
                    "--url-query {}",
                    quote(&format!("{}={}{}", query, prefix, s.args[0]))
                )];
            }
            if redact {
                return vec![format!(
                    "-H \"{}: {}$APIC_API_KEY\"",
                    escape_double(&header),
                    escape_double(prefix)
                )];
            }
            vec![format!(
                "-H {}",
                quote(&format!("{}: {}{}", header, prefix, s.args[0]))
            )]
        }
        "digest" => {
            if redact {
                return vec![r#"--digest --user "$APIC_USER:$APIC_PASSWORD""#.to_string()];
            }
            vec![format!(
                "--digest --user {}",
                quote(&format!("{}:{}", s.args[0], s.args[1]))
            )]
        }
        "aws" => {
            let mut service = s.option("service");
            if service.is_empty() {
                service = "execute-api";
            }
            let region = s.option("region");
            let sigv4 = if region.is_empty() {
                format!(
                    "--aws-sigv4 \"aws:amz:$AWS_REGION:{}\"",
                    escape_double(service)
                )
            } else {
                format!(
                    "--aws-sigv4 {}",
                    quote(&format!("aws:amz:{region}:{service}"))
                )
            };
            vec![
                sigv4,
                r#"--user "$AWS_ACCESS_KEY_ID:$AWS_SECRET_ACCESS_KEY""#.to_string(),
                "${AWS_SESSION_TOKEN:+-H}".to_string(),
                "${AWS_SESSION_TOKEN:+x-amz-security-token:$AWS_SESSION_TOKEN}".to_string(),
            ]
        }
        "oauth2" => vec![r#"-H "Authorization: Bearer $TOKEN""#.to_string()],
        "exec" => {
            let (header, prefix) = exec_header(s);
            let args: Vec<String> = s.args.iter().map(|a| quote(a)).collect();
            vec![format!(
                "-H {}$({})",
                quote(&format!("{header}: {prefix}")),
                args.join(" ")
            )]
        }
        _ => Vec::new(),
    }
}

/// Escapes what a double-quoted shell word would otherwise expand.
pub(crate) fn escape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
