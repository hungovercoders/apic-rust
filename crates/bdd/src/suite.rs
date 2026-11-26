//! Runs feature files: checks the options, the paths, the phrases, the
//! tag expression and the Gherkin up front, then runs the scenarios and
//! maps what happened to an exit code.

use std::fs;
use std::io::Write;
use std::sync::{Arc, Mutex};

use apic_gocompat::quote;
use apic_httpc::CancelToken;
use apic_project::path;
use apic_runner::{usage, Code, Error, EXIT_ASSERT, EXIT_OK};

use crate::colors::Out;
use crate::engine::{self, FeatureModel};
use crate::gherkin;
use crate::phrases::compile_phrases;
use crate::state::{lock, Config, Output, Run};
use crate::steps::Steps;
use crate::summary::{summarize, Summary};
use crate::tags::apply_tag_filter;

/// Exit codes from [`run`], aligned with the rest of apic.
pub const EXIT_PASSED: i32 = EXIT_OK;
pub const EXIT_FAILED: i32 = EXIT_ASSERT;
pub use apic_runner::{EXIT_TRANSPORT, EXIT_USAGE};

/// Lists the accepted `--format` values.
pub const FORMATS: [&str; 4] = ["pretty", "progress", "cucumber", "junit"];

/// A feature held in memory rather than read from a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feature {
    /// What reports call it, in place of a path.
    pub name: String,
    pub contents: Vec<u8>,
}

/// Controls one run.
#[derive(Clone, Default)]
pub struct Options {
    pub config: Config,
    /// Feature files or directories; relative paths resolve against the
    /// project root.
    pub paths: Vec<String>,
    /// In-memory features (tests and MCP); used when `paths` is empty.
    pub features: Vec<Feature>,
    /// One of [`FORMATS`]; default pretty.
    pub format: String,
    /// The report destination; default standard output.
    pub output: Option<Output>,
    /// A tag expression, e.g. `@smoke && ~@slow`.
    pub tags: String,
    pub stop_on_failure: bool,
    pub no_colors: bool,
}

impl Options {
    /// The options for a configuration, everything else at its default.
    pub fn new(config: Config) -> Options {
        Options {
            config,
            ..Options::default()
        }
    }
}

/// A writer that locks its destination for each write only, so nothing
/// else that shares the destination is kept waiting.
struct SharedWriter(Output);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        lock(&self.0).flush()
    }
}

/// Executes the features and returns an exit code. The error carries the
/// typed failure when the run ended on a usage problem (exit 2: no
/// features, bad phrase, unknown environment, unknown request, missing
/// variable) or a transport problem (exit 3); assertion failures are
/// reported by code only.
pub fn run(cancel: &CancelToken, opts: &Options) -> (i32, Option<Error>) {
    let format = if opts.format.is_empty() {
        "pretty"
    } else {
        opts.format.as_str()
    };
    let output: Output = match &opts.output {
        Some(out) => out.clone(),
        None => Arc::new(Mutex::new(std::io::stdout())),
    };
    run_to(cancel, opts, format, output, opts.no_colors)
}

