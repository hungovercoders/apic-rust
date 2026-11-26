//! Generates the JSON schemas for the files apic reads and writes:
//! apic.yaml, http-client.env.json (and the private file, same shape) and
//! .apic/session.json. They are published on the docs site so editors
//! validate and complete the files, and the VS Code extension registers
//! them directly.
//!
//! The apic.yaml schema is built by walking the project crate's table of
//! configuration keys, so a new key cannot be added to the configuration
//! without appearing here; the generator fails if a key has no description
//! in the table below. Run it with `task schemas`; the
//! `schemas_are_current` test fails when the committed files are stale.

#![forbid(unsafe_code)]

mod schema;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::Path;

use apic_gocompat::{path_error, quote, FlagSet};
use apic_project::{ConfigField, FieldKind, CONFIG_FIELDS};
use serde_json::{json, Value};

use schema::Schema;

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";
const BASE_URL: &str = "https://apic.sh/schemas/";

/// Documents every key of apic.yaml by its dotted path. The generator
/// refuses to run when a key is missing here.
const DESCRIPTIONS: &[(&str, &str)] = &[
    ("env", "Environment from http-client.env.json to use when --env is not given."),
    ("dir", "Subdirectory of the project root to scan for .http and .rest files. Default: the root itself."),
    ("timeout", "Default request timeout as a Go duration, for example 10s or 1m30s. Default 30s. `--timeout` and `# @timeout` override it."),
    ("retry", "Default retry policy for requests without `# @retry`: `<attempts> [interval]`, for example `10 2s`. A request is re-sent until its assertions pass or the attempts are spent; the interval is a Go duration and defaults to 1s. `--retry` overrides it and `--no-retry` switches retries off."),
    ("proxy", "Send every request through this proxy: an http, https or socks5 URL, with credentials in the userinfo if the proxy needs them. Beats HTTP_PROXY and HTTPS_PROXY; `--proxy` beats it for one command and `--no-proxy` sends directly."),
    ("noProxy", "Hosts that bypass `proxy`: a host name, `host:port`, `.example.com` for a domain and everything under it, an IP or a CIDR, or `*` for everything."),
    ("cookies", "Keep a cookie jar: cookies a response sets are sent with later requests to the same site and stored per environment in .apic/cookies.json, like captured values. Off by default; `--cookies` switches it on for one command and `# @no-cookies` exempts a request."),
    ("history", "Keep the last N responses of each named request, per environment, in .apic/history, for `apic history` and the UI's history tab. Off (0) by default, since responses may hold data you do not want on disk; a `--redact` run stores the redacted form. `--no-session` and `apic run --data` record nothing."),
    ("maxBodyBytes", "Largest response body apic reads into memory, in bytes. Default 67108864 (64 MiB); a larger response fails the request."),
    ("auth", "Project-wide authentication defaults; see the Authentication guide."),
    ("auth.default", "An auth spec applied to every request without its own `# @auth`, for example `aws region=eu-west-2` or `bearer {{token}}`. May use {{variables}}."),
    ("auth.allowExec", "Permit `# @auth exec ...`, which runs a command from a request file. Off by default because agents edit request files."),
    ("test", "Defaults for `apic test`."),
    ("test.paths", "Feature files or directories `apic test` runs when none are given, relative to the project root. Default: features."),
    ("tls", "TLS settings: a private CA to trust and a client certificate to present (mTLS). Paths are relative to the project root. `--cacert`, `--cert` and `--key` override them for one command; a JetBrains `SSLConfiguration` block in the env files is read too."),
    ("tls.caFile", "PEM file with certificates to trust in addition to the system roots, for an API behind a private CA."),
    ("tls.certFile", "PEM client certificate presented to servers that ask for one."),
    ("tls.keyFile", "PEM private key for certFile; defaults to certFile when both are in one file. Refused when world-readable."),
    ("tls.verifyHost", "Verify the server's certificate. Default true; `--insecure` turns it off for one command."),
    ("tls.hosts", "Overrides for particular hosts, by host name (`api.internal.example.com`) or wildcard (`*.internal.example.com`)."),
    ("tls.hosts.*", "The override for one host; unset keys fall back to the settings above."),
    ("tls.hosts.*.caFile", "PEM file with certificates to trust for this host."),
    ("tls.hosts.*.certFile", "PEM client certificate for this host."),
    ("tls.hosts.*.keyFile", "PEM private key for this host's certFile."),
    ("tls.hosts.*.verifyHost", "Verify this host's certificate. Default true."),
];

