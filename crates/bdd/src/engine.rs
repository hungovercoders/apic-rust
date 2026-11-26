//! The scenario runner: matches each step of each pickle against the step
//! definitions, runs it, records the result and tells the formatter.
//!
//! The rules it follows: a step no definition matches is undefined and one
//! that several match is ambiguous, and both fail the run; after a step
//! fails, the scenario's remaining steps are skipped (still matched, so an
//! undefined one is reported); a scenario hook runs before the first step
//! and one after the step that fails, or after the last.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use regex::Regex;

use crate::colors::{self, ColorFn, Out};
use crate::gherkin::{Document, Pickle, PickleArgument, PickleStep};

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Passed,
    Failed,
    Skipped,
    Undefined,
    Ambiguous,
}

impl Status {
    pub(crate) fn color(self) -> ColorFn {
        match self {
            Status::Passed => colors::green,
            Status::Failed => colors::red,
            Status::Skipped => colors::cyan,
            _ => colors::yellow,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
            Status::Undefined => "undefined",
            Status::Ambiguous => "ambiguous",
        }
    }
}

/// Why a step, or a hook, did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StepError {
    /// The scenario is to be skipped: not a failure.
    Skip(String),
    /// No definition matches the step.
    Undefined(String),
    /// More than one definition matches the step.
    Ambiguous(String),
    Failed(String),
}

impl StepError {
    pub(crate) fn skip() -> StepError {
        StepError::Skip("skipped".to_string())
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            StepError::Skip(m)
            | StepError::Undefined(m)
            | StepError::Ambiguous(m)
            | StepError::Failed(m) => m,
        }
    }

    pub(crate) fn is_skip(&self) -> bool {
        matches!(self, StepError::Skip(_))
    }

    /// The same error with words in front, keeping what it is.
    fn prefixed(self, prefix: &str) -> StepError {
        let wrap = |m: String| format!("{prefix}: {m}");
        match self {
            StepError::Skip(m) => StepError::Skip(wrap(m)),
            StepError::Undefined(m) => StepError::Undefined(wrap(m)),
            StepError::Ambiguous(m) => StepError::Ambiguous(wrap(m)),
            StepError::Failed(m) => StepError::Failed(wrap(m)),
        }
    }
}

/// Reports whether an error fails the run: anything but a skip.
fn should_fail(err: Option<&StepError>) -> bool {
    err.is_some_and(|e| !e.is_skip())
}

/// One step definition: a pattern and the label reports print for it.
pub(crate) struct StepDef {
    /// The pattern as written, shown when a step is ambiguous.
    pub(crate) expr: String,
    pub(crate) regex: Regex,
    /// `file:line -> function`, as the pretty report prints after `#`.
    pub(crate) id: String,
}

/// What the runner drives: the hooks and the step implementations.
pub(crate) trait World {
    /// Runs before a scenario's first step.
    fn before(&mut self) -> Result<(), StepError>;
    /// Runs once the scenario's outcome is known, with the error of the
    /// step that decided it.
    fn after(&mut self, err: Option<&StepError>);
    /// Runs definition `def` with the text its pattern captured and the
    /// step's table or doc string.
    fn call(
        &mut self,
        def: usize,
        captures: &[String],
        argument: Option<&PickleArgument>,
    ) -> Result<(), StepError>;
}

/// A parsed feature with the pickles selected from it.
pub(crate) struct FeatureModel {
    pub(crate) uri: String,
    pub(crate) doc: Document,
    pub(crate) pickles: Vec<Pickle>,
}

#[derive(Debug, Clone)]
pub(crate) struct StepResult {
    pub(crate) status: Status,
    pub(crate) finished_at: Instant,
    pub(crate) err: Option<String>,
    pub(crate) pickle_id: String,
    pub(crate) step_id: String,
    /// The label of the definition that matched, if one did.
    pub(crate) def: Option<String>,
}

/// Everything the formatters read: the features and what has run so far.
/// Listings come back in the text order of their ids (`10` before `9`),
/// which the reports' layout depends on.
pub(crate) struct Storage {
    features: Vec<FeatureModel>,
    pickle_index: HashMap<String, (usize, usize)>,
    step_index: HashMap<String, (usize, usize, usize)>,
    pickle_results: BTreeMap<String, Instant>,
    step_results: BTreeMap<String, StepResult>,
    pub(crate) started_at: Instant,
}

impl Storage {
    fn new(features: Vec<FeatureModel>) -> Storage {
        let mut pickle_index = HashMap::new();
        let mut step_index = HashMap::new();
        for (f, feature) in features.iter().enumerate() {
            for (p, pickle) in feature.pickles.iter().enumerate() {
                pickle_index.insert(pickle.id.clone(), (f, p));
                for (s, step) in pickle.steps.iter().enumerate() {
                    step_index.insert(step.id.clone(), (f, p, s));
                }
            }
        }
        Storage {
            features,
            pickle_index,
            step_index,
            pickle_results: BTreeMap::new(),
            step_results: BTreeMap::new(),
            started_at: Instant::now(),
        }
    }