fn run_to(
    cancel: &CancelToken,
    opts: &Options,
    format: &str,
    output: Output,
    no_colors: bool,
) -> (i32, Option<Error>) {
    let other = |msg: String| (EXIT_USAGE, Some(Error::Other(msg)));
    if !FORMATS.contains(&format) {
        return other(format!(
            "unknown format {} (one of pretty, progress, cucumber, junit)",
            quote(format)
        ));
    }
    let paths = match resolve_paths(opts) {
        Ok(paths) => paths,
        Err(msg) => return other(msg),
    };
    // Stop-on-failure is handled by the hooks: later scenarios still get
    // results, as skipped, so the report stays complete.
    let mut run = Run::new(&opts.config, opts.stop_on_failure);
    // Fail early on unknown environments and bad phrases instead of per
    // scenario.
    if let Err(err) = run.new_scenario(&opts.config.env) {
        return (EXIT_USAGE, Some(err));
    }
    let phrases = match compile_phrases(&opts.config.project) {
        Ok(phrases) => phrases,
        Err(msg) => return other(msg),
    };
    if let Err(err) = check_tags(&opts.tags) {
        return (EXIT_USAGE, Some(err));
    }
    let features = match load_features(opts, &paths) {
        Ok(features) => features,
        Err(err) => return (EXIT_USAGE, Some(err)),
    };

    let (mut steps, defs) = Steps::new(run, cancel, &phrases);
    let mut formatter = crate::fmt::new(format, "apic");
    let failed = if opts.config.redact {
        // The whole report is held back until the run completes, so values
        // captured late in the run are masked in lines written earlier.
        let mut buf: Vec<u8> = Vec::new();
        let failed = {
            let mut out = Out::new(&mut buf, no_colors);
            engine::run(features, &defs, &mut steps, formatter.as_mut(), &mut out)
        };
        if let Err(err) = flush_masked(&steps.run, format, &buf, &output) {
            return other(err.to_string());
        }
        failed
    } else {
        let mut writer = SharedWriter(output);
        let mut out = Out::new(&mut writer, no_colors);
        engine::run(features, &defs, &mut steps, formatter.as_mut(), &mut out)
    };
    // Typed step errors keep the CLI's exit-code contract: definition
    // problems are 2, unreachable servers are 3, assertion failures are 1.
    if let Some(err) = steps.run.usage_err.take() {
        return (EXIT_USAGE, Some(err));
    }
    if let Some(err) = steps.run.transport_err.take() {
        return (EXIT_TRANSPORT, Some(err));
    }
    if failed {
        (EXIT_FAILED, None)
    } else {
        (EXIT_PASSED, None)
    }
}

/// Writes the masked report. Structured reports are masked value by value
/// so a secret that happens to equal a status, element or attribute name
/// cannot change the report's meaning; plain-text substitution is the
/// fallback.
fn flush_masked(run: &Run<'_>, format: &str, buf: &[u8], output: &Output) -> std::io::Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    let structured = match format {
        "cucumber" => run.mask_json(buf).ok(),
        "junit" => run.mask_xml(buf).ok(),
        _ => None,
    };
    let masked = structured.unwrap_or_else(|| run.mask(&String::from_utf8_lossy(buf)).into_bytes());
    lock(output).write_all(&masked)
}

/// Runs with the cucumber formatter into memory and returns the parsed
/// summary plus the raw report. Used by MCP and tests. When a usage or
/// transport error ends the run, the summary of what ran is still
/// returned alongside the error whenever a report was produced.
pub fn run_summary(
    cancel: &CancelToken,
    opts: &Options,
) -> (Option<Summary>, Vec<u8>, i32, Option<Error>) {
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let (code, err) = run_to(cancel, opts, "cucumber", buf.clone(), true);
    let report = std::mem::take(&mut *lock(&buf));
    if report.is_empty() {
        return (None, report, code, err);
    }
    match summarize(&report) {
        // A typed step error (exit 2 or 3) still comes with the report of
        // what ran.
        Ok(sum) => (Some(sum), report, code, err),
        Err(perr) => (None, report, code, err.or(Some(Error::Other(perr)))),
    }
}

fn above(rel: &str) -> bool {
    rel == ".." || rel.starts_with(&format!("..{}", std::path::MAIN_SEPARATOR))
}

fn uses_memory_features(o: &Options) -> bool {
    !o.features.is_empty() && o.paths.is_empty()
}

