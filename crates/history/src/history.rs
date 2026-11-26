use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use apic_gocompat::{check_json, compact_json, indent_json, json_string, path_error, Time};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;

/// The history directory under the project's .apic directory.
pub const DIR: &str = "history";

/// Names the environment of a run that used none, as the session does.
pub const DEFAULT_ENV: &str = apic_session::DEFAULT_ENV;

/// A `get` for an entry that does not exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeError {
    pub name: String,
    pub index: i64,
    pub have: usize,
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.have {
            0 => write!(f, "no history for {}", self.name),
            1 => write!(f, "{} has 1 history entry, no #{}", self.name, self.index),
            n => write!(
                f,
                "{} has {} history entries, no #{}",
                self.name, n, self.index
            ),
        }
    }
}

/// Why the history could not be read or written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// An entry was asked for that does not exist.
    Range(RangeError),
    /// The file system, or a result that is not JSON, got in the way.
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Range(e) => e.fmt(f),
            Error::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}

impl From<apic_session::Error> for Error {
    fn from(e: apic_session::Error) -> Error {
        Error::Other(e.0)
    }
}

fn io_err(op: &str, path: &Path, err: &io::Error) -> Error {
    Error::Other(path_error(op, path, err))
}

/// The history of one project.
#[derive(Clone, Debug)]
pub struct Store {
    /// The .apic/history directory.
    root: PathBuf,
    keep: i64,
}

fn rfc3339<S: Serializer>(t: &Time, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&t.format_rfc3339_nano())
}

/// One recorded response. `index` counts from 1, the newest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub index: usize,
    #[serde(serialize_with = "rfc3339")]
    pub time: Time,
    pub ok: bool,
    pub status: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status_text: String,
    pub duration_ms: i64,
    pub size: i64,
    /// The entry's path relative to the project root.
    pub file: String,
    /// The run's result as `apic run --json` prints it, with sensitive
    /// headers masked and, for a --redact run, every value. None in a
    /// listing.
    #[serde(skip)]
    pub result: Option<String>,
}

/// The on-disk form of an entry, as far as reading one goes.
#[derive(Deserialize)]
struct FileIn {
    #[serde(default)]
    time: Option<String>,
    #[serde(default, rename = "env")]
    _env: Option<String>,
    #[serde(default, rename = "request")]
    _request: Option<String>,
    #[serde(default)]
    result: Option<Box<RawValue>>,
}

/// The part of a stored result the listing reads.
#[derive(Default, Deserialize)]
struct Summary {
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    response: Option<SummaryResponse>,
}

#[derive(Deserialize)]
struct SummaryResponse {
    #[serde(default)]
    status: Option<i64>,
    #[serde(default)]
    status_text: Option<String>,
    #[serde(default)]
    duration_ms: Option<i64>,
    #[serde(default)]
    size: Option<i64>,
}

fn env_key(env: &str) -> &str {
    if env.is_empty() {
        DEFAULT_ENV
    } else {
        env
    }
}

/// Turns a request or environment name into one safe path segment:
/// letters, digits, '-' and '_' stay, everything else (a dot, a slash, a
/// space) is %XX-encoded, so no name can climb out of the directory or
/// collide with another.
fn component(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for &c in name.as_bytes() {
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' {
            out.push(char::from(c));
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Undoes [`component`]; None for a name it could not have made.
fn path_unescape(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// The name of an entry's file: its UTC time, which sorts as text in the
/// order the entries were written.
fn stamp(at: Time) -> String {
    let c = at.utc().civil();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}.{:09}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.nanosecond
    )
}

/// An entry file name as its time and the -N suffix two entries recorded
/// in the same nanosecond get.
fn split_name(name: &str) -> (&str, i64) {
    let name = name.strip_suffix(".json").unwrap_or(name);
    let Some((base, suffix)) = name.split_once('-') else {
        return (name, 1);
    };
    // The number at the start of the suffix, whatever follows it.
    let suffix = suffix.trim_start_matches([' ', '\t', '\n', '\r']);
    let (sign, digits) = match suffix.strip_prefix(['+', '-']) {
        Some(rest) => (&suffix[..1], rest),
        None => ("", suffix),
    };
    let len = digits.bytes().take_while(u8::is_ascii_digit).count();
    match format!("{sign}{}", &digits[..len]).parse::<i64>() {
        Ok(n) if len > 0 => (base, n),
        _ => (name, 0),
    }
}

/// Orders entry files by time, then by the -N suffix.
fn cmp_name(a: &str, b: &str) -> Ordering {
    split_name(a).cmp(&split_name(b))
}

/// The entry files in `dir`, oldest first. The names are UTC timestamps,
/// so their order is the order they were written in.
fn entry_files(dir: &Path) -> Result<Vec<String>, Error> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_err("open", dir, &err)),
    };
    let mut names = Vec::new();
    for de in entries {
        let de = de.map_err(|err| io_err("readdir", dir, &err))?;
        let name = de.file_name().to_string_lossy().into_owned();
        let is_dir = de.file_type().is_ok_and(|t| t.is_dir());
        if !is_dir && name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort_by(|a, b| cmp_name(a, b));
    Ok(names)
}

