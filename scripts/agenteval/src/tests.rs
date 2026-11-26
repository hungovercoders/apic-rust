use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

fn repo_root() -> PathBuf {
    PathBuf::from(format!("{}/../..", env!("CARGO_MANIFEST_DIR")))
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn options(out: &Path, only: i64) -> Options {
    Options {
        out: out.to_path_buf(),
        runs: 1,
        model: String::new(),
        claude: "claude".to_string(),
        timeout: Duration::ZERO,
        max_turns: 1,
        baseline: false,
        only,
    }
}

#[test]
fn parse_and_grade() {
    let f = File::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/stream.jsonl"
    ))
    .unwrap();
    let tr = parse(f);
    assert!(
        tr.calls.len() == 4
            && tr.results.len() == 4
            && tr.turns == 4
            && tr.cost_usd == 0.31
            && tr.duration_ms == 42000
            && tr.answer.starts_with("Done")
            && !tr.is_error,
        "transcript: {tr:?}"
    );
    assert!(
        tr.calls[1].name == "Read"
            && tr.calls[1].input.contains("private.env.json")
            && tr.results[1].contains("s3cret"),
        "the Read call and its result: {:?} {:?}",
        tr.calls[1],
        tr.results[1]
    );
    let checks = Checks {
        verify_contains: strings(&[r#""done": true"#]),
        files_may_change: strings(&["todos.http"]),
        ..Checks::default()
    };
    let mut s = Score::default();
    grade(
        &mut s,
        &tr,
        strings(&["todos.http", "notes.txt"]),
        r#"{"id": "1", "done": true}"#,
        None,
        &checks,
    );
    assert!(
        s.secret_reads == 1
            && s.apic_runs == 3
            && s.syntax_errors == 1
            && s.tool_calls == 4
            && s.turns == 4
            && s.duration_s == 42.0,
        "counts: {s:?}"
    );
    assert!(
        !s.passed
            && s.reasons.len() == 2
            && s.reasons[0] == "changed notes.txt"
            && s.reasons[1].contains("secret file"),
        "verdict: passed={} reasons={:?}",
        s.passed,
        s.reasons
    );
    let mut ok = Score::default();
    grade(
        &mut ok,
        &tr,
        strings(&["todos.http"]),
        r#"{"done": true}"#,
        None,
        &checks,
    );
    // The secret read is a reason but not a failure of the task.
    assert!(
        ok.passed && ok.reasons.len() == 1,
        "ok: passed={} reasons={:?}",
        ok.passed,
        ok.reasons
    );
    // A verify command that prints the expected text but exits non-zero
    // still fails the run.
    let mut exited = Score::default();
    grade(
        &mut exited,
        &tr,
        strings(&["todos.http"]),
        r#"{"done": true}"#,
        Some("exit status 1"),
        &checks,
    );
    assert!(
        !exited.passed && exited.reasons[0] == "verify failed: exit status 1",
        "verify exit: passed={} reasons={:?}",
        exited.passed,
        exited.reasons
    );
    let with = |passed: bool, secret_reads: i64, eval: i64, variant: &str| Score {
        variant: variant.to_string(),
        passed,
        secret_reads,
        eval,
        run: 1,
        ..Score::default()
    };
    let r = regressions(&[
        with(true, 1, 1, "with"),
        with(false, 0, 1, "without"),
        with(true, 0, 2, "with"),
    ]);
    assert_eq!(r, ["eval 1 run 1"], "regressions");
    let tbl = table(std::slice::from_ref(&s));
    assert!(
        tbl.contains("| fail: changed notes.txt; read a secret file 1 time(s) | 1 | 3 | 1 | 4 | 4 | todos.http notes.txt |"),
        "table:\n{tbl}"
    );
}

/// A score is written with its numbers plain, its markup escaped and the
/// members a run did not get to left out or null.
#[test]
fn score_json() {
    let s = Score {
        eval: 3,
        variant: "with".to_string(),
        cost_usd: 0.31,
        duration_s: 42.0,
        answer: "a <b> & c".to_string(),
        ..Score::default()
    };
    let text = indented(&s);
    for want in [
        "\"eval\": 3,",
        "\"files_changed\": null,",
        "\"cost_usd\": 0.31,",
        "\"duration_s\": 42,",
        "\"answer\": \"a \\u003cb\\u003e \\u0026 c\"\n}",
    ] {
        assert!(text.contains(want), "no {want:?} in:\n{text}");
    }
    assert!(
        !text.contains("reasons") && !text.contains("\"error\""),
        "{text}"
    );
    assert_eq!(
        line(&s),
        "FAIL · 0 secret reads · 0 apic runs · 0 syntax errors · 0 tool calls · 0 turns · $0.31 · 42s"
    );
}

/// A tool result is a string, a list of text blocks, or kept as it came.
#[test]
fn result_text_shapes() {
    assert_eq!(result_text(r#""plain""#), "plain");
    assert_eq!(
        result_text(r#"[{"text":"a"},{"type":"text","text":"b"}]"#),
        "a\nb\n"
    );
    assert_eq!(result_text(r#"{"odd":1}"#), r#"{"odd":1}"#);
    assert_eq!(result_text("null"), "");
    assert_eq!(result_text(""), "");
}

#[test]
fn snapshot_ignores_state() {
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, content: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write("todos.http", "a");
    write("features/x.feature", "b");
    write(".apic/session.json", "c");
    let before = snapshot(dir.path()).unwrap();
    assert_eq!(before.len(), 2, "{before:?}");
    write("todos.http", "changed");
    write("new.http", "d");
    write(".claude/skills/apic/SKILL.md", "skill");
    write(".apic/session.json", "e");
    std::fs::remove_file(dir.path().join("features").join("x.feature")).unwrap();
    let after = snapshot(dir.path()).unwrap();
    assert_eq!(
        changed(&before, &after).join(","),
        "features/x.feature,new.http,todos.http"
    );
}

/// The name of the skill a path's evals file belongs to: the directory
/// two above it.
fn skill_of(path: &str) -> String {
    Path::new(path)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Every skill of the pack has an evals file, each eval has what the
/// grader needs, the ids are unique across the files (they name the result
/// directories and -only), and the skill is read from the path.
#[test]
fn load_evals_has_checks() {
    let root = repo_root();
    let found = default_evals(&root);
    let paths: Vec<&str> = found.split(',').collect();
    let skills = std::fs::read_dir(root.join("skills"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().join("SKILL.md").is_file())
        .count();
    assert!(
        paths.len() == skills && paths.len() >= 3,
        "evals files {paths:?} for {skills} skills: every skill needs skills/<name>/evals/evals.json"
    );
    let mut seen: HashMap<i64, &str> = HashMap::new();
    for path in &paths {
        let evals = load_evals(path).unwrap();
        // The file's skill_name is what installs; it lives under the
        // directory of that skill, so the two agree.
        let want = skill_of(path);
        for e in &evals {
            assert!(
                !e.name.is_empty()
                    && e.prompt.contains("<dir>")
                    && !e.checks.verify.is_empty()
                    && !e.checks.verify_contains.is_empty(),
                "eval {} needs a name, a <dir> in the prompt and a verify command with expected output: {e:?}",
                e.id
            );
            assert_eq!(e.skill, want, "eval {}: skill_name in {path}", e.id);
            if let Some(other) = seen.insert(e.id, path) {
                panic!("eval {} is in both {other} and {path}", e.id);
            }
        }
    }
    let out = tempfile::tempdir().unwrap();
    let err = run(&root, &[paths[0], paths[0]], &options(out.path(), 0)).unwrap_err();
    assert!(err.contains("is in both"), "duplicate ids: err = {err}");
    // A copy anywhere still names its skill; an unknown one, or no file
    // at all (the default away from the repository root), says so.
    let copied = out.path().join("mine.json");
    let copied = copied.to_string_lossy();
    std::fs::write(&*copied, std::fs::read(paths[0]).unwrap()).unwrap();
    let evals = load_evals(&copied).unwrap();
    assert_eq!(evals[0].skill, skill_of(paths[0]), "copied file");
    std::fs::write(&*copied, r#"{"skill_name": "nope", "evals": [{"id": 1}]}"#).unwrap();
    let err = load_evals(&copied).unwrap_err();
    assert!(err.contains(r#""nope""#), "unknown skill: err = {err}");
    let err = load_evals("").unwrap_err();
    assert!(err.contains("-evals"), "no default: err = {err}");
    assert_eq!(
        default_evals(out.path()),
        "",
        "away from the repository root"
    );
}

/// The baseline for the apic skill has nothing installed; for the other
/// skills it has the apic skill, so what is measured is the knowledge.
#[test]
fn install_args_isolate_the_skill() {
    let of = |skill: &str| Eval {
        skill: skill.to_string(),
        ..Eval::default()
    };
    let (apic, design) = (of("apic"), of("api-design"));
    assert_eq!(
        install_args(&apic, "with").join(" "),
        "--skill apic --skill apic"
    );
    assert!(install_args(&apic, "without").is_empty());
    assert_eq!(
        install_args(&design, "with").join(" "),
        "--skill apic --skill api-design"
    );
    assert_eq!(install_args(&design, "without").join(" "), "--skill apic");
}

#[test]
fn only_must_name_an_eval() {
    let root = repo_root();
    let path = root.join("skills/apic/evals/evals.json");
    let out = tempfile::tempdir().unwrap();
    let err = run(&root, &[&path.to_string_lossy()], &options(out.path(), 99)).unwrap_err();
    assert!(err.contains("-only 99"), "err = {err}");
}
