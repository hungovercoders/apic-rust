//! The `pretty` format: every feature as it is written, each step
//! coloured by its outcome and followed by where its definition lives,
//! outlines as their steps once and then one table row per example.

use std::sync::LazyLock;

use regex::Regex;

use super::{s, summary};
use crate::colors::{black_bold, cyan, cyan_bold, green, red, red_bold, white_bold, ColorFn, Out};
use crate::engine::{id_number, FeatureModel, Formatter, Status, Storage};
use crate::gherkin::{
    DocString, Examples, Pickle, PickleStep, Rule, RuleChild, Scenario, Step, Table,
};

const INDENT: isize = 2;

static OUTLINE_PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("<[^>]+>").expect("a valid pattern"));

pub(super) struct Pretty {
    first_feature: bool,
}

impl Default for Pretty {
    fn default() -> Pretty {
        Pretty {
            first_feature: true,
        }
    }
}

fn runes(text: &str) -> isize {
    text.chars().count() as isize
}

fn keyword_and_name(keyword: &str, name: &str) -> String {
    let mut title = white_bold(&format!("{keyword}:"));
    if !name.is_empty() {
        title.push(' ');
        title.push_str(name);
    }
    title
}

/// Where something is in its feature file, as a trailing comment.
fn line(path: &str, line: usize) -> String {
    // The path may already end in this line number.
    let suffix = format!(":{line}");
    let path = path.strip_suffix(&suffix).unwrap_or(path);
    format!(" {}", black_bold(&format!("# {path}:{line}")))
}

fn length_pickle_step(keyword: &str, text: &str) -> isize {
    INDENT * 2 + runes(&format!("{} {text}", keyword.trim()))
}

fn length_pickle(keyword: &str, name: &str) -> isize {
    INDENT + runes(&format!("{}: {name}", keyword.trim()))
}

fn longest_step(steps: &[Step], pickle_length: isize) -> isize {
    steps
        .iter()
        .map(|st| length_pickle_step(&st.keyword, &st.text))
        .fold(pickle_length, isize::max)
}

/// The width of the scenario's title line and of its widest line.
fn scenario_lengths(feature: &FeatureModel, scenario: &Scenario) -> (isize, isize) {
    let header = length_pickle(&scenario.keyword, &scenario.name);
    let mut max = longest_step(&scenario.steps, header);
    if let Some(bg) = feature.doc.find_background(&scenario.id) {
        max = longest_step(&bg.steps, max);
    }
    (header, max)
}

fn print_scenario_header(out: &mut Out<'_>, feature: &FeatureModel, sc: &Scenario, filling: isize) {
    let text = format!(
        "{}{}{}{}",
        s(INDENT),
        keyword_and_name(&sc.keyword, &sc.name),
        s(filling),
        line(&feature.uri, sc.line)
    );
    out.println(&format!("\n{text}"));
}

fn print_row(out: &mut Out<'_>, cells: &[String], max: &[isize], clr: ColorFn) {
    let cells: Vec<String> = cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let val = clr(cell);
            let pad = max.get(i).copied().unwrap_or(0) - runes(&val);
            format!("{val}{}", s(pad))
        })
        .collect();
    out.println(&format!("{}| {} |", s(INDENT * 3), cells.join(" | ")));
}

/// The widest cell of each column, measured as it will be printed.
fn column_widths<'a>(
    rows: impl Iterator<Item = &'a Vec<String>>,
    columns: usize,
    clrs: &[ColorFn],
) -> Vec<isize> {
    let mut longest = vec![0; columns];
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(columns) {
            for c in clrs {
                longest[i] = longest[i].max(runes(&c(cell)));
            }
            longest[i] = longest[i].max(runes(cell));
        }
    }
    longest
}

fn longest_example_row(t: &Examples, clrs: &[ColorFn]) -> Vec<isize> {
    let Some(header) = &t.table_header else {
        return Vec::new();
    };
    let rows = std::iter::once(&header.cells).chain(t.table_body.iter().map(|r| &r.cells));
    column_widths(rows, header.cells.len(), clrs)
}

