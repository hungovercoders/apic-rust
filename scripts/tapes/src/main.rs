//! Records the course's VHS tapes the same way every time. It builds apic,
//! serves the demo API on the port the lessons show, gives each tape a
//! scratch directory with a fresh apic-demo project and the files its
//! lesson's runnable blocks create (the lessons crate), and runs vhs there
//! with a fixed HOME and time zone, so two recordings of a tape differ
//! only in timings. Run it with `task tapes` (vhs, ttyd, ffmpeg and jq on
//! PATH); `task tapes:check` validates every tape's syntax with vhs alone,
//! which CI does, so a tape that only a person has run cannot quietly
//! break.

#![forbid(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use apic_gocompat::FlagSet;
use record::Step;

/// Where the tapes live, relative to the repository root.
const TAPES_DIR: &str = "docs/learn/tapes";
const DEMO_TAPE: &str = "docs/assets/demo.tape";

/// One VHS script: its name (01, 05, demo) and its path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tape {
    name: String,
    path: PathBuf,
}

/// What each tape expects to find beyond the demo project: the state the
/// lesson has built up by the time its terminal parts begin, as steps run
/// in its scratch directory. A tape not listed needs only apic-demo and
/// the running API.
fn setup() -> Vec<(&'static str, Vec<Step>)> {
    vec![
        // the staging environment and the private env file
        ("02", vec![Step::lesson("02", &[3, 7])]),
        // logged in, so the todos requests have a token
        ("04", vec![Step::lesson("04", &[1])]),
        // logged in, and the apic-auth project with its exec and signed requests
        ("06", vec![Step::lesson("06", &[1, 8, 10, 11, 12])]),
        // the feature files the tape runs, and a login for the ones that
        // need it; the lesson deletes the undefined feature after showing
        // it, the tape shows it
        (
            "07",
            vec![Step::lesson("07", &[3, 4, 5, 6, 8])
                .replacing("rm apic-demo/features/undefined.feature\n", "")],
        ),
        (
            "09",
            vec![
                // the OpenAPI document and the Postman collection and environment
                Step::lesson("09", &[1, 11]),
                // the tidied files the tape copies in, written to tidy/
                // instead of into the project the tape is about to import
                Step::sh("mkdir -p tidy"),
                Step::lesson("09", &[8, 9]).replacing("todos-api/", "tidy/"),
                // block 11 imported the collection; the tape does that on camera
                Step::sh("rm -rf todos-api postman-api"),
            ],
        ),
    ]
}

fn setup_for(name: &str) -> Vec<Step> {
    setup()
        .into_iter()
        .find(|(tape, _)| *tape == name)
        .map(|(_, steps)| steps)
        .unwrap_or_default()
}

fn main() {
    let mut flags = FlagSet::new("tapes");
    flags.bool(
        "check",
        false,
        "validate every tape's syntax with vhs and record nothing",
    );
    flags.bool(
        "prepare",
        false,
        "set each tape's scratch directory up and keep it, recording nothing",
    );
    flags.string(
        "only",
        "",
        "comma-separated tape names to record, e.g. 01,05,demo (default: all)",
    );
    flags.string("out", "dist/tapes", "directory to write the GIFs into");
    flags.bool(
        "keep",
        false,
        "keep each tape's scratch directory for inspection",
    );
    flags.parse_env();
    let root = Path::new(".");
    let tapes = find(root)
        .and_then(|tapes| record::pick(&tapes, |t| t.name.clone(), flags.get("only")))
        .unwrap_or_else(|err| fail(&err));
    if flags.get_bool("check") {
        let failed = validate(&tapes, &mut std::io::stdout());
        if failed > 0 {
            fail(&format!("{failed} of {} tapes are invalid", tapes.len()));
        }
        return;
    }
    let prepare = flags.get_bool("prepare");
    let keep = flags.get_bool("keep") || prepare;
    if let Err(err) = record_all(root, &tapes, Path::new(flags.get("out")), keep, prepare) {
        fail(&err);
    }
}

fn fail(err: &str) -> ! {
    eprintln!("tapes: {err}");
    std::process::exit(1);
}

/// Every file under `dir` whose name ends in `suffix`, sorted. A directory
/// that is not there holds none.
fn files_ending(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(suffix))
            .collect(),
        Err(_) => Vec::new(),
    };
    found.sort();
    found
}

/// Lists the lesson tapes in order, then the README's demo tape.
fn find(root: &Path) -> Result<Vec<Tape>, String> {
    let mut paths = files_ending(&root.join(TAPES_DIR), ".tape");
    paths.push(root.join(DEMO_TAPE));
    let cwd = std::env::current_dir().map_err(|err| err.to_string())?;
    Ok(paths
        .into_iter()
        .map(|p| Tape {
            name: p
                .file_name()
                .map(|n| n.to_string_lossy().trim_end_matches(".tape").to_string())
                .unwrap_or_default(),
            path: absolute(&cwd, &p),
        })
        .collect())
}

