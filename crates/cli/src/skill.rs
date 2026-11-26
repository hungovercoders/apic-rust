//! apic skill: print or install the Agent Skills compiled into the binary.

use std::collections::BTreeMap;
use std::path::Path;

use apic_gocompat::quote;
use apic_project::path as fp;
use apic_runner::Code;
use serde::Serialize;

use crate::io::wln;
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::{theme, write_file};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

/// Where `apic skill install` and `apic init` put the skills, relative
/// to the project: the directory Claude Code reads, and the one the Agent
/// Skills ecosystem (Codex, Cursor and the `skills` CLI) shares.
pub fn skill_dirs() -> Vec<String> {
    vec![fp::join(".claude", "skills"), fp::join(".agents", "skills")]
}

pub(crate) fn spec() -> CommandSpec {
    let names = apic_skills::names();
    let mut cmd = CommandSpec::new(
        "skill [name]",
        "Print an Agent Skill from the pack that briefs an AI agent on apic and on APIs",
    )
    .long(
        r#"skill prints a skill's SKILL.md, the briefing an AI agent reads before
working on a project. The pack has three, each a directory of the apic
repository compiled into the binary:

  apic          how to run, write and test requests with apic: the validate,
                list, describe, run workflow, the --json shape and exit
                codes, the error codes to branch on, how to write a request
                and where secrets belong (the default)
  api-design    how to design the HTTP API those requests call: resources,
                methods and status codes, errors, pagination, versioning,
                idempotency and OpenAPI, each proved with an apic request
  api-security  how to keep that API safe and show it: the OWASP API
                Security Top 10 as checks, authentication, authorisation,
                input handling, headers and secrets, each a request or a
                scenario apic runs

An agent that can run commands can read one with "apic skill <name>";
"apic skill install" writes the apic skill into the project so agents that
load skills find it on their own, and "apic skill install --all" the pack."#,
    )
    .example(
        "  apic skill                  # print SKILL.md of the apic skill
  apic skill api-design       # another skill of the pack
  apic skill --json           # every file of the skill, and the pack's names
  apic skill install --all    # the pack, into .claude/skills and .agents/skills of the project",
    )
    .args(ArgRule::Max(1))
    .runs(run)
    .subcommand(
        CommandSpec::new(
            "install [dir]",
            "Write the apic skill, or the whole pack, into a project or your home directory",
        )
        .long(
            "install writes the apic skill into <dir>/.claude/skills/apic (Claude
Code) and <dir>/.agents/skills/apic (Codex, Cursor and the other agents
that share that directory), dir being the project (-C) unless given; it
must exist (apic init creates a project, and installs the skill as it
does). The apic skill is about using apic, so it is the default; --all
adds api-design and api-security, which are opinions about the API itself
(status codes, error shapes, pagination, the OWASP checks), and --skill
picks exactly the skills named. --user writes the same directories under
your home directory instead, where those agents read skills for every
project, so one install briefs them everywhere; it takes no dir and no
--to. Files that already hold the same text are left alone; others are
overwritten, since the skills are apic's text rather than the project's.
Commit a project install so every clone briefs its agents.",
        )
        .example(
            "  apic skill install
  apic skill install ./api
  apic skill install --all                    # the apic, api-design and api-security skills
  apic skill install --user --all             # once, for every project on this machine
  apic skill install --to .claude/skills      # Claude Code only
  apic skill install --skill api-security     # exactly this skill",
        )
        .args(ArgRule::Max(1))
        .flag(FlagSpec::string_array(
            "to",
            "directory under the project to install into, instead of .claude/skills and .agents/skills (repeatable)",
        ))
        .flag(FlagSpec::string_array(
            "skill",
            &format!(
                "a skill of the pack to install, instead of the apic skill alone: {} (repeatable)",
                names.join(", ")
            ),
        ))
        .flag(FlagSpec::bool("all", &format!("install the whole pack: {}", names.join(", "))))
        .flag(FlagSpec::bool(
            "user",
            "install into .claude/skills and .agents/skills of your home directory, for every project, instead of the project's",
        ))
        .runs(install),
    );
    cmd.valid_args = names;
    cmd
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let names = apic_skills::names();
    let name = inv.args.first().map_or(apic_skills::NAME, String::as_str);
    let Some(files) = apic_skills::files(name) else {
        let mut msg = format!("unknown skill {}; the pack has {}", quote(name), names.join(", "));
        let sub = App::spec()
            .find("skill")
            .map(|cmd| crate::parse::suggestions_for(cmd, name))
            .unwrap_or_default();
        if !sub.is_empty() {
            msg.push_str(&format!("; did you mean apic skill {}?", sub.join(" or ")));
        }
        return Err(fail(Code::Flag, msg));
    };
    if a.g.json {
        #[derive(Serialize)]
        struct Out<'a> {
            name: &'a str,
            files: &'a BTreeMap<String, String>,
            skills: &'a [String],
        }
        return a.write_json(&Out { name, files: &files, skills: &names });
    }
    let text = files.get("SKILL.md").map_or("", String::as_str);
    a.stdout.write_text(text).map_err(CliError::other)
}