fn print_table(out: &mut Out<'_>, t: &Table, c: ColorFn) {
    let Some(first) = t.rows.first() else {
        return;
    };
    let max = column_widths(t.rows.iter(), first.len(), &[c]);
    for row in &t.rows {
        print_row(out, row, &max, c);
    }
}

fn print_doc_string(out: &mut Out<'_>, doc: &DocString) {
    let ct = if doc.media_type.is_empty() {
        String::new()
    } else {
        format!(" {}", cyan(&doc.media_type))
    };
    out.println(&format!("{}{}{ct}", s(INDENT * 3), cyan(&doc.delimiter)));
    for ln in doc.content.split('\n') {
        out.println(&format!("{}{}", s(INDENT * 3), cyan(ln)));
    }
    out.println(&format!("{}{}", s(INDENT * 3), cyan(&doc.delimiter)));
}

fn is_first_scenario_in_rule(rule: Option<&Rule>, scenario: &Scenario) -> bool {
    let Some(rule) = rule else {
        return false;
    };
    rule.children
        .iter()
        .find_map(|c| match c {
            RuleChild::Scenario(sc) => Some(sc),
            RuleChild::Background(_) => None,
        })
        .is_some_and(|first| first.id == scenario.id)
}

impl Pretty {
    /// A scenario without steps: its background and title, nothing run.
    fn print_undefined_pickle(&self, st: &Storage, out: &mut Out<'_>, pickle: &Pickle) {
        let Some(feature) = st.feature(&pickle.uri) else {
            return;
        };
        let Some(scenario) = feature.doc.find_scenario(&pickle.ast_node_ids[0]) else {
            return;
        };
        let (header, max) = scenario_lengths(feature, scenario);
        if let Some(bg) = feature.doc.find_background(&pickle.ast_node_ids[0]) {
            out.println(&format!(
                "\n{}{}",
                s(INDENT),
                keyword_and_name(&bg.keyword, &bg.name)
            ));
            for step in &bg.steps {
                out.println(&format!(
                    "{}{} {}",
                    s(INDENT * 2),
                    cyan(step.keyword.trim()),
                    cyan(&step.text)
                ));
            }
        }
        // Scenario headers and examples are not printed more than once.
        if !scenario.examples.is_empty() {
            let Some((table, row)) = pickle
                .ast_node_ids
                .get(1)
                .and_then(|id| feature.doc.find_example(id))
            else {
                return;
            };
            let first_row = table.table_body.first().is_some_and(|r| r.id == row.id);
            let first_table = scenario.examples[0].line == table.line;
            if !(first_table && first_row) {
                return;
            }
        }
        print_scenario_header(out, feature, scenario, max - header);
        for examples in &scenario.examples {
            let max = longest_example_row(examples, &[cyan, cyan]);
            out.println("");
            out.println(&format!(
                "{}{}",
                s(INDENT * 2),
                keyword_and_name(&examples.keyword, &examples.name)
            ));
            if let Some(header) = &examples.table_header {
                print_row(out, &header.cells, &max, cyan);
            }
            for row in &examples.table_body {
                print_row(out, &row.cells, &max, cyan);
            }
        }
    }
}