/// What reading one entry file came to.
enum Read {
    Entry(Entry),
    /// The file is gone, or is not a whole entry: a write cut short by a
    /// killed process, or one still being written by another.
    Skip,
}

impl Store {
    /// The store for a project root keeping `keep` entries per request and
    /// environment. Nothing is written until [`Store::record`].
    pub fn new(root: &Path, keep: i64) -> Store {
        Store {
            root: root.join(apic_session::DIR).join(DIR),
            keep,
        }
    }

    /// The number of entries kept per request and environment.
    pub fn keep(&self) -> i64 {
        self.keep
    }

    fn dir(&self, env: &str, name: &str) -> PathBuf {
        self.root
            .join(component(env_key(env)))
            .join(component(name))
    }

    /// Stores `result`, the JSON of a run's result, as the newest entry
    /// for `name` in `env`, then drops the oldest entries beyond `keep`.
    pub fn record(&self, env: &str, name: &str, at: Time, result: &str) -> Result<(), Error> {
        if self.keep <= 0 {
            return Ok(());
        }
        let dir = self.dir(env, name);
        if let Some(apic_dir) = self.root.parent() {
            apic_session::ensure_dir(apic_dir)?;
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|err| io_err("mkdir", &dir, &err))?;
        let result = if result.is_empty() { "null" } else { result };
        check_json(result.as_bytes()).map_err(|e| {
            Error::Other(format!(
                "json: error calling MarshalJSON for type json.RawMessage: {e}"
            ))
        })?;
        let compact = format!(
            "{{\"time\":\"{}\",\"env\":{},\"request\":{},\"result\":{}}}",
            at.utc().format_rfc3339_nano(),
            json_string(env_key(env), true),
            json_string(name, true),
            compact_json(result, true)
        );
        let mut data = indent_json(&compact, "", "  ");
        data.push('\n');
        let (mut f, path) = create(&dir, at)?;
        if let Err(err) = f.write_all(data.as_bytes()) {
            drop(f);
            let _ = fs::remove_file(&path);
            return Err(io_err("write", &path, &err));
        }
        drop(f);
        self.prune(&dir)
    }

    /// Removes the oldest entries in `dir` beyond `keep`.
    fn prune(&self, dir: &Path) -> Result<(), Error> {
        let names = entry_files(dir)?;
        let extra = (names.len() as i64 - self.keep).max(0) as usize;
        for name in &names[..extra] {
            let path = dir.join(name);
            fs::remove_file(&path).map_err(|err| io_err("remove", &path, &err))?;
        }
        Ok(())
    }

    /// The entries for `name` in `env`, newest first, without their
    /// results. No history is an empty list, not an error.
    pub fn list(&self, env: &str, name: &str) -> Result<Vec<Entry>, Error> {
        let mut entries = self.entries(env, name)?;
        for e in &mut entries {
            e.result = None;
        }
        Ok(entries)
    }

    /// Entry `index` (1 is the newest) for `name` in `env`, with its
    /// result.
    pub fn get(&self, env: &str, name: &str, index: i64) -> Result<Entry, Error> {
        let mut entries = self.entries(env, name)?;
        if index < 1 || index > entries.len() as i64 {
            return Err(Error::Range(RangeError {
                name: name.to_string(),
                index,
                have: entries.len(),
            }));
        }
        Ok(entries.swap_remove(index as usize - 1))
    }

    /// Reads every entry for `name` in `env`, newest first and numbered
    /// from one. A file that cannot be parsed is passed over rather than
    /// failing the whole history; it ages out like any other entry.
    fn entries(&self, env: &str, name: &str) -> Result<Vec<Entry>, Error> {
        let dir = self.dir(env, name);
        let names = entry_files(&dir)?;
        let mut out = Vec::with_capacity(names.len());
        for file in names.iter().rev() {
            if let Read::Entry(mut e) = self.read(&dir.join(file))? {
                e.index = out.len() + 1;
                out.push(e);
            }
        }
        Ok(out)
    }