fn description(path: &str) -> Option<&'static str> {
    DESCRIPTIONS
        .iter()
        .find(|(key, _)| *key == path)
        .map(|(_, text)| *text)
}

/// Adds example values to a few keys, for editor completion.
fn examples(path: &str) -> Vec<Value> {
    let values = match path {
        "env" => json!(["dev", "staging"]),
        "dir" => json!(["api", "requests"]),
        "timeout" => json!(["10s", "1m"]),
        "retry" => json!(["10 2s", "5 500ms"]),
        "history" => json!([20]),
        "auth.default" => json!([
            "bearer {{token}}",
            "aws service=execute-api region=eu-west-2"
        ]),
        "proxy" => json!(["http://proxy.internal:3128", "socks5://127.0.0.1:1080"]),
        "noProxy" => json!([["localhost", ".internal"]]),
        "tls.caFile" => json!(["certs/internal-ca.pem"]),
        "tls.certFile" => json!(["certs/client.pem"]),
        "tls.keyFile" => json!(["certs/client-key.pem"]),
        "test.paths" => json!([["features", "smoke.feature"]]),
        _ => return Vec::new(),
    };
    match values {
        Value::Array(list) => list,
        _ => Vec::new(),
    }
}

/// Where the schemas live: the docs site publishes them at the URLs the
/// schema ids name, and the VS Code extension bundles a copy so they
/// validate offline.
const DIRS: &[&str] = &["website/public/schemas", "editors/vscode/schemas"];

fn main() {
    let mut flags = FlagSet::new("schemas");
    flags.string(
        "out",
        "",
        "one directory to write the schemas into (default: every directory in DIRS)",
    );
    flags.parse_env();
    let out = flags.get("out");
    let dirs: Vec<&str> = if out.is_empty() {
        DIRS.to_vec()
    } else {
        vec![out]
    };
    for dir in dirs {
        if let Err(err) = write(dir) {
            eprintln!("schemas: {err}");
            std::process::exit(1);
        }
    }
}

/// Returns every schema by file name.
fn files() -> Result<BTreeMap<&'static str, Schema>, String> {
    Ok(BTreeMap::from([
        ("apic.schema.json", config_schema()?),
        ("http-client.env.schema.json", env_schema()),
        ("session.schema.json", session_schema()),
    ]))
}