/// An outline's step: nothing until the example's last step has run,
/// then the steps (for the first example only) and the example's row.
fn print_outline_example(
    st: &Storage,
    out: &mut Out<'_>,
    feature: &FeatureModel,
    scenario: &Scenario,
    pickle: &Pickle,
    step: &PickleStep,
    background_steps: usize,
) {
    let mut error_msg = String::new();
    let mut clr: ColorFn = green;
    let (header, max_length) = scenario_lengths(feature, scenario);
    let Some((table, row)) = pickle
        .ast_node_ids
        .get(1)
        .and_then(|id| feature.doc.find_example(id))
    else {
        return;
    };
    let print_example_header = table.table_body.first().is_some_and(|r| r.id == row.id);
    let first_table = scenario.examples[0].line == table.line;
    let results = st.step_results_until(&pickle.id, &step.id);
    let first_executed = results.len() == background_steps + 1;
    if first_table && print_example_header && first_executed {
        print_scenario_header(out, feature, scenario, max_length - header);
    }
    if table.table_body.is_empty() {
        return;
    }
    if results.len() != pickle.steps.len() {
        // Examples are printed once all their steps have finished.
        return;
    }
    for result in results {
        match result.status {
            Status::Failed | Status::Ambiguous => {
                error_msg = result.err.clone().unwrap_or_default();
                clr = result.status.color();
            }
            Status::Undefined => clr = result.status.color(),
            _ => {}
        }
        if !(first_table && print_example_header) {
            continue;
        }
        // The first example also prints the steps.
        let Some(pickle_step) = st.pickle_step(&result.step_id) else {
            continue;
        };
        let Some(ast_step) = feature.doc.find_step(&pickle_step.ast_node_id) else {
            continue;
        };
        let mut text = String::new();
        if let Some(def) = &result.def {
            let mut pos = 0;
            for m in OUTLINE_PLACEHOLDER.find_iter(&ast_step.text) {
                text.push_str(&cyan(&ast_step.text[pos..m.start()]));
                text.push_str(&cyan_bold(m.as_str()));
                pos = m.end();
            }
            text.push_str(&cyan(&ast_step.text[pos..]));
            let step_length = length_pickle_step(&ast_step.keyword, &ast_step.text);
            text.push_str(&s(max_length - step_length));
            text.push(' ');
            text.push_str(&black_bold(&format!("# {def}")));
        }
        out.println(&format!(
            "{}{} {text}",
            s(INDENT * 2),
            cyan(ast_step.keyword.trim())
        ));
        if let Some(arg) = &pickle_step.argument {
            if let Some(table) = &arg.data_table {
                print_table(out, table, cyan);
            }
            if let Some(doc) = &ast_step.doc_string {
                print_doc_string(out, doc);
            }
        }
    }
    let max = longest_example_row(table, &[clr, cyan]);
    if print_example_header {
        out.println("");
        out.println(&format!(
            "{}{}",
            s(INDENT * 2),
            keyword_and_name(&table.keyword, &table.name)
        ));
        if let Some(header) = &table.table_header {
            print_row(out, &header.cells, &max, cyan);
        }
    }
    print_row(out, &row.cells, &max, clr);
    if !error_msg.is_empty() {
        out.println(&format!("{}{}", s(INDENT * 4), red_bold(&error_msg)));
    }
}

impl Formatter for Pretty {
    fn feature(&mut self, _st: &Storage, out: &mut Out<'_>, feature: &FeatureModel) {
        if !self.first_feature {
            out.println("");
        }
        self.first_feature = false;
        let Some(f) = &feature.doc.feature else {
            return;
        };
        out.println(&keyword_and_name(&f.keyword, &f.name));
        if !f.description.trim().is_empty() {
            for ln in f.description.split('\n') {
                out.println(&format!("{}{}", s(INDENT), ln.trim()));
            }
        }
    }

    fn pickle(&mut self, st: &Storage, out: &mut Out<'_>, pickle: &Pickle) {
        if pickle.steps.is_empty() {
            self.print_undefined_pickle(st, out, pickle);
        }
    }

