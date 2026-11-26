//! Running the commands auth depends on: the `@auth exec` command and the
//! AWS CLI. A command is found on the search path the caller gives, run
//! with its output captured, and killed when the caller cancels.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use apic_gocompat::{io_error_text, quote};
use apic_httpc::CancelToken;

/// Why a command gave no output.
#[derive(Debug)]
pub(crate) struct Failure {
    /// What went wrong: `exit status 1`, or why it could not start.
    pub message: String,
    /// What the command wrote to its standard error.
    pub stderr: String,
    /// The caller cancelled and the command was killed.
    pub cancelled: bool,
}

impl Failure {
    fn new(message: String) -> Failure {
        Failure {
            message,
            stderr: String::new(),
            cancelled: false,
        }
    }
}

#[cfg(windows)]
const PATH_VAR_NAME: &str = "%PATH%";
#[cfg(not(windows))]
const PATH_VAR_NAME: &str = "$PATH";

fn has_separator(name: &str) -> bool {
    name.contains('/') || (cfg!(windows) && name.contains('\\'))
}

#[cfg(unix)]
fn check_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {}", path.display(), io_error_text(&e)))?;
    if meta.is_dir() {
        return Err("is a directory".to_string());
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return Err("permission denied".to_string());
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_executable(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {}", path.display(), io_error_text(&e)))?;
    if meta.is_dir() {
        return Err("is a directory".to_string());
    }
    Ok(())
}

#[cfg(not(windows))]
fn candidates(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[cfg(windows)]
fn candidates(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    if Path::new(name).extension().is_some() {
        out.push(name.to_string());
    }
    for ext in [".com", ".exe", ".bat", ".cmd"] {
        out.push(format!("{name}{ext}"));
    }
    out
}

/// Finds a command the way a shell does: a name with a path separator is
/// that file; any other name is looked for in the directories of
/// `search_path`.
pub(crate) fn look_path(name: &str, search_path: &str) -> Result<String, String> {
    if has_separator(name) {
        return match check_executable(Path::new(name)) {
            Ok(()) => Ok(name.to_string()),
            Err(e) => Err(format!("exec: {}: {}", quote(name), e)),
        };
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in search_path.split(sep) {
        let dir = if dir.is_empty() { "." } else { dir };
        for candidate in candidates(name) {
            let path = Path::new(dir).join(&candidate);
            if check_executable(&path).is_ok() {
                return Ok(path.to_string_lossy().into_owned());
            }
        }
    }
    Err(format!(
        "exec: {}: executable file not found in {}",
        quote(name),
        PATH_VAR_NAME
    ))
}

#[cfg(unix)]
fn signal_text(status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    let sig = status.signal()?;
    let name = match sig {
        1 => "hangup",
        2 => "interrupt",
        3 => "quit",
        4 => "illegal instruction",
        6 => "aborted",
        8 => "floating point exception",
        9 => "killed",
        11 => "segmentation fault",
        13 => "broken pipe",
        14 => "alarm clock",
        15 => "terminated",
        _ => return Some(format!("signal: signal {sig}")),
    };
    let dumped = if status.core_dumped() {
        " (core dumped)"
    } else {
        ""
    };
    Some(format!("signal: {name}{dumped}"))
}

#[cfg(not(unix))]
fn signal_text(_status: &ExitStatus) -> Option<String> {
    None
}

fn status_text(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => signal_text(status).unwrap_or_else(|| "exit status -1".to_string()),
    }
}

fn read_all(mut from: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = from.read_to_end(&mut buf);
        buf
    })
}

/// Runs a command and returns what it wrote to its standard output. The
/// command gets no input. It is killed if `cancel` fires first.
pub(crate) fn output(
    cancel: &CancelToken,
    search_path: &str,
    name: &str,
    args: &[String],
) -> Result<Vec<u8>, Failure> {
    // A name with a path separator is run as it is; any other is looked
    // for on the search path first.
    let program = if has_separator(name) {
        name.to_string()
    } else {
        look_path(name, search_path).map_err(Failure::new)?
    };
    let mut child = Command::new(&program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::new(format!("fork/exec {}: {}", program, io_error_text(&e))))?;
    let stdout = child.stdout.take().map(read_all);
    let stderr = child.stderr.take().map(read_all);
    let mut cancelled = false;
    let mut pause = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(e) => break Err(io_error_text(&e)),
        }
        if cancel.wait_timeout(pause) {
            cancelled = true;
            let _ = child.kill();
            break child.wait().map_err(|e| io_error_text(&e));
        }
        pause = (pause * 2).min(Duration::from_millis(10));
    };
    let out = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
    let err = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    let fail = |message: String| Failure {
        message,
        stderr: String::from_utf8_lossy(&err).into_owned(),
        cancelled,
    };
    match status {
        Err(message) => Err(fail(message)),
        Ok(_) if cancelled => Err(fail("context canceled".to_string())),
        Ok(status) if !status.success() => Err(fail(status_text(&status))),
        Ok(_) => Ok(out),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const PATH: &str = "/usr/local/bin:/usr/bin:/bin";

    #[test]
    fn runs_and_reports() {
        let cancel = CancelToken::new();
        let out = output(&cancel, PATH, "sh", &["-c".into(), "echo hi".into()]).unwrap();
        assert_eq!(out, b"hi\n");
        let err = output(
            &cancel,
            PATH,
            "sh",
            &["-c".into(), "echo no >&2; exit 3".into()],
        )
        .unwrap_err();
        assert_eq!(
            (err.message.as_str(), err.stderr.as_str()),
            ("exit status 3", "no\n")
        );
        let err = output(&cancel, PATH, "definitely-not-a-command-xyz", &[]).unwrap_err();
        assert_eq!(
            err.message,
            "exec: \"definitely-not-a-command-xyz\": executable file not found in $PATH"
        );
        let err = output(&cancel, PATH, "./no/such/file", &[]).unwrap_err();
        assert_eq!(
            err.message,
            "fork/exec ./no/such/file: no such file or directory"
        );
        assert_eq!(
            look_path("./no/such/file", PATH).unwrap_err(),
            "exec: \"./no/such/file\": stat ./no/such/file: no such file or directory"
        );
        assert_eq!(look_path("sh", "/nonexistent::/bin").unwrap(), "/bin/sh");
    }

    #[test]
    fn kills_on_cancel() {
        let cancel = CancelToken::new();
        let c = cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c.cancel();
        });
        let started = std::time::Instant::now();
        let err = output(&cancel, PATH, "sleep", &["30".into()]).unwrap_err();
        t.join().unwrap();
        assert!(err.cancelled && err.message == "context canceled");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
