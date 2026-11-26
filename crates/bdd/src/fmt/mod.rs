//! The report formats: `pretty`, `progress`, `cucumber` (JSON) and `junit`
//! (XML), and the summary the two text formats end with.

mod cucumber;
mod junit;
mod pretty;
mod progress;

use std::sync::LazyLock;
use std::time::Instant;

use apic_gocompat::format_duration;
use regex::Regex;

use crate::colors::{cyan, green, red, yellow, Out};
use crate::engine::{Formatter, Status, Storage};
use crate::gherkin::PickleArgument;

pub(crate) use junit::{escape_attr, escape_text};

/// The formatter for a `--format` name.
pub(crate) fn new(format: &str, suite: &str) -> Box<dyn Formatter> {
    match format {
        "progress" => Box::new(progress::Progress::default()),
        "cucumber" => Box::new(cucumber::Cuke),
        "junit" => Box::new(junit::JUnit::new(suite)),
        _ => Box::new(pretty::Pretty::default()),
    }
}

/// `n` blanks; a negative count is one blank.
fn s(n: isize) -> String {
    if n < 0 {
        " ".to_string()
    } else {
        " ".repeat(n as usize)
    }
}

/// The closing lines of a text report: scenario and step counts by
/// outcome, the time taken, and snippets for the undefined steps.
fn summary(st: &Storage, out: &mut Out<'_>) {
    let (mut total_sc, mut passed_sc, mut undefined_sc) = (0, 0, 0);
    let (mut total_st, mut passed_st, mut failed_st, mut skipped_st) = (0, 0, 0, 0);
    let (mut undefined_st, mut ambiguous_st) = (0, 0);
    for pickle_id in st.pickle_results() {
        // A scenario counts under the outcome of its last decisive step.
        let mut status = Status::Passed;
        total_sc += 1;
        let results = st.step_results_by_pickle(pickle_id);
        if results.is_empty() {
            status = Status::Undefined;
        }
        for r in results {
            total_st += 1;
            match r.status {
                Status::Passed => passed_st += 1,
                Status::Failed => {
                    status = Status::Failed;
                    failed_st += 1;
                }
                Status::Ambiguous => {
                    status = Status::Ambiguous;
                    ambiguous_st += 1;
                }
                Status::Skipped => skipped_st += 1,
                Status::Undefined => {
                    status = Status::Undefined;
                    undefined_st += 1;
                }
            }
        }
        if status == Status::Passed {
            passed_sc += 1;
        } else if status == Status::Undefined {
            undefined_sc += 1;
        }
    }
    let (mut steps, mut parts, mut scenarios) = (Vec::new(), Vec::new(), Vec::new());
    if passed_st > 0 {
        steps.push(green(&format!("{passed_st} passed")));
    }
    if failed_st > 0 {
        parts.push(red(&format!("{failed_st} failed")));
        steps.push(red(&format!("{failed_st} failed")));
    }
    if ambiguous_st > 0 {
        parts.push(yellow(&format!("{ambiguous_st} ambiguous")));
        steps.push(yellow(&format!("{ambiguous_st} ambiguous")));
    }
    if undefined_st > 0 {
        parts.push(yellow(&format!("{undefined_sc} undefined")));
        steps.push(yellow(&format!("{undefined_st} undefined")));
    } else if undefined_sc > 0 {
        // There may be scenarios without steps.
        parts.push(yellow(&format!("{undefined_sc} undefined")));
    }
    if skipped_st > 0 {
        steps.push(cyan(&format!("{skipped_st} skipped")));
    }
    if passed_sc > 0 {
        scenarios.push(green(&format!("{passed_sc} passed")));
    }
    scenarios.extend(parts);
    let elapsed = Instant::now().duration_since(st.started_at);

    out.println("");
    if total_sc == 0 {
        out.println("No scenarios");
    } else {
        out.print(&format!(
            "{total_sc} scenarios ({})\n",
            scenarios.join(", ")
        ));
    }
    if total_st == 0 {
        out.println("No steps");
    } else {
        out.print(&format!("{total_st} steps ({})\n", steps.join(", ")));
    }
    out.println(&format_duration(elapsed));

    let text = snippets(st);
    if !text.is_empty() {
        out.println("");
        out.println(&yellow(
            "You can implement step definitions for undefined steps with these snippets:",
        ));
        out.println(&yellow(&text));
    }
}

static SNIPPET_EXPR_CLEANUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([/\[\]\(\)\\^\$\.\|\?\*\+'])").expect("a valid pattern"));
static SNIPPET_EXPR_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([^0-9A-Za-z_]|^)"(?:[^"]*)"([^0-9A-Za-z_]|$)"#).expect("a valid pattern")
});
static SNIPPET_METHOD_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z_ ]").expect("a valid pattern"));
static SNIPPET_NUMBERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([0-9]+)").expect("a valid pattern"));

struct Snippet {
    method: String,
    expr: String,
    doc_string: bool,
    data_table: bool,
}