fn resolve_paths(o: &Options) -> Result<Vec<String>, String> {
    if uses_memory_features(o) {
        return Ok(Vec::new());
    }
    let project = &o.config.project;
    let mut paths: &[String] = &o.paths;
    if paths.is_empty() {
        paths = &project.config.test.paths;
    }
    let default = ["features".to_string()];
    if paths.is_empty() {
        paths = &default;
    }
    let root = path::eval_symlinks(&project.root)
        .map_err(|e| apic_gocompat::path_error("lstat", std::path::Path::new(&project.root), &e))?;
    let mut out = Vec::new();
    for p in paths {
        let abs = if path::is_abs(p) {
            p.clone()
        } else {
            path::join(&project.root, p)
        };
        let Ok(real) = path::eval_symlinks(&abs) else {
            return Err(format!(
                "no features at {p} (paths are relative to the project root {})",
                project.root
            ));
        };
        if path::rel(&root, &real).map_or(true, |rel| above(&rel)) {
            return Err(format!(
                "feature path {p} is outside the project root {}",
                project.root
            ));
        }
        let Ok(info) = fs::metadata(&real) else {
            return Err(format!("no features at {p}"));
        };
        if info.is_dir() {
            if check_feature_files(&root, &real)? == 0 {
                return Err(format!("no .feature files under {real}"));
            }
        } else if !real.ends_with(".feature") {
            return Err(format!("{p} is not a .feature file"));
        }
        out.push(real);
    }
    Ok(out)
}

/// Every file under `dir` that is not a directory, in name order; a
/// symbolic link is listed, not followed.
fn walk(dir: &str, visit: &mut dyn FnMut(&str) -> Result<(), String>) -> Result<(), String> {
    let read_err = |e: std::io::Error| {
        format!(
            "cannot read {dir}: {}",
            apic_gocompat::path_error("open", std::path::Path::new(dir), &e)
        )
    };
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)
        .map_err(read_err)?
        .collect::<Result<_, _>>()
        .map_err(read_err)?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let full = path::join(dir, &name);
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&full, visit)?;
        } else {
            visit(&full)?;
        }
    }
    Ok(())
}

/// Counts the .feature files under `dir` and rejects any that resolve
/// (through symlinks) outside the project root.
fn check_feature_files(root: &str, dir: &str) -> Result<usize, String> {
    let mut count = 0;
    walk(dir, &mut |file| {
        if !file.ends_with(".feature") {
            return Ok(());
        }
        let real = path::eval_symlinks(file).map_err(|e| {
            format!(
                "cannot resolve {file}: {}",
                apic_gocompat::path_error("lstat", std::path::Path::new(file), &e)
            )
        })?;
        if path::rel(root, &real).map_or(true, |rel| above(&rel)) {
            return Err(format!(
                "feature file {file} resolves outside the project root {root}"
            ));
        }
        count += 1;
        Ok(())
    })?;
    Ok(count)
}

/// Lists every .feature file the run would read, after the same root
/// checks as the run itself. Callers use it to protect inputs.
pub fn feature_files(o: &Options) -> Result<Vec<String>, String> {
    let paths = resolve_paths(o)?;
    let mut files = Vec::new();
    for p in paths {
        let info = fs::metadata(&p)
            .map_err(|e| apic_gocompat::path_error("stat", std::path::Path::new(&p), &e))?;
        if !info.is_dir() {
            files.push(p);
            continue;
        }
        walk(&p, &mut |file| {
            if file.ends_with(".feature") {
                files.push(path::eval_symlinks(file).unwrap_or_else(|_| file.to_string()));
            }
            Ok(())
        })?;
    }
    Ok(files)
}

