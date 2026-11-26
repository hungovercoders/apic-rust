//! Compiles a document into pickles: each scenario with its backgrounds'
//! steps in front, each outline once per example row with the `<name>`
//! placeholders filled in.

use super::{
    DataTable, DocString, Document, FeatureChild, Pickle, PickleArgument, PickleDocString,
    PickleStep, Rule, RuleChild, Scenario, Step, Table, TableRow, Tag,
};

/// The pickles of a document, in file order. Step ids are drawn before
/// their pickle's.
pub(crate) fn pickles(
    doc: &Document,
    uri: &str,
    new_id: &mut dyn FnMut() -> String,
) -> Vec<Pickle> {
    let mut out = Vec::new();
    let Some(feature) = &doc.feature else {
        return out;
    };
    let mut background: Vec<&Step> = Vec::new();
    let tags: Vec<&Tag> = feature.tags.iter().collect();
    for child in &feature.children {
        match child {
            FeatureChild::Background(bg) => background.extend(bg.steps.iter()),
            FeatureChild::Rule(rule) => {
                compile_rule(&mut out, rule, &tags, &background, uri, new_id)
            }
            FeatureChild::Scenario(sc) => {
                compile_any(&mut out, sc, &tags, &background, uri, new_id)
            }
        }
    }
    out
}

fn compile_rule(
    out: &mut Vec<Pickle>,
    rule: &Rule,
    feature_tags: &[&Tag],
    feature_background: &[&Step],
    uri: &str,
    new_id: &mut dyn FnMut() -> String,
) {
    let mut background: Vec<&Step> = feature_background.to_vec();
    let mut tags: Vec<&Tag> = feature_tags.to_vec();
    tags.extend(rule.tags.iter());
    for child in &rule.children {
        match child {
            RuleChild::Background(bg) => background.extend(bg.steps.iter()),
            RuleChild::Scenario(sc) => compile_any(out, sc, &tags, &background, uri, new_id),
        }
    }
}

fn compile_any(
    out: &mut Vec<Pickle>,
    scenario: &Scenario,
    tags: &[&Tag],
    background: &[&Step],
    uri: &str,
    new_id: &mut dyn FnMut() -> String,
) {
    if scenario.examples.is_empty() {
        compile_scenario(out, scenario, tags, background, uri, new_id);
    } else {
        compile_outline(out, scenario, tags, background, uri, new_id);
    }
}

fn compile_scenario(
    out: &mut Vec<Pickle>,
    scenario: &Scenario,
    inherited: &[&Tag],
    background: &[&Step],
    uri: &str,
    new_id: &mut dyn FnMut() -> String,
) {
    let mut steps = Vec::new();
    if !scenario.steps.is_empty() {
        for step in background {
            steps.push(pickle_step(step, None, new_id));
        }
        for step in &scenario.steps {
            steps.push(pickle_step(step, None, new_id));
        }
    }
    let id = new_id();
    out.push(Pickle {
        id,
        uri: uri.to_string(),
        name: scenario.name.clone(),
        steps,
        tags: tag_names(inherited, &scenario.tags, &[]),
        ast_node_ids: vec![scenario.id.clone()],
    });
}

fn compile_outline(
    out: &mut Vec<Pickle>,
    scenario: &Scenario,
    inherited: &[&Tag],
    background: &[&Step],
    uri: &str,
    new_id: &mut dyn FnMut() -> String,
) {
    for examples in &scenario.examples {
        let Some(header) = &examples.table_header else {
            continue;
        };
        for row in &examples.table_body {
            let mut steps = Vec::new();
            if !scenario.steps.is_empty() {
                for step in background {
                    steps.push(pickle_step(step, None, new_id));
                }
            }
            for step in &scenario.steps {
                steps.push(pickle_step(step, Some((header, row)), new_id));
            }
            let id = new_id();
            out.push(Pickle {
                id,
                uri: uri.to_string(),
                name: interpolate(&scenario.name, Some((header, row))),
                steps,
                tags: tag_names(inherited, &scenario.tags, &examples.tags),
                ast_node_ids: vec![scenario.id.clone(), row.id.clone()],
            });
        }
    }
}

fn tag_names(inherited: &[&Tag], own: &[Tag], examples: &[Tag]) -> Vec<String> {
    inherited
        .iter()
        .map(|t| t.name.clone())
        .chain(own.iter().map(|t| t.name.clone()))
        .chain(examples.iter().map(|t| t.name.clone()))
        .collect()
}

type Row<'a> = Option<(&'a TableRow, &'a TableRow)>;

fn pickle_step(step: &Step, row: Row<'_>, new_id: &mut dyn FnMut() -> String) -> PickleStep {
    PickleStep {
        id: new_id(),
        text: interpolate(&step.text, row),
        ast_node_id: step.id.clone(),
        argument: argument(step, row),
    }
}

fn argument(step: &Step, row: Row<'_>) -> Option<PickleArgument> {
    if step.data_table.is_none() && step.doc_string.is_none() {
        return None;
    }
    Some(PickleArgument {
        doc_string: step.doc_string.as_ref().map(|d| doc_string(d, row)),
        data_table: step.data_table.as_ref().map(|t| data_table(t, row)),
    })
}

fn data_table(table: &DataTable, row: Row<'_>) -> Table {
    Table {
        rows: table
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| interpolate(c, row)).collect())
            .collect(),
    }
}

fn doc_string(doc: &DocString, row: Row<'_>) -> PickleDocString {
    PickleDocString {
        media_type: interpolate(&doc.media_type, row),
        content: interpolate(&doc.content, row),
    }
}

/// Replaces each `<header>` with the row's value, one column after the
/// other.
fn interpolate(s: &str, row: Row<'_>) -> String {
    let Some((header, values)) = row else {
        return s.to_string();
    };
    let mut out = s.to_string();
    for (i, name) in header.cells.iter().enumerate() {
        let value = values.cells.get(i).map_or("", String::as_str);
        out = out.replace(&format!("<{name}>"), value);
    }
    out
}
