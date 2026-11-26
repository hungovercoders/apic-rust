//! apic history: the responses a request returned before, and what
//! changed.

use std::path::Path;
use std::sync::Arc;

use apic_gocompat::quote;
use apic_history::{Change, Entry, Recorded, Store};
use apic_project::Project;
use apic_runner::Code;
use serde::Serialize;

use crate::io::wln;
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::{plural, theme, unknown_request};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

/// The history of one request in the environment in effect.
pub(crate) struct HistoryTarget {
    pub store: Store,
    pub project: Arc<Project>,
    /// The environment in effect, "default" for none.
    pub env: String,
    /// The request's # @name.
    pub name: String,
}

impl App {
    /// Opens the project's history for the environment in effect: --env,
    /// else apic.yaml's env, else "default". It builds no runner, so a
    /// session or cookie jar apic cannot read never stands in the way of
    /// reading or clearing the history. Reading needs no `history:` in
    /// apic.yaml: what an earlier setting recorded stays readable.
    fn history_store(&self) -> Result<(Store, Arc<Project>, String), CliError> {
        let p = self.load_project()?;
        let mut env = self.g.env.clone();
        if env.is_empty() {
            env = p.config.env.clone();
        }
        if env.is_empty() {
            env = apic_history::DEFAULT_ENV.to_string();
        }
        Ok((Store::new(Path::new(&p.root), p.config.history), p, env))
    }

    /// Resolves target to the key a request's history is kept under (see
    /// `apic_runner::history_key`). A name with history but no request
    /// any more (renamed or deleted since) still resolves, so its history
    /// can be read and cleared; a name several requests share does not,
    /// since the history of each is kept apart under file#name.
    pub(crate) fn history_for(&self, target: &str) -> Result<HistoryTarget, CliError> {
        let (store, p, env) = self.history_store()?;
        let rerr = match p.resolve(target) {
            Ok(reqs) if reqs.len() == 1 => {
                if reqs[0].name.is_empty() {
                    return Err(fail(
                        Code::UnknownRequest,
                        format!("{target} has no # @name; history keeps named requests only"),
                    ));
                }
                let name = apic_runner::history_key(&p, &reqs[0]);
                return Ok(HistoryTarget { store, project: p, env, name });
            }
            Ok(reqs) => {
                return Err(fail(
                    Code::Ambiguous,
                    format!(
                        "{target} names {} requests; pick one with {target}#<name>",
                        reqs.len()
                    ),
                ))
            }
            Err(e) => e,
        };
        let shared = p.requests().iter().filter(|r| r.name == target).count();
        if shared > 1 {
            return Err(fail(Code::Ambiguous, rerr.to_string()));
        }
        if store.list(&env, target).is_ok_and(|entries| !entries.is_empty()) {
            return Ok(HistoryTarget { store, project: p, env, name: target.to_string() });
        }
        Err(unknown_request(rerr))
    }
}

/// Renders "1 entry" or "3 entries".
fn entry_count(n: usize) -> String {
    if n == 1 {
        "1 entry".to_string()
    } else {
        format!("{n} entries")
    }
}

/// The hint for a project that records nothing.
pub(crate) fn history_off(p: &Project) -> &'static str {
    if p.config.history > 0 {
        return "";
    }
    "history is off: set `history: 20` in apic.yaml to keep the last 20 responses of each request"
}

/// Reads an entry number argument.
fn entry_index(arg: &str) -> Result<i64, CliError> {
    match arg.parse::<i64>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(fail(
            Code::Flag,
            format!("{} is not an entry number: 1 is the newest", quote(arg)),
        )),
    }
}

