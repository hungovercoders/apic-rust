//! apic select: a value from a request's last recorded response, without
//! sending the request again.

use std::sync::LazyLock;

use apic_gocompat::quote;
use apic_runner::{Code, MASKED};
use apic_selector::Kind;
use regex::Regex;
use serde::Serialize;
use serde_json::value::RawValue;

use crate::history::{history_err, history_off};
use crate::io::wln;
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "select <request> <selector>",
        "Read a value out of a request's last recorded response, without sending it again",
    )
    .long(
        r##"select evaluates a selector, the kind "# @assert" and "# @capture" take,
against the newest response recorded for a request in the current
environment (or entry N with --entry, 1 being the newest), and prints the
value. Nothing is sent: it is for looking at a response again, or at the
rest of a body that apic run --body-limit cut, without repeating a call
that may have changed something.

It reads the response history, so apic.yaml needs history: N (apic init
and apic demo set history: 10). Sensitive response headers are stored
masked, so header.set-cookie and cookie.<name> read as ***."##,
    )
    .example(
        "  apic select list-todos 'body.$.items[0].id'
  apic select list-todos 'body.$.items.#'
  apic select login status
  apic select login body --entry 2 --json",
    )
    .args(ArgRule::Exact(2))
    .flag(FlagSpec::int("entry", 1, "which recorded response, 1 being the newest"))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let entry = inv.flags.int("entry");
    if entry < 1 {
        return Err(fail(Code::Flag, "--entry counts from 1, the newest"));
    }
    let sel = inv.args[1].as_str();
    if let Err(e) = apic_selector::check(sel) {
        return Err(fail(Code::Flag, format!("selector {}: {e}", quote(sel))));
    }
    if RE_HEADER_MULTI.is_match(sel.trim()) {
        return Err(fail(Code::Flag, format!("selector {}: the history keeps one value per header, joined with commas, so header.<name>.# and header.<name>[n] cannot be read from it; use header.<name>", quote(sel))));
    }
    let t = a.history_for(&inv.args[0])?;
    let e = match t.store.get(&t.env, &t.name, entry) {
        Ok(e) => e,
        Err(err) => {
            // Nothing recorded at all, with history off, is the one case
            // the hint helps; an entry past a history that has some is a
            // bad argument like any other.
            if let apic_history::Error::Range(re) = &err {
                let hint = history_off(&t.project);
                if re.have == 0 && !hint.is_empty() {
                    return Err(fail(
                        Code::Session,
                        format!(
                            "{} has no recorded response in {}: {hint}, then run it once",
                            t.name, t.env
                        ),
                    ));
                }
            }
            return Err(history_err(err));
        }
    };
    let res = apic_runner::parse_result(e.result.as_deref().unwrap_or("null"))
        .map_err(|err| fail(Code::Session, format!("history entry {entry} of {}: {err}", t.name)))?;
    let Some(resp) = res.raw() else {
        return Err(fail(
            Code::Session,
            format!("history entry {entry} of {}: the request got no response", t.name),
        ));
    };
    if resp.body == MASKED.as_bytes() {
        return Err(fail(
            Code::Flag,
            format!(
                "history entry {entry} of {} was recorded by a --redact run: its body and header values are masked",
                t.name
            ),
        ));
    }
    let value = apic_selector::select_value(resp, sel)
        .map_err(|err| fail(Code::Flag, format!("selector {}: {err}", quote(sel))))?;
    if a.g.json {
        let v = match &value {
            None => "null".to_string(),
            // A number, boolean, null, array or object as itself.
            Some(v) if v.kind != Kind::String => match serde_json::from_str::<serde_json::Value>(&v.text) {
                Ok(parsed) => crate::shared::plain_json(&parsed),
                Err(_) => apic_gocompat::json_string(&v.text, false),
            },
            Some(v) => apic_gocompat::json_string(&v.text, false),
        };
        #[derive(Serialize)]
        struct Out<'a> {
            request: &'a str,
            env: &'a str,
            entry: usize,
            time: String,
            selector: &'a str,
            found: bool,
            value: &'a RawValue,
        }
        let raw = RawValue::from_string(v).map_err(CliError::other)?;
        a.write_json(&Out {
            request: &t.name,
            env: &t.env,
            entry: e.index,
            time: e.time.format_rfc3339_nano(),
            selector: sel,
            found: value.is_some(),
            value: &raw,
        })?;
        if value.is_none() {
            return Err(CliError::exit(apic_runner::EXIT_ASSERT));
        }
        return Ok(());
    }
    let Some(value) = value else {
        return Err(CliError::Exit {
            code: apic_runner::EXIT_ASSERT,
            msg: format!(
                "nothing at {sel} in {}'s response of {}",
                t.name,
                apic_output::history_time(e.time)
            ),
        });
    };
    let mut text = value.text;
    if (value.kind == Kind::Object || value.kind == Kind::Array) && apic_gocompat::check_json(text.as_bytes()).is_ok() {
        text = apic_gocompat::indent_json(&text, "", "  ");
    }
    wln!(a.stdout, "{text}");
    Ok(())
}

/// Matches the header selectors that need a header's separate values,
/// which a history entry does not keep: it holds one value per header,
/// joined with commas as `apic run --json` prints them, and that is what
/// header.<name> reads.
static RE_HEADER_MULTI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^headers?\.[^.\[\]]+(\.#|\[-?\d+\])$").expect("valid pattern"));
