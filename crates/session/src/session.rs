use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use apic_gocompat::{check_json, indent_json, json_string, path_error};
use serde_json::Value;

pub const DIR: &str = ".apic";
pub const FILE: &str = "session.json";
pub const DEFAULT_ENV: &str = "default";

/// A session file, cookie file or directory that could not be read or
/// written. The message is the whole of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// The on-disk session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Store {
    path: Option<PathBuf>,
    /// Environment name to captured values.
    pub envs: BTreeMap<String, BTreeMap<String, String>>,
}

pub(crate) fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Reads a file, or None when it does not exist.
pub(crate) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    match fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            let op = if path.is_dir() { "read" } else { "open" };
            Err(Error(path_error(op, path, &err)))
        }
    }
}

/// Writes a file only its owner can read, replacing what was there.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(data)
}

fn key(env: &str) -> &str {
    if env.is_empty() {
        DEFAULT_ENV
    } else {
        env
    }
}

pub(crate) fn env_key(env: &str) -> String {
    key(env).to_string()
}

impl Store {
    /// A session that is never written to disk. Captured values and cached
    /// tokens still work within the process.
    pub fn new_memory() -> Store {
        Store::default()
    }

    /// A memory-only copy of the session: what it held when taken, changed
    /// and read without touching the original or the disk. Each iteration
    /// of `apic run --data` starts from one.
    pub fn snapshot(&self) -> Store {
        Store {
            path: None,
            envs: self.envs.clone(),
        }
    }

    /// Loads the session for a project root, or an empty one.
    pub fn open(root: &Path) -> Result<Store, Error> {
        let mut s = Store {
            path: Some(root.join(DIR).join(FILE)),
            envs: BTreeMap::new(),
        };
        let Some(data) = read_optional(s.path.as_deref().unwrap_or(root))? else {
            return Ok(s);
        };
        check_json(&data).map_err(Error)?;
        let doc: Value = serde_json::from_slice(&data).map_err(|e| Error(format!("json: {e}")))?;
        let mismatch = |v: &Value, want: &str| {
            Error(format!(
                "json: cannot unmarshal {} into Go struct field Store.envs of type {}",
                json_type(v),
                want
            ))
        };
        let envs = match &doc {
            Value::Null => return Ok(s),
            Value::Object(o) => o.get("envs"),
            other => {
                return Err(Error(format!(
                    "json: cannot unmarshal {} into Go value of type session.Store",
                    json_type(other)
                )))
            }
        };
        let envs = match envs {
            None | Some(Value::Null) => return Ok(s),
            Some(Value::Object(envs)) => envs,
            Some(other) => return Err(mismatch(other, "map[string]map[string]string")),
        };
        for (env, vars) in envs {
            let mut m = BTreeMap::new();
            match vars {
                Value::Null => {}
                Value::Object(vars) => {
                    for (name, v) in vars {
                        match v {
                            Value::Null => m.insert(name.clone(), String::new()),
                            Value::String(v) => m.insert(name.clone(), v.clone()),
                            other => return Err(mismatch(other, "string")),
                        };
                    }
                }
                other => return Err(mismatch(other, "map[string]string")),
            }
            s.envs.insert(env.clone(), m);
        }
        Ok(s)
    }

    /// A copy of the captured values for `env`.
    pub fn vars(&self, env: &str) -> BTreeMap<String, String> {
        self.envs.get(key(env)).cloned().unwrap_or_default()
    }

    /// One captured value.
    pub fn get(&self, env: &str, name: &str) -> Option<&str> {
        self.envs.get(key(env))?.get(name).map(String::as_str)
    }

    /// Merges `vars` into the captured values of `env` (in memory; call
    /// [`Store::save`]).
    pub fn set<K: Into<String>, V: Into<String>>(
        &mut self,
        env: &str,
        vars: impl IntoIterator<Item = (K, V)>,
    ) {
        let m = self.envs.entry(env_key(env)).or_default();
        for (n, v) in vars {
            m.insert(n.into(), v.into());
        }
    }

    /// Drops captured values for `env`, or for every env when `env` is "*".
    pub fn clear(&mut self, env: &str) {
        if env == "*" {
            self.envs.clear();
            return;
        }
        self.envs.remove(key(env));
    }

    /// The environments that have captured values, sorted.
    pub fn env_names(&self) -> Vec<String> {
        self.envs
            .iter()
            .filter(|(_, vars)| !vars.is_empty())
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Writes the session to disk, creating `.apic/` and its `.gitignore`.
    /// A memory-only store is a no-op.
    pub fn save(&self) -> Result<(), Error> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        // Refuse to write tokens into a directory that is not ignored:
        // the .gitignore is what keeps a captured OAuth2 access and
        // refresh token out of a commit, so failing to create it is not
        // something to shrug off.
        if let Some(dir) = path.parent() {
            ensure_dir(dir)?;
        }
        let mut compact = String::from("{\"envs\":{");
        for (i, (env, vars)) in self.envs.iter().enumerate() {
            if i > 0 {
                compact.push(',');
            }
            compact.push_str(&json_string(env, true));
            compact.push_str(":{");
            for (j, (name, value)) in vars.iter().enumerate() {
                if j > 0 {
                    compact.push(',');
                }
                compact.push_str(&json_string(name, true));
                compact.push(':');
                compact.push_str(&json_string(value, true));
            }
            compact.push('}');
        }
        compact.push_str("}}");
        let mut data = indent_json(&compact, "", "  ");
        data.push('\n');
        write_private(path, data.as_bytes()).map_err(|err| Error(path_error("open", path, &err)))
    }
}

/// Creates a `.apic` directory with the `.gitignore` that keeps it out of
/// commits. The session, the cookie jar and the history all write through
/// it.
pub fn ensure_dir(dir: &Path) -> Result<(), Error> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|err| Error(path_error("mkdir", dir, &err)))?;
    let gi = dir.join(".gitignore");
    if matches!(fs::metadata(&gi), Err(err) if err.kind() == io::ErrorKind::NotFound) {
        write_private(
            &gi,
            b"# created by apic; session state must not be committed\n*\n",
        )
        .map_err(|err| {
            Error(format!(
                "{}: {}",
                gi.display(),
                path_error("open", &gi, &err)
            ))
        })?;
    }
    Ok(())
}
