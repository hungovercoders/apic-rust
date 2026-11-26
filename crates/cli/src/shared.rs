//! Helpers the inspection commands share.

use std::sync::Arc;

use apic_httpfile::Request;
use apic_output::Theme;
use apic_runner::{Code, Runner};
use serde::Serialize;

use crate::root::{fail, App, CliError, CliResult};

/// The shared style set; colour is switched off before a command runs
/// when the output is not a terminal.
pub(crate) fn theme() -> Theme {
    Theme::default()
}

/// A value as the indented JSON document the commands print: two spaces,
/// nothing escaped for HTML, a newline at the end.
pub(crate) fn json_text<T: Serialize + ?Sized>(v: &T) -> Result<String, CliError> {
    let compact = serde_json::to_string(v).map_err(CliError::other)?;
    let compact = apic_gocompat::escape_line_separators(&compact);
    Ok(apic_gocompat::indent_json(&compact, "", "  ") + "\n")
}

impl App {
    pub(crate) fn write_json<T: Serialize + ?Sized>(&self, v: &T) -> CliResult {
        let text = json_text(v)?;
        self.stdout.write_text(&text).map_err(CliError::other)
    }

    /// Loads the runner and resolves exactly one request.
    pub(crate) fn single(&self, target: &str) -> Result<(Runner, Arc<Request>), CliError> {
        let r = self.new_runner()?;
        let reqs = r.project.resolve(target).map_err(unknown_request)?;
        if reqs.len() != 1 {
            return Err(fail(
                Code::Ambiguous,
                format!(
                    "{target} names {} requests; pick one with {target}#<name>",
                    reqs.len()
                ),
            ));
        }
        let req = reqs[0].clone();
        Ok((r, req))
    }
}

/// A target that names no request, as E201 with the names it may have
/// been meant for.
pub(crate) fn unknown_request(err: apic_project::Error) -> CliError {
    let msg = err.to_string();
    apic_runner::UsageError::wrapping(Code::UnknownRequest, msg, &apic_runner::Error::Project(err))
        .into()
}

/// The same for a lookup the runner made.
pub(crate) fn unknown_target(err: apic_runner::Error) -> CliError {
    apic_runner::UsageError::wrapping(Code::UnknownRequest, err.to_string(), &err).into()
}

/// Renders "1 file" or "3 files".
pub(crate) fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Reads a file; the error is worded `open PATH: reason`.
pub(crate) fn read_file(path: impl AsRef<std::path::Path>) -> Result<Vec<u8>, String> {
    let path = path.as_ref();
    std::fs::read(path).map_err(|e| read_error(path, &e))
}

/// Words a failed read: reading a directory fails in the read, not the
/// open.
fn read_error(path: &std::path::Path, e: &std::io::Error) -> String {
    let op = if path.is_dir() { "read" } else { "open" };
    apic_gocompat::path_error(op, path, e)
}

/// Reads a file as text, bytes that are not UTF-8 replaced.
pub(crate) fn read_text(path: impl AsRef<std::path::Path>) -> Result<String, String> {
    read_file(path).map(|data| String::from_utf8_lossy(&data).into_owned())
}

/// Writes a file, created with `mode` when it does not exist yet; the
/// error is worded `open PATH: reason`.
pub(crate) fn write_file(
    path: impl AsRef<std::path::Path>,
    data: &[u8],
    mode: u32,
) -> Result<(), String> {
    use std::io::Write;
    let path = path.as_ref();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut f = opts
        .open(path)
        .map_err(|e| apic_gocompat::path_error("open", path, &e))?;
    f.write_all(data)
        .map_err(|e| apic_gocompat::path_error("write", path, &e))
}

/// The metadata of a path; the error is worded `stat PATH: reason`.
pub(crate) fn stat(path: impl AsRef<std::path::Path>) -> Result<std::fs::Metadata, String> {
    let path = path.as_ref();
    std::fs::metadata(path).map_err(|e| apic_gocompat::path_error("stat", path, &e))
}

/// Serialises an empty list as `null`, which is what a list that was
/// never filled has always been printed as.
pub(crate) fn null_when_empty<S: serde::Serializer, T: Serialize>(
    v: &&[T],
    s: S,
) -> Result<S::Ok, S::Error> {
    if v.is_empty() {
        s.serialize_none()
    } else {
        v.serialize(s)
    }
}

/// The permission bits of a file, for writing it back as it was.
pub(crate) fn mode_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o644
    }
}

/// A decoded JSON value written back out: keys sorted, numbers as a
/// 64-bit float prints them.
pub(crate) fn plain_json(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => apic_gocompat::format_float_json(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => apic_gocompat::json_string(s, false),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(plain_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}:{}", apic_gocompat::json_string(k, false), plain_json(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}
