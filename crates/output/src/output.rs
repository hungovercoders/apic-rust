use std::io::{self, Write};

use apic_gocompat::{check_json, indent_json};
use apic_runner::Result;

use crate::render::{result, result_name, summary_table, Options};
use crate::theme::Theme;

/// Writes one result as a single JSON line. The results of requests
/// `# @ref` ran first are written as lines of their own, before it, so a
/// consumer of `apic run --json` sees exactly one object per request sent;
/// the object itself carries no ran_first.
pub fn json(w: &mut dyn Write, res: &Result) -> io::Result<()> {
    for dep in &res.deps {
        json(w, dep)?;
    }
    if res.deps.is_empty() {
        return json_nested(w, res);
    }
    let mut flat = res.clone();
    flat.deps = Vec::new();
    json_nested(w, &flat)
}

/// Writes one result as a single JSON line with the results of its
/// `# @ref` dependencies nested under ran_first.
pub fn json_nested(w: &mut dyn Write, res: &Result) -> io::Result<()> {
    w.write_all(res.to_json().as_bytes())?;
    w.write_all(b"\n")
}

/// Writes only the response body, pretty-printed when it is JSON.
pub fn body(w: &mut dyn Write, res: &Result) {
    if res.raw().is_none() {
        return;
    }
    let _ = w.write_all(&pretty_json(&res.display_raw_body()));
    let _ = w.write_all(b"\n");
}

/// Writes a readable report of a result, after the reports of the
/// requests `# @ref` ran first, each under a line saying why it ran.
pub fn human(w: &mut dyn Write, res: &Result, verbose: bool) {
    deps(w, res, verbose);
    let o = Options {
        verbose,
        ..Options::default()
    };
    let _ = w.write_all(result(&Theme::default(), res, o).as_bytes());
}

/// Writes the reports of the requests `# @ref` ran before `res`, each
/// followed by a line saying why it ran.
pub fn deps(w: &mut dyn Write, res: &Result, verbose: bool) {
    let t = Theme::default();
    for dep in &res.deps {
        human(w, dep, verbose);
        let line = t
            .dim
            .render(&format!("↳ ran {} first (# @ref)", result_name(dep)));
        let _ = w.write_all((line + "\n\n").as_bytes());
    }
}

/// Writes the per-request table and totals of a flow.
pub fn summary(w: &mut dyn Write, results: &[Result]) {
    let table = summary_table(&Theme::default(), results);
    let _ = w.write_all(format!("\n{table}").as_bytes());
}

fn trim_space(b: &[u8]) -> &[u8] {
    match std::str::from_utf8(b) {
        Ok(s) => s.trim().as_bytes(),
        Err(_) => b.trim_ascii(),
    }
}

/// Whether `b` is a JSON object or array.
pub(crate) fn is_json(b: &[u8]) -> bool {
    let t = trim_space(b);
    matches!(t.first(), Some(b'{' | b'[')) && check_json(t).is_ok()
}

/// A JSON object or array indented by two spaces; anything else as it is.
pub(crate) fn pretty_json(b: &[u8]) -> Vec<u8> {
    if !is_json(b) {
        return b.to_vec();
    }
    match std::str::from_utf8(trim_space(b)) {
        Ok(s) => indent_json(s, "", "  ").into_bytes(),
        Err(_) => b.to_vec(),
    }
}