/// Gives a missing entry the code of any other bad argument.
pub(crate) fn history_err(err: impl Into<apic_history::Error>) -> CliError {
    match err.into() {
        e @ apic_history::Error::Range(_) => fail(Code::Flag, e.to_string()),
        e => fail(Code::Session, format!("history: {e}")),
    }
}

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "history [request]",
        "List the responses a request returned before (needs history: N in apic.yaml)",
    )
    .long(
        "List the responses recorded for a request in the current environment,
newest first, or with no request, the requests that have any.

Recording is off until apic.yaml sets history: N, which keeps the last N
responses of each named request per environment in .apic/history. Entries
are stored as apic run --json prints a result: sensitive headers masked,
and everything when the run used --redact.",
    )
    .example(
        "  apic history login
  apic history login --show 2
  apic history diff login
  apic history clear login",
    )
    .args(ArgRule::Max(1))
    .flag(FlagSpec::int("show", 0, "print entry N (1 is the newest) as apic run printed it"))
    .flag(FlagSpec::bool("verbose", "with --show, include the request and response headers").short('v'))
    .runs(run)
    .subcommand(
        CommandSpec::new(
            "diff <request> [from] [to]",
            "Show what changed between two responses (default: the last two)",
        )
        .example(
            "  apic history diff get-user
  apic history diff get-user 1 3",
        )
        .long(
            "Compare two recorded responses of a request: the status, then the body.
JSON bodies are compared by structure, keys in sorted order and arrays
index by index; other text line by line. Headers are left out, since a
Date or a request id differs every time. Entries are numbered from 1, the
newest; the default compares 2 with 1.",
        )
        .args(ArgRule::Range(1, 3))
        .runs(diff),
    )
    .subcommand(
        CommandSpec::new(
            "clear [request]",
            "Forget the history of a request, or of every request with --all",
        )
        .example(
            "  apic history clear get-user
  apic history clear --all",
        )
        .long(
            "Forget the history of one request in the current environment. With
--all and no request, forget every request's history in the environment;
add --every-env to forget it in every environment.",
        )
        .args(ArgRule::Max(1))
        .flag(FlagSpec::bool("all", "clear every request's history in the environment"))
        .flag(FlagSpec::bool(
            "every-env",
            "clear every request's history in every environment (implies --all)",
        ))
        .runs(clear),
    )
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let show = inv.flags.int("show");
    if inv.args.is_empty() {
        if show > 0 {
            return Err(fail(Code::Flag, "--show needs a request"));
        }
        return a.history_requests();
    }
    let t = a.history_for(&inv.args[0])?;
    if show > 0 {
        return a.history_show(&t, show, inv.flags.bool("verbose"));
    }
    a.history_list(&t)
}

fn diff(a: &mut App, inv: &Invocation) -> CliResult {
    let (mut from, mut to) = (2, 1);
    if inv.args.len() > 1 {
        from = entry_index(&inv.args[1])?;
    }
    if inv.args.len() > 2 {
        to = entry_index(&inv.args[2])?;
    }
    let t = a.history_for(&inv.args[0])?;
    a.history_diff(&t, from, to)
}

fn clear(a: &mut App, inv: &Invocation) -> CliResult {
    let every_env = inv.flags.bool("every-env");
    let all = inv.flags.bool("all") || every_env;
    if all && !inv.args.is_empty() {
        return Err(fail(Code::Flag, "--all clears every request; give it no request"));
    }
    if !all && inv.args.is_empty() {
        return Err(a.history_clear_needs_scope());
    }
    let (store, mut env, name) = if inv.args.len() == 1 {
        let t = a.history_for(&inv.args[0])?;
        (t.store, t.env, t.name)
    } else {
        let (store, _, env) = a.history_store()?;
        (store, env, String::new())
    };
    if every_env {
        env = "*".to_string();
    }
    let n = store.clear(&env, &name).map_err(history_err)?;
    if a.g.json {
        #[derive(Serialize)]
        struct Out {
            cleared: String,
            #[serde(skip_serializing_if = "String::is_empty")]
            request: String,
            entries: usize,
        }
        return a.write_json(&Out { cleared: env, request: name, entries: n });
    }
    wln!(a.stdout, "cleared {}", entry_count(n));
    Ok(())
}

impl App {
    /// Refuses a bare `history clear`, which would otherwise wipe a whole
    /// environment by accident; a project with a request named clear is
    /// told how to list its history instead.
    fn history_clear_needs_scope(&self) -> CliError {
        let mut msg = "name the request whose history to clear, or pass --all for every request in the environment (--every-env for every environment)".to_string();
        if let Ok(p) = self.load_project() {
            if let Some(r) = p.requests().iter().find(|r| r.name == "clear") {
                msg.push_str(&format!(
                    "; to list the history of the request named clear, use apic history {}#clear",
                    r.file.path
                ));
            }
        }
        fail(Code::Flag, msg)
    }

