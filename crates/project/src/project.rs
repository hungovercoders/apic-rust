use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use apic_gocompat::{io_error_text, quote};
use apic_httpfile::{Diagnostic, File, Request};

use crate::config::{Config, CONFIG_FILE};
use crate::path;

/// A loaded set of `.http` files.
#[derive(Debug, Clone, Default)]
pub struct Project {
    /// Absolute path of the project root (where `apic.yaml` and the env
    /// files live).
    pub root: String,
    pub config: Config,
    /// The request files, sorted by path.
    pub files: Vec<Arc<File>>,
    /// What the parser reported, file by file.
    pub diagnostics: Vec<Diagnostic>,
    pub(crate) by_name: BTreeMap<String, Vec<Arc<Request>>>,
    pub(crate) by_path: BTreeMap<String, Arc<File>>,
}

/// Why a project could not be loaded, or a target not resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The target names no request or file.
    NotFound(NotFoundError),
    /// Anything else, as the message to show.
    Message(String),
}

impl Error {
    pub(crate) fn msg(text: impl Into<String>) -> Error {
        Error::Message(text.into())
    }

    /// The [`NotFoundError`] inside, when that is what this is.
    pub fn not_found(&self) -> Option<&NotFoundError> {
        match self {
            Error::NotFound(err) => Some(err),
            Error::Message(_) => None,
        }
    }

