//! The example project `apic demo` writes, embedded at build time.

use std::fs;
use std::io::Write;
use std::path::Path;

use apic_gocompat::{mkdir_all, path_error};
use apic_project::path;

/// The bundled files, as `(path, contents)` in path order; paths use `/`.
pub(crate) static PROJECT: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/project.rs"));

const PRIVATE_ENV_FILE: &str = "http-client.private.env.json";

/// Creates or replaces a file. A new file is private to its owner when
/// asked, and readable by everyone otherwise.
fn write_file(target: &str, content: &[u8], private: bool) -> std::io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(if private { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = private;
    opts.open(target)?.write_all(content)
}

/// Writes the bundled example project into `dir` (creating it if needed):
/// apic.yaml, http-client.private.env.json, the .http files and features/
/// verbatim from `project/`, plus a generated http-client.env.json
/// pointing at `http://localhost:<port>`. Files that already exist are
/// left alone unless `force` is set. Returns the paths written and the
/// paths skipped.
pub fn write_project(
    dir: &str,
    port: u16,
    force: bool,
) -> Result<(Vec<String>, Vec<String>), String> {
    mkdir_all(Path::new(dir))?;
    let mut written = Vec::new();
    let mut skipped = Vec::new();

    let mut write = |name: &str, content: &[u8]| -> Result<(), String> {
        let target = path::join(dir, name);
        if !force && fs::metadata(&target).is_ok() {
            skipped.push(target);
            return Ok(());
        }
        write_file(&target, content, name == PRIVATE_ENV_FILE)
            .map_err(|e| path_error("open", Path::new(&target), &e))?;
        written.push(target);
        Ok(())
    };

    for (rel, content) in PROJECT {
        let native = rel.replace('/', std::path::MAIN_SEPARATOR_STR);
        if let Some((parent, _)) = native.rsplit_once(std::path::MAIN_SEPARATOR) {
            mkdir_all(Path::new(&path::join(dir, parent)))?;
        }
        write(&native, content)?;
    }

    let env_json =
        format!("{{\n  \"local\": {{\n    \"baseUrl\": \"http://localhost:{port}\"\n  }}\n}}\n");
    write("http-client.env.json", env_json.as_bytes())?;

    Ok((written, skipped))
}