fn install(a: &mut App, inv: &Invocation) -> CliResult {
    let f = &inv.flags;
    let (into, only) = (f.strings("to"), f.strings("skill"));
    let (user, all) = (f.bool("user"), f.bool("all"));
    let mut dir = inv.args.first().cloned().unwrap_or_else(|| a.g.dir.clone());
    let dirs = if into.is_empty() { skill_dirs() } else { into.clone() };
    if user {
        if inv.args.len() == 1 || !into.is_empty() || f.changed("dir") {
            return Err(fail(
                Code::Flag,
                "--user installs under your home directory, so it takes no dir, no -C and no --to",
            ));
        }
        dir = home_dir().map_err(|e| {
            fail(Code::File, format!("--user: cannot find your home directory: {e}"))
        })?;
    }
    let mut names = vec![apic_skills::NAME.to_string()];
    if all {
        if !only.is_empty() {
            return Err(fail(
                Code::Flag,
                "--all installs the whole pack, so it takes no --skill",
            ));
        }
        names = apic_skills::names();
    }
    if !only.is_empty() {
        // A name given twice is installed once, so no file is reported
        // both written and unchanged.
        let mut picked: Vec<String> = Vec::new();
        for name in &only {
            if !apic_skills::has(name) {
                return Err(fail(
                    Code::Flag,
                    format!(
                        "--skill {}: unknown skill; the pack has {}",
                        quote(name),
                        apic_skills::names().join(", ")
                    ),
                ));
            }
            if !picked.contains(name) {
                picked.push(name.clone());
            }
        }
        names = picked;
    }
    let done = write_skill(&dir, &dirs, &names, SkillWrite { create: false, overwrite: true, user })?;
    a.print_written(&done)
}

impl App {
    /// Reports the files an install wrote and the ones it found current.
    pub(crate) fn print_written(&self, done: &Written) -> CliResult {
        if self.g.json {
            #[derive(Serialize)]
            struct Out<'a> {
                written: &'a [String],
                unchanged: &'a [String],
            }
            return self.write_json(&Out { written: &done.written, unchanged: &done.unchanged });
        }
        let t = theme();
        for f in &done.written {
            wln!(self.stdout, "{} {f}", t.ok.render("wrote"));
        }
        for f in &done.unchanged {
            wln!(self.stdout, "{} {f}", t.dim.render("unchanged"));
        }
        Ok(())
    }
}

/// The user's home directory, from the environment.
fn home_dir() -> Result<String, String> {
    let (var, missing) = if cfg!(windows) {
        ("USERPROFILE", "%userprofile% is not defined")
    } else {
        ("HOME", "$HOME is not defined")
    };
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(missing.to_string()),
    }
}

/// Says how [`write_skill`] treats what it finds: `create` makes a
/// missing root (apic init) where install requires one; `overwrite`
/// replaces a file holding other text (install) where init keeps it and
/// reports it skipped; `user` means the root is the home directory
/// (--user), whose errors say so, and whose .claude and .agents may be
/// links a dotfile manager keeps elsewhere, so they are followed rather
/// than confined.
#[derive(Clone, Copy, Default)]
pub(crate) struct SkillWrite {
    pub create: bool,
    pub overwrite: bool,
    pub user: bool,
}

