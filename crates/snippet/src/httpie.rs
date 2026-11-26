use crate::curl::{escape_double, quote};
use crate::snippet::{path_base, Kind, Request, Secret};

/// Renders an HTTPie command (<https://httpie.io/cli>), POSIX-shell
/// quoted like the curl command.
pub(crate) fn httpie(m: &Request) -> String {
    // HTTPie speaks HTTP/1.1 only, so a pinned HTTP/1.1 needs nothing.
    let mut parts: Vec<String> = m
        .notes(true, true, true)
        .iter()
        .map(|n| format!("# {n}"))
        .collect();
    let mut cmd = vec!["http --ignore-stdin".to_string()];
    if !m.parts.is_empty() {
        cmd.push("--multipart".to_string());
    }
    if let Some(t) = &m.tls {
        if t.insecure {
            cmd.push("--verify=no".to_string());
        } else if !t.ca_file.is_empty() {
            cmd.push(format!("--verify={}", quote(&t.ca_file)));
        }
        if !t.cert_file.is_empty() {
            cmd.push(format!("--cert={}", quote(&t.cert_file)));
            if !t.key_file.is_empty() && t.key_file != t.cert_file {
                cmd.push(format!("--cert-key={}", quote(&t.key_file)));
            }
        }
    }
    if !m.proxy.is_empty() {
        cmd.push(format!("--proxy={}", quote(&format!("http:{}", m.proxy))));
        cmd.push(format!("--proxy={}", quote(&format!("https:{}", m.proxy))));
    }
    if let Some(c) = &m.cred {
        if matches!(c.kind, Kind::Basic | Kind::Digest) {
            if c.kind == Kind::Digest {
                cmd.push("--auth-type=digest".to_string());
            }
            cmd.push(format!("--auth={}", shell_pair(&c.user, &c.pass)));
        }
    }
    if !m.body.is_empty() {
        cmd.push(format!("--raw={}", quote(&m.body)));
    }
    cmd.push(m.method.clone());
    cmd.push(quote(&m.url));
    for h in &m.headers {
        if h.value.is_empty() {
            // HTTPie's spelling of an empty header
            cmd.push(quote(&format!("{};", h.name)));
            continue;
        }
        cmd.push(quote(&format!("{}:{}", h.name, h.value)));
    }
    if let Some(c) = &m.cred {
        match c.kind {
            Kind::Header => cmd.push(shell_item(&format!("{}:{}", c.name, c.prefix), &c.value)),
            Kind::Query => cmd.push(shell_item(&format!("{}=={}", c.name, c.prefix), &c.value)),
            _ => {}
        }
    }
    for p in &m.parts {
        if p.file.is_empty() {
            cmd.push(quote(&format!("{}={}", p.name, m.part_value(p))));
            continue;
        }
        let mut item = format!("{}@{}", p.name, p.file);
        if !p.content_type.is_empty() {
            item.push_str(";type=");
            item.push_str(&p.content_type);
        }
        if !p.filename.is_empty() && p.filename != path_base(&p.file) {
            item.push_str(";filename=");
            item.push_str(&p.filename);
        }
        cmd.push(quote(&item));
    }
    parts.push(cmd.join(" \\\n  "));
    parts.join("\n")
}

/// Text followed by a secret: quoted literally, or with the environment
/// variable expanded inside double quotes.
fn shell_item(text: &str, v: &Secret) -> String {
    if !v.env.is_empty() {
        return format!("\"{}${}\"", escape_double(text), v.env);
    }
    quote(&format!("{}{}", text, v.literal))
}

fn shell_pair(user: &Secret, pass: &Secret) -> String {
    if !user.env.is_empty() {
        return format!("\"${}:${}\"", user.env, pass.env);
    }
    quote(&format!("{}:{}", user.literal, pass.literal))
}
