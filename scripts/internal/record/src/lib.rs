//! What the course's recorders share. `scripts/tapes` and
//! `scripts/episodes` both serve the demo API on the port the lessons show,
//! run a lesson's own commands as setup in a scratch directory, and hand
//! vhs an environment with nothing of the recording machine's in the
//! picture; one copy of each keeps the two recordings alike. The helpers
//! for building the apic binary and running a command with its output
//! gathered are here too, for every generator that needs them.

#![forbid(unsafe_code)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use apic_gocompat::quote;
use apic_lessons::Block;

/// The port the lessons show in their URLs; a recording's picture carries
/// it, so it is not negotiable the way learncheck's is.
pub const PORT: u16 = 8089;

/// One piece of a recording's setup: a shell snippet, or runnable blocks
/// of a lesson page (1-based, in page order), optionally with a text
/// substitution applied to them first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub sh: String,
    /// The page's number prefix, "09".
    pub lesson: String,
    pub blocks: Vec<i64>,
    /// The text to replace and what replaces it; nothing when the first
    /// is empty.
    pub replace: [String; 2],
}

impl Step {
    /// A step that runs a shell snippet.
    pub fn sh(code: &str) -> Step {
        Step {
            sh: code.to_string(),
            ..Step::default()
        }
    }

    /// A step that runs blocks of a lesson.
    pub fn lesson(lesson: &str, blocks: &[i64]) -> Step {
        Step {
            lesson: lesson.to_string(),
            blocks: blocks.to_vec(),
            ..Step::default()
        }
    }

    /// The same step with `old` replaced by `new` in its blocks.
    pub fn replacing(mut self, old: &str, new: &str) -> Step {
        self.replace = [old.to_string(), new.to_string()];
        self
    }

    /// The shell the step runs: its snippet, or the lesson blocks it names
    /// (from `blocks`, the lesson's runnable blocks in page order), with
    /// the substitution applied. A substitution that matches none of the
    /// blocks is an error, since the step would silently run the unchanged
    /// lesson commands.
    pub fn code(&self, blocks: &[Block]) -> Result<String, String> {
        if !self.sh.is_empty() {
            return Ok(format!("{}\n", self.sh));
        }
        let mut code = String::new();
        for n in &self.blocks {
            let block = usize::try_from(*n)
                .ok()
                .filter(|n| *n >= 1)
                .and_then(|n| blocks.get(n - 1));
            let Some(block) = block else {
                return Err(format!(
                    "lesson {} has {} runnable blocks, no block {n}",
                    self.lesson,
                    blocks.len()
                ));
            };
            code.push_str(&block.code);
        }
        let [old, new] = &self.replace;
        if !old.is_empty() {
            if !code.contains(old.as_str()) {
                let named: Vec<String> = self.blocks.iter().map(i64::to_string).collect();
                return Err(format!(
                    "nothing to replace: {} is not in blocks [{}] of lesson {}",
                    quote(old),
                    named.join(" "),
                    self.lesson
                ));
            }
            code = code.replace(old.as_str(), new);
        }
        Ok(code)
    }

    /// Reads the step's lesson, if it names one, and runs its code in
    /// `work`.
    pub fn run(&self, root: &Path, work: &Path, env: &[String]) -> Result<(), String> {
        let blocks = if self.sh.is_empty() {
            apic_lessons::blocks(root, &self.lesson).map_err(|err| err.to_string())?
        } else {
            Vec::new()
        };
        shell(work, env, &self.code(&blocks)?)
    }
}

/// Runs `code` with `sh -e` in `work`, as learncheck runs the lessons'
/// blocks, with colour off since nothing is recording.
pub fn shell(work: &Path, env: &[String], code: &str) -> Result<(), String> {
    let mut env = env.to_vec();
    env.push("NO_COLOR=1".to_string());
    let (status, out) =
        shell_output(work, &env, code).map_err(|err| format!("setup failed: {err}\n"))?;
    if !status.success() {
        return Err(format!(
            "setup failed: {}\n{}",
            exit_text(status),
            String::from_utf8_lossy(&out)
        ));
    }
    Ok(())
}

/// Feeds `code` to `sh -e` in `work` under exactly `env`, and returns how
/// it ended with what it wrote, standard output and error as one stream.
pub fn shell_output(
    work: &Path,
    env: &[String],
    code: &str,
) -> std::io::Result<(ExitStatus, Vec<u8>)> {
    // The outer shell only joins the two streams; the inner one reads the
    // code from standard input.
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "exec sh -e 2>&1"]).current_dir(work);
    set_env(&mut cmd, env);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("a piped stdin");
    let code = code.to_string();
    // Written from a thread of its own: a script that prints a lot before
    // it has read all of itself would otherwise block both ends.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(code.as_bytes());
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    Ok((out.status, out.stdout))
}

/// Replaces a command's environment with `env`, a list of `NAME=value`
/// in which a later entry wins over an earlier one of the same name.
pub fn set_env(cmd: &mut Command, env: &[String]) {
    cmd.env_clear();
    for kv in env {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        if !k.is_empty() {
            cmd.env(k, v);
        }
    }
}

