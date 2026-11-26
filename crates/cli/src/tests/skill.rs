use std::path::Path;

use serde_json::Value;

use super::{execute, join, must_read, must_write, s, tmp, ProcessGuard};
use crate::skill_dirs;

fn repo_skill(name: &str) -> String {
    let path = format!("{}/../../skills/{name}/SKILL.md", env!("CARGO_MANIFEST_DIR"));
    must_read(path).replace("\r\n", "\n")
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .map(|a| a.iter().map(|s| s.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default()
}

/// How many files the whole pack holds; an install writes them into each
/// of the skill directories.
fn pack_files() -> usize {
    apic_skills::names().iter().map(|n| apic_skills::paths(n).len()).sum()
}

/// The skill the binary carries is the one in skills/apic, so an agent
/// that runs `apic skill` reads what the repository publishes.
#[test]
fn skill_prints_the_embedded_skill() {
    let (code, out, errb) = execute(&["skill"]);
    assert!(
        code == 0 && out.starts_with("---\nname: apic\n") && out.contains("## Running requests"),
        "code={code} err={errb}"
    );
    assert_eq!(out, repo_skill("apic"), "apic skill differs from skills/apic/SKILL.md");
    let (code, out, _) = execute(&["--json", "skill"]);
    let got: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("code={code} {e}: {out}"));
    assert_eq!(code, 0);
    assert!(
        got["name"] == "apic"
            && got["files"]["SKILL.md"].as_str().unwrap_or("").starts_with("---")
            && got["files"]["references/cheatsheet.md"].as_str().unwrap_or("").contains("# apic cheat sheet"),
        "json: {got}"
    );
    assert_eq!(strings(&got, "skills"), apic_skills::names());
}

/// The pack's other skills are printed by name, and a name that is not in
/// the pack is a usage error that lists what is.
#[test]
fn skill_prints_the_named_skill() {
    for name in ["api-design", "api-security"] {
        let (code, out, errb) = execute(&["skill", name]);
        assert!(
            code == 0 && out.starts_with(&format!("---\nname: {name}\n")),
            "{name}: code={code} err={errb}"
        );
        assert_eq!(out, repo_skill(name), "apic skill {name} differs from skills/{name}/SKILL.md");
    }
    let (code, out, errb) = execute(&["--json", "skill", "api-security"]);
    let got: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("code={code} {e} err={errb}: {out}"));
    assert!(
        code == 0
            && got["name"] == "api-security"
            && got["files"]["references/checks.md"].as_str().unwrap_or("").contains("## Release checklist"),
        "json: name={}",
        got["name"]
    );
    let (code, _, errb) = execute(&["--json", "skill", "api-desing"]);
    assert!(
        code == 2 && errb.contains(r#""code":"E203""#) && errb.contains("api-design"),
        "unknown skill: code={code} err={errb}"
    );
}

#[test]
fn skill_install_writes_once_and_reports_unchanged() {
    let tmp = tmp();
    let dir = s(&tmp);
    let dirs = skill_dirs().len();
    let (code, out, errb) = execute(&["skill", "install", &dir, "--all", "--no-color"]);
    assert_eq!(code, 0, "err={errb}");
    for rel in [
        ".claude/skills/apic/SKILL.md",
        ".claude/skills/apic/references/cheatsheet.md",
        ".agents/skills/apic/SKILL.md",
        ".agents/skills/apic/references/cheatsheet.md",
        ".claude/skills/api-design/SKILL.md",
        ".claude/skills/api-design/references/openapi.md",
        ".agents/skills/api-security/SKILL.md",
        ".agents/skills/api-security/references/owasp-api-top-10.md",
    ] {
        let path = join(&dir, rel);
        assert!(Path::new(&path).exists(), "{rel} not written");
        assert!(out.contains(&format!("wrote {path}")), "output lacks wrote {path}:\n{out}");
    }
    // A second install finds the same text and changes nothing; an edited
    // copy is put back, since the text is apic's.
    must_write(join(&dir, ".agents/skills/apic/SKILL.md"), "edited");
    let (code, out, _) = execute(&["--json", "skill", "install", &dir, "--all"]);
    let got: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("code={code} {e}: {out}"));
    assert_eq!(code, 0);
    let (written, unchanged) = (strings(&got, "written"), strings(&got, "unchanged"));
    assert!(
        written.len() == 1
            && written[0].ends_with(".agents/skills/apic/SKILL.md")
            && unchanged.len() == pack_files() * dirs - 1,
        "second install: {got}"
    );
    // --to picks the directories; -C is the default project.
    let only = super::tmp();
    let (code, _, errb) = execute(&["-C", &s(&only), "skill", "install", "--to", "tools/skills"]);
    assert_eq!(code, 0, "err={errb}");
    assert!(Path::new(&join(only.path(), "tools/skills/apic/SKILL.md")).exists());
    assert!(
        !Path::new(&join(only.path(), ".claude")).exists(),
        "--to should replace the default directories, not add to them"
    );
    // Without --all the apic skill alone is installed: the other two are
    // opinions about the reader's API, which a project opts into.
    let plain = super::tmp();
    let (code, out, errb) = execute(&["--json", "skill", "install", &s(&plain)]);
    assert_eq!(code, 0, "default install: err={errb}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let written = strings(&got, "written");
    assert!(
        written.len() == apic_skills::paths("apic").len() * dirs && !written.iter().any(|p| p.contains("api-")),
        "default install wrote {written:?}"
    );
    let (code, _, errb) = execute(&["skill", "install", &s(&plain), "--all", "--skill", "apic"]);
    assert!(code == 2 && errb.contains("E203"), "--all --skill: code={code} err={errb}");
    // --skill picks part of the pack, a name given twice counts once, and
    // an unknown name is refused before anything is written.
    let some = super::tmp();
    let (code, out, errb) = execute(&[
        "--json", "skill", "install", &s(&some), "--skill", "apic", "--skill", "api-security", "--skill", "apic",
    ]);
    assert_eq!(code, 0, "--skill: err={errb}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let (written, unchanged) = (strings(&got, "written"), strings(&got, "unchanged"));
    let want = (apic_skills::paths("apic").len() + apic_skills::paths("api-security").len()) * dirs;
    assert!(
        written.len() == want && unchanged.is_empty() && !written.iter().any(|p| p.contains("api-design")),
        "--skill apic --skill api-security --skill apic: written {written:?} unchanged {unchanged:?}"
    );
    let (code, _, errb) = execute(&["skill", "install", &s(&some), "--skill", "nope"]);
    assert!(
        code == 2 && errb.contains("E203") && errb.contains("api-design"),
        "--skill nope: code={code} err={errb}"
    );
}

/// --user writes the same directories under the home directory, for
/// every project, follows a .claude or .agents that a dotfile manager
/// links elsewhere, writes nothing into the project, and takes neither a
/// dir, -C nor --to, which would contradict it.
#[test]
fn skill_install_user_writes_under_home() {
    let mut guard = ProcessGuard::new();
    let home = tmp();
    guard.set_env("HOME", &s(&home));
    guard.set_env("USERPROFILE", &s(&home));
    let project = tmp();
    guard.chdir(project.path());
    let dirs = skill_dirs().len();
    let (code, out, errb) = execute(&["--json", "skill", "install", "--user", "--skill", "apic"]);
    assert_eq!(code, 0, "err={errb}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let written = strings(&got, "written");
    assert!(
        written.len() == apic_skills::paths("apic").len() * dirs
            && written[0] == join(home.path(), ".claude/skills/apic/SKILL.md"),
        "written: {written:?}"
    );
    assert!(Path::new(&join(home.path(), ".agents/skills/apic/references/cheatsheet.md")).exists());
    assert_eq!(
        std::fs::read_dir(project.path()).unwrap().count(),
        0,
        "--user should write nothing into the project"
    );
    let (code, _, _) = execute(&["--json", "skill", "install", "--user", "--skill", "apic"]);
    assert_eq!(code, 0, "second --user install");
    let project_dir = s(&project);
    let refused: [&[&str]; 3] = [
        &["skill", "install", &project_dir, "--user"],
        &["skill", "install", "--user", "--to", "x"],
        &["-C", &project_dir, "skill", "install", "--user"],
    ];
    for args in refused {
        let (code, _, errb) = execute(args);
        assert!(code == 2 && errb.contains("E203"), "{args:?}: code={code} err={errb}");
    }
    // A linked ~/.claude is followed to wherever the dotfiles live.
    #[cfg(unix)]
    {
        let dotfiles = tmp();
        let linked = tmp();
        guard.set_env("HOME", &s(&linked));
        guard.set_env("USERPROFILE", &s(&linked));
        if std::os::unix::fs::symlink(dotfiles.path(), linked.path().join(".claude")).is_err() {
            return; // symlinks unavailable
        }
        let (code, _, errb) = execute(&["skill", "install", "--user", "--skill", "apic"]);
        assert_eq!(code, 0, "linked ~/.claude: err={errb}");
        assert!(
            Path::new(&join(dotfiles.path(), "skills/apic/SKILL.md")).exists(),
            "the link should have been followed"
        );
        // A home that is not a directory names itself, not a project.
        let missing = join(linked.path(), "missing");
        guard.set_env("HOME", &missing);
        guard.set_env("USERPROFILE", &missing);
        let (code, _, errb) = execute(&["skill", "install", "--user"]);
        assert!(
            code == 2 && errb.contains("home directory") && !errb.contains("apic init"),
            "missing home: code={code} err={errb}"
        );
    }
}

/// The help names every skill of the pack, so adding one without saying
/// so fails here rather than leaving the help stale; a mistyped
/// subcommand is pointed at the right one, not only at the skill names.
#[test]
fn skill_help_names_the_pack() {
    let (_, long, _) = execute(&["skill", "--help"]);
    let (_, install, _) = execute(&["skill", "install", "--help"]);
    for name in apic_skills::names() {
        assert!(long.contains(&format!("\n  {name}")), "apic skill --help does not list {name}");
        assert!(install.contains(&name), "apic skill install --help does not name {name}");
    }
    let (code, _, errb) = execute(&["skill", "instal"]);
    assert!(
        code == 2 && errb.contains("apic skill install"),
        "apic skill instal: code={code} err={errb}"
    );
}

/// apic init briefs the project's agents by default, so a scaffolded
/// project is agent-ready without a second command.
#[test]
fn init_writes_the_skill_unless_asked_not_to() {
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, out, errb) = execute(&["init", &dir, "--no-color"]);
    assert!(
        code == 0 && out.contains(&join(&dir, ".claude/skills/apic/SKILL.md")),
        "code={code} out={out} err={errb}"
    );
    assert!(Path::new(&join(&dir, ".agents/skills/apic/references/cheatsheet.md")).exists());
    // init writes the apic skill alone; the pack is `apic skill install
    // --all`.
    assert!(
        !Path::new(&join(&dir, ".agents/skills/api-design")).exists(),
        "init should not write api-design"
    );
    // An edited copy is kept on a second init, like every other file, and
    // reported as skipped; --force replaces it, as `apic skill install`
    // always does.
    let edited = join(&dir, ".claude/skills/apic/SKILL.md");
    must_write(&edited, "tailored");
    let (code, out, _) = execute(&["--json", "init", &dir]);
    let got: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("second init: code={code} {e}: {out}"));
    assert_eq!(code, 0);
    assert!(
        strings(&got, "written").is_empty() && strings(&got, "skipped").contains(&edited),
        "second init: {got}"
    );
    assert_eq!(
        must_read(&edited),
        "tailored",
        "init overwrote an edited skill file without --force"
    );
    let (code, _, errb) = execute(&["init", &dir, "--force"]);
    assert_eq!(code, 0, "--force: err={errb}");
    assert_ne!(must_read(&edited), "tailored", "--force should replace the edited skill file");
    let bare = super::tmp();
    let (code, _, errb) = execute(&["init", &s(&bare), "--no-skill"]);
    assert_eq!(code, 0, "err={errb}");
    assert!(
        !Path::new(&join(bare.path(), ".claude")).exists(),
        "--no-skill should write no skill"
    );
}

/// The root help tells an agent where the briefing is.
#[test]
fn root_help_points_agents_at_the_skill() {
    let (_, out, _) = execute(&["--help"]);
    assert!(out.contains("apic skill prints the briefing"), "root help:\n{out}");
}

/// The install stays under the project: a --to that climbs out, or a
/// symlink in the way, is refused rather than followed. A project named
/// relative to the working directory, as `apic init my-api` does, is
/// still inside it.
#[test]
fn skill_install_stays_under_the_project() {
    let guard = ProcessGuard::new();
    let cwd = tmp();
    guard.chdir(cwd.path());
    let (code, _, errb) = execute(&["init", "my-api"]);
    assert_eq!(code, 0, "relative project: err={errb}");
    assert!(Path::new("my-api/.agents/skills/apic/SKILL.md").exists());
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, _, errb) = execute(&["skill", "install", &dir, "--to", "../outside"]);
    assert!(
        code == 2 && errb.contains("resolves outside"),
        "--to ../outside: code={code} err={errb}"
    );
    assert!(
        !tmp.path().parent().unwrap().join("outside").exists(),
        "something was written outside the project"
    );
    // A mistyped project is an error, not a new directory tree.
    let typo = join(&dir, "aip");
    let (code, _, errb) = execute(&["skill", "install", &typo]);
    assert!(
        code == 2 && errb.contains("not a directory"),
        "missing project: code={code} err={errb}"
    );
    assert!(!Path::new(&typo).exists(), "a missing project directory was created");
    #[cfg(unix)]
    {
        let elsewhere = super::tmp();
        std::fs::create_dir_all(join(&dir, ".claude")).unwrap();
        if std::os::unix::fs::symlink(elsewhere.path(), join(&dir, ".claude/skills")).is_err() {
            return; // symlinks unavailable
        }
        let (code, _, errb) = execute(&["skill", "install", &dir]);
        assert!(
            code == 2 && errb.contains("resolves outside"),
            "symlinked skills dir: code={code} err={errb}"
        );
        assert_eq!(
            std::fs::read_dir(elsewhere.path()).unwrap().count(),
            0,
            "the symlink was followed out of the project"
        );
    }
}

/// The paths reported are the ones the user named, joined like the
/// scaffold's, not the resolved ones: on macOS a temporary directory is a
/// symlink into /private, and `apic init --json` must list one shape.
#[cfg(unix)]
#[test]
fn skill_install_reports_paths_as_named() {
    let guard = ProcessGuard::new();
    let cwd = tmp();
    guard.chdir(cwd.path());
    std::fs::create_dir_all("real").unwrap();
    if std::os::unix::fs::symlink("real", "link").is_err() {
        return; // symlinks unavailable
    }
    let (code, out, errb) = execute(&["--json", "skill", "install", "link"]);
    assert_eq!(code, 0, "err={errb}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let written = strings(&got, "written");
    assert!(
        written.len() == apic_skills::paths("apic").len() * skill_dirs().len()
            && written[0] == "link/.claude/skills/apic/SKILL.md",
        "written: {written:?}"
    );
    assert!(
        Path::new("real/.agents/skills/apic/SKILL.md").exists(),
        "the files should land under the real directory the link names"
    );
}
