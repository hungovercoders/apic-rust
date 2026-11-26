use std::collections::BTreeSet;

use apic_gocompat::quote as go_quote;

use crate::snippet::{base_name, Kind, Request, Secret};

/// Renders a Go program using net/http, written as gofmt would write it
/// (a test holds it to that where gofmt is installed).
pub(crate) fn go_code(m: &Request) -> String {
    let mut imports: BTreeSet<&str> = BTreeSet::from(["fmt", "io", "net/http"]);
    let mut body = String::new();
    let mut w = |line: String| {
        body.push_str(&line);
        body.push('\n');
    };
    let c = m.cred.as_ref();

    if !m.parts.is_empty() {
        imports.insert("bytes");
        imports.insert("mime/multipart");
        w("var buf bytes.Buffer".to_string());
        w("form := multipart.NewWriter(&buf)".to_string());
        for p in &m.parts {
            if p.file.is_empty() {
                w(format!(
                    "if err := form.WriteField({}, {}); err != nil {{\npanic(err)\n}}",
                    go_quote(&p.name),
                    go_quote(m.part_value(p))
                ));
                continue;
            }
            imports.insert("os");
            imports.insert("net/textproto");
            let name = if p.filename.is_empty() {
                base_name(&p.file)
            } else {
                &p.filename
            };
            let ct = if p.content_type.is_empty() {
                "application/octet-stream"
            } else {
                &p.content_type
            };
            w("{".to_string());
            w(format!(
                "data, err := os.ReadFile({})\nif err != nil {{\npanic(err)\n}}",
                go_quote(&p.file)
            ));
            w("h := textproto.MIMEHeader{}".to_string());
            w(format!(
                "h.Set(\"Content-Disposition\", {})",
                go_quote(&format!(
                    "form-data; name={}; filename={}",
                    go_quote(&p.name),
                    go_quote(name)
                ))
            ));
            w(format!("h.Set(\"Content-Type\", {})", go_quote(ct)));
            w("part, err := form.CreatePart(h)\nif err != nil {\npanic(err)\n}".to_string());
            w("if _, err := part.Write(data); err != nil {\npanic(err)\n}".to_string());
            w("}".to_string());
        }
        w("if err := form.Close(); err != nil {\npanic(err)\n}".to_string());
        w(format!(
            "req, err := http.NewRequest({}, {}, &buf)",
            go_quote(&m.method),
            go_quote(&m.url)
        ));
    } else if !m.body.is_empty() {
        imports.insert("strings");
        w(format!(
            "req, err := http.NewRequest({}, {}, strings.NewReader({}))",
            go_quote(&m.method),
            go_quote(&m.url),
            go_quote(&m.body)
        ));
    } else {
        w(format!(
            "req, err := http.NewRequest({}, {}, nil)",
            go_quote(&m.method),
            go_quote(&m.url)
        ));
    }
    w("if err != nil {\npanic(err)\n}".to_string());
    for h in &m.headers {
        if h.name.eq_ignore_ascii_case("Host") {
            w(format!("req.Host = {}", go_quote(&h.value)));
            continue;
        }
        w(format!(
            "req.Header.Add({}, {})",
            go_quote(&h.name),
            go_quote(&h.value)
        ));
    }
    if !m.parts.is_empty() {
        w("req.Header.Set(\"Content-Type\", form.FormDataContentType())".to_string());
    }
    if let Some(c) = c {
        if c.uses_env() {
            imports.insert("os");
        }
        match c.kind {
            Kind::Header => w(format!(
                "req.Header.Set({}, {})",
                go_quote(&c.name),
                go_concat(&c.prefix, &c.value)
            )),
            Kind::Query => w(format!(
                "q := req.URL.Query()\nq.Add({}, {})\nreq.URL.RawQuery = q.Encode()",
                go_quote(&c.name),
                go_concat(&c.prefix, &c.value)
            )),
            Kind::Basic => w(format!(
                "req.SetBasicAuth({}, {})",
                go_secret(&c.user),
                go_secret(&c.pass)
            )),
            _ => {}
        }
    }
    let mut client = "http.DefaultClient";
    let t = m.tls.as_ref();
    let insecure = t.is_some_and(|t| t.insecure);
    if insecure || !m.proxy.is_empty() {
        client = "client";
        w("transport := http.DefaultTransport.(*http.Transport).Clone()".to_string());
        if insecure {
            imports.insert("crypto/tls");
            w(
                "transport.TLSClientConfig = &tls.Config{InsecureSkipVerify: true} // as apic --insecure"
                    .to_string(),
            );
        }
        if !m.proxy.is_empty() {
            imports.insert("net/url");
            w(format!(
                "proxy, err := url.Parse({})\nif err != nil {{\npanic(err)\n}}",
                go_quote(&m.proxy)
            ));
            w("transport.Proxy = http.ProxyURL(proxy)".to_string());
        }
        w("client := &http.Client{Transport: transport}".to_string());
    }
    w(format!(
        "resp, err := {client}.Do(req)\nif err != nil {{\npanic(err)\n}}"
    ));
    w("defer resp.Body.Close()".to_string());
    w("data, err := io.ReadAll(resp.Body)\nif err != nil {\npanic(err)\n}".to_string());
    w("fmt.Println(resp.Status)\nfmt.Println(string(data))".to_string());

    let mut src = String::new();
    // Only --insecure and the proxy are carried over; a CA or client
    // certificate would need a tls.Config built from the files.
    let tls_done = t.is_none_or(|t| t.ca_file.is_empty() && t.cert_file.is_empty());
    for n in m.notes(tls_done, true, false) {
        src.push_str(&format!("// {n}\n"));
    }
    src.push_str("package main\n\nimport (\n");
    for name in &imports {
        src.push_str(&format!("\t{}\n", go_quote(name)));
    }
    src.push_str(&format!(")\n\nfunc main() {{\n{}}}", indent(&body)));
    src
}

/// Puts each line of a function body at its brace depth, one tab per
/// level inside func main.
fn indent(body: &str) -> String {
    let mut b = String::new();
    let mut depth: usize = 1;
    for line in body.trim_end_matches('\n').split('\n') {
        if line.starts_with('}') {
            depth = depth.saturating_sub(1);
        }
        b.push_str(&"\t".repeat(depth));
        b.push_str(line);
        b.push('\n');
        if line.ends_with('{') {
            depth += 1;
        }
    }
    b
}

fn go_secret(v: &Secret) -> String {
    if !v.env.is_empty() {
        return format!("os.Getenv({})", go_quote(&v.env));
    }
    go_quote(&v.literal)
}

fn go_concat(prefix: &str, v: &Secret) -> String {
    if !v.env.is_empty() {
        if prefix.is_empty() {
            return go_secret(v);
        }
        // gofmt's spacing inside a call
        return format!("{}+{}", go_quote(prefix), go_secret(v));
    }
    go_quote(&format!("{}{}", prefix, v.literal))
}