    pub(crate) fn feature(&self, uri: &str) -> Option<&FeatureModel> {
        self.features.iter().find(|f| f.uri == uri)
    }

    /// The features, by URI.
    pub(crate) fn features(&self) -> Vec<&FeatureModel> {
        let mut out: Vec<&FeatureModel> = self.features.iter().collect();
        out.sort_by(|a, b| a.uri.cmp(&b.uri));
        out
    }

    pub(crate) fn pickle(&self, id: &str) -> Option<&Pickle> {
        let (f, p) = *self.pickle_index.get(id)?;
        Some(&self.features[f].pickles[p])
    }

    pub(crate) fn pickle_step(&self, id: &str) -> Option<&PickleStep> {
        let (f, p, s) = *self.step_index.get(id)?;
        Some(&self.features[f].pickles[p].steps[s])
    }

    /// A feature's pickles in the order they run.
    pub(crate) fn pickles(&self, uri: &str) -> Vec<&Pickle> {
        let mut out: Vec<&Pickle> = self
            .feature(uri)
            .map(|f| f.pickles.iter().collect())
            .unwrap_or_default();
        out.sort_by_key(|p| id_number(&p.id));
        out
    }

    /// When a pickle started, if it has.
    pub(crate) fn pickle_result(&self, pickle_id: &str) -> Option<Instant> {
        self.pickle_results.get(pickle_id).copied()
    }

    /// The ids of the pickles that started.
    pub(crate) fn pickle_results(&self) -> Vec<&str> {
        self.pickle_results.keys().map(String::as_str).collect()
    }

    pub(crate) fn step_result(&self, step_id: &str) -> Option<&StepResult> {
        self.step_results.get(step_id)
    }

    pub(crate) fn step_results_by_pickle(&self, pickle_id: &str) -> Vec<&StepResult> {
        self.step_results
            .values()
            .filter(|r| r.pickle_id == pickle_id)
            .collect()
    }

    /// A pickle's step results up to and including one step's.
    pub(crate) fn step_results_until(&self, pickle_id: &str, step_id: &str) -> Vec<&StepResult> {
        let mut out = Vec::new();
        for r in self.step_results_by_pickle(pickle_id) {
            out.push(r);
            if r.step_id == step_id {
                break;
            }
        }
        out
    }

    pub(crate) fn step_results_by_status(&self, status: Status) -> Vec<&StepResult> {
        self.step_results
            .values()
            .filter(|r| r.status == status)
            .collect()
    }

    fn insert_step_result(
        &mut self,
        status: Status,
        pickle: &Pickle,
        step: &PickleStep,
        def: Option<String>,
        err: Option<String>,
    ) {
        self.step_results.insert(
            step.id.clone(),
            StepResult {
                status,
                finished_at: Instant::now(),
                err,
                pickle_id: pickle.id.clone(),
                step_id: step.id.clone(),
                def,
            },
        );
    }
}

/// An id as a number, for the listings that follow run order.
pub(crate) fn id_number(id: &str) -> u64 {
    id.parse().unwrap_or(0)
}