/// `p` as an absolute path without `.` and `..` parts, against `cwd` when
/// it is relative.
fn absolute(cwd: &Path, p: &Path) -> PathBuf {
    use std::path::Component;
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Runs `vhs validate` on every tape and returns how many failed,
/// reporting each on `w`.
fn validate(tapes: &[Tape], w: &mut dyn Write) -> usize {
    let mut failed = 0;
    for t in tapes {
        let (out, result) =
            record::combined_output(Command::new("vhs").arg("validate").arg(&t.path));
        if result.is_err() {
            failed += 1;
            let text = String::from_utf8_lossy(&out);
            let _ = writeln!(w, "✗ {}\n{}", t.path.display(), text.trim_end_matches('\n'));
            continue;
        }
        let _ = writeln!(w, "✓ {}", t.path.display());
    }
    failed
}

fn record_all(
    root: &Path,
    tapes: &[Tape],
    out: &Path,
    keep: bool,
    prepare_only: bool,
) -> Result<(), String> {
    // vhs records a shell, so a missing tool would not fail the recording:
    // it would be filmed as "command not found". Check up front for vhs
    // and what it drives, and for jq, which most tapes pipe through.
    for tool in ["vhs", "ttyd", "ffmpeg", "jq"] {
        if !record::on_path(tool) && !prepare_only {
            return Err(format!("{tool} is not on PATH; recording needs vhs, ttyd and ffmpeg (https://github.com/charmbracelet/vhs#installation) and jq, which the tapes pipe through"));
        }
    }
    std::fs::create_dir_all(out).map_err(|err| err.to_string())?;
    let bin = tempfile::Builder::new()
        .prefix("apic-tapes-bin-")
        .tempdir()
        .map_err(|err| err.to_string())?;
    record::build_apic(root, bin.path(), None)?;
    for t in tapes {
        record_one(root, bin.path(), t, out, keep, prepare_only)
            .map_err(|err| format!("{}: {err}", t.name))?;
    }
    Ok(())
}

/// Gives the tape a scratch directory with the demo project, the state its
/// setup describes and the demo API listening, then runs vhs there and
/// moves what it wrote into `out`.
fn record_one(
    root: &Path,
    bin: &Path,
    t: &Tape,
    out: &Path,
    keep: bool,
    prepare_only: bool,
) -> Result<(), String> {
    let scratch = tempfile::Builder::new()
        .prefix(&format!("apic-tapes-{}-", t.name))
        .tempdir()
        .map_err(|err| err.to_string())?;
    let work = if keep {
        scratch.keep()
    } else {
        scratch.path().to_path_buf()
    };
    let parent = |p: &str| {
        Path::new(p)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    };
    for d in [PathBuf::from("home"), parent(TAPES_DIR), parent(DEMO_TAPE)] {
        std::fs::create_dir_all(work.join(d)).map_err(|err| err.to_string())?;
    }
    apic_demoapi::write_project(
        &work.join("apic-demo").to_string_lossy(),
        record::PORT,
        true,
    )?;
    let server = record::serve()?;
    let env = record::environment(bin, &work);
    let result = (|| {
        for s in setup_for(&t.name) {
            s.run(root, &work, &env)?;
        }
        if prepare_only {
            println!("{}: prepared in {}", t.name, work.display());
            return Ok(());
        }
        let mut vhs = Command::new("vhs");
        vhs.arg(&t.path).current_dir(&work);
        record::set_env(&mut vhs, &env);
        match vhs.status() {
            Ok(status) if status.success() => {}
            Ok(status) => return Err(format!("vhs: {}", record::exit_text(status))),
            Err(err) => return Err(format!("vhs: {err}")),
        }
        // A tape's Output is a repository-relative path, written under the
        // scratch directory; the result belongs in out.
        let docs = work.join("docs");
        let mut gifs: Vec<PathBuf> = Vec::new();
        let mut nested: Vec<PathBuf> = Vec::new();
        for dir in subdirs(&docs) {
            gifs.extend(files_ending(&dir, ".gif"));
            for inner in subdirs(&dir) {
                nested.extend(files_ending(&inner, ".gif"));
            }
        }
        gifs.extend(nested);
        if gifs.is_empty() {
            return Err("vhs wrote no GIF under the scratch directory; does the tape's Output start with docs/?".to_string());
        }
        for g in gifs {
            let dst = out.join(g.file_name().unwrap_or_default());
            move_file(&g, &dst).map_err(|err| err.to_string())?;
            println!("{}: wrote {}", t.name, dst.display());
        }
        if keep {
            println!("{}: scratch directory kept at {}", t.name, work.display());
        }
        Ok(())
    })();
    server.shutdown();
    result
}

/// The directories directly under `dir`, sorted.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => Vec::new(),
    };
    found.sort();
    found
}