    fn step(&mut self, st: &Storage, out: &mut Out<'_>, pickle: &Pickle, step: &PickleStep) {
        let Some(feature) = st.feature(&pickle.uri) else {
            return;
        };
        let scenario_id = &pickle.ast_node_ids[0];
        let background = feature.doc.find_background(scenario_id);
        let Some(scenario) = feature.doc.find_scenario(scenario_id) else {
            return;
        };
        let rule = feature.doc.find_rule(scenario_id);
        let Some(ast_step) = feature.doc.find_step(&step.ast_node_id) else {
            return;
        };

        let mut background_step = false;
        let mut first_background_step = false;
        let mut background_steps = 0;
        if let Some(bg) = background {
            background_steps = bg.steps.len();
            if let Some(idx) = bg.steps.iter().position(|s| s.id == step.ast_node_id) {
                background_step = true;
                first_background_step = idx == 0;
            }
        }
        // A background is printed once: with the feature's first scenario,
        // or a rule's.
        let first_pickle = (rule.is_none()
            && feature.pickles.first().is_some_and(|p| p.id == pickle.id))
            || is_first_scenario_in_rule(rule, scenario);
        if background_step && !first_pickle {
            return;
        }
        if background_step && first_background_step {
            if let Some(bg) = background {
                out.println(&format!(
                    "\n{}{}",
                    s(INDENT),
                    keyword_and_name(&bg.keyword, &bg.name)
                ));
            }
        }
        if !background_step && !scenario.examples.is_empty() {
            print_outline_example(st, out, feature, scenario, pickle, step, background_steps);
            return;
        }

        let (header, max_length) = scenario_lengths(feature, scenario);
        let step_length = length_pickle_step(&ast_step.keyword, &step.text);
        let first_scenario_step = scenario
            .steps
            .first()
            .is_some_and(|s| s.id == step.ast_node_id);
        if !background_step && first_scenario_step {
            print_scenario_header(out, feature, scenario, max_length - header);
        }
        let Some(result) = st.step_result(&step.id) else {
            return;
        };
        let clr = result.status.color();
        let mut text = format!(
            "{}{} {}",
            s(INDENT * 2),
            clr(ast_step.keyword.trim()),
            clr(&step.text)
        );
        if let Some(def) = &result.def {
            text.push_str(&s(max_length - step_length + 1));
            text.push_str(&black_bold(&format!("# {def}")));
        }
        out.println(&text);
        if let Some(arg) = &step.argument {
            if let Some(table) = &arg.data_table {
                print_table(out, table, cyan);
            }
            if let Some(doc) = &ast_step.doc_string {
                print_doc_string(out, doc);
            }
        }
        if let Some(err) = &result.err {
            out.println(&format!("{}{}", s(INDENT * 2), red_bold(err)));
        }
    }

    fn summary(&mut self, st: &Storage, out: &mut Out<'_>) {
        let mut failed = st.step_results_by_status(Status::Failed);
        if !failed.is_empty() {
            out.println(&format!("\n--- {}\n", red("Failed steps:")));
            failed.sort_by_key(|r| id_number(&r.step_id));
            for fail in failed {
                let found = st.pickle(&fail.pickle_id).and_then(|pickle| {
                    let step = st.pickle_step(&fail.step_id)?;
                    let feature = st.feature(&pickle.uri)?;
                    let scenario = feature.doc.find_scenario(&pickle.ast_node_ids[0])?;
                    let ast_step = feature.doc.find_step(&step.ast_node_id)?;
                    Some((pickle, step, feature, scenario, ast_step))
                });
                let Some((pickle, step, feature, scenario, ast_step)) = found else {
                    continue;
                };
                let scenario_desc = format!("{}: {}", scenario.keyword, pickle.name);
                let step_desc = format!("{} {}", ast_step.keyword.trim(), step.text);
                out.println(&format!(
                    "{}{}{}",
                    s(INDENT),
                    red(&scenario_desc),
                    line(&feature.uri, scenario.line)
                ));
                out.println(&format!(
                    "{}{}{}",
                    s(INDENT * 2),
                    red(&step_desc),
                    line(&feature.uri, ast_step.line)
                ));
                out.println(&format!(
                    "{}{}{}\n",
                    s(INDENT * 3),
                    red("Error: "),
                    red_bold(fail.err.as_deref().unwrap_or("<nil>"))
                ));
            }
        }
        summary(st, out);
    }
}
