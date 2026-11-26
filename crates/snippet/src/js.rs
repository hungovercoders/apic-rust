use apic_gocompat::json_string;

use crate::snippet::{base_name, Kind, Request, Secret};

/// Renders fetch for Node 18+ (an ES module, for the top-level await and
/// the file reads a multipart body needs) or a browser.
pub(crate) fn js(m: &Request) -> String {
    let mut b = String::new();
    for n in m.notes(false, false, false) {
        b.push_str(&format!("// {n}\n"));
    }
    if m.parts.iter().any(|p| !p.file.is_empty()) {
        b.push_str("import { readFile } from \"node:fs/promises\";\n\n");
    }
    let c = m.cred.as_ref();
    let mut url = js_quote(&m.url);
    if let Some(c) = c.filter(|c| c.kind == Kind::Query) {
        b.push_str(&format!("const url = new URL({});\n", js_quote(&m.url)));
        b.push_str(&format!(
            "url.searchParams.append({}, {});\n",
            js_quote(&c.name),
            js_concat(&c.prefix, &c.value)
        ));
        url = "url".to_string();
    }
    let mut headers: Vec<String> = m
        .headers
        .iter()
        .map(|h| format!("    {}: {},", js_quote(&h.name), js_quote(&h.value)))
        .collect();
    if let Some(c) = c {
        match c.kind {
            Kind::Header => headers.push(format!(
                "    {}: {},",
                js_quote(&c.name),
                js_concat(&c.prefix, &c.value)
            )),
            Kind::Basic => headers.push(format!(
                "    \"Authorization\": \"Basic \" + btoa({} + \":\" + {}),",
                js_secret(&c.user),
                js_secret(&c.pass)
            )),
            _ => {}
        }
    }
    if !m.parts.is_empty() {
        b.push_str("const form = new FormData();\n");
        for p in &m.parts {
            if p.file.is_empty() {
                b.push_str(&format!(
                    "form.append({}, {});\n",
                    js_quote(&p.name),
                    js_quote(m.part_value(p))
                ));
                continue;
            }
            let name = if p.filename.is_empty() {
                base_name(&p.file)
            } else {
                &p.filename
            };
            let opts = if p.content_type.is_empty() {
                String::new()
            } else {
                format!(", {{ type: {} }}", js_quote(&p.content_type))
            };
            b.push_str(&format!(
                "form.append({}, new Blob([await readFile({})]{}), {});\n",
                js_quote(&p.name),
                js_quote(&p.file),
                opts,
                js_quote(name)
            ));
        }
    }
    let mut opts = vec![format!("  method: {},", js_quote(&m.method))];
    if !headers.is_empty() {
        opts.push(format!("  headers: {{\n{}\n  }},", headers.join("\n")));
    }
    if !m.parts.is_empty() {
        opts.push("  body: form,".to_string());
    } else if !m.body.is_empty() {
        opts.push(format!("  body: {},", js_quote(&m.body)));
    }
    b.push_str(&format!(
        "const response = await fetch({}, {{\n{}\n}});\n",
        url,
        opts.join("\n")
    ));
    b.push_str("console.log(response.status);\nconsole.log(await response.text());");
    b
}

/// A JavaScript string literal (JSON's syntax is a subset), without
/// JSON's escaping of <, > and &, which a URL is full of.
fn js_quote(s: &str) -> String {
    json_string(s, false)
}

fn js_secret(v: &Secret) -> String {
    if !v.env.is_empty() {
        return format!("process.env.{}", v.env);
    }
    js_quote(&v.literal)
}

fn js_concat(prefix: &str, v: &Secret) -> String {
    if !v.env.is_empty() {
        if prefix.is_empty() {
            return js_secret(v);
        }
        return format!("{} + {}", js_quote(prefix), js_secret(v));
    }
    js_quote(&format!("{}{}", prefix, v.literal))
}
