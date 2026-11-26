//! apic fmt: rewrite .http files in their canonical form.

use std::collections::{BTreeMap, HashSet};

use apic_gocompat::quote;
use apic_project::path as fp;
use apic_runner::Code;
use serde::Serialize;

use crate::io::{w, wln};
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::{plural, read_text, stat, write_file};
use crate::spec::{CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new("fmt [path...]", "Rewrite .http files in their canonical form")
        .long(
            r#"fmt rewrites request files in place: one blank line between blocks,
directives in a fixed order (name, description, disabled, step, auth, ref,
forceRef, sleep, retry, timeout, no-redirect, no-session, no-cookies,
assert, capture, then the rest as written), header names in canonical case, query continuations
indented, JSON bodies pretty-printed when they hold no {{placeholders}},
trailing whitespace removed. Comments, unknown directives, editor script
blocks and file bodies are kept as they are, and formatting twice changes
nothing.

Without paths every .http and .rest file of the project is formatted. A
path may be a file or a directory, or file.http#name (or file.http#3) to
format that one request and leave the rest of the file as written. "-"
reads stdin and writes the result to stdout, which is what editors use."#,
        )
        .example(
            "  apic fmt
  apic fmt users.http auth.http
  apic fmt users.http#get-user   # one request; the rest of the file untouched
  apic fmt --check          # exit 1 and list the files that would change (CI)
  apic fmt --diff           # show what would change
  apic fmt - < users.http",
        )
        .flag(FlagSpec::bool(
            "check",
            "do not write; list the files that would change and exit 1 if any",
        ))
        .flag(FlagSpec::bool(
            "diff",
            "do not write; print a unified diff of what would change and exit 1 if any",
        ))
        .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let (check, diff) = (inv.flags.bool("check"), inv.flags.bool("diff"));
    let args = &inv.args;
    if args.len() == 1 && args[0] == "-" {
        let data = a.stdin.read_all().map_err(|e| {
            fail(
                Code::File,
                format!("reading stdin: {}", apic_gocompat::io_error_text(&e)),
            )
        })?;
        let text = String::from_utf8_lossy(&data);
        return a
            .stdout
            .write_text(&apic_httpfile::format(&text))
            .map_err(CliError::other);
    }
    let files = a.fmt_targets(args)?;
    let mut changed: Vec<String> = Vec::new();
    // A file named for two of its requests is listed once.
    let mut seen: HashSet<String> = HashSet::new();
    let mut diffs: BTreeMap<String, String> = BTreeMap::new();
    for target in &files {
        let file = &target.path;
        let data = read_text(file).map_err(|e| fail(Code::File, e))?;
        let mut shown = a.fmt_rel(file);
        let formatted = if target.request.is_empty() {
            apic_httpfile::format(&data)
        } else {
            let Some(formatted) = apic_httpfile::format_request(&data, &target.request) else {
                return Err(unknown_in_file(&shown, &target.request, &data));
            };
            // Diffs are per request: the same file can appear twice.
            shown.push('#');
            shown.push_str(&target.request);
            formatted
        };
        if formatted == data {
            continue;
        }
        let first = seen.insert(file.clone());
        if first {
            changed.push(file.clone());
        }
        if diff {
            diffs.insert(shown.clone(), unified_diff(&shown, &data, &formatted));
        }
        if a.g.json {
            // stdout is the JSON document alone; the writes are the same
            // as in text mode.
            if !check && !diff {
                write_formatted(file, &formatted)?;
            }
        } else if diff {
            w!(a.stdout, "{}", diffs[&shown]);
        } else if check {
            if first {
                wln!(a.stdout, "{}", a.fmt_rel(file));
            }
        } else {
            write_formatted(file, &formatted)?;
            wln!(a.stdout, "formatted {shown}");
        }
    }
    if a.g.json {
        #[derive(Serialize)]
        struct Out {
            files: usize,
            changed: Vec<String>,
            formatted: bool,
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            diff: BTreeMap<String, String>,
        }
        a.write_json(&Out {
            files: files.len(),
            changed: changed.iter().map(|c| a.fmt_rel(c)).collect(),
            formatted: !check && !diff,
            diff: if diff { diffs } else { BTreeMap::new() },
        })?;
    } else if changed.is_empty() {
        wln!(a.stdout, "{} already formatted", plural(files.len(), "file"));
    }
    if (check || diff) && !changed.is_empty() {
        return Err(CliError::exit(apic_runner::EXIT_ASSERT));
    }
    Ok(())
}