/// The process's own environment as a list of `NAME=value`.
pub fn os_environ() -> Vec<String> {
    std::env::vars_os()
        .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
        .collect()
}

/// `dir` put first on the process's PATH, as the value of PATH.
pub fn path_with(dir: &Path) -> String {
    let sep = if cfg!(windows) { ';' } else { ':' };
    format!(
        "{}{sep}{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// How a command ended, in words: `exit status 1`, or the signal that
/// ended it.
pub fn exit_text(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => status.to_string(),
    }
}

/// Runs a command and returns what it wrote, standard output then standard
/// error, or why it failed: that it could not be started, or how it ended.
pub fn combined_output(cmd: &mut Command) -> (Vec<u8>, Result<(), String>) {
    match cmd.stdin(Stdio::null()).output() {
        Err(err) => (Vec::new(), Err(err.to_string())),
        Ok(out) => {
            let mut all = out.stdout;
            all.extend_from_slice(&out.stderr);
            let result = if out.status.success() {
                Ok(())
            } else {
                Err(exit_text(out.status))
            };
            (all, result)
        }
    }
}

/// Reports whether a program of that name is on PATH.
pub fn on_path(tool: &str) -> bool {
    look_path(tool).is_some()
}

/// Finds a program on PATH.
pub fn look_path(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// What a recording's setup and vhs run under: the fresh apic in `bin`
/// first on PATH, a HOME of its own under `work`, UTC and a UTF-8 locale,
/// and nothing of the recording machine's that would show in the picture:
/// no NO_COLOR, no proxy (`apic env` reports the one it sees) and no APIC_
/// variable, since an exported APIC_VAR_baseUrl or APIC_VAR_user would
/// override the demo project's values in every shot.
pub fn environment(bin: &Path, work: &Path) -> Vec<String> {
    let mut env: Vec<String> = os_environ()
        .into_iter()
        .filter(|kv| {
            let upper = kv
                .split_once('=')
                .map_or(kv.as_str(), |(k, _)| k)
                .to_uppercase();
            !(matches!(
                upper.as_str(),
                "PATH"
                    | "HOME"
                    | "TZ"
                    | "LANG"
                    | "LC_ALL"
                    | "NO_COLOR"
                    | "HTTP_PROXY"
                    | "HTTPS_PROXY"
                    | "ALL_PROXY"
                    | "NO_PROXY"
            ) || upper.starts_with("APIC_"))
        })
        .collect();
    env.extend([
        format!("PATH={}", path_with(bin)),
        format!("HOME={}", work.join("home").display()),
        "TZ=UTC".to_string(),
        "LANG=C.UTF-8".to_string(),
        format!("APIC_DEMO_URL=http://localhost:{PORT}"),
    ]);
    env
}

/// Starts the demo API on [`PORT`], waiting a few seconds for the port
/// when a recording's own `apic demo` is still letting go of it. The
/// server stops when it is shut down or dropped.
pub fn serve() -> Result<apic_httpd::Server, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let listener = loop {
        match apic_httpd::Listener::bind(&format!("127.0.0.1:{PORT}")) {
            Ok(listener) => break listener,
            Err(err) => {
                if Instant::now() > deadline {
                    return Err(format!(
                        "the lessons show port {PORT}, which is in use: {err}"
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    };
    Ok(apic_httpd::Server::start(listener, apic_demoapi::new()))
}

/// Keeps the items named in `only`, comma-separated, in the order given;
/// an empty `only` keeps them all, and a name that matches nothing is an
/// error, since a typo would otherwise record nothing and say so late.
pub fn pick<T: Clone>(
    items: &[T],
    name: impl Fn(&T) -> String,
    only: &str,
) -> Result<Vec<T>, String> {
    if only.is_empty() {
        return Ok(items.to_vec());
    }
    let mut picked = Vec::new();
    for n in only.split(',') {
        let n = n.trim();
        let before = picked.len();
        picked.extend(items.iter().filter(|it| name(it) == n).cloned());
        if picked.len() == before {
            let names: Vec<String> = items.iter().map(&name).collect();
            return Err(format!("no {} among {}", quote(n), names.join(", ")));
        }
    }
    Ok(picked)
}

/// Builds the apic binary from the workspace at `root` and copies it into
/// `dir`, so what runs is the tree under test. `version`, when given, is
/// stamped into the binary in place of `dev`.
pub fn build_apic(root: &Path, dir: &Path, version: Option<&str>) -> Result<PathBuf, String> {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "build",
        "-q",
        "-p",
        "apic",
        "--message-format=json-render-diagnostics",
    ])
    .current_dir(root)
    .stdin(Stdio::null())
    .stderr(Stdio::inherit());
    if let Some(version) = version {
        cmd.env("APIC_VERSION", version);
    }
    let out = cmd
        .output()
        .map_err(|err| format!("building apic: {err}"))?;
    if !out.status.success() {
        return Err(format!("building apic: {}", exit_text(out.status)));
    }
    let built = built_executable(&String::from_utf8_lossy(&out.stdout))
        .ok_or("building apic: cargo did not report where it put the binary")?;
    let name = if cfg!(windows) { "apic.exe" } else { "apic" };
    let dest = dir.join(name);
    std::fs::copy(&built, &dest).map_err(|err| {
        format!(
            "building apic: copying {} to {}: {err}",
            built.display(),
            dest.display()
        )
    })?;
    Ok(dest)
}

/// Picks the apic executable out of cargo's build messages, one JSON
/// object a line.
fn built_executable(messages: &str) -> Option<PathBuf> {
    messages
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "apic")
        .filter_map(|m| m["executable"].as_str().map(PathBuf::from))
        .next()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn block(code: &str) -> Block {
        Block {
            code: code.to_string(),
            ..Block::default()
        }
    }

    /// A step's code is its snippet, or its blocks in order with the
    /// substitution applied across them; a block the lesson lacks or a
    /// substitution that matches nothing is an error.
    #[test]
    fn step_code() {
        let blocks = [block("one\n"), block("two\nrm it\n"), block("three\n")];
        assert_eq!(Step::sh("echo hi").code(&[]).unwrap(), "echo hi\n");
        let got = Step::lesson("01", &[1, 2])
            .replacing("rm it\n", "")
            .code(&blocks);
        assert_eq!(got.as_deref(), Ok("one\ntwo\n"));
        let err = Step::lesson("01", &[4]).code(&blocks).unwrap_err();
        assert!(err.contains("no block 4"), "missing block: {err}");
        let err = Step::lesson("01", &[1])
            .replacing("rm it\n", "")
            .code(&blocks)
            .unwrap_err();
        assert!(
            err.contains("nothing to replace"),
            "substitution matching nothing: {err}"
        );
    }

    /// The recording environment carries none of the machine's apic
    /// settings, proxies or colour switches, and sets its own PATH, HOME
    /// and demo URL.
    #[test]
    fn environment_is_clean() {
        for kv in [
            "APIC_VAR_user=me",
            "apic_var_baseUrl=x",
            "APIC_USER=me",
            "HTTPS_PROXY=http://p",
            "no_proxy=*",
            "NO_COLOR=1",
            "KEEP_ME=1",
        ] {
            let (k, v) = kv.split_once('=').unwrap();
            std::env::set_var(k, v);
        }
        let keys: HashMap<String, String> =
            environment(Path::new("/bin-dir"), Path::new("/work-dir"))
                .into_iter()
                .map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap();
                    (k.to_string(), v.to_string())
                })
                .collect();
        for gone in [
            "APIC_VAR_user",
            "apic_var_baseUrl",
            "APIC_USER",
            "HTTPS_PROXY",
            "no_proxy",
            "NO_COLOR",
        ] {
            assert!(!keys.contains_key(gone), "environment still has {gone}");
        }
        let home = Path::new("/work-dir").join("home").display().to_string();
        for (k, want) in [
            ("KEEP_ME", "1"),
            ("HOME", home.as_str()),
            ("TZ", "UTC"),
            ("APIC_DEMO_URL", "http://localhost:8089"),
        ] {
            assert_eq!(keys.get(k).map(String::as_str), Some(want), "{k}");
        }
        assert!(
            keys["PATH"].starts_with("/bin-dir"),
            "PATH={:?} does not start with the binary's directory",
            keys["PATH"]
        );
    }

    /// `pick` keeps the named items in the order asked for and refuses a
    /// name that matches nothing.
    #[test]
    fn pick_by_name() {
        let items = ["01".to_string(), "02".to_string(), "demo".to_string()];
        let id = |s: &String| s.clone();
        assert_eq!(pick(&items, id, "").unwrap().len(), 3);
        assert_eq!(pick(&items, id, "demo, 01").unwrap().join(","), "demo,01");
        assert!(pick(&items, id, "03").is_err(), "pick(03) found something");
    }

    /// The executable is read from cargo's messages, and a build that
    /// reports none has none.
    #[test]
    fn executable_from_build_messages() {
        let messages = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"apic-cli"},"executable":null}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"apic"},"executable":"/t/debug/apic"}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n"
        );
        assert_eq!(
            built_executable(messages),
            Some(PathBuf::from("/t/debug/apic"))
        );
        assert_eq!(built_executable("not json\n"), None);
    }

    /// A shell snippet runs in the directory given, and a failing one
    /// reports how it ended with what it wrote.
    #[test]
    fn shell_runs_and_reports() {
        let dir = std::env::temp_dir();
        let env = vec![format!(
            "PATH={}",
            std::env::var("PATH").unwrap_or_default()
        )];
        shell(&dir, &env, "true\n").unwrap();
        let err = shell(&dir, &env, "echo out; echo err >&2; exit 3\n").unwrap_err();
        assert_eq!(err, "setup failed: exit status 3\nout\nerr\n");
    }
}