/// Parses every selected feature up front so a syntax error is reported
/// as a usage error naming the file and position, then compiles the
/// scenarios the tag expression selects. Ids are numbered across the
/// files of one run.
fn load_features(o: &Options, paths: &[String]) -> Result<Vec<FeatureModel>, Error> {
    let syntax = |name: &str, err: gherkin::ParseErrors| -> Error {
        usage(Code::Features, format!("{name}: {err}")).into()
    };
    // Syntax is checked for everything before anything is compiled.
    for f in &o.features {
        gherkin::parse(&f.contents, &mut String::new).map_err(|e| syntax(&f.name, e))?;
    }
    let mut sources: Vec<(String, Vec<u8>)> = Vec::new();
    if !uses_memory_features(o) {
        let files = feature_files(o).map_err(Error::Other)?;
        for f in &files {
            let data = fs::read(f).map_err(|e| -> Error {
                let msg = apic_gocompat::path_error("open", std::path::Path::new(f), &e);
                usage(Code::Features, msg).into()
            })?;
            gherkin::parse(&data, &mut String::new).map_err(|e| syntax(f, e))?;
        }
        // The run reads each path as given: a directory's files under the
        // directory's own name.
        for p in paths {
            if fs::metadata(p).is_ok_and(|m| m.is_dir()) {
                let _ = walk(p, &mut |file| {
                    if file.ends_with(".feature") {
                        if let Ok(data) = fs::read(file) {
                            sources.push((file.to_string(), data));
                        }
                    }
                    Ok(())
                });
            } else if let Ok(data) = fs::read(p) {
                sources.push((p.clone(), data));
            }
        }
    }
    let mut out: Vec<FeatureModel> = Vec::new();
    let compile =
        |sources: &[(String, Vec<u8>)], out: &mut Vec<FeatureModel>| -> Result<(), Error> {
            let mut next = 0u64;
            let mut new_id = || {
                let id = next.to_string();
                next += 1;
                id
            };
            let start = out.len();
            for (uri, data) in sources {
                let doc = gherkin::parse(data, &mut new_id).map_err(|e| syntax(uri, e))?;
                let pickles = gherkin::pickles(&doc, uri, &mut new_id);
                if out[start..].iter().any(|f| &f.uri == uri) {
                    continue;
                }
                out.push(FeatureModel {
                    uri: uri.clone(),
                    doc,
                    pickles,
                });
            }
            Ok(())
        };
    let memory: Vec<(String, Vec<u8>)> = o
        .features
        .iter()
        .map(|f| (f.name.clone(), f.contents.clone()))
        .collect();
    compile(&memory, &mut out)?;
    compile(&sources, &mut out)?;
    // A feature the tag expression leaves empty is not part of the run.
    let mut selected = Vec::new();
    for mut f in out {
        f.pickles = apply_tag_filter(&o.tags, f.pickles);
        if f.doc.feature.is_some() && !f.pickles.is_empty() {
            selected.push(f);
        }
    }
    Ok(selected)
}

/// Validates a tag expression: `,` is OR, `&&` is AND, `~` negates, `@`
/// is optional. An operand must name a tag, so an expression such as
/// `@smoke && ` is refused rather than read as something else.
fn check_tags(expr: &str) -> Result<(), Error> {
    if expr.trim().is_empty() {
        return Ok(());
    }
    for or in expr.split(',') {
        for operand in or.split("&&") {
            let tag = operand.trim();
            let tag = tag.strip_prefix('~').unwrap_or(tag);
            let tag = tag.strip_prefix('@').unwrap_or(tag);
            // Only `,` and `&&` combine tags; other operator characters
            // would become part of a tag name and silently match nothing.
            if tag.is_empty()
                || tag.contains(['@', '~', ' ', '\t', '&', '|', '!', '(', ')', '"', '\''])
            {
                return Err(usage(
                    Code::Features,
                    format!(
                        "invalid tag expression {}: use tags joined by `,` (or) and `&&` (and), `~` to negate, e.g. \"@smoke && ~@slow\"",
                        quote(expr)
                    ),
                )
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_expressions() {
        for good in ["", " ", "@smoke", "smoke", "@a && ~@b", "@a,@b", "~a"] {
            assert!(check_tags(good).is_ok(), "{good}");
        }
        for bad in ["@smoke && ", ",", "@a || @b", "!(a)", "@a @b", "~~a", "@@a"] {
            assert!(check_tags(bad).is_err(), "{bad}");
        }
    }
}