    fn read(&self, path: &Path) -> Result<Read, Error> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Read::Skip),
            Err(err) => return Err(io_err("open", path, &err)),
        };
        let Ok(f) = serde_json::from_slice::<FileIn>(&data) else {
            return Ok(Read::Skip);
        };
        let Some(result) = f.result.map(|r| r.get().to_string()) else {
            return Ok(Read::Skip);
        };
        let time = match f.time.as_deref() {
            // An entry without a time has the first instant there is.
            None => Time::from_civil(1, 1, 1, 0, 0, 0, 0, 0),
            Some(text) => match Time::parse_rfc3339(text) {
                Ok(t) => t,
                Err(_) => return Ok(Read::Skip),
            },
        };
        let sum = match result.trim_start().as_bytes().first() {
            Some(b'{') => match serde_json::from_str::<Summary>(&result) {
                Ok(sum) => sum,
                Err(_) => return Ok(Read::Skip),
            },
            Some(b'n') => Summary::default(),
            _ => return Ok(Read::Skip),
        };
        let mut e = Entry {
            index: 0,
            time,
            ok: sum.ok.unwrap_or(false),
            status: 0,
            status_text: String::new(),
            duration_ms: 0,
            size: 0,
            file: String::new(),
            result: Some(result),
        };
        if let Some(r) = sum.response {
            e.status = r.status.unwrap_or(0);
            e.status_text = r.status_text.unwrap_or_default();
            e.duration_ms = r.duration_ms.unwrap_or(0);
            e.size = r.size.unwrap_or(0);
        }
        let project = self.root.parent().and_then(Path::parent);
        if let Some(rel) = project.and_then(|p| path.strip_prefix(p).ok()) {
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            e.file = parts.join("/");
        }
        Ok(Read::Entry(e))
    }

    /// The requests with history in `env`, by name.
    pub fn requests(&self, env: &str) -> Result<Vec<Recorded>, Error> {
        let env_dir = self.root.join(component(env_key(env)));
        let entries = match fs::read_dir(&env_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(io_err("open", &env_dir, &err)),
        };
        let mut out = Vec::new();
        for de in entries {
            let de = de.map_err(|err| io_err("readdir", &env_dir, &err))?;
            if !de.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let dir_name = de.file_name().to_string_lossy().into_owned();
            let Some(request) = path_unescape(&dir_name) else {
                continue; // not a directory apic made
            };
            let files = entry_files(&env_dir.join(&dir_name))?;
            if !files.is_empty() {
                out.push(Recorded {
                    request,
                    entries: files.len(),
                });
            }
        }
        out.sort_by(|a, b| a.request.cmp(&b.request));
        Ok(out)
    }

    /// Removes the history of `name` in `env`; an empty name clears the
    /// whole environment, and env "*" every environment. It reports how
    /// many entries it removed.
    pub fn clear(&self, env: &str, name: &str) -> Result<usize, Error> {
        let dir = if env == "*" {
            self.root.clone()
        } else if name.is_empty() {
            self.root.join(component(env_key(env)))
        } else {
            self.dir(env, name)
        };
        let Some(n) = count_entries(&dir)? else {
            return Ok(0);
        };
        match fs::remove_dir_all(&dir) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(io_err("remove", &dir, &err)),
            _ => Ok(n),
        }
    }
}

/// How many .json files are under `dir`; None when it does not exist.
fn count_entries(dir: &Path) -> Result<Option<usize>, Error> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_err("open", dir, &err)),
    };
    let mut n = 0;
    for de in entries {
        let de = de.map_err(|err| io_err("readdir", dir, &err))?;
        let path = de.path();
        if de.file_type().is_ok_and(|t| t.is_dir()) {
            n += count_entries(&path)?.unwrap_or(0);
        } else if path.to_string_lossy().ends_with(".json") {
            n += 1;
        }
    }
    Ok(Some(n))
}

/// Claims a new entry file in `dir`, named so that it sorts after every
/// entry already there: the time it is recorded at, or, when that is not
/// later than the newest entry (several entries in one clock tick, or a
/// clock that went back), the newest one's name with the next -N suffix.
/// The file is created exclusively, so two processes that pick the same
/// name take turns instead of one overwriting the other.
fn create(dir: &Path, at: Time) -> Result<(fs::File, PathBuf), Error> {
    for _ in 0..100 {
        let names = entry_files(dir)?;
        let mut name = format!("{}.json", stamp(at));
        if let Some(newest) = names.last() {
            if cmp_name(newest, &name) != Ordering::Less {
                let (base, n) = split_name(newest);
                name = format!("{}-{}.json", base, n + 1);
            }
        }
        let path = dir.join(&name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue, // another process took it first
            Err(err) => return Err(io_err("open", &path, &err)),
        }
    }
    Err(Error::Other(format!(
        "{}: no free name for a new entry",
        dir.display()
    )))
}

/// A request that has history in an environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Recorded {
    pub request: String,
    pub entries: usize,
}