    /// The names the user probably meant, best first; empty when there is
    /// no guess.
    pub fn suggestions(&self) -> &[String] {
        match self {
            Error::NotFound(err) => err.suggestions(),
            Error::Message(_) => &[],
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(err) => f.write_str(&err.msg),
            Error::Message(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for Error {}

impl From<NotFoundError> for Error {
    fn from(err: NotFoundError) -> Error {
        Error::NotFound(err)
    }
}

/// Says a target names no request or file. `closest` holds the known
/// names nearest to the one given, best first, when any is near; the
/// runner's usage errors keep them, so `--json` and MCP carry them as
/// `suggestions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotFoundError {
    pub msg: String,
    pub closest: Vec<String>,
}

impl NotFoundError {
    /// The error for a target nothing known is near.
    fn plain(msg: String) -> Error {
        not_found(msg, Vec::new())
    }

    /// The names the user probably meant.
    pub fn suggestions(&self) -> &[String] {
        &self.closest
    }
}

impl fmt::Display for NotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for NotFoundError {}

/// Builds the error, adding "did you mean" when there is a guess.
fn not_found(mut msg: String, closest: Vec<String>) -> Error {
    if !closest.is_empty() {
        msg.push_str("; ");
        msg.push_str(&apic_suggest::phrase(&closest));
    }
    Error::NotFound(NotFoundError { msg, closest })
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules" | "vendor" | ".git" | ".apic" | "testdata"
    )
}

fn is_request_file(name: &str) -> bool {
    matches!(path::ext(name), ".http" | ".rest")
}

fn fs_error(op: &str, path: &str, err: &std::io::Error) -> Error {
    Error::msg(format!("{op} {path}: {}", io_error_text(err)))
}

/// Lists the request files under a directory the way [`load`] reads them:
/// every `*.http` and `*.rest`, sorted, skipping hidden directories,
/// `node_modules`, `vendor` and `testdata`. `apic fmt` uses the same walk
/// so the two commands agree on what a project holds.
///
/// Symbolic links are not followed: a link to a directory is not walked,
/// and a `dir` that is itself a link holds nothing.
pub fn discover(dir: impl AsRef<Path>) -> Result<Vec<String>, Error> {
    let dir = dir.as_ref().to_string_lossy().into_owned();
    let mut paths = Vec::new();
    let meta = fs::symlink_metadata(&dir).map_err(|err| fs_error("lstat", &dir, &err))?;
    if meta.is_dir() {
        walk(&dir, &mut paths)?;
    } else if is_request_file(&path::base(&dir)) {
        paths.push(dir);
    }
    paths.sort();
    Ok(paths)
}

fn walk(dir: &str, paths: &mut Vec<String>) -> Result<(), Error> {
    let open = |err: std::io::Error| fs_error("open", dir, &err);
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(open)? {
        let entry = entry.map_err(open)?;
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        entries.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
    }
    entries.sort();
    for (name, is_dir) in entries {
        let child = path::join(dir, &name);
        if is_dir {
            if !skip_dir(&name) && !name.starts_with('.') {
                walk(&child, paths)?;
            }
        } else if is_request_file(&name) {
            paths.push(child);
        }
    }
    Ok(())
}

/// Discovers and parses every `*.http` and `*.rest` file under `root`.
pub fn load(root: impl AsRef<Path>) -> Result<Arc<Project>, Error> {
    load_overlay(root, &HashMap::new())
}

/// [`load`] with some files' content supplied rather than read: `overlay`
/// maps an absolute path to the text to parse in its place, as an editor
/// holds a buffer that is not saved yet. A request file in the overlay
/// that is not on disk yet is loaded too.
pub fn load_overlay(
    root: impl AsRef<Path>,
    overlay: &HashMap<String, String>,
) -> Result<Arc<Project>, Error> {
    let root = root.as_ref().to_string_lossy();
    let abs = path::abs(&root).map_err(|err| Error::msg(io_error_text(&err)))?;
    let mut config = Config::default();
    if let Ok(data) = fs::read(path::join(&abs, CONFIG_FILE)) {
        config =
            Config::from_yaml(&data).map_err(|err| Error::msg(format!("{CONFIG_FILE}: {err}")))?;
    }
    let scan = if config.dir.is_empty() {
        abs.clone()
    } else {
        path::join(&abs, &config.dir)
    };
    let paths = with_overlay(&scan, discover(&scan)?, overlay);
    let mut files = Vec::with_capacity(paths.len());
    let mut diagnostics = Vec::new();
    for file_path in &paths {
        let content = match overlay.get(file_path) {
            Some(content) => content.clone(),
            None => {
                let data = fs::read(file_path).map_err(|err| fs_error("open", file_path, &err))?;
                String::from_utf8_lossy(&data).into_owned()
            }
        };
        // A file is known by its path from the root, with forward slashes.
        let rel = path::to_slash(&path::rel(&abs, file_path).unwrap_or_default());
        let (file, diags) = apic_httpfile::parse(&rel, &content);
        diagnostics.extend(diags);
        files.push(file);
    }
    Ok(Arc::new(Project::new(abs, config, files, diagnostics)))
}

/// Adds the overlay's request files under `scan` that [`discover`] did not
/// find (new, unsaved files), keeping the list sorted. A file `discover`
/// would skip (under a hidden directory, `node_modules`, `vendor`,
/// `testdata` or `.apic`) stays out, open or not, so a project is the same
/// set of files in an editor as on the command line.
fn with_overlay(
    scan: &str,
    mut paths: Vec<String>,
    overlay: &HashMap<String, String>,
) -> Vec<String> {
    let have: HashSet<String> = paths.iter().cloned().collect();
    let mut added = false;
    for p in overlay.keys() {
        if have.contains(p) || !is_request_file(p) {
            continue;
        }
        match within(scan, p) {
            Some(rel) if !skipped(&rel) => {
                paths.push(p.clone());
                added = true;
            }
            _ => {}
        }
    }
    if added {
        paths.sort();
    }
    paths
}

/// Reports whether [`discover`] leaves out a file at `rel`, a path
/// relative to the directory it walks: one under a directory it skips.
fn skipped(rel: &str) -> bool {
    path::to_slash(&path::dir(rel))
        .split('/')
        .any(|d| d != "." && (skip_dir(d) || d.starts_with('.')))
}

/// Reports whether `path` (existing or not) lies under `root`, lexically:
/// both are cleaned and the relative path must not start with `..`. It
/// returns that relative path. Callers that must see through symlinks
/// resolve both sides first.
pub fn within(root: &str, path: &str) -> Option<String> {
    let rel = path::rel(&path::clean(root), &path::clean(path)).ok()?;
    let above = format!("..{}", std::path::MAIN_SEPARATOR);
    if rel == ".." || rel.starts_with(&above) {
        return None;
    }
    Some(rel)
}

/// The names for a message: the first dozen, then "...".
fn some(names: &[String]) -> Vec<String> {
    if names.len() > 12 {
        let mut out = names[..12].to_vec();
        out.push("...".to_string());
        return out;
    }
    names.to_vec()
}

impl Project {
    /// A project over files that are already parsed, indexed as [`load`]
    /// indexes them. Each file's `path` must be its path from `root`,
    /// with forward slashes.
    pub fn new(
        root: impl Into<String>,
        config: Config,
        files: Vec<Arc<File>>,
        diagnostics: Vec<Diagnostic>,
    ) -> Project {
        let mut by_name: BTreeMap<String, Vec<Arc<Request>>> = BTreeMap::new();
        let mut by_path = BTreeMap::new();
        for file in &files {
            by_path.insert(file.path.clone(), Arc::clone(file));
            for request in &file.requests {
                if !request.name.is_empty() {
                    by_name
                        .entry(request.name.clone())
                        .or_default()
                        .push(Arc::clone(request));
                }
            }
        }
        Project {
            root: root.into(),
            config,
            files,
            diagnostics,
            by_name,
            by_path,
        }
    }

    /// Returns every request in file order.
    pub fn requests(&self) -> Vec<Arc<Request>> {
        self.files
            .iter()
            .flat_map(|f| f.requests.iter().cloned())
            .collect()
    }

    /// Finds a request by name. It fails when the name is ambiguous.
    pub fn lookup(&self, name: &str) -> Result<Arc<Request>, Error> {
        let found = self.by_name.get(name).map_or(&[][..], Vec::as_slice);
        match found {
            [] => Err(self.name_not_found(name)),
            [one] => Ok(Arc::clone(one)),
            many => {
                let places: Vec<String> = many
                    .iter()
                    .map(|r| format!("{}:{}", r.file.path, r.line))
                    .collect();
                Err(Error::msg(format!(
                    "request name {} is defined more than once ({}); use file#name",
                    quote(name),
                    places.join(", ")
                )))
            }
        }
    }

    /// Turns a command-line target into one or more requests:
    ///
    /// ```text
    /// get-user            a request by @name
    /// users.http          every request in that file, in order
    /// users.http#get-user a named request in a specific file
    /// users.http#3        the third request in that file
    /// ```
    pub fn resolve(&self, target: &str) -> Result<Vec<Arc<Request>>, Error> {
        let Some((file, frag)) = target.split_once('#') else {
            if let Some(f) = self.file(target) {
                return Ok(f.requests.clone());
            }
            return Ok(vec![self.lookup(target)?]);
        };
        let Some(f) = self.file(file) else {
            let files = self.file_paths();
            let closest = apic_suggest::closest(&self.relative(file), &files);
            if !closest.is_empty() {
                return Err(not_found(
                    format!("no .http file {} in {}", quote(file), self.root),
                    closest,
                ));
            }
            return Err(NotFoundError::plain(format!(
                "no .http file {} in {}; files: {} (run `apic list`)",
                quote(file),
                self.root,
                some(&files).join(", ")
            )));
        };
        if let Ok(n) = frag.parse::<i64>() {
            if n < 1 || n as u64 > f.requests.len() as u64 {
                return Err(Error::msg(format!(
                    "{} has {} requests, no #{}",
                    f.path,
                    f.requests.len(),
                    n
                )));
            }
            return Ok(vec![Arc::clone(&f.requests[n as usize - 1])]);
        }
        let mut names = Vec::new();
        for r in &f.requests {
            if r.name == frag {
                return Ok(vec![Arc::clone(r)]);
            }
            if !r.name.is_empty() {
                names.push(r.name.clone());
            }
        }
        let closest = apic_suggest::closest(frag, &names);
        if !closest.is_empty() {
            return Err(not_found(
                format!("no request named {} in {}", quote(frag), f.path),
                closest,
            ));
        }
        if names.is_empty() {
            return Err(NotFoundError::plain(format!(
                "no request named {} in {}, which has no named requests; use {}#N",
                quote(frag),
                f.path,
                file
            )));
        }
        Err(NotFoundError::plain(format!(
            "no request named {} in {}; it has: {}",
            quote(frag),
            f.path,
            some(&names).join(", ")
        )))
    }

    /// Turns an absolute path under the root into the slash form the
    /// project keys its files by, so it can be compared with them; any
    /// other path is returned as given.
    fn relative(&self, name: &str) -> String {
        if !path::is_abs(name) {
            return name.to_string();
        }
        match path::rel(&self.root, name) {
            Ok(rel) if !rel.starts_with("..") => path::to_slash(&rel),
            _ => name.to_string(),
        }
    }

    /// Returns the request file at `path` (relative to the root, or
    /// absolute), or `None`. The whole path is the file name: a `#` in it
    /// is not a fragment.
    pub fn file(&self, path: &str) -> Option<&Arc<File>> {
        if !path.ends_with(".http") && !path.ends_with(".rest") {
            return None;
        }
        let clean = path::to_slash(&path::clean(path));
        if let Some(f) = self.by_path.get(&clean) {
            return Some(f);
        }
        let abs = path::abs(path).ok()?;
        let rel = path::rel(&self.root, &abs).ok()?;
        self.by_path.get(&path::to_slash(&rel))
    }

    fn name_not_found(&self, name: &str) -> Error {
        let mut names: Vec<String> = self.by_name.keys().cloned().collect();
        if self.files.is_empty() {
            return NotFoundError::plain(format!(
                "no request named {}: no .http files found under {} (new here? apic init scaffolds a project, and apic ui --demo shows a working one)",
                quote(name),
                self.root
            ));
        }
        if names.is_empty() {
            return NotFoundError::plain(format!(
                "no request named {}: no named requests found under {} (add `# @name {}` above a request line)",
                quote(name),
                self.root,
                name
            ));
        }
        // A bare target may also be a file, so a file name is a fair guess.
        let known = names.len();
        names.extend(self.file_paths());
        let closest = apic_suggest::closest(&self.relative(name), &names);
        if !closest.is_empty() {
            return Error::NotFound(NotFoundError {
                msg: format!(
                    "no request named {}; {} (run `apic list` for every id)",
                    quote(name),
                    apic_suggest::phrase(&closest)
                ),
                closest,
            });
        }
        NotFoundError::plain(format!(
            "no request named {}; known: {} (run `apic list`)",
            quote(name),
            some(&names[..known]).join(", ")
        ))
    }

    /// Lists the request files by their path from the root, sorted.
    fn file_paths(&self) -> Vec<String> {
        self.by_path.keys().cloned().collect()
    }

    /// Returns the first request that captures a variable of this name.
    pub fn captured_by(&self, name: &str) -> Option<Arc<Request>> {
        self.files
            .iter()
            .flat_map(|f| f.requests.iter())
            .find(|r| r.captures.iter().any(|c| c.name == name))
            .cloned()
    }
}
