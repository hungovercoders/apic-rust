//! apic snippet: a request as code in another language.

use apic_gocompat::quote;
use apic_runner::Code;
use apic_snippet::LANGUAGES;
use serde::Serialize;

use crate::io::wln;
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "snippet <request>",
        "Print the request as code: curl, HTTPie, PowerShell, Python, JavaScript or Go",
    )
    .long(
        "Print code that sends what apic would, variables resolved: a curl or
HTTPie command, PowerShell's Invoke-RestMethod, Python requests, JavaScript
fetch or a Go program using net/http.

Without --redact the snippet runs as printed, credentials included. With it
the values are masked and credentials come from the environment: TOKEN for a
bearer or OAuth2 token, APIC_USER and APIC_PASSWORD for basic and digest,
APIC_API_KEY for an API key. What a language cannot do on its own, such as
AWS SigV4 signing, is said in a comment at the top.",
    )
    .example(
        "  apic snippet get-user --lang python
  apic snippet create-order --lang powershell --redact
  apic snippet login --lang go > login.go",
    )
    .args(ArgRule::Exact(1))
    .flag(
        FlagSpec::string(
            "lang",
            "curl",
            &format!("language: {}", LANGUAGES.join(", ")),
        )
        .short('l'),
    )
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let lang = inv.flags.string("lang");
    let (id, code) = a.snippet(&inv.args[0], &lang)?;
    if a.g.json {
        #[derive(Serialize)]
        struct Out {
            id: String,
            lang: String,
            code: String,
        }
        return a.write_json(&Out { id, lang, code });
    }
    wln!(a.stdout, "{code}");
    Ok(())
}

impl App {
    /// Resolves a request and renders it in `lang`, refusing when a
    /// variable is missing: code with a hole in it would not send the
    /// request. Returns the request's id and the code.
    pub(crate) fn snippet(&self, target: &str, lang: &str) -> Result<(String, String), CliError> {
        if !LANGUAGES.contains(&lang) {
            return Err(fail(
                Code::Flag,
                format!(
                    "unknown language {}: one of {}",
                    quote(lang),
                    LANGUAGES.join(", ")
                ),
            ));
        }
        let (r, req) = self.single(target)?;
        let res = r.resolve(&req)?;
        let d = r.describe(&req);
        if !d.ready {
            let missing: Vec<String> = d
                .variables
                .iter()
                .filter(|v| v.missing)
                .map(|v| v.name.clone())
                .collect();
            return Err(r.missing_error(&req, &missing).into());
        }
        let code = apic_snippet::render(lang, &res, self.g.redact).map_err(CliError::other)?;
        Ok((req.id(), code))
    }
}