/// Renames `src` to `dst`, copying when the two are on different
/// filesystems (the scratch directory is under the system temp directory,
/// the output under the repository), where a rename fails.
fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::write(dst, std::fs::read(src)?)?;
    std::fs::remove_file(src)
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(format!("{}/../..", env!("CARGO_MANIFEST_DIR")))
    }

    /// Every setup entry names a tape that exists, and lesson blocks the
    /// page really has; a substitution names text that appears in those
    /// blocks.
    #[test]
    fn setup_names_real_tapes_and_blocks() {
        for (name, steps) in setup() {
            let tape = root().join(TAPES_DIR).join(format!("{name}.tape"));
            assert!(tape.is_file(), "setup[{name:?}]: no such tape");
            for s in steps {
                if !s.sh.is_empty() {
                    continue;
                }
                let blocks = apic_lessons::blocks(root(), &s.lesson)
                    .unwrap_or_else(|err| panic!("setup[{name:?}]: {err}"));
                let code = s
                    .code(&blocks)
                    .unwrap_or_else(|err| panic!("setup[{name:?}]: {err}"));
                assert!(
                    s.replace[0].is_empty() || !code.contains(&s.replace[0]),
                    "setup[{name:?}]: the substitution of {:?} left some behind",
                    s.replace[0]
                );
            }
        }
    }

    /// Every numbered lesson has a tape, or its episode script says there
    /// is none and why, so a lesson cannot be left without a recording by
    /// accident.
    #[test]
    fn every_lesson_has_a_tape_or_says_why_not() {
        let numbered = Regex::new(r"^[0-9][0-9]-.*\.md$").unwrap();
        let pages: Vec<PathBuf> = files_ending(&root().join(apic_lessons::DIR), ".md")
            .into_iter()
            .filter(|p| numbered.is_match(&p.file_name().unwrap().to_string_lossy()))
            .collect();
        assert!(!pages.is_empty(), "no lesson pages");
        for p in pages {
            let base = p.file_name().unwrap().to_string_lossy().into_owned();
            let n = &base[..2];
            if root().join(TAPES_DIR).join(format!("{n}.tape")).is_file() {
                continue;
            }
            let data = std::fs::read_to_string(&p).unwrap();
            assert!(
                data.contains("No tape for this lesson"),
                "{base} has no tape and does not say why; add {TAPES_DIR}/{n}.tape or \"No tape for this lesson\" to its episode script"
            );
        }
    }

    /// A tape's Output is where the recorder expects to pick the GIF up.
    #[test]
    fn every_tape_writes_under_docs() {
        let tapes = find(&root()).unwrap();
        assert!(tapes.len() >= 5, "found only {} tapes", tapes.len());
        let re = Regex::new(r"(?m)^Output (docs/[^\t\n\f\r ]+\.gif)$").unwrap();
        for tp in tapes {
            assert!(
                tp.path.is_absolute(),
                "{} is not absolute",
                tp.path.display()
            );
            let data = std::fs::read_to_string(&tp.path).unwrap();
            assert!(
                re.is_match(&data),
                "{}: no `Output docs/….gif` line",
                tp.path.display()
            );
        }
    }

    /// With vhs on PATH, every tape parses; CI installs it for this. VHS
    /// strings take no backslash escapes, so a command that needs both
    /// quote kinds goes in backticks: this is the test that catches the
    /// alternative.
    #[test]
    fn every_tape_is_valid() {
        if !record::on_path("vhs") {
            return; // vhs is not on PATH
        }
        let tapes = find(&root()).unwrap();
        let failed = validate(&tapes, &mut std::io::stdout());
        assert_eq!(failed, 0, "{failed} tapes are invalid");
    }

    /// The tapes come in lesson order with the demo tape last, each named
    /// by its file.
    #[test]
    fn find_orders_the_tapes() {
        let tapes = find(&root()).unwrap();
        let names: Vec<&str> = tapes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names.first(), Some(&"01"));
        assert_eq!(names.last(), Some(&"demo"));
        let lessons = &names[..names.len() - 1];
        assert!(lessons.windows(2).all(|w| w[0] < w[1]), "{names:?}");
        assert_eq!(
            absolute(Path::new("/a/b"), Path::new("./../c/x.tape")),
            PathBuf::from("/a/c/x.tape")
        );
    }
}