/// What a report is told as the run goes on. Every step outcome arrives
/// through `step`, after its result is stored.
pub(crate) trait Formatter {
    fn feature(&mut self, _st: &Storage, _out: &mut Out<'_>, _feature: &FeatureModel) {}
    fn pickle(&mut self, _st: &Storage, _out: &mut Out<'_>, _pickle: &Pickle) {}
    fn step(&mut self, _st: &Storage, _out: &mut Out<'_>, _pickle: &Pickle, _step: &PickleStep) {}
    fn summary(&mut self, st: &Storage, out: &mut Out<'_>);
}

struct Suite<'a> {
    defs: &'a [StepDef],
    world: &'a mut dyn World,
    fmt: &'a mut dyn Formatter,
    storage: Storage,
}

/// The definition a step matched and what its pattern captured.
struct Match {
    def: usize,
    captures: Vec<String>,
}

impl Suite<'_> {
    /// Finds the definition for a step's text. More than one is an error
    /// that lists them all.
    fn match_step(&self, text: &str) -> Result<Option<Match>, StepError> {
        let mut first = None;
        let mut matching = Vec::new();
        for (i, def) in self.defs.iter().enumerate() {
            let Some(caps) = def.regex.captures(text) else {
                continue;
            };
            matching.push(def.expr.as_str());
            if first.is_none() {
                first = Some(Match {
                    def: i,
                    captures: caps
                        .iter()
                        .skip(1)
                        .map(|m| m.map_or(String::new(), |m| m.as_str().to_string()))
                        .collect(),
                });
            }
        }
        if matching.len() > 1 {
            return Err(StepError::Ambiguous(format!(
                "ambiguous step definition, step text: {text}\n    matches:\n        {}",
                matching.join("\n        ")
            )));
        }
        Ok(first)
    }

    /// The part of a step's run that decides its error; the caller records
    /// the outcome.
    fn attempt(
        &mut self,
        out: &mut Out<'_>,
        pickle: &Pickle,
        step: &PickleStep,
        scenario_failed: bool,
        is_first: bool,
        matched: &mut Option<Match>,
    ) -> Option<StepError> {
        let hook_err = if is_first {
            self.world
                .before()
                .err()
                .map(|e| e.prefixed("before scenario hook failed"))
        } else {
            None
        };
        let match_result = self.match_step(&step.text);
        let match_err = match match_result {
            Ok(m) => {
                *matched = m;
                None
            }
            Err(e) => Some(e),
        };
        let def_id =
            |m: &Option<Match>, defs: &[StepDef]| m.as_ref().map(|m| defs[m.def].id.clone());
        if let Some(e) = hook_err {
            self.storage.insert_step_result(
                Status::Failed,
                pickle,
                step,
                def_id(matched, self.defs),
                None,
            );
            return Some(e);
        }
        if let Some(e) = match_err {
            return Some(e);
        }
        let Some(m) = matched.as_ref() else {
            self.storage
                .insert_step_result(Status::Undefined, pickle, step, None, None);
            self.fmt.step(&self.storage, out, pickle, step);
            return Some(StepError::Undefined(format!(
                "step is undefined: {}",
                step.text
            )));
        };
        if scenario_failed {
            self.storage.insert_step_result(
                Status::Skipped,
                pickle,
                step,
                Some(self.defs[m.def].id.clone()),
                None,
            );
            self.fmt.step(&self.storage, out, pickle, step);
            return None;
        }
        self.world
            .call(m.def, &m.captures, step.argument.as_ref())
            .err()
    }

    fn run_step(
        &mut self,
        out: &mut Out<'_>,
        pickle: &Pickle,
        step: &PickleStep,
        scenario_err: Option<&StepError>,
        is_first: bool,
        is_last: bool,
    ) -> Option<StepError> {
        let mut matched = None;
        let err = self.attempt(
            out,
            pickle,
            step,
            scenario_err.is_some(),
            is_first,
            &mut matched,
        );
        let early_return = scenario_err.is_some() || matches!(err, Some(StepError::Undefined(_)));
        // The scenario hook runs on the failing step, or on the last one.
        if !should_fail(scenario_err) && (is_last || should_fail(err.as_ref())) {
            self.world.after(err.as_ref());
        }
        if early_return {
            return err;
        }
        let def = matched.as_ref().map(|m| self.defs[m.def].id.clone());
        let (status, message) = match &err {
            None => (Status::Passed, None),
            Some(StepError::Skip(_)) => (Status::Skipped, None),
            Some(StepError::Ambiguous(m)) => (Status::Ambiguous, Some(m.clone())),
            Some(e) => (Status::Failed, Some(e.message().to_string())),
        };
        self.storage
            .insert_step_result(status, pickle, step, def, message);
        self.fmt.step(&self.storage, out, pickle, step);
        err
    }

    fn run_pickle(&mut self, out: &mut Out<'_>, pickle: &Pickle) -> Option<StepError> {
        self.storage
            .pickle_results
            .insert(pickle.id.clone(), Instant::now());
        self.fmt.pickle(&self.storage, out, pickle);
        if pickle.steps.is_empty() {
            return Some(StepError::Undefined(
                "step is undefined: no steps in scenario".to_string(),
            ));
        }
        let mut scenario_err: Option<StepError> = None;
        let last = pickle.steps.len() - 1;
        for (i, step) in pickle.steps.iter().enumerate() {
            let step_err =
                self.run_step(out, pickle, step, scenario_err.as_ref(), i == 0, i == last);
            if scenario_err.is_none() || should_fail(step_err.as_ref()) {
                scenario_err = step_err;
            }
        }
        scenario_err
    }
}

/// Runs every pickle of every feature, in order, and has the formatter
/// print its summary. Reports whether anything failed.
pub(crate) fn run(
    features: Vec<FeatureModel>,
    defs: &[StepDef],
    world: &mut dyn World,
    fmt: &mut dyn Formatter,
    out: &mut Out<'_>,
) -> bool {
    let order: Vec<String> = features.iter().map(|f| f.uri.clone()).collect();
    // Each pickle is run from a copy, so the storage can be written to
    // while it runs.
    let pickles: Vec<Vec<Pickle>> = features.iter().map(|f| f.pickles.clone()).collect();
    let mut suite = Suite {
        defs,
        world,
        fmt,
        storage: Storage::new(features),
    };
    let mut failed = false;
    for (uri, feature_pickles) in order.iter().zip(&pickles) {
        for (i, pickle) in feature_pickles.iter().enumerate() {
            if i == 0 {
                if let Some(feature) = suite.storage.feature(uri) {
                    suite.fmt.feature(&suite.storage, out, feature);
                }
            }
            let err = suite.run_pickle(out, pickle);
            if should_fail(err.as_ref()) {
                failed = true;
            }
        }
    }
    suite.fmt.summary(&suite.storage, out);
    failed
}