    fn history_requests(&self) -> CliResult {
        let (store, p, env) = self.history_store()?;
        let reqs = store.requests(&env).map_err(history_err)?;
        if self.g.json {
            #[derive(Serialize)]
            struct Out<'a> {
                env: &'a str,
                keep: i64,
                requests: &'a [Recorded],
            }
            return self.write_json(&Out { env: &env, keep: store.keep(), requests: &reqs });
        }
        let t = theme();
        if reqs.is_empty() {
            wln!(self.stdout, "no history in {env}");
            let hint = history_off(&p);
            if !hint.is_empty() {
                wln!(self.stdout, "{}", t.dim.render(hint));
            }
            return Ok(());
        }
        wln!(self.stdout, "{}", t.bold.render(&env));
        for r in &reqs {
            wln!(self.stdout, "  {} {}", r.request, t.dim.render(&entry_count(r.entries)));
        }
        Ok(())
    }

    fn history_list(&self, target: &HistoryTarget) -> CliResult {
        let entries = target.store.list(&target.env, &target.name).map_err(history_err)?;
        if self.g.json {
            #[derive(Serialize)]
            struct Out<'a> {
                env: &'a str,
                request: &'a str,
                keep: i64,
                #[serde(serialize_with = "crate::shared::null_when_empty")]
                entries: &'a [Entry],
            }
            return self.write_json(&Out {
                env: &target.env,
                request: &target.name,
                keep: target.store.keep(),
                entries: &entries,
            });
        }
        let t = theme();
        if entries.is_empty() {
            wln!(self.stdout, "no history for {} in {}", target.name, target.env);
            let hint = history_off(&target.project);
            if !hint.is_empty() {
                wln!(self.stdout, "{}", t.dim.render(hint));
            }
            return Ok(());
        }
        wln!(
            self.stdout,
            "{} {}",
            t.bold.render(&target.name),
            t.dim.render(&format!("· {} · {}", target.env, entry_count(entries.len())))
        );
        self.stdout.print(&apic_output::history_entries(&t, &entries));
        Ok(())
    }

    fn history_show(&self, target: &HistoryTarget, index: i64, verbose: bool) -> CliResult {
        let e = target.store.get(&target.env, &target.name, index).map_err(history_err)?;
        let result = e.result.clone().unwrap_or_else(|| "null".to_string());
        if self.g.json {
            // The entry's own fields, then the result as it was stored.
            let entry = serde_json::to_string(&e).map_err(CliError::other)?;
            let text = format!("{},\"result\":{result}}}", entry.trim_end_matches('}'));
            let text = apic_gocompat::indent_json(&text, "", "  ") + "\n";
            return self.stdout.write_text(&text).map_err(CliError::other);
        }
        let res = apic_runner::parse_result(&result)
            .map_err(|err| history_err(apic_history::Error::Other(format!("{}: {err}", e.file))))?;
        wln!(
            self.stdout,
            "{}",
            theme().dim.render(&format!(
                "{} · {} · {}",
                target.name,
                target.env,
                apic_output::history_entry(&e)
            ))
        );
        apic_output::human(&mut self.stdout.clone(), &res, verbose);
        Ok(())
    }

    fn history_diff(&self, target: &HistoryTarget, from: i64, to: i64) -> CliResult {
        let fe = match target.store.get(&target.env, &target.name, from) {
            Err(apic_history::Error::Range(re)) if re.have < 2 => {
                return Err(fail(
                    Code::Flag,
                    format!(
                        "{} has {} in {}; a diff needs two (run it again)",
                        target.name,
                        entry_count(re.have),
                        target.env
                    ),
                ))
            }
            other => other.map_err(history_err)?,
        };
        let te = target.store.get(&target.env, &target.name, to).map_err(history_err)?;
        let changes = apic_history::compare(
            fe.result.as_deref().unwrap_or("null"),
            te.result.as_deref().unwrap_or("null"),
        )
        .map_err(|e| history_err(apic_history::Error::Other(e)))?;
        if self.g.json {
            #[derive(Serialize)]
            struct Out<'a> {
                env: &'a str,
                request: &'a str,
                from: &'a Entry,
                to: &'a Entry,
                changes: &'a [Change],
            }
            return self.write_json(&Out {
                env: &target.env,
                request: &target.name,
                from: &fe,
                to: &te,
                changes: &changes,
            });
        }
        let t = theme();
        wln!(
            self.stdout,
            "{} {}",
            t.bold.render(&target.name),
            t.dim.render(&format!(
                "{} → {}",
                apic_output::history_entry(&fe),
                apic_output::history_entry(&te)
            ))
        );
        if changes.is_empty() {
            wln!(self.stdout, "no changes in status or body");
            return Ok(());
        }
        self.stdout.print(&apic_output::changes(&t, &changes, 80));
        wln!(self.stdout, "{}", t.dim.render(&plural(changes.len(), "change")));
        Ok(())
    }
}