/// What writing a set of files came to, by path as shown.
#[derive(Default)]
pub(crate) struct Written {
    pub written: Vec<String>,
    pub unchanged: Vec<String>,
    pub skipped: Vec<String>,
}

/// Writes the named skills into each of dirs under root, as
/// `<dir>/<skill>/<file>`. A file already holding the same text is
/// reported as unchanged rather than rewritten, so a second install is a
/// no-op; one holding other text is overwritten or kept as `how` says. In
/// a project every path is confined to root, symlinks followed, so
/// neither `--to ../x` nor a link under .claude/skills can put the files
/// outside it; --user rejects those flags, and its paths are the home
/// directory's.
pub(crate) fn write_skill(
    root: &str,
    dirs: &[String],
    names: &[String],
    how: SkillWrite,
) -> Result<Written, CliError> {
    let missing = if how.user {
        "--user writes the skill pack under your home directory"
    } else {
        "the skill pack is written into an existing project (apic init creates one)"
    };
    let abs_root = write_root(root, how.create, missing)?;
    let mut out = Written::default();
    for skill in names {
        let files = apic_skills::files(skill).unwrap_or_default();
        let paths = apic_skills::paths_of(&files);
        for dir in dirs {
            for name in &paths {
                let rel = fp::join(&fp::join(dir, skill), name);
                let (shown, target) = if how.user {
                    let shown = fp::join(&abs_root, &rel);
                    (shown.clone(), shown)
                } else {
                    confined_path(root, &abs_root, &rel, "the skill pack")?
                };
                let content = files.get(name).map_or("", String::as_str).as_bytes();
                if let Ok(existing) = std::fs::read(&target) {
                    if existing == content {
                        out.unchanged.push(shown);
                        continue;
                    }
                    if !how.overwrite {
                        out.skipped.push(shown);
                        continue;
                    }
                }
                apic_gocompat::mkdir_all(Path::new(&fp::dir(&target))).map_err(|e| fail(Code::File, e))?;
                write_file(&target, content, 0o644).map_err(|e| fail(Code::File, e))?;
                out.written.push(shown);
            }
        }
    }
    Ok(out)
}

/// Returns the absolute path of root, the directory the user named for
/// the files a command writes: made when create is set (apic init),
/// otherwise required to exist (install, where a missing one is a
/// mistyped project, or --user, where it is the home directory).
/// `missing` finishes the error for a root that is not a directory. The
/// path is absolute because confinement compares real paths: `apic init
/// my-api` names the root relative to the working directory, and on macOS
/// a temporary directory is a symlink.
pub(crate) fn write_root(root: &str, create: bool, missing: &str) -> Result<String, CliError> {
    if create {
        apic_gocompat::mkdir_all(Path::new(root)).map_err(|e| fail(Code::File, e))?;
    } else if !std::fs::metadata(root).is_ok_and(|info| info.is_dir()) {
        return Err(fail(Code::File, format!("{root} is not a directory; {missing}")));
    }
    fp::abs(root).map_err(|e| fail(Code::File, apic_gocompat::io_error_text(&e)))
}

/// Resolves rel, a path the user named under root, for a file a command
/// writes: the path to report (root joined with rel, as the scaffold's
/// are, or rel itself when it is absolute) and the real target, which
/// must lie under abs_root, symlinks followed, so neither `--to ../x` nor
/// a link under the root can put the file outside it. `what` names the
/// files in the error.
pub(crate) fn confined_path(
    root: &str,
    abs_root: &str,
    rel: &str,
    what: &str,
) -> Result<(String, String), CliError> {
    let shown = if fp::is_abs(rel) { rel.to_string() } else { fp::join(root, rel) };
    match apic_project::confine(abs_root, abs_root, rel) {
        Ok(target) => Ok((shown, target)),
        Err(e) if e.is_outside_root() => Err(fail(
            Code::File,
            format!("{shown} resolves outside {root}; {what} is written under it"),
        )),
        Err(e) => Err(fail(Code::File, format!("{shown}: {e}"))),
    }
}
