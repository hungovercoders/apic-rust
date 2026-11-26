//! The `junit` format: one `<testsuite>` per feature and one `<testcase>`
//! per scenario run, for CI systems.

use std::collections::HashMap;
use std::time::Instant;

use crate::colors::Out;
use crate::engine::{FeatureModel, Formatter, Status, Storage};

pub(super) struct JUnit {
    suite: String,
}

impl JUnit {
    pub(super) fn new(suite: &str) -> JUnit {
        JUnit {
            suite: suite.to_string(),
        }
    }
}

/// Reports whether XML may carry the character at all.
fn in_character_range(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn escape(s: &str, escape_newline: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' => out.push_str("&#x9;"),
            '\n' if escape_newline => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c if !in_character_range(c) => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// Escapes text for an attribute value.
pub(crate) fn escape_attr(s: &str) -> String {
    escape(s, true)
}

/// Escapes text between tags, where a newline stays as it is.
pub(crate) fn escape_text(s: &str) -> String {
    escape(s, false)
}

/// A span in seconds, in the shortest form that reads back the same.
fn seconds(from: Instant, to: Instant) -> String {
    let d = to.saturating_duration_since(from);
    let secs = d.as_secs() as f64 + f64::from(d.subsec_nanos()) / 1e9;
    format!("{secs}")
}

#[derive(Default)]
struct TestCase {
    name: String,
    status: String,
    time: String,
    failure: Option<String>,
    /// Message and type of each `<error>`.
    errors: Vec<(String, &'static str)>,
}

struct TestSuite {
    name: String,
    tests: usize,
    failures: usize,
    errors: usize,
    time: String,
    cases: Vec<TestCase>,
}

impl Formatter for JUnit {
    fn summary(&mut self, st: &Storage, out: &mut Out<'_>) {
        let mut features = st.features();
        features.retain(|f| f.doc.feature.is_some());
        let name = |f: &FeatureModel| {
            f.doc
                .feature
                .as_ref()
                .map(|x| x.name.clone())
                .unwrap_or_default()
        };
        features.sort_by_key(|f| name(f));
        let run_started = st.started_at;
        let total_time = seconds(run_started, Instant::now());
        let (mut tests, mut failures, mut errors) = (0, 0, 0);
        let mut suites = Vec::new();
        for feature in features {
            let pickles = st.pickles(&feature.uri);
            let mut ts = TestSuite {
                name: name(feature),
                tests: 0,
                failures: 0,
                errors: 0,
                time: String::new(),
                cases: Vec::new(),
            };
            let mut names: HashMap<&str, usize> = HashMap::new();
            for pickle in &pickles {
                *names.entry(pickle.name.as_str()).or_default() += 1;
            }
            let mut first_started = run_started;
            let mut last_finished = run_started;
            // Scenarios that share a name (an outline's examples) are numbered.
            let mut outline_no: HashMap<&str, usize> = HashMap::new();
            for (idx, pickle) in pickles.iter().enumerate() {
                let mut tc = TestCase {
                    name: pickle.name.clone(),
                    ..TestCase::default()
                };
                if names[pickle.name.as_str()] > 1 {
                    let n = outline_no.entry(pickle.name.as_str()).or_default();
                    *n += 1;
                    tc.name.push_str(&format!(" #{n}"));
                }
                let started = st.pickle_result(&pickle.id);
                match started {
                    None => tc.status = Status::Skipped.as_str().to_string(),
                    Some(at) => {
                        if idx == 0 {
                            first_started = at;
                        }
                        last_finished = at;
                    }
                }
                if let Some(result) = pickle.steps.last().and_then(|s| st.step_result(&s.id)) {
                    last_finished = result.finished_at;
                }
                if let Some(at) = started {
                    tc.time = seconds(at, last_finished);
                }
                ts.tests += 1;
                tests += 1;
                for result in st.step_results_by_pickle(&pickle.id) {
                    let text = st
                        .pickle_step(&result.step_id)
                        .map_or("", |s| s.text.as_str());
                    match result.status {
                        Status::Passed => tc.status = "passed".to_string(),
                        Status::Failed => {
                            tc.status = "failed".to_string();
                            tc.failure = Some(format!(
                                "Step {text}: {}",
                                result.err.as_deref().unwrap_or("%!s(<nil>)")
                            ));
                        }
                        Status::Ambiguous => {
                            tc.status = "ambiguous".to_string();
                            tc.errors.push((format!("Step {text}"), "ambiguous"));
                        }
                        Status::Skipped => tc.errors.push((format!("Step {text}"), "skipped")),
                        Status::Undefined => {
                            tc.status = "undefined".to_string();
                            tc.errors.push((format!("Step {text}"), "undefined"));
                        }
                    }
                }
                match tc.status.as_str() {
                    "failed" => {
                        ts.failures += 1;
                        failures += 1;
                    }
                    "undefined" => {
                        ts.errors += 1;
                        errors += 1;
                    }
                    _ => {}
                }
                ts.cases.push(tc);
            }
            ts.time = seconds(first_started, last_finished);
            suites.push(ts);
        }

        out.print("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        let mut xml = format!(
            "<testsuites name=\"{}\" tests=\"{tests}\" skipped=\"0\" failures=\"{failures}\" errors=\"{errors}\" time=\"{total_time}\">",
            escape_attr(&self.suite)
        );
        for ts in &suites {
            xml.push_str(&format!(
                "\n  <testsuite name=\"{}\" tests=\"{}\" skipped=\"0\" failures=\"{}\" errors=\"{}\" time=\"{}\">",
                escape_attr(&ts.name),
                ts.tests,
                ts.failures,
                ts.errors,
                ts.time
            ));
            for tc in &ts.cases {
                xml.push_str(&format!(
                    "\n    <testcase name=\"{}\" status=\"{}\" time=\"{}\">",
                    escape_attr(&tc.name),
                    tc.status,
                    tc.time
                ));
                if let Some(message) = &tc.failure {
                    xml.push_str(&format!(
                        "\n      <failure message=\"{}\"></failure>",
                        escape_attr(message)
                    ));
                }
                for (message, kind) in &tc.errors {
                    xml.push_str(&format!(
                        "\n      <error message=\"{}\" type=\"{kind}\"></error>",
                        escape_attr(message)
                    ));
                }
                if tc.failure.is_some() || !tc.errors.is_empty() {
                    xml.push_str("\n    ");
                }
                xml.push_str("</testcase>");
            }
            if !ts.cases.is_empty() {
                xml.push_str("\n  ");
            }
            xml.push_str("</testsuite>");
        }
        if !suites.is_empty() {
            xml.push('\n');
        }
        xml.push_str("</testsuites>");
        out.print(&xml);
    }
}