/// Rewrites a file in place, keeping its mode.
fn write_formatted(file: &str, formatted: &str) -> CliResult {
    let info = stat(file).map_err(|e| fail(Code::File, e))?;
    write_file(file, formatted.as_bytes(), crate::shared::mode_of(&info)).map_err(|e| fail(Code::File, e))
}

/// A file to format, and the one request in it when the argument was
/// file.http#name.
struct FmtTarget {
    path: String,
    request: String,
}

impl App {
    /// Resolves the files to format: the project's request files, or the
    /// files and directories named (walked the way the project is). An
    /// argument that is not a file but has a #name suffix names one
    /// request of the file before the #.
    fn fmt_targets(&self, args: &[String]) -> Result<Vec<FmtTarget>, CliError> {
        if args.is_empty() {
            let p = self.load_project()?;
            let mut files: Vec<String> = p.files.iter().map(|f| fp::join(&p.root, &f.path)).collect();
            files.sort();
            return Ok(files
                .into_iter()
                .map(|path| FmtTarget { path, request: String::new() })
                .collect());
        }
        let abs = |p: &str| {
            if fp::is_abs(p) {
                p.to_string()
            } else {
                fp::join(&self.g.dir, p)
            }
        };
        let mut targets = Vec::new();
        for arg in args {
            let mut path = abs(arg);
            let mut request = String::new();
            let mut info = stat(&path);
            if info.is_err() {
                // A file named with a # in it wins over a request
                // selector, so the selector is tried only when the path as
                // given is not one. The cut is on the argument, at its
                // last #, so a # in the project directory or an earlier
                // path element is left alone.
                if let Some(i) = arg.rfind('#').filter(|i| *i > 0 && *i < arg.len() - 1) {
                    if let Ok(fi) = std::fs::metadata(abs(&arg[..i])) {
                        if !fi.is_dir() {
                            path = abs(&arg[..i]);
                            request = arg[i + 1..].to_string();
                            info = Ok(fi);
                        }
                    }
                }
            }
            let info = info.map_err(|e| fail(Code::File, e))?;
            if !info.is_dir() {
                targets.push(FmtTarget { path, request });
                continue;
            }
            let found = apic_project::discover(&path).map_err(|e| fail(Code::File, e.to_string()))?;
            targets.extend(found.into_iter().map(|path| FmtTarget { path, request: String::new() }));
        }
        Ok(targets)
    }

    /// Shows a path relative to the project directory when it is under it.
    fn fmt_rel(&self, file: &str) -> String {
        let Ok(base) = fp::abs(&self.g.dir) else {
            return file.to_string();
        };
        match fp::rel(&base, file) {
            Ok(rel) if !rel.starts_with("..") => fp::to_slash(&rel),
            _ => file.to_string(),
        }
    }
}

/// Renders the change from a to b for one file, the way `diff -u` does,
/// from a longest-common-subsequence of the lines. Files are small, so
/// the quadratic table is fine.
pub(crate) fn unified_diff(name: &str, a: &str, b: &str) -> String {
    let (al, no_newline_a) = diff_lines(a);
    let (bl, no_newline_b) = diff_lines(b);
    let ops = diff_ops(&al, &bl);
    let mut d = HunkWriter {
        out: format!("--- {name}\n+++ {name}\n"),
        ops: &ops,
        n: al.len(),
        m: bl.len(),
        no_newline_a,
        no_newline_b,
    };
    let mut k = 0;
    while k < ops.len() {
        if ops[k].kind == ' ' {
            k += 1;
            continue;
        }
        let start = k.saturating_sub(DIFF_CONTEXT);
        let end = hunk_end(&ops, k);
        d.write(start, end);
        k = end;
    }
    d.out
}

/// How many unchanged lines a hunk shows around a change.
const DIFF_CONTEXT: usize = 3;

/// One line of the diff: kept (' '), deleted ('-') or inserted ('+').
struct DiffOp {
    kind: char,
    text: String,
}

/// Walks the longest common subsequence of a and b into the edit script,
/// deletions before insertions, the way diff -u prints them.
fn diff_ops(al: &[String], bl: &[String]) -> Vec<DiffOp> {
    let (n, m) = (al.len(), bl.len());
    let lcs = lcs_table(al, bl);
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && al[i] == bl[j] {
            ops.push(DiffOp { kind: ' ', text: al[i].clone() });
            i += 1;
            j += 1;
        } else if i < n && (j >= m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push(DiffOp { kind: '-', text: al[i].clone() });
            i += 1;
        } else {
            ops.push(DiffOp { kind: '+', text: bl[j].clone() });
            j += 1;
        }
    }
    ops
}