fn write(dir: &str) -> Result<(), String> {
    let files = files()?;
    let dir = Path::new(dir);
    std::fs::create_dir_all(dir).map_err(|err| path_error("mkdir", dir, &err))?;
    for (name, schema) in &files {
        let path = dir.join(name);
        std::fs::write(&path, schema.render()).map_err(|err| path_error("open", &path, &err))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

/// Builds the apic.yaml schema from the configuration's keys.
fn config_schema() -> Result<Schema, String> {
    let mut s = struct_schema(CONFIG_FIELDS, "")?;
    s.schema = DRAFT;
    s.id = format!("{BASE_URL}apic.schema.json");
    s.title = "apic.yaml";
    s.description =
        "Per-project defaults for apic, read from apic.yaml in the project root. Every key is optional."
            .to_string();
    Ok(s)
}

fn struct_schema(fields: &[ConfigField], prefix: &str) -> Result<Schema, String> {
    let mut props = BTreeMap::new();
    for f in fields {
        let path = if prefix.is_empty() {
            f.key.to_string()
        } else {
            format!("{prefix}.{}", f.key)
        };
        let Some(desc) = description(&path) else {
            return Err(format!(
                "apic.yaml key {} (configuration field {}) has no description in scripts/schemas",
                quote(&path),
                f.name
            ));
        };
        let mut prop = field_schema(&f.kind, &path)?;
        prop.description = desc.to_string();
        let ex = examples(&path);
        if !ex.is_empty() {
            prop.examples = ex;
        }
        props.insert(f.key.to_string(), prop);
    }
    Ok(Schema {
        ty: "object",
        properties: Some(props),
        additional_properties: Some(Box::new(Schema::never())),
        ..Schema::default()
    })
}

fn field_schema(kind: &FieldKind, path: &str) -> Result<Schema, String> {
    match kind {
        FieldKind::String => {
            let mut s = Schema::of("string");
            if path == "timeout" {
                s.pattern = r"^([0-9]+(\.[0-9]+)?(ns|us|µs|ms|s|m|h))+$";
            }
            Ok(s)
        }
        FieldKind::Bool => Ok(Schema::of("boolean")),
        FieldKind::Int => Ok(Schema {
            minimum: Some(0.0),
            ..Schema::of("integer")
        }),
        FieldKind::Slice(inner) => Ok(Schema {
            items: Some(Box::new(field_schema(inner, path)?)),
            ..Schema::of("array")
        }),
        FieldKind::Struct(fields) => struct_schema(fields, path),
        FieldKind::Optional(inner) => field_schema(inner, path),
        FieldKind::Map(inner) => {
            let any = format!("{path}.*");
            let Some(desc) = description(&any) else {
                return Err(format!(
                    "apic.yaml key {} has no description in scripts/schemas",
                    quote(&any)
                ));
            };
            let mut item = field_schema(inner, &any)?;
            item.description = desc.to_string();
            Ok(Schema::of("object").additional(item))
        }
    }
}

/// Describes http-client.env.json and http-client.private.env.json:
/// environment names mapping to variables. Values are stringified when
/// read, so numbers, booleans and null are accepted.
fn env_schema() -> Schema {
    // The value schema used in two places lives in $defs and is referenced
    // from each, as are the two JetBrains blocks.
    let value = || Schema::refer("#/$defs/value");
    let blocks = || {
        [
            (
                "SSLConfiguration",
                Schema::refer("#/$defs/sslConfiguration"),
            ),
            ("Security", Schema::refer("#/$defs/security")),
        ]
    };
    let text = |desc: &str| Schema::described("string", desc);
    let flag = |desc: &str| Schema::described("boolean", desc);
    let certificate = Schema {
        one_of: vec![
            Schema::of("string"),
            Schema {
                required: vec!["path"],
                ..Schema::of("object").props([
                    ("path", text("PEM client certificate.")),
                    ("keyPath", text("PEM private key; defaults to path.")),
                    (
                        "format",
                        text("Accepted for compatibility; apic reads PEM only."),
                    ),
                ])
            },
        ],
        ..Schema::default()
            .desc("The client certificate: a PEM path, or an object with `path` and `keyPath`.")
    };
    let ssl = Schema::described("object", "JetBrains HTTP Client's TLS block: a client certificate to present and whether to verify the server. Not a variable. Paths are relative to the project root.").props([
        ("clientCertificate", certificate),
        ("hasCertificatePassphrase", flag("The key is encrypted. apic cannot prompt for a passphrase and refuses such a key.")),
        ("verifyHostCertificate", flag("Verify the server's certificate. Default true.")),
    ]);
    let configuration = Schema::described("object", "One OAuth2 configuration. String fields may hold {{placeholders}}.").props([
        ("Type", text("Only OAuth2 is read.").one_of_strings(["OAuth2"])),
        ("Grant Type", text("The grant. Implicit is refused; Authorization Code runs with PKCE.").one_of_strings(["Client Credentials", "Password", "Device Authorization", "Authorization Code", "Implicit"])),
        ("Token URL", text("The token endpoint.")),
        ("Auth URL", text("The authorization endpoint, for Authorization Code.")),
        ("Device Auth URL", text("The device authorization endpoint, for Device Authorization.")),
        ("Redirect URL", text("Where the provider sends the browser back, for Authorization Code: http://localhost:<port>/<path>, where apic listens.")),
        ("Client ID", text("The client ID.")),
        ("Client Secret", text("The client secret; keep it in http-client.private.env.json.")),
        ("Scope", text("Space-separated scopes.")),
        ("Username", text("For the Password grant.")),
        ("Password", text("For the Password grant; keep it in http-client.private.env.json.")),
        ("Client Credentials", text("How the client authenticates at the token endpoint. Default in body.").one_of_strings(["basic", "in body", "none"])),
        ("Use ID Token", flag("$auth.token gives the ID token rather than the access token.")),
        ("PKCE", Schema::default().desc("Accepted; apic always uses PKCE for Authorization Code.")),
        ("Acquire Automatically", flag("Accepted; apic fetches a token when a request needs one.")),
        ("Custom Request Parameters", Schema::described("object", "Extra parameters. apic sends audience; others are reported by apic validate.")),
    ]);
    let security = Schema::described("object", "JetBrains HTTP Client's Security block. Not a variable. apic reads its Auth configurations.").props([(
        "Auth",
        Schema::described("object", "OAuth2 configurations by name. A request uses one as {{$auth.token(\"name\")}} or {{$auth.idToken(\"name\")}}; the private file's fields win over the public file's.").additional(configuration),
    )]);
    Schema {
        schema: DRAFT,
        id: format!("{BASE_URL}http-client.env.schema.json"),
        title: "http-client.env.json",
        defs: BTreeMap::from([
            (
                "value".to_string(),
                Schema {
                    types: vec!["string", "number", "boolean", "null"],
                    ..Schema::default().desc(
                        "A variable value. Non-strings are converted to text when substituted.",
                    )
                },
            ),
            ("sslConfiguration".to_string(), ssl),
            ("security".to_string(), security),
        ]),
        additional_properties: Some(Box::new(Schema {
            property_names: Some(Box::new(Schema {
                pattern: r"^[A-Za-z_$][\w.-]*$",
                ..Schema::default()
            })),
            ..Schema::described("object", "The variables of one environment.")
                .props(blocks())
                .additional(value())
        })),
        examples: vec![json!({
            "$shared": {"userId": 42},
            "dev": {"baseUrl": "https://dev.example.com"},
            "prod": {"baseUrl": "https://api.example.com"},
        })],
        ..Schema::described("object", "Per-environment variables for .http files, the format shared by JetBrains HTTP Client, kulala.nvim, httpyac and apic. The same shape is used by http-client.private.env.json, which holds secrets and is gitignored.").props([(
            "$shared",
            Schema::described("object", "Variables that apply to every environment; an environment's own value wins.")
                .props(blocks())
                .additional(value()),
        )])
    }
}

/// Describes .apic/session.json, which apic writes itself.
fn session_schema() -> Schema {
    Schema {
        schema: DRAFT,
        id: format!("{BASE_URL}session.schema.json"),
        title: ".apic/session.json",
        required: vec!["envs"],
        additional_properties: Some(Box::new(Schema::never())),
        ..Schema::described("object", "Values captured by `# @capture` and tokens cached by `# @auth`, per environment. Written by apic; `apic session` shows it and `apic session clear` empties it.").props([(
            "envs",
            Schema::described("object", "Environment name (or `default` when none was selected) to its captured values.").additional(
                Schema::described("object", "Captured values by name. Keys starting with `$oauth2:` or `$exec:` hold cached tokens as JSON text.")
                    .additional(Schema::of("string")),
            ),
        )])
    }
}
