use std::io::Write;
use std::sync::LazyLock;

use apic_gocompat::{check_json, indent_json, Time};
use apic_runner::{Result as RunResult, BASE64};
use serde::Deserialize;

use crate::template::{Template, Val};

const SOURCE: &str = include_str!("../report.html.tmpl");

/// The parsed template. The source is kept LF on every platform; the fold
/// is the belt to that brace, so a report is byte-identical whatever a
/// build machine checked out.
static PAGE: LazyLock<Template> = LazyLock::new(|| {
    Template::parse(&SOURCE.replace("\r\n", "\n")).expect("report.html.tmpl parses")
});

/// What the report says about the run as a whole.
#[derive(Clone, Debug, Default)]
pub struct Meta {
    /// "apic run" or "apic test".
    pub title: String,
    /// apic version.
    pub version: String,
    /// Environment in effect, "" for none.
    pub env: String,
    /// When the run started; `None` means now.
    pub time: Option<Time>,
    /// --redact was in force.
    pub redacted: bool,
    /// Project root, for the header.
    pub project: String,
}

/// Why a report could not be written.
#[derive(Debug)]
pub enum Error {
    /// The cucumber report is not what `apic test` produces.
    Parse(String),
    /// The page could not be rendered.
    Template(String),
    /// The destination refused the page.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "parse cucumber report: {e}"),
            Error::Template(e) => write!(f, "template: {e}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

fn render(w: &mut dyn Write, block: &str, page: &Val) -> Result<(), Error> {
    let html = PAGE.execute(block, page).map_err(Error::Template)?;
    w.write_all(html.as_bytes()).map_err(Error::Io)
}

/// The fields every page has, with the title and the time filled in.
fn fill(m: &Meta, title: &str) -> Vec<(&'static str, Val)> {
    let title = if m.title.is_empty() { title } else { &m.title };
    let time = m.time.unwrap_or_else(Time::now);
    // In UTC, to the second.
    let time = Time::from_unix(time.unix(), 0);
    vec![
        ("Title", title.into()),
        ("Version", m.version.as_str().into()),
        ("Env", m.env.as_str().into()),
        ("Time", Val::Time(time)),
        ("Redacted", m.redacted.into()),
        ("Project", m.project.as_str().into()),
    ]
}

fn page(meta: Vec<(&'static str, Val)>, fields: Vec<(&'static str, Val)>) -> Val {
    Val::Map(meta.into_iter().chain(fields).collect())
}

fn kv(key: &str, value: &str) -> Val {
    Val::map([("Key", key.into()), ("Value", value.into())])
}

const MILLISECOND: i64 = 1_000_000;

/// Writes the HTML report of an `apic run`: one section per request,
/// dependencies nested.
pub fn run(w: &mut dyn Write, meta: &Meta, results: &[RunResult]) -> Result<(), Error> {
    let (mut passed, mut failed, mut skipped) = (0usize, 0usize, 0usize);
    let mut duration = 0i64;
    let mut requests = Vec::new();
    for res in results {
        let (v, took) = view(res);
        requests.push(v);
        duration += took;
        if !res.skipped.is_empty() {
            skipped += 1;
        } else if res.ok {
            passed += 1;
        } else {
            failed += 1;
        }
    }
    let p = page(
        fill(meta, "apic run"),
        vec![
            ("Passed", passed.into()),
            ("Failed", failed.into()),
            ("Skipped", skipped.into()),
            ("Duration", Val::Duration(duration)),
            ("Requests", Val::List(requests)),
        ],
    );
    render(w, "run", &p)
}

/// One request as the page shows it, and how long its response took, in
/// nanoseconds.
fn view(res: &RunResult) -> (Val, i64) {
    let q = &res.request;
    let name = if q.name.is_empty() {
        format!("{}#{}", q.file, q.line)
    } else {
        q.name.clone()
    };
    let mut status = 0;
    let mut status_text = String::new();
    let mut duration = 0;
    let mut size = 0usize;
    let mut timings = Val::Nil;
    let mut response_headers = Vec::new();
    let mut response_body = String::new();
    if let Some(r) = res.display_response() {
        status = r.status;
        status_text = r.status_text.clone();
        duration = r.duration_ms * MILLISECOND;
        size = r.size;
        if let Some(t) = &r.timings {
            timings = Val::map([
                ("DNSMs", t.dns_ms.into()),
                ("ConnectMs", t.connect_ms.into()),
                ("TLSMs", t.tls_ms.into()),
                ("TTFBMs", t.ttfb_ms.into()),
                ("TotalMs", t.total_ms.into()),
                ("Reused", t.reused.into()),
            ]);
        }
        response_headers = r.headers.iter().map(|(k, v)| kv(k, v)).collect();
        response_body = String::from_utf8_lossy(&res.display_raw_body()).into_owned();
        if r.body_encoding == BASE64 {
            response_body = format!("(binary body, {} bytes)", r.size);
        }
    }
    let asserts = Val::list(res.display_asserts(), |a| {
        Val::map([
            ("Expr", a.expr.into()),
            ("Actual", a.actual.into()),
            ("Expected", a.expected.into()),
            ("Error", a.error.into()),
            ("Pass", a.pass.into()),
        ])
    });
    let v = Val::map([
        ("Name", name.into()),
        ("Method", q.method.as_str().into()),
        ("URL", q.display_url(res.redact).into()),
        ("File", q.file.as_str().into()),
        ("Line", q.line.into()),
        ("OK", res.ok.into()),
        ("Skipped", res.skipped.as_str().into()),
        ("Status", status.into()),
        ("StatusText", status_text.into()),
        ("Duration", Val::Duration(duration)),
        ("Size", size.into()),
        ("Attempts", res.attempts.into()),
        ("Auth", q.auth.as_str().into()),
        (
            "RequestHeaders",
            Val::list(q.display_headers(res.redact), |h| kv(&h.name, &h.value)),
        ),
        ("RequestBody", q.display_body(res.redact).into()),
        ("ResponseHeaders", Val::List(response_headers)),
        ("ResponseBody", response_body.into()),
        ("Asserts", asserts),
        // The captures by name. Where the body was saved is not among
        // them: the list has only ever held what was captured.
        (
            "Captures",
            Val::list(res.display_captures(), |(k, v)| kv(&k, &v)),
        ),
        ("Errors", Val::list(&res.errors, |e| e.as_str().into())),
        ("Deps", Val::list(&res.deps, |d| view(d).0)),
        ("Timings", timings),
    ]);
    (v, duration)
}

/// Indents a JSON body for reading; anything else is returned as it is.
pub(crate) fn pretty_json(s: &str) -> String {
    let t = s.trim();
    if !(t.starts_with('{') || t.starts_with('[')) || check_json(t.as_bytes()).is_err() {
        return s.to_string();
    }
    indent_json(t, "", "  ")
}

/// The subset of the cucumber JSON the report reads.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CukeFeature {
    uri: String,
    name: String,
    elements: Option<Vec<CukeElement>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CukeElement {
    keyword: String,
    name: String,
    r#type: String,
    steps: Option<Vec<CukeStep>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CukeStep {
    keyword: String,
    name: String,
    result: CukeResult,
    doc_string: Option<CukeDocString>,
    rows: Option<Vec<CukeRow>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CukeResult {
    status: String,
    duration: i64,
    error_message: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CukeDocString {
    value: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CukeRow {
    cells: Option<Vec<String>>,
}

#[derive(Clone)]
struct StepView {
    status: String,
    duration: i64,
    view: Val,
}

/// Writes the HTML report of an `apic test` from its cucumber JSON report,
/// which is already masked under --redact.
pub fn features(w: &mut dyn Write, meta: &Meta, cucumber: &[u8]) -> Result<(), Error> {
    let mut feats: Vec<CukeFeature> = Vec::new();
    if !cucumber.trim_ascii().is_empty() {
        let parsed: Option<Vec<CukeFeature>> =
            serde_json::from_slice(cucumber).map_err(|e| Error::Parse(e.to_string()))?;
        feats = parsed.unwrap_or_default();
    }
    let (mut scenarios, mut passed, mut failed, mut skipped, mut undefined, mut steps_total) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut duration = 0i64;
    let mut feature_views = Vec::new();
    for f in feats {
        let mut feature_passed = true;
        let mut scenario_views = Vec::new();
        // A background element comes before each scenario it applies to;
        // its steps are shown under that scenario.
        let mut background: Vec<StepView> = Vec::new();
        for el in f.elements.unwrap_or_default() {
            let steps: Vec<StepView> = el
                .steps
                .unwrap_or_default()
                .into_iter()
                .map(|st| StepView {
                    status: st.result.status.clone(),
                    duration: st.result.duration,
                    view: Val::map([
                        ("Keyword", st.keyword.trim().into()),
                        ("Name", st.name.into()),
                        ("Status", st.result.status.into()),
                        ("Error", st.result.error_message.into()),
                        (
                            "DocString",
                            st.doc_string.map(|d| d.value).unwrap_or_default().into(),
                        ),
                        (
                            "Rows",
                            Val::list(st.rows.unwrap_or_default(), |r| {
                                Val::list(r.cells.unwrap_or_default(), Val::from)
                            }),
                        ),
                        ("Duration", Val::Duration(st.result.duration)),
                    ]),
                })
                .collect();
            if el.r#type == "background" {
                background = steps;
                continue;
            }
            if el.r#type != "scenario" {
                continue;
            }
            let all: Vec<StepView> = std::mem::take(&mut background)
                .into_iter()
                .chain(steps)
                .collect();
            let mut status = "passed".to_string();
            let mut took = 0i64;
            let mut ran = false;
            for sv in &all {
                steps_total += 1;
                took += sv.duration;
                match sv.status.as_str() {
                    "failed" | "undefined" | "pending" | "ambiguous" => {
                        if status == "passed" {
                            status = sv.status.clone();
                        }
                        if sv.status == "undefined" {
                            undefined += 1;
                        }
                    }
                    "passed" => ran = true,
                    _ => {}
                }
            }
            if status == "passed" && !ran && !all.is_empty() {
                status = "skipped".to_string();
            }
            scenarios += 1;
            duration += took;
            match status.as_str() {
                "passed" => passed += 1,
                "skipped" => skipped += 1,
                _ => {
                    failed += 1;
                    feature_passed = false;
                }
            }
            scenario_views.push(Val::map([
                ("Name", el.name.into()),
                ("Keyword", el.keyword.into()),
                ("Status", status.into()),
                ("Duration", Val::Duration(took)),
                ("Steps", Val::list(all, |s| s.view)),
            ]));
        }
        feature_views.push(Val::map([
            ("Name", f.name.into()),
            ("URI", f.uri.into()),
            ("Passed", feature_passed.into()),
            ("Scenarios", Val::List(scenario_views)),
        ]));
    }
    let p = page(
        fill(meta, "apic test"),
        vec![
            ("Scenarios", scenarios.into()),
            ("Passed", passed.into()),
            ("Failed", failed.into()),
            ("Skipped", skipped.into()),
            ("Undefined", undefined.into()),
            ("Steps", steps_total.into()),
            ("Duration", Val::Duration(duration)),
            ("Features", Val::List(feature_views)),
        ],
    );
    render(w, "features", &p)
}
