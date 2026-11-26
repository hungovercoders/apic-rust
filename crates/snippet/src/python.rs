use apic_gocompat::quote;
use apic_runner::{FormPart, TlsInfo};

use crate::snippet::{base_name, Kind, Request, Secret};

/// Renders a script using the requests library.
pub(crate) fn python(m: &Request) -> String {
    let mut b = String::new();
    for n in m.notes(true, true, true) {
        b.push_str(&format!("# {n}\n"));
    }
    if m.cred.as_ref().is_some_and(|c| c.uses_env()) {
        b.push_str("import os\n\n");
    }
    b.push_str("import requests\n");
    if m.cred.as_ref().is_some_and(|c| c.kind == Kind::Digest) {
        b.push_str("from requests.auth import HTTPDigestAuth\n");
    }
    b.push('\n');
    let mut args = vec![py_quote(&m.method), py_quote(&m.url)];
    args.extend(py_auth_args(m));
    args.extend(py_part_args(m));
    if !m.body.is_empty() {
        args.push(format!("data={}.encode()", py_quote(&m.body)));
    }
    args.extend(py_tls_args(m.tls.as_ref()));
    if !m.proxy.is_empty() {
        args.push(format!(
            "proxies={{\"http\": {}, \"https\": {}}}",
            py_quote(&m.proxy),
            py_quote(&m.proxy)
        ));
    }
    b.push_str(&format!(
        "response = requests.request(\n    {},\n)\n",
        args.join(",\n    ")
    ));
    b.push_str("print(response.status_code)\nprint(response.text)");
    b
}

/// The headers= argument, with a header credential last, then the params=
/// or auth= one a query, basic or digest credential needs.
fn py_auth_args(m: &Request) -> Vec<String> {
    let mut args = Vec::new();
    let mut headers: Vec<String> = m
        .headers
        .iter()
        .map(|h| format!("        {}: {},", py_quote(&h.name), py_quote(&h.value)))
        .collect();
    let c = m.cred.as_ref();
    if let Some(c) = c.filter(|c| c.kind == Kind::Header) {
        headers.push(format!(
            "        {}: {},",
            py_quote(&c.name),
            py_concat(&c.prefix, &c.value)
        ));
    }
    if !headers.is_empty() {
        args.push(format!("headers={{\n{}\n    }}", headers.join("\n")));
    }
    if let Some(c) = c.filter(|c| c.kind == Kind::Query) {
        args.push(format!(
            "params={{{}: {}}}",
            py_quote(&c.name),
            py_concat(&c.prefix, &c.value)
        ));
    }
    if let Some(c) = c.filter(|c| matches!(c.kind, Kind::Basic | Kind::Digest)) {
        let pair = format!("{}, {}", py_secret(&c.user), py_secret(&c.pass));
        if c.kind == Kind::Digest {
            args.push(format!("auth=HTTPDigestAuth({pair})"));
        } else {
            args.push(format!("auth=({pair})"));
        }
    }
    args
}

/// The data= and files= arguments for a multipart body.
fn py_part_args(m: &Request) -> Vec<String> {
    let (mut args, mut data, mut files) = (Vec::new(), Vec::new(), Vec::new());
    for p in &m.parts {
        if p.file.is_empty() {
            data.push(format!(
                "        {}: {},",
                py_quote(&p.name),
                py_quote(m.part_value(p))
            ));
            continue;
        }
        files.push(format!("        {}: {}),", py_quote(&p.name), py_file(p)));
    }
    if !data.is_empty() {
        args.push(format!("data={{\n{}\n    }}", data.join("\n")));
    }
    if !files.is_empty() {
        args.push(format!("files={{\n{}\n    }}", files.join("\n")));
    }
    args
}

/// A file part's tuple, less its closing parenthesis.
fn py_file(p: &FormPart) -> String {
    let name = if p.filename.is_empty() {
        base_name(&p.file)
    } else {
        &p.filename
    };
    let mut entry = format!("({}, open({}, \"rb\")", py_quote(name), py_quote(&p.file));
    if !p.content_type.is_empty() {
        entry.push_str(", ");
        entry.push_str(&py_quote(&p.content_type));
    }
    entry
}

/// The verify= and cert= arguments for apic's TLS settings.
fn py_tls_args(t: Option<&TlsInfo>) -> Vec<String> {
    let Some(t) = t else {
        return Vec::new();
    };
    let mut args = Vec::new();
    if t.insecure {
        args.push("verify=False".to_string());
    } else if !t.ca_file.is_empty() {
        args.push(format!("verify={}", py_quote(&t.ca_file)));
    }
    if !t.cert_file.is_empty() {
        if !t.key_file.is_empty() && t.key_file != t.cert_file {
            args.push(format!(
                "cert=({}, {})",
                py_quote(&t.cert_file),
                py_quote(&t.key_file)
            ));
        } else {
            args.push(format!("cert={}", py_quote(&t.cert_file)));
        }
    }
    args
}

/// A Python string literal: the escapes written (\n, \t, \xhh, \uhhhh,
/// \Uhhhhhhhh) are Python's too.
fn py_quote(s: &str) -> String {
    quote(s)
}

fn py_secret(v: &Secret) -> String {
    if !v.env.is_empty() {
        return format!("os.environ[{}]", py_quote(&v.env));
    }
    py_quote(&v.literal)
}

fn py_concat(prefix: &str, v: &Secret) -> String {
    if !v.env.is_empty() {
        if prefix.is_empty() {
            return py_secret(v);
        }
        return format!("{} + {}", py_quote(prefix), py_secret(v));
    }
    py_quote(&format!("{}{}", prefix, v.literal))
}
