//! The project's `# @step` phrases, compiled into step definitions.

use std::collections::BTreeMap;
use std::sync::Arc;

use apic_gocompat::quote;
use apic_httpfile::Request;
use apic_phrase::Phrase;
use apic_project::Project;
use regex::Regex;

/// A `# @step` directive ready to register. It keeps the request itself: a
/// file/name target string could be misread when a file name contains the
/// `#` delimiter.
pub(crate) struct CompiledPhrase {
    pub(crate) phrase: Phrase,
    pub(crate) regex: Regex,
    pub(crate) req: Arc<Request>,
}

/// Validates every `# @step` directive in the project before any scenario
/// runs, so a bad phrase is a usage error even when a tag filter selects
/// no scenarios. Equivalent matchers are rejected.
pub(crate) fn compile_phrases(p: &Project) -> Result<Vec<CompiledPhrase>, String> {
    let mut out: Vec<CompiledPhrase> = Vec::new();
    for req in p.requests() {
        for d in &req.directives {
            if d.key != "step" {
                continue;
            }
            let text = &d.value;
            let at = |err: String| format!("{}:{}: {err}", req.file.path, d.line);
            let ph = apic_phrase::parse(text).map_err(at)?;
            ph.conflicts_with_builtin().map_err(at)?;
            for prev in &out {
                if ph.conflicts_with(&prev.phrase) {
                    return Err(at(format!(
                        "@step {} is ambiguous with @step {} on {} (run `apic validate`)",
                        quote(text),
                        quote(&prev.phrase.text),
                        prev.req.id()
                    )));
                }
            }
            let regex = ph.compile().map_err(|e| at(e.to_string()))?;
            out.push(CompiledPhrase {
                phrase: ph,
                regex,
                req: req.clone(),
            });
        }
    }
    Ok(out)
}

/// Lists every declared phrase with the request it runs, for `list`.
pub fn phrases(p: &Project) -> BTreeMap<String, Arc<Request>> {
    let mut out = BTreeMap::new();
    for req in p.requests() {
        for text in req.steps() {
            out.insert(text, req.clone());
        }
    }
    out
}
