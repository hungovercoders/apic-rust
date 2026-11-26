//! The built-in step vocabulary: what each step does, and the adapter
//! that hands a matched step's captures, table or doc string to it.

use std::collections::BTreeMap;

use apic_assert::Expr;
use apic_gocompat::quote;
use apic_httpc::CancelToken;
use apic_phrase::unquote;
use apic_runner::{usage, Code, Error};

use crate::engine::{StepDef, StepError, World};
use crate::gherkin::{PickleArgument, PickleDocString, Table};
use crate::jsonmatch::{json_contains, json_equal};
use crate::phrases::CompiledPhrase;
use crate::state::{check_var_name, excerpt, lock, table_vars, undo_all, Run, Scenario};

/// One line of the vocabulary: a step as it is written and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VocabularyEntry {
    pub pattern: &'static str,
    pub purpose: &'static str,
}

const fn entry(pattern: &'static str, purpose: &'static str) -> VocabularyEntry {
    VocabularyEntry { pattern, purpose }
}

/// Documents the built-in steps for `apic test --steps` and the docs.
pub static VOCABULARY: &[VocabularyEntry] = &[
    entry(r#"the environment is "<name>""#, "switch the scenario to another environment"),
    entry(r#"the variable "<name>" is "<value>""#, "set a variable for later steps"),
    entry("the variables: (table name | value)", "set several variables"),
    entry(r#"I run "<request>""#, "send a request by id; fails if its # @assert or # @capture fail"),
    entry(r#"I run "<request>" with: (table name | value)"#, "send a request with variables"),
    entry(r#"I run the file "<file.http>""#, "send every request in a file, stopping at the first failure"),
    entry("the response status is <n> / is not <n>", "status code"),
    entry("the response is successful / a client error / a server error", "2xx / 4xx / 5xx"),
    entry(
        r#"the response body "<path>" is "<value>""#,
        "also: is not, contains, starts with, ends with, matches; <path> like $.items[0].id, $..id or $.items[?(@.done == true)].id",
    ),
    entry(r#"the response header "<name>" is "<value>""#, "same operators as for the body"),
    entry(
        r#"the response cookie "<name>" is "<value>""#,
        "a cookie the response set; same operators, also exists / does not exist",
    ),
    entry(r#"the response body "<path>" exists / does not exist"#, "presence of a value"),
    entry(
        r#"the response body "<path>" has length <n>"#,
        "characters of a string, elements of an array, keys of an object",
    ),
    entry(
        r#"the response body "<path>" is a number"#,
        "also: a string, an integer, a boolean, an array, an object, null",
    ),
    entry(
        r#"the response body "<path>" is empty / is not empty"#,
        "an empty string, array or object",
    ),
    entry(
        r#"the response body matches the schema "<file.json>""#,
        r#"JSON Schema (2020-12 or draft-07), path from the project root; also: the response body "<path>" matches the schema "<file.json>""#,
    ),
    entry("the response body is: (doc string)", "semantic JSON equality"),
    entry("the response body contains: (doc string)", "JSON subset match"),
    entry("the response time is under <n> ms", "round-trip time"),
    entry(
        r#"I capture the response body "<path>" as "<name>""#,
        "store a value; also: header, cookie",
    ),
];

fn op_word(word: &str) -> &'static str {
    match word {
        "is" | "equals" => "==",
        "is not" => "!=",
        "contains" => "contains",
        "starts with" => "startsWith",
        "ends with" => "endsWith",
        "matches" => "matches",
        _ => "",
    }
}

fn type_predicate(kind: &str) -> &'static str {
    match kind {
        "a string" => "isString",
        "a number" => "isNumber",
        "an integer" => "isInteger",
        "a boolean" => "isBoolean",
        "an array" => "isArray",
        "an object" => "isObject",
        "null" => "isNull",
        _ => "",
    }
}

/// What a step function takes, in order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Int,
    Table,
    Doc,
}

/// A matched step's arguments, converted to what its function takes.
struct Args<'a> {
    strings: Vec<String>,
    ints: Vec<i64>,
    table: Option<&'a Table>,
    doc: Option<&'a PickleDocString>,
}

impl Args<'_> {
    fn s(&self, i: usize) -> &str {
        self.strings.get(i).map_or("", String::as_str)
    }

    fn int(&self, i: usize) -> i64 {
        self.ints.get(i).copied().unwrap_or(0)
    }
}

type StepFn = fn(&mut Steps<'_, '_>, &Args<'_>) -> Result<(), Error>;

struct Handler {
    /// The label a report prints for the definition.
    func: &'static str,
    kinds: &'static [Kind],
    run: StepFn,
}

/// The implementation bound to each built-in step, by name.
fn handler(name: &str) -> Option<Handler> {
    use Kind::{Doc, Int, Str, Table};
    let h = |func, kinds, run| Some(Handler { func, kinds, run });
    match name {
        "environment" => h("stepEnvironment", &[Str], step_environment),
        "variable" => h("stepVariable", &[Str, Str], step_variable),
        "variables" => h("stepVariables", &[Table], step_variables),
        "run" => h("stepRun", &[Str], step_run),
        "run-with" => h("stepRunWith", &[Str, Table], step_run_with),
        "run-file" => h("stepRunFile", &[Str], step_run_file),
        "status" => h("stepStatus", &[Int], step_status),
        "status-not" => h("stepStatusNot", &[Int], step_status_not),
        "status-class" => h("stepStatusClass", &[Str], step_status_class),
        "compare" => h("stepCompare", &[Str, Str, Str, Str], step_compare),
        "exists" => h("stepExists", &[Str, Str, Str], step_exists),
        "length" => h("stepLength", &[Str, Int], step_length),
        "type" => h("stepType", &[Str, Str], step_type),
        "empty" => h("stepEmpty", &[Str, Str], step_empty),
        "schema" => h("stepSchema", &[Str, Str], step_schema),
        "body-equals" => h("stepBodyEquals", &[Doc], step_body_equals),
        "body-contains" => h("stepBodyContains", &[Doc], step_body_contains),
        "duration" => h("stepDuration", &[Int], step_duration),
        "capture" => h("stepCapture", &[Str, Str, Str], step_capture),
        _ => None,
    }
}

/// The labels reports print for a step definition (`# file:line ->
/// function` in the pretty format, `file:line` as the cucumber report's
/// match location). They are part of the report text that tools compare,
/// so they stay as they have always read.
const DEFINITION_PACKAGE: &str = "github.com/hungovercoders/apic/internal/bdd";
const BUILTIN_LOCATION: &str = "steps.go:75";
const PHRASE_LOCATION: &str = "phrases.go:69";

enum Bound {
    Builtin(Handler),
    Phrase(usize),
}

/// The step definitions of a run and the state their functions work on.
pub(crate) struct Steps<'r, 'c> {
    pub(crate) run: Run<'c>,
    cancel: &'r CancelToken,
    scenario: Option<Scenario>,
    bound: Vec<Bound>,
    phrases: &'r [CompiledPhrase],
}

impl<'r, 'c> Steps<'r, 'c> {
    /// Registers the built-in vocabulary, then the project's phrases, and
    /// returns the definitions in that order.
    pub(crate) fn new(
        run: Run<'c>,
        cancel: &'r CancelToken,
        phrases: &'r [CompiledPhrase],
    ) -> (Steps<'r, 'c>, Vec<StepDef>) {
        let mut defs = Vec::new();
        let mut bound = Vec::new();
        for b in apic_phrase::builtin() {
            let h = handler(b.name)
                .unwrap_or_else(|| panic!("no handler for built-in step {}", b.name));
            defs.push(StepDef {
                expr: b.regex.to_string(),
                regex: apic_phrase::compile(b.regex).expect("a valid built-in pattern"),
                id: format!("{BUILTIN_LOCATION} -> {DEFINITION_PACKAGE}.{}", h.func),
            });
            bound.push(Bound::Builtin(h));
        }
        for (i, cp) in phrases.iter().enumerate() {
            defs.push(StepDef {
                expr: cp.phrase.regex.clone(),
                regex: cp.regex.clone(),
                id: format!(
                    "{PHRASE_LOCATION} -> {DEFINITION_PACKAGE}.handlerFor.func{}",
                    cp.phrase.params.len() + 1
                ),
            });
            bound.push(Bound::Phrase(i));
        }
        let steps = Steps {
            run,
            cancel,
            scenario: None,
            bound,
            phrases,
        };
        (steps, defs)
    }

    fn sc(&self) -> Result<&Scenario, Error> {
        self.scenario
            .as_ref()
            .ok_or_else(|| Error::Other("no scenario state (internal error)".to_string()))
    }

    fn render(&mut self, text: &str) -> Result<String, Error> {
        let sc = self
            .scenario
            .as_ref()
            .ok_or_else(|| Error::Other("no scenario state (internal error)".to_string()))?;
        sc.render(&mut self.run, text)
    }

    fn run_target(&mut self, target: &str, vars: &BTreeMap<String, String>) -> Result<(), Error> {
        let Some(sc) = self.scenario.as_mut() else {
            return Err(Error::Other(
                "no scenario state (internal error)".to_string(),
            ));
        };
        sc.run(&mut self.run, self.cancel, target, vars)
    }

    /// Runs a phrase's request with the phrase's parameters as variables.
    fn run_phrase(&mut self, index: usize, captures: &[String]) -> Result<(), Error> {
        self.sc()?;
        let cp = &self.phrases[index];
        let mut vars = cp.phrase.values(captures);
        for v in vars.values_mut() {
            *v = self.render(v)?;
        }
        let Some(sc) = self.scenario.as_mut() else {
            return Err(Error::Other(
                "no scenario state (internal error)".to_string(),
            ));
        };
        sc.run_requests(
            &mut self.run,
            self.cancel,
            std::slice::from_ref(&cp.req),
            &vars,
        )
    }

    /// Evaluates one assertion against the last response.
    fn check(&mut self, sel: &str, op: &str, expected: &str) -> Result<(), Error> {
        let e = Expr {
            selector: sel.to_string(),
            op: op.to_string(),
            value: expected.to_string(),
            ..Expr::default()
        };
        self.check_expr(&e, expected)
    }

    /// Evaluates a parsed assertion against the last response; a schema it
    /// names is read from the project root.
    fn check_expr(&mut self, e: &Expr, expected: &str) -> Result<(), Error> {
        let sc = self.sc()?;
        let res = sc.require_last()?;
        let Some(raw) = res.raw() else {
            return Ok(());
        };
        let runner = sc.r.clone();
        let loader =
            apic_assert::schemas(|path| runner.project_file(path).map_err(|e| e.to_string()));
        let r = apic_assert::eval_with(
            e,
            expected,
            raw,
            apic_assert::Options {
                schema: Some(&loader),
            },
        );
        if res.redact {
            if !r.error.is_empty() || !r.pass {
                let text = format!(
                    "assertion failed: {} (values hidden by --redact)\n{}",
                    apic_runner::redact_expr(&r.expr),
                    self.run.describe_failure(&res)
                );
                return Err(self.run.fail(Error::Other(text)));
            }
            return Ok(());
        }
        if !r.error.is_empty() {
            let text = format!(
                "{}: {}\n{}",
                r.expr,
                r.error,
                self.run.describe_failure(&res)
            );
            return Err(self.run.fail(Error::Other(text)));
        }
        if !r.pass {
            let text = format!(
                "expected {}, got {}\n{}",
                r.expr,
                quote(&excerpt(&r.actual, 120)),
                self.run.describe_failure(&res)
            );
            return Err(self.run.fail(Error::Other(text)));
        }
        Ok(())
    }

    fn body_match(&mut self, doc: Option<&PickleDocString>, exact: bool) -> Result<(), Error> {
        let sc = self.sc()?;
        let res = sc.require_last()?;
        let Some(raw) = res.raw() else {
            return Ok(());
        };
        let doc = doc.ok_or_else(nil_argument)?;
        let expected = self.render(&doc.content)?;
        let outcome = if exact {
            json_equal(&raw.body, expected.as_bytes())
        } else {
            json_contains(&raw.body, expected.as_bytes())
        };
        if let Err(why) = outcome {
            if res.redact {
                return Err(Error::Other(
                    "response body does not match the expected document (details hidden by --redact)"
                        .to_string(),
                ));
            }
            let text = format!(
                "response body mismatch: {why}\n{}",
                self.run.describe_failure(&res)
            );
            return Err(self.run.fail(Error::Other(text)));
        }
        Ok(())
    }
}

/// What a step reports when the table or doc string its function reads is
/// not the kind of argument the step was given.
fn nil_argument() -> Error {
    Error::Other("runtime error: invalid memory address or nil pointer dereference".to_string())
}

/// Converts what a step matched into what its function takes: the
/// captures, then the step's own argument.
fn convert<'a>(
    kinds: &[Kind],
    captures: &[String],
    argument: Option<&'a PickleArgument>,
) -> Result<Args<'a>, String> {
    let given = captures.len() + usize::from(argument.is_some());
    if given < kinds.len() {
        return Err(format!(
            "func expected more arguments than given: expected {} arguments, matched {given} from step",
            kinds.len()
        ));
    }
    let mut args = Args {
        strings: Vec::new(),
        ints: Vec::new(),
        table: None,
        doc: None,
    };
    for (i, kind) in kinds.iter().enumerate() {
        let capture = captures.get(i);
        match kind {
            Kind::Str | Kind::Int => {
                let s = match capture {
                    Some(s) => s.clone(),
                    // A string read from the step's argument is its doc string.
                    None => match argument.and_then(|a| a.doc_string.as_ref()) {
                        Some(doc) => doc.content.clone(),
                        None => {
                            return Err(format!(
                                "cannot convert argument {i}: DocString is not set"
                            ))
                        }
                    },
                };
                if *kind == Kind::Int {
                    let n: i64 = s.parse().map_err(|_| {
                        format!(
                            "cannot convert argument {i}: \"{s}\" to int: strconv.ParseInt: parsing {}: value out of range",
                            quote(&s)
                        )
                    })?;
                    args.ints.push(n);
                    args.strings.push(String::new());
                } else {
                    args.strings.push(s);
                    args.ints.push(0);
                }
            }
            Kind::Table | Kind::Doc => {
                if capture.is_some() {
                    let want = if *kind == Kind::Table {
                        "*messages.PickleTable"
                    } else {
                        "*messages.PickleDocString"
                    };
                    return Err(format!(
                        "cannot convert argument {i}: \"{}\" of type \"string\" to {want}",
                        capture.map_or("", String::as_str)
                    ));
                }
                args.table = argument.and_then(|a| a.data_table.as_ref());
                args.doc = argument.and_then(|a| a.doc_string.as_ref());
                args.strings.push(String::new());
                args.ints.push(0);
            }
        }
    }
    Ok(args)
}

impl World for Steps<'_, '_> {
    fn before(&mut self) -> Result<(), StepError> {
        self.scenario = None;
        if self.run.halted {
            // --stop-on-failure: later scenarios are recorded as skipped
            // rather than aborting the run, which would leave the report
            // incomplete.
            return Err(StepError::skip());
        }
        let env = self.run.cfg.env.clone();
        match self.run.new_scenario(&env) {
            Ok(sc) => {
                self.scenario = Some(sc);
                Ok(())
            }
            Err(err) => Err(StepError::Failed(err.to_string())),
        }
    }

    fn after(&mut self, err: Option<&StepError>) {
        if err.is_some_and(|e| !e.is_skip()) && self.run.stop_on_failure {
            self.run.halted = true;
        }
    }

    fn call(
        &mut self,
        def: usize,
        captures: &[String],
        argument: Option<&PickleArgument>,
    ) -> Result<(), StepError> {
        let failed = |e: Error| StepError::Failed(e.to_string());
        match &self.bound[def] {
            Bound::Phrase(index) => {
                let index = *index;
                let wanted = self.phrases[index].phrase.params.len();
                let given = captures.len() + usize::from(argument.is_some());
                if given < wanted {
                    return Err(StepError::Failed(format!(
                        "func expected more arguments than given: expected {wanted} arguments, matched {given} from step"
                    )));
                }
                self.run_phrase(index, captures).map_err(failed)
            }
            Bound::Builtin(h) => {
                let run = h.run;
                let args = convert(h.kinds, captures, argument).map_err(StepError::Failed)?;
                run(self, &args).map_err(failed)
            }
        }
    }
}

fn step_environment(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let env = w.render(a.s(0))?;
    let sc = w.sc()?;
    let (store, jar) = (sc.r.session.clone(), sc.r.jar.clone());
    let mut next = match w.run.scenario_with(&env, store, jar) {
        Ok(next) => next,
        Err(err) => return Err(w.run.fail(err)),
    };
    let sc = w.sc()?;
    // The scenario's state survives the switch: variables set by steps,
    // what the session held for the previous environment, values captured
    // so far and the last response. Session values are keyed by
    // environment, so they are carried as captures of this scenario.
    for (k, v) in sc.r.vars() {
        next.r.set_var(&k, &v);
    }
    if let Some(session) = &sc.r.session {
        let mut cache = BTreeMap::new();
        let held = lock(session).vars(&sc.r.opts.env);
        for (k, v) in held {
            if k.starts_with('$') {
                // Auth caches ($oauth2:, $exec:) are read from the session
                // under the current environment, not from captures.
                cache.insert(k, v);
                continue;
            }
            next.r.capture(&k, &v);
        }
        if !cache.is_empty() {
            if let Some(next_session) = &next.r.session {
                let mut store = lock(next_session);
                store.set(&next.r.opts.env, cache);
                // A no-op for the in-memory store.
                if let Err(err) = store.save() {
                    drop(store);
                    return Err(w.run.fail(Error::Other(format!("session: {err}"))));
                }
            }
        }
    }
    for (k, v) in sc.r.captured() {
        next.r.capture(&k, &v);
    }
    for (k, v) in sc.r.results() {
        // Keeps {{name.response...}} references working.
        next.r.set_result(&k, v);
    }
    next.last = sc.last.clone();
    w.scenario = Some(next);
    Ok(())
}

fn step_variable(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    if let Err(err) = check_var_name(a.s(0)) {
        return Err(w.run.fail(err));
    }
    let v = w.render(a.s(1))?;
    w.sc()?.r.set_var(a.s(0), &v);
    Ok(())
}

fn step_variables(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let rows = match table_vars(a.table.ok_or_else(nil_argument)?) {
        Ok(rows) => rows,
        Err(err) => return Err(w.run.fail(err)),
    };
    // Rows apply in order so `| url | {{base}}/x |` may follow `| base | … |`.
    for row in rows {
        let rv = w.render(&row.value)?;
        w.sc()?.r.set_var(&row.name, &rv);
    }
    Ok(())
}

fn step_run(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let target = w.render(a.s(0))?;
    w.run_target(&target, &BTreeMap::new())
}

/// Runs a whole .http file; a request id or a `file#fragment` target is
/// refused so the step means what it says.
fn step_run_file(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let target = w.render(a.s(0))?;
    // The whole target is a file name first (odd#name.http is a file);
    // only then is a `#` read as a fragment, which this step refuses.
    let file = w.sc()?.r.project.file(&target).cloned();
    if let Some(f) = file {
        let Some(sc) = w.scenario.as_mut() else {
            return Ok(());
        };
        return sc.run_requests(&mut w.run, w.cancel, &f.requests, &BTreeMap::new());
    }
    let lower = target.to_lowercase();
    let is_file = lower.ends_with(".http") || lower.ends_with(".rest");
    if target.contains('#') || !is_file {
        let q = quote(&target);
        return Err(w.run.fail(usage(
            Code::Features,
            format!("I run the file: {q} is not a .http/.rest file (use `I run {q}` for a single request)"),
        )));
    }
    w.run_target(&target, &BTreeMap::new())
}

fn step_run_with(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let rows = match table_vars(a.table.ok_or_else(nil_argument)?) {
        Ok(rows) => rows,
        Err(err) => return Err(w.run.fail(err)),
    };
    // Rows render in order, each seeing the ones above it, and the table
    // may supply the target itself, so it stays scoped while the target
    // renders.
    let Some(sc) = w.scenario.as_ref() else {
        return Ok(());
    };
    let (vars, undo) = sc.render_table(&mut w.run, &rows)?;
    let target = sc.render(&mut w.run, a.s(0));
    undo_all(&sc.r, undo);
    w.run_target(&target?, &vars)
}

fn step_status(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.check("status", "==", &a.int(0).to_string())
}

fn step_status_not(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.check("status", "!=", &a.int(0).to_string())
}

fn step_status_class(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    let res = w.sc()?.require_last()?;
    let Some(raw) = res.raw() else {
        return Ok(());
    };
    let class = a.s(0);
    let st = raw.status;
    let ok = match class {
        "successful" => (200..300).contains(&st),
        "a client error" => (400..500).contains(&st),
        "a server error" => (500..600).contains(&st),
        _ => false,
    };
    if !ok {
        let text = format!(
            "expected the response to be {class}, got {st} {}\n{}",
            raw.status_text,
            w.run.describe_failure(&res)
        );
        return Err(w.run.fail(Error::Other(text)));
    }
    Ok(())
}

fn step_compare(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let sel = w.render(a.s(1))?;
    let expected = w.render(a.s(3))?;
    w.check(&selector(a.s(0), &sel), op_word(a.s(2)), &expected)
}

fn step_exists(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let sel = w.render(a.s(1))?;
    let op = if a.s(2) == "does not exist" {
        "not exists"
    } else {
        "exists"
    };
    w.check(&selector(a.s(0), &sel), op, "")
}

fn body_expr(sel: &str, op: &str) -> Expr {
    Expr {
        selector: selector("body", sel),
        op: op.to_string(),
        ..Expr::default()
    }
}

fn step_length(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let sel = w.render(a.s(0))?;
    let e = Expr {
        length: true,
        ..body_expr(&sel, "==")
    };
    w.check_expr(&e, &a.int(1).to_string())
}

fn step_type(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let sel = w.render(a.s(0))?;
    w.check_expr(&body_expr(&sel, type_predicate(a.s(1))), "")
}

fn step_empty(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let sel = w.render(a.s(0))?;
    let op = if a.s(1) == "is not empty" {
        "not isEmpty"
    } else {
        "isEmpty"
    };
    w.check_expr(&body_expr(&sel, op), "")
}

fn step_schema(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.sc()?;
    let sel = w.render(a.s(0))?;
    let file = w.render(a.s(1))?;
    let e = Expr {
        value: file.clone(),
        ..body_expr(&sel, "matchesSchema")
    };
    w.check_expr(&e, &file)
}

fn step_duration(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.check("duration", "<", &a.int(0).to_string())
}

fn step_capture(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    let (place, name) = (a.s(0), a.s(2));
    w.sc()?;
    if let Err(err) = check_var_name(name) {
        return Err(w.run.fail(err));
    }
    let res = w.sc()?.require_last()?;
    let Some(raw) = res.raw() else {
        return Ok(());
    };
    let sel = w.render(a.s(1))?;
    let e = Expr {
        selector: selector(place, &sel),
        op: "exists".to_string(),
        ..Expr::default()
    };
    let r = apic_assert::eval(&e, "", raw);
    if !r.error.is_empty() {
        return Err(w
            .run
            .fail(Error::Other(format!("capture {sel}: {}", r.error))));
    }
    if !r.pass {
        let text = format!(
            "capture {name}: nothing at {}\n{}",
            selector(place, &sel),
            w.run.describe_failure(&res)
        );
        return Err(w.run.fail(Error::Other(text)));
    }
    w.run.note_secrets([&r.actual]);
    let sc = w.sc()?;
    // The same precedence as # @capture: below --var, above env files.
    sc.r.capture(name, &r.actual);
    // With --use-session the value outlives the scenario, like a request
    // capture.
    if w.run.cfg.use_session {
        if let Some(session) = &sc.r.session {
            let mut store = lock(session);
            store.set(&sc.r.opts.env, [(name, r.actual.as_str())]);
            store
                .save()
                .map_err(|err| Error::Other(format!("session: {err}")))?;
        }
    }
    Ok(())
}

fn step_body_equals(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.body_match(a.doc, true)
}

fn step_body_contains(w: &mut Steps<'_, '_>, a: &Args<'_>) -> Result<(), Error> {
    w.body_match(a.doc, false)
}

/// The assertion selector for a step's `body`, `header` or `cookie` and
/// what it names.
pub(crate) fn selector(place: &str, sel: &str) -> String {
    let sel = unquote(sel.trim());
    if place == "header" || place == "cookie" {
        return format!("{place}.{sel}");
    }
    if sel.is_empty() || sel == "$" {
        "body.$".to_string()
    } else if sel.starts_with("$.") || sel.starts_with("$[") {
        format!("body.{sel}")
    } else if sel.starts_with('[') || sel.starts_with("..") {
        format!("body.${sel}")
    } else {
        format!("body.$.{sel}")
    }
}
