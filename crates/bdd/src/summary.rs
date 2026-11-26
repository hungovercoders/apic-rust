//! The machine-readable outcome of a run, read back from the cucumber
//! JSON report.

use serde::{Deserialize, Serialize};

/// The machine-readable outcome of a run, derived from the cucumber JSON
/// report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// No scenario failed; check `scenarios` for how many ran.
    pub ok: bool,
    pub scenarios: usize,
    pub passed: usize,
    pub failed: usize,
    /// Not run, e.g. after `--stop-on-failure`.
    pub skipped: usize,
    pub undefined: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Failure>,
}

/// Describes one failed or undefined step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub feature: String,
    pub scenario: String,
    pub step: String,
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

#[derive(Deserialize)]
struct CukeFeature {
    #[serde(default)]
    name: String,
    #[serde(default)]
    elements: Option<Vec<CukeElement>>,
}

#[derive(Deserialize)]
struct CukeElement {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    steps: Option<Vec<CukeStep>>,
}

#[derive(Deserialize)]
struct CukeStep {
    #[serde(default)]
    keyword: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    result: CukeResult,
}

#[derive(Deserialize, Default)]
struct CukeResult {
    #[serde(default)]
    status: String,
    #[serde(default)]
    error_message: String,
}

fn failure(feature: &str, scenario: &str, st: &CukeStep) -> Option<Failure> {
    matches!(
        st.result.status.as_str(),
        "failed" | "undefined" | "pending" | "ambiguous"
    )
    .then(|| Failure {
        feature: feature.to_string(),
        scenario: scenario.to_string(),
        step: format!("{} {}", st.keyword.trim(), st.name),
        status: st.result.status.clone(),
        error: st.result.error_message.clone(),
    })
}

/// Parses a cucumber JSON report.
pub fn summarize(report: &[u8]) -> Result<Summary, String> {
    let features: Option<Vec<CukeFeature>> =
        serde_json::from_slice(report).map_err(|err| format!("parse cucumber report: {err}"))?;
    let mut s = Summary::default();
    for f in features.unwrap_or_default() {
        // A background element comes before each scenario it applies to;
        // its failures belong to that scenario.
        let mut pending: Vec<Failure> = Vec::new();
        for el in f.elements.unwrap_or_default() {
            let steps = el.steps.unwrap_or_default();
            if el.kind == "background" {
                pending.extend(steps.iter().filter_map(|st| failure(&f.name, "", st)));
                continue;
            }
            if el.kind != "scenario" {
                continue;
            }
            s.scenarios += 1;
            let mut failed = !pending.is_empty();
            for mut pf in pending.drain(..) {
                pf.scenario = el.name.clone();
                if pf.status == "undefined" {
                    s.undefined += 1;
                }
                s.failures.push(pf);
            }
            let mut ran = false;
            for st in &steps {
                if let Some(fail) = failure(&f.name, &el.name, st) {
                    failed = true;
                    if fail.status == "undefined" {
                        s.undefined += 1;
                    }
                    s.failures.push(fail);
                } else if st.result.status == "passed" {
                    ran = true;
                }
            }
            if failed {
                s.failed += 1;
            } else if !ran && !steps.is_empty() {
                // Every step skipped: the scenario never ran.
                s.skipped += 1;
            } else {
                s.passed += 1;
            }
        }
    }
    // An empty selection (e.g. a tag matching nothing) is not a failure;
    // `scenarios` says how many ran.
    s.ok = s.failed == 0;
    Ok(s)
}
