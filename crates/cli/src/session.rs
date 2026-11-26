//! apic session: captured values and cookies, and clearing them.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::PoisonError;

use apic_gocompat::Time;
use apic_runner::{Code, MASKED};
use apic_session::Cookie;
use serde::Serialize;

use crate::io::wln;
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::theme;
use crate::spec::{CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new("session", "Show captured values stored for later runs")
        .example(
            "  apic session
  apic session --env staging --json",
        )
        .runs(show)
        .subcommand(
            CommandSpec::new("cookies", "List the cookies stored per environment (values masked)")
                .example(
                    "  apic session cookies
  apic session cookies --env staging",
                )
                .runs(cookies),
        )
        .subcommand(
            CommandSpec::new("clear", "Forget captured values for the current environment (or --all)")
                .example(
                    "  apic session clear
  apic session clear --env staging
  apic session clear --all",
                )
                .flag(FlagSpec::bool("all", "clear every environment"))
                .runs(clear),
        )
}

fn open_jar(root: &str) -> Result<apic_session::Jar, CliError> {
    apic_session::open_jar(Path::new(root)).map_err(|e| fail(Code::Session, format!("cookies: {e}")))
}

fn cookie_line(c: &Cookie, now: Time) -> String {
    theme()
        .dim
        .render(&format!("({}{} · {})", c.domain, c.path, c.expiry_text(now)))
}

fn show(a: &mut App, _: &Invocation) -> CliResult {
    let r = a.new_runner()?;
    let Some(session) = &r.session else {
        return Err(fail(Code::Session, "session disabled by --no-session"));
    };
    let session = session.lock().unwrap_or_else(PoisonError::into_inner);
    if a.g.json {
        return a.write_json(&mask_session_envs(&session.envs, Time::now()));
    }
    let mut envs = session.env_names();
    // Cookies are listed whether or not this command switched the jar
    // on: they are in the session directory either way.
    let jar = match &r.jar {
        Some(jar) => jar.clone(),
        None => open_jar(&r.project.root)?,
    };
    for e in jar.env_names() {
        if !envs.contains(&e) {
            envs.push(e);
        }
    }
    envs.sort();
    if envs.is_empty() {
        wln!(a.stdout, "session is empty");
        return Ok(());
    }
    let t = theme();
    let arrow = t.capture.render("↳");
    for e in &envs {
        wln!(a.stdout, "{}", t.bold.render(e));
        for (k, v) in &session.vars(e) {
            if is_auth_cache_key(k) {
                wln!(a.stdout, "  {arrow} {k} = {}", apic_auth::describe_cached(v, Time::now()));
                continue;
            }
            wln!(a.stdout, "  {arrow} {k} = {}", apic_output::truncate(v, 60));
        }
        for c in jar.cookies(e) {
            wln!(
                a.stdout,
                "  {arrow} cookie {} = {MASKED} {}",
                c.name,
                cookie_line(&c, Time::now())
            );
        }
    }
    Ok(())
}

fn cookies(a: &mut App, _: &Invocation) -> CliResult {
    let p = a.load_project()?;
    let jar = open_jar(&p.root)?;
    let mut out: BTreeMap<String, Vec<CookieInfo>> = BTreeMap::new();
    for e in jar.env_names() {
        for c in jar.cookies(&e) {
            out.entry(e.clone()).or_default().push(CookieInfo {
                name: c.name,
                domain: c.domain,
                path: c.path,
                expires: c.expires.map(Time::format_rfc3339_nano),
                secure: c.secure,
                http_only: c.http_only,
            });
        }
    }
    if a.g.json {
        return a.write_json(&out);
    }
    if out.is_empty() {
        wln!(a.stdout, "no cookies stored");
        return Ok(());
    }
    let t = theme();
    for e in jar.env_names() {
        wln!(a.stdout, "{}", t.bold.render(&e));
        for c in jar.cookies(&e) {
            wln!(
                a.stdout,
                "  {} {} = {MASKED} {}",
                t.capture.render("↳"),
                c.name,
                cookie_line(&c, Time::now())
            );
        }
    }
    Ok(())
}

fn clear(a: &mut App, inv: &Invocation) -> CliResult {
    let r = a.new_runner()?;
    let Some(session) = &r.session else {
        return Err(fail(Code::Session, "session disabled by --no-session"));
    };
    let target = if inv.flags.bool("all") { "*".to_string() } else { r.opts.env.clone() };
    {
        let mut session = session.lock().unwrap_or_else(PoisonError::into_inner);
        session.clear(&target);
        session.save().map_err(CliError::other)?;
    }
    // Cookies go with the captures, switched on or not.
    let jar = open_jar(&r.project.root)?;
    jar.clear(&target);
    jar.save().map_err(CliError::other)?;
    if a.g.json {
        return a.write_json(&BTreeMap::from([("cleared", target)]));
    }
    wln!(a.stdout, "session cleared");
    Ok(())
}

/// What `apic session cookies --json` prints per cookie: the value stays
/// out, like a secret.
#[derive(Serialize)]
struct CookieInfo {
    name: String,
    domain: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    secure: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    http_only: bool,
}

fn mask_session_envs(
    envs: &BTreeMap<String, BTreeMap<String, String>>,
    now: Time,
) -> BTreeMap<String, BTreeMap<String, String>> {
    envs.iter()
        .map(|(env, vars)| {
            let masked = vars
                .iter()
                .map(|(k, v)| {
                    let v = if is_auth_cache_key(k) { apic_auth::describe_cached(v, now) } else { v.clone() };
                    (k.clone(), v)
                })
                .collect();
            (env.clone(), masked)
        })
        .collect()
}

fn is_auth_cache_key(k: &str) -> bool {
    k.starts_with("$oauth2:") || k.starts_with("$exec:")
}