/// Holds, at [i][j], the length of the longest common subsequence of
/// al[i..] and bl[j..].
fn lcs_table(al: &[String], bl: &[String]) -> Vec<Vec<usize>> {
    let (n, m) = (al.len(), bl.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if al[i] == bl[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    lcs
}

/// Finds where the hunk holding the change at k ends: it takes in every
/// change less than 2*DIFF_CONTEXT unchanged lines from the last, then
/// DIFF_CONTEXT lines of trailing context.
fn hunk_end(ops: &[DiffOp], k: usize) -> usize {
    let mut end = k;
    while end < ops.len() {
        if ops[end].kind != ' ' {
            end += 1;
            continue;
        }
        let mut next = end;
        while next < ops.len() && ops[next].kind == ' ' {
            next += 1;
        }
        if next >= ops.len() || next - end > 2 * DIFF_CONTEXT {
            return (end + DIFF_CONTEXT).min(ops.len());
        }
        end = next;
    }
    end
}

/// Counts the lines of a and of b among ops.
fn line_counts(ops: &[DiffOp]) -> (usize, usize) {
    let a = ops.iter().filter(|o| o.kind != '+').count();
    let b = ops.iter().filter(|o| o.kind != '-').count();
    (a, b)
}

/// Writes the hunks of one file's diff.
struct HunkWriter<'a> {
    out: String,
    ops: &'a [DiffOp],
    /// Lines in a and in b.
    n: usize,
    m: usize,
    no_newline_a: bool,
    no_newline_b: bool,
}

impl HunkWriter<'_> {
    /// Writes ops[start..end] as one hunk.
    fn write(&mut self, start: usize, end: usize) {
        let (a_start, b_start) = line_counts(&self.ops[..start]);
        let (a_count, b_count) = line_counts(&self.ops[start..end]);
        self.out.push_str(&format!(
            "@@ -{},{a_count} +{},{b_count} @@\n",
            a_start + 1,
            b_start + 1
        ));
        let (mut a_seen, mut b_seen) = (a_start, b_start);
        for o in &self.ops[start..end] {
            self.out.push(o.kind);
            self.out.push_str(o.text.strip_suffix(NO_NEWLINE).unwrap_or(&o.text));
            self.out.push('\n');
            if o.kind != '+' {
                a_seen += 1;
            }
            if o.kind != '-' {
                b_seen += 1;
            }
            if self.last_without_newline(o.kind, a_seen, b_seen) {
                self.out.push_str("\\ No newline at end of file\n");
            }
        }
    }

    /// Reports whether the line just written is the last of a text that
    /// has no final newline, which diff -u marks.
    fn last_without_newline(&self, kind: char, a_seen: usize, b_seen: usize) -> bool {
        match kind {
            '-' => self.no_newline_a && a_seen == self.n,
            '+' => self.no_newline_b && b_seen == self.m,
            _ => self.no_newline_a && self.no_newline_b && a_seen == self.n,
        }
    }
}

/// Marks the last line of a text that does not end with a newline, so it
/// never compares equal to the same text with one: that is a difference
/// the formatter makes and the diff must show.
const NO_NEWLINE: &str = "\x00";

fn diff_lines(s: &str) -> (Vec<String>, bool) {
    if s.is_empty() {
        return (Vec::new(), false);
    }
    if let Some(body) = s.strip_suffix('\n') {
        return (body.split('\n').map(str::to_string).collect(), false);
    }
    let mut lines: Vec<String> = s.split('\n').map(str::to_string).collect();
    if let Some(last) = lines.last_mut() {
        last.push_str(NO_NEWLINE);
    }
    (lines, true)
}

/// The E201 for file.http#name when the file has no such request, with
/// the file's own names as the guess.
fn unknown_in_file(shown: &str, name: &str, data: &str) -> CliError {
    let (f, _) = apic_httpfile::parse(shown, data);
    let names: Vec<&str> = f
        .requests
        .iter()
        .filter(|r| !r.name.is_empty())
        .map(|r| r.name.as_str())
        .collect();
    let closest = apic_suggest::closest(name, &names);
    if closest.is_empty() {
        return fail(
            Code::UnknownRequest,
            format!("{shown}: no request named {} (apic list shows the names)", quote(name)),
        );
    }
    let mut ue = apic_runner::usage(
        Code::UnknownRequest,
        format!(
            "{shown}: no request named {}; {} (apic list shows the names)",
            quote(name),
            apic_suggest::phrase(&closest)
        ),
    );
    ue.suggestions = closest;
    ue.into()
}
