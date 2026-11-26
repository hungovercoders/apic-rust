//! Runs the commands in the course pages so a lesson cannot rot: it builds
//! apic, starts `apic demo` in a scratch directory, then extracts every
//! fenced block in the site's learn/*.md and migrate/*.md that is preceded
//! by an `<!-- learn -->` comment (the lessons crate) and runs it there
//! with `sh -e`, in page order (the course first, then the migration
//! guides).
//!
//! Blocks that need credentials, an editor or a browser are simply not
//! marked. Run it with `task learn:check`; CI runs it on every change to
//! the course, the demo API or the CLI.

#![forbid(unsafe_code)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use apic_gocompat::FlagSet;
use apic_lessons::Block;

/// The course and the migration guides, relative to the repository root.
const LEARN: &str = "website/src/content/docs/learn";
const MIGRATE: &str = "website/src/content/docs/migrate";

fn main() {
    let mut flags = FlagSet::new("learncheck");
    flags.string(
        "docs",
        &format!("{LEARN},{MIGRATE}"),
        "comma-separated directories holding the pages, run in this order",
    );
    flags.bool("keep", false, "keep the scratch directory for inspection");
    flags.parse_env();
    let dirs: Vec<&str> = flags.get("docs").split(',').collect();
    if let Err(err) = run(Path::new("."), &dirs, flags.get_bool("keep")) {
        eprintln!("learncheck: {err}");
        std::process::exit(1);
    }
}

/// Stops a child process when it goes out of scope.
struct Stopped(Child);

impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Every `*.md` directly under `dir`, sorted.
fn pages_in(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
            .collect(),
        // A directory that is not there holds no pages.
        Err(_) => Vec::new(),
    };
    found.sort();
    found
}

/// Runs the pages under `dirs`, which are relative to `root`, the
/// repository root.
fn run(root: &Path, dirs: &[&str], keep: bool) -> Result<(), String> {
    let mut pages: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        pages.extend(pages_in(&root.join(dir)));
    }
    let mut blocks: Vec<Block> = Vec::new();
    for p in &pages {
        let data = std::fs::read(p).map_err(|err| format!("{}: {err}", p.display()))?;
        // A page is named as the caller named its directory, not by where
        // the root happens to be.
        let shown = p.strip_prefix(root).unwrap_or(p).to_string_lossy();
        blocks.extend(apic_lessons::extract(
            &shown,
            &String::from_utf8_lossy(&data),
        ));
    }
    if blocks.is_empty() {
        return Err(format!(
            "no runnable blocks under {} (mark one with <!-- learn -->)",
            dirs.join(", ")
        ));
    }

    let scratch = tempfile::Builder::new()
        .prefix("learncheck-")
        .tempdir()
        .map_err(|err| err.to_string())?;
    let work = if keep {
        scratch.keep()
    } else {
        scratch.path().to_path_buf()
    };
    let bin = work.join("bin");
    std::fs::create_dir_all(&bin).map_err(|err| err.to_string())?;
    let apic = record::build_apic(root, &bin, None)?;

    // The lessons say `apic demo` writes ./apic-demo and serves on 8089;
    // the harness does the same in the scratch directory, on the default
    // port when it is free so the pages' URLs match, else on a free one.
    let mut port = 8089;
    if !port_free(port) {
        port = free_port();
    }
    let demo = Command::new(&apic)
        .args([
            "demo",
            "--out",
            "apic-demo",
            "--port",
            &port.to_string(),
            "--force",
        ])
        .current_dir(&work)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|err| format!("starting apic demo: {err}"))?;
    let _demo = Stopped(demo);
    wait_for(
        &format!("http://127.0.0.1:{port}/health"),
        Duration::from_secs(10),
    )?;
    println!(
        "learncheck: apic demo on port {port}, {} blocks from {} pages",
        blocks.len(),
        pages.len()
    );

    let mut env = record::os_environ();
    env.extend([
        format!("PATH={}", record::path_with(&bin)),
        format!("APIC_DEMO_URL=http://localhost:{port}"),
        "NO_COLOR=1".to_string(),
    ]);
    let mut failed = 0;
    for b in &blocks {
        let place = format!("{}:{}", b.file, b.line);
        match record::shell_output(&work, &env, &b.code) {
            Ok((status, _)) if status.success() => println!("✓ {place}"),
            Ok((_, out)) => {
                failed += 1;
                let text = String::from_utf8_lossy(&out);
                println!("✗ {place}\n{}", indent(text.trim_end_matches('\n')));
            }
            Err(err) => {
                failed += 1;
                println!("✗ {place}\n{}", indent(&err.to_string()));
            }
        }
    }
    if keep {
        println!("learncheck: scratch directory kept at {}", work.display());
    }
    if failed > 0 {
        return Err(format!("{failed} of {} blocks failed", blocks.len()));
    }
    Ok(())
}

fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|ln| ln.local_addr())
        .map_or(0, |addr| addr.port())
}

/// Polls `url` until it answers 200, or the deadline passes.
fn wait_for(url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let client = apic_httpc::Client::new();
    while Instant::now() < deadline {
        let req = apic_httpc::Request::get(url).timeout(Duration::from_secs(2));
        if client.send(&req).is_ok_and(|res| res.status_code == 200) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("apic demo did not come up in time".to_string())
}

fn indent(s: &str) -> String {
    let lines: Vec<String> = s.lines().map(|line| format!("    {line}")).collect();
    lines.join("\n").trim_end_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the binary and serves the demo, so it runs only when asked
    /// for: `cargo test -p learncheck -- --ignored`.
    #[test]
    #[ignore = "builds the binary and serves the demo"]
    fn template_runs_end_to_end() {
        let root = PathBuf::from(format!("{}/../..", env!("CARGO_MANIFEST_DIR")));
        if let Err(err) = run(&root, &[LEARN, MIGRATE], false) {
            panic!("{err}");
        }
    }

    /// The course and the migration guides have pages with blocks to run,
    /// in page order, each named by its path from the repository root.
    #[test]
    fn pages_have_runnable_blocks() {
        let root = PathBuf::from(format!("{}/../..", env!("CARGO_MANIFEST_DIR")));
        for dir in [LEARN, MIGRATE] {
            let pages = pages_in(&root.join(dir));
            assert!(!pages.is_empty(), "no pages under {dir}");
            assert!(pages.windows(2).all(|w| w[0] < w[1]), "{dir} is not sorted");
            let blocks: usize = pages
                .iter()
                .map(|p| {
                    let data = std::fs::read_to_string(p).unwrap();
                    apic_lessons::extract(&p.to_string_lossy(), &data).len()
                })
                .sum();
            assert!(blocks > 0, "no runnable blocks under {dir}");
        }
        assert!(pages_in(&root.join("no-such-directory")).is_empty());
    }

    #[test]
    fn indent_keeps_lines_apart() {
        assert_eq!(indent("a\n\nb"), "    a\n    \n    b");
        assert_eq!(indent(""), "");
    }
}
