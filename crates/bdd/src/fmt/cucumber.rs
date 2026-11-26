//! The `cucumber` format: the JSON report other tools read. One object
//! per feature, in name order, with an element per scenario run and its
//! steps' results; durations are in nanoseconds.

use std::time::Instant;

use apic_gocompat::{indent_json, json_string};

use crate::colors::Out;
use crate::engine::{id_number, FeatureModel, Formatter, Status, Storage};
use crate::gherkin::{Pickle, Tag};

pub(super) struct Cuke;

fn string(s: &str) -> String {
    json_string(s, true)
}

fn cuke_id(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

fn tag_json(name: &str, line: usize) -> String {
    format!("{{\"name\":{},\"line\":{line}}}", string(name))
}

fn tags_json(tags: &[Tag]) -> Vec<String> {
    tags.iter().map(|t| tag_json(&t.name, t.line)).collect()
}

/// `"key":[...]`, left out when there is nothing in it.
fn list(key: &str, items: &[String]) -> String {
    if items.is_empty() {
        String::new()
    } else {
        format!(",\"{key}\":[{}]", items.join(","))
    }
}

fn steps_json(st: &Storage, feature: &FeatureModel, pickle: &Pickle) -> Vec<String> {
    let mut results = st.step_results_by_pickle(&pickle.id);
    results.sort_by_key(|r| id_number(&r.step_id));
    let mut started: Option<Instant> = st.pickle_result(&pickle.id);
    let mut out = Vec::new();
    for result in results {
        // A step's time runs from the end of the one before it.
        let duration = started.map(|s| result.finished_at.saturating_duration_since(s).as_nanos());
        started = Some(result.finished_at);
        let Some(pickle_step) = st.pickle_step(&result.step_id) else {
            continue;
        };
        let Some(step) = feature.doc.find_step(&pickle_step.ast_node_id) else {
            continue;
        };
        let mut json = format!(
            "{{\"keyword\":{},\"name\":{},\"line\":{}",
            string(&step.keyword),
            string(&pickle_step.text),
            step.line
        );
        let arg = pickle_step.argument.as_ref();
        if let (Some(doc), Some(ast_doc)) =
            (arg.and_then(|a| a.doc_string.as_ref()), &step.doc_string)
        {
            json.push_str(&format!(
                ",\"doc_string\":{{\"value\":{},\"content_type\":{},\"line\":{}}}",
                string(&doc.content),
                string(doc.media_type.trim()),
                ast_doc.line
            ));
        }
        let no_match = matches!(result.status, Status::Undefined | Status::Ambiguous);
        let location = if no_match {
            format!("{}:{}", pickle.uri, step.line)
        } else {
            result
                .def
                .as_deref()
                .and_then(|d| d.split(' ').next())
                .unwrap_or("")
                .to_string()
        };
        json.push_str(&format!(
            ",\"match\":{{\"location\":{}}}",
            string(&location)
        ));
        json.push_str(&format!(
            ",\"result\":{{\"status\":\"{}\"",
            result.status.as_str()
        ));
        if let Some(err) = result.err.as_deref().filter(|e| !e.is_empty()) {
            json.push_str(&format!(",\"error_message\":{}", string(err)));
        }
        if let Some(d) = duration.filter(|_| !no_match && result.status != Status::Skipped) {
            json.push_str(&format!(",\"duration\":{d}"));
        }
        json.push('}');
        if let Some(table) = arg
            .and_then(|a| a.data_table.as_ref())
            .filter(|t| !t.rows.is_empty())
        {
            let rows: Vec<String> = table
                .rows
                .iter()
                .map(|row| {
                    let cells: Vec<String> = row.iter().map(|c| string(c)).collect();
                    format!("{{\"cells\":[{}]}}", cells.join(","))
                })
                .collect();
            json.push_str(&list("rows", &rows));
        }
        json.push('}');
        out.push(json);
    }
    out
}

fn element_json(
    st: &Storage,
    feature: &FeatureModel,
    pickle: &Pickle,
    feature_id: &str,
    feature_tags: &[String],
) -> Option<String> {
    let scenario = feature.doc.find_scenario(&pickle.ast_node_ids[0])?;
    let mut line = scenario.line;
    let mut suffix = String::new();
    let mut tags = feature_tags.to_vec();
    tags.extend(tags_json(&scenario.tags));
    if let Some(row_id) = pickle.ast_node_ids.get(1) {
        if let Some((example, _)) = feature.doc.find_example(row_id) {
            tags.extend(tags_json(&example.tags));
        }
        for example in &scenario.examples {
            for (idx, row) in example.table_body.iter().enumerate() {
                if &row.id == row_id {
                    suffix.push_str(&format!(";{};{}", cuke_id(&example.name), idx + 2));
                    line = row.line;
                }
            }
        }
    }
    let id = format!("{feature_id};{}{suffix}", cuke_id(&pickle.name));
    Some(format!(
        "{{\"id\":{},\"keyword\":{},\"name\":{},\"description\":{},\"line\":{line},\"type\":\"scenario\"{}{}}}",
        string(&id),
        string(&scenario.keyword),
        string(&pickle.name),
        string(&scenario.description),
        list("tags", &tags),
        list("steps", &steps_json(st, feature, pickle)),
    ))
}

impl Formatter for Cuke {
    fn summary(&mut self, st: &Storage, out: &mut Out<'_>) {
        let mut features = st.features();
        features.retain(|f| f.doc.feature.is_some());
        features.sort_by(|a, b| {
            let name = |f: &FeatureModel| f.doc.feature.as_ref().map(|x| x.name.clone());
            name(a).cmp(&name(b))
        });
        let mut report = Vec::new();
        for model in features {
            let Some(feature) = &model.doc.feature else {
                continue;
            };
            let id = cuke_id(&feature.name);
            let comments: Vec<String> = model
                .doc
                .comments
                .iter()
                .map(|c| {
                    format!(
                        "{{\"value\":{},\"line\":{}}}",
                        string(c.text.trim()),
                        c.line
                    )
                })
                .collect();
            let tags = tags_json(&feature.tags);
            let elements: Vec<String> = st
                .pickles(&model.uri)
                .into_iter()
                .filter_map(|p| element_json(st, model, p, &id, &tags))
                .collect();
            report.push(format!(
                "{{\"uri\":{},\"id\":{},\"keyword\":{},\"name\":{},\"description\":{},\"line\":{}{}{}{}}}",
                string(&model.uri),
                string(&id),
                string(&feature.keyword),
                string(&feature.name),
                string(&feature.description),
                feature.line,
                list("comments", &comments),
                list("tags", &tags),
                list("elements", &elements),
            ));
        }
        let compact = format!("[{}]", report.join(","));
        out.print(&format!("{}\n", indent_json(&compact, "", "    ")));
    }
}