impl Snippet {
    /// The parameter list: one per number or quoted string in the
    /// pattern, then the doc string or table.
    fn args(&self) -> String {
        const INT: &str = "(\\d+)";
        const STR: &str = "\"([^\"]*)\"";
        let mut args: Vec<&str> = Vec::new();
        let mut pos = 0;
        loop {
            let part = &self.expr[pos..];
            match (part.find(INT), part.find(STR)) {
                (None, None) => break,
                (Some(i), None) => {
                    pos += i + INT.len();
                    args.push("int");
                }
                (None, Some(q)) => {
                    pos += q + STR.len();
                    args.push("string");
                }
                (Some(i), Some(q)) if i < q => {
                    pos += i + INT.len();
                    args.push("int");
                }
                (Some(_), Some(q)) => {
                    pos += q + STR.len();
                    args.push("string");
                }
            }
        }
        if self.doc_string {
            args.push("*godog.DocString");
        }
        if self.data_table {
            args.push("*godog.Table");
        }
        // Neighbours of one type share its name: `arg1, arg2 int`.
        let mut ret = String::new();
        let mut last = "";
        for (i, arg) in args.iter().enumerate() {
            if last.is_empty() || last == *arg {
                ret.push_str(&format!("arg{}, ", i + 1));
            } else {
                ret = format!(
                    "{} {last}, arg{}, ",
                    ret.trim_end_matches([',', ' ']),
                    i + 1
                );
            }
            last = arg;
        }
        format!("{} {last}", ret.trim_end_matches([',', ' ']))
            .trim()
            .to_string()
    }
}

/// Capitalises the first letter of every word.
fn ascii_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut new_word = true;
    for c in s.chars() {
        let c = if new_word { c.to_ascii_uppercase() } else { c };
        out.push(c);
        new_word = !c.is_ascii_alphanumeric();
    }
    out
}

/// Code suggestions for the undefined steps, one per distinct pattern.
fn snippets(st: &Storage) -> String {
    let undefined = st.step_results_by_status(Status::Undefined);
    if undefined.is_empty() {
        return String::new();
    }
    let mut index = 0;
    let mut snips: Vec<Snippet> = Vec::new();
    for u in undefined {
        let Some(step) = st.pickle_step(&u.step_id) else {
            continue;
        };
        let text = step.text.as_str();
        let expr = SNIPPET_EXPR_CLEANUP.replace_all(text, "\\$1");
        let expr = SNIPPET_NUMBERS.replace_all(&expr, "(\\d+)");
        let expr = SNIPPET_EXPR_QUOTED.replace_all(&expr, "${1}\"([^\"]*)\"${2}");
        let expr = format!("^{}$", expr.trim());

        let name = SNIPPET_NUMBERS.replace_all(text, " ");
        let name = SNIPPET_EXPR_QUOTED.replace_all(&name, " ");
        let name = SNIPPET_METHOD_NAME.replace_all(&name, "");
        let mut method = String::new();
        for (i, w) in name.trim().split(' ').enumerate() {
            if i != 0 {
                method.push_str(&ascii_title(w));
            } else if let Some(first) = w.chars().next() {
                method.extend(first.to_lowercase());
                method.push_str(&w[first.len_utf8()..]);
            }
        }
        if method.is_empty() {
            index += 1;
            method = format!("StepDefinitioninition{index}");
        }
        if snips.iter().any(|s| s.expr == expr) {
            continue;
        }
        let arg: Option<&PickleArgument> = step.argument.as_ref();
        snips.push(Snippet {
            method,
            expr,
            doc_string: arg.is_some_and(|a| a.doc_string.is_some()),
            data_table: arg.is_some_and(|a| a.data_table.is_some()),
        });
    }
    snips.sort_by(|a, b| a.method.cmp(&b.method));

    let mut buf = String::from("\n");
    for snip in &snips {
        buf.push_str(&format!(
            "func {}({}) error {{\n\treturn godog.ErrPending\n}}\n\n",
            snip.method,
            snip.args()
        ));
    }
    buf.push_str("func InitializeScenario(ctx *godog.ScenarioContext) { ");
    for snip in &snips {
        buf.push_str(&format!("\n\tctx.Step(`{}`, {})", snip.expr, snip.method));
    }
    buf.push_str("\n}\n");
    // There may be trailing blanks.
    buf.replace(" \n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_arguments() {
        let snip = |expr: &str, doc, table| Snippet {
            method: String::new(),
            expr: expr.to_string(),
            doc_string: doc,
            data_table: table,
        };
        assert_eq!(snip("^plain$", false, false).args(), "");
        assert_eq!(
            snip(r#"^a (\d+) "([^"]*)"$"#, false, false).args(),
            "arg1 int, arg2 string"
        );
        assert_eq!(
            snip(r"^a (\d+) (\d+)$", false, false).args(),
            "arg1, arg2 int"
        );
        assert_eq!(
            snip(r#"^a "([^"]*)":$"#, false, true).args(),
            "arg1 string, arg2 *godog.Table"
        );
        assert_eq!(snip("^a:$", true, false).args(), "arg1 *godog.DocString");
    }

    #[test]
    fn titles() {
        assert_eq!(ascii_title("something"), "Something");
        assert_eq!(ascii_title("a-b c"), "A-B C");
    }
}
