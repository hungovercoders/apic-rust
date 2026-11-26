//! apic env: the environments and the variables in effect.

use apic_runner::{AuthConfigInfo, ProxyInfo, VarInfo};
use serde::Serialize;

use crate::io::wln;
use crate::root::{App, CliResult, Invocation};
use crate::shared::theme;
use crate::spec::CommandSpec;

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new("env", "Show environments and the variables in effect")
        .example(
            "  apic env
  apic env --env staging
  apic env --json | jq .environments",
        )
        .runs(run)
}

fn run(a: &mut App, _: &Invocation) -> CliResult {
    let r = a.new_runner()?;
    let mut vars = r.env_vars();
    let auths = r.auth_configs();
    let names = r.envs.names();
    if a.g.json {
        for v in &mut vars {
            if v.secret {
                v.value = "***".to_string();
            }
        }
        #[derive(Serialize)]
        struct Out<'a> {
            root: &'a str,
            environments: &'a [String],
            #[serde(skip_serializing_if = "str::is_empty")]
            current: &'a str,
            #[serde(serialize_with = "crate::shared::null_when_empty")]
            files: &'a [String],
            #[serde(skip_serializing_if = "Option::is_none")]
            proxy: Option<ProxyInfo>,
            #[serde(serialize_with = "crate::shared::null_when_empty")]
            variables: &'a [VarInfo],
            #[serde(skip_serializing_if = "<[AuthConfigInfo]>::is_empty")]
            auth: &'a [AuthConfigInfo],
        }
        return a.write_json(&Out {
            root: &r.project.root,
            environments: &names,
            current: &r.opts.env,
            files: &r.envs.found,
            proxy: r.proxy_info(""),
            variables: &vars,
            auth: &auths,
        });
    }
    let t = theme();
    if names.is_empty() {
        wln!(
            a.stdout,
            "no environments (create http-client.env.json in {})",
            r.project.root
        );
    } else {
        let marked: Vec<String> = names
            .iter()
            .map(|n| {
                if *n == r.opts.env {
                    t.accent.render(&format!("{n}*"))
                } else {
                    n.clone()
                }
            })
            .collect();
        wln!(a.stdout, "{} {}", t.bold.render("environments:"), marked.join(" "));
    }
    if !r.envs.found.is_empty() {
        wln!(a.stdout, "{} {}", t.dim.render("files:"), r.envs.found.join(", "));
    }
    if let Some(p) = r.proxy_info("") {
        wln!(a.stdout, "{} {p}", t.dim.render("proxy:"));
    }
    if !vars.is_empty() {
        a.stdout.print(&apic_output::section(&t, "variables"));
        a.stdout.print(&apic_output::variables(&t, &vars, false));
    }
    if !auths.is_empty() {
        // JetBrains Security.Auth: what {{$auth.token("name")}} runs.
        a.stdout.print(&apic_output::section(&t, "auth"));
        for c in &auths {
            let what = if c.error.is_empty() { c.spec.clone() } else { t.fail.render(&c.error) };
            wln!(
                a.stdout,
                "  {} {what} {}",
                t.bold.render(&c.name),
                t.dim.render(&format!("({})", c.files.join(", ")))
            );
        }
    }
    Ok(())
}
