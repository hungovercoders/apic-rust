//! apic init: scaffold a new project.

use std::path::Path;

use apic_gocompat::quote;
use apic_project::path as fp;
use apic_runner::Code;
use serde::Serialize;

use crate::io::{w, wln};
use crate::mcp::{write_mcp_config, MCP_CONFIG_FILE};
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::{theme, write_file};
use crate::skill::{skill_dirs, write_skill, SkillWrite};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "init [dir]",
        "Scaffold a new apic project: config, env files and a first request",
    )
    .long(
        "init writes a small, working project into dir (default: the current
directory): apic.yaml, http-client.env.json, a gitignored
http-client.private.env.json, api.http with one annotated request, a
features/smoke.feature to run with apic test, the apic Agent Skill under
.claude/skills and .agents/skills so an AI agent working in the project
knows how to use apic (see apic skill; --no-skill leaves it out, apic
skill install refreshes it, and apic skill install --all adds the
api-design and api-security skills), and a .mcp.json registering apic
mcp for the agents that read it
(--no-mcp leaves it out; apic mcp install writes it elsewhere). Existing
files are left alone unless --force is given, an apic entry already in
.mcp.json included; the .gitignore is appended to and the rest of
.mcp.json kept.",
    )
    .example(
        "  apic init
  apic init api --base-url https://dev.example.com --env dev
  apic init --json",
    )
    .args(ArgRule::Max(1))
    .flag(FlagSpec::string("base-url", "https://api.example.com", "baseUrl for the environment"))
    .flag(FlagSpec::string("env", "dev", "name of the first environment"))
    .flag(FlagSpec::bool("force", "overwrite existing files"))
    .flag(FlagSpec::bool(
        "no-skill",
        "do not write the Agent Skill into .claude/skills and .agents/skills",
    ))
    .flag(FlagSpec::bool("no-mcp", "do not register apic mcp in .mcp.json"))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let f = &inv.flags;
    let (base_url, env_name) = (f.string("base-url"), f.string("env"));
    let force = f.bool("force");
    let dir = inv.args.first().map_or(".", String::as_str);
    if env_name.is_empty() {
        return Err(fail(Code::Flag, "--env must not be empty"));
    }
    let (mut written, mut skipped) = write_init_project(dir, &base_url, &env_name, force)?;
    if !f.bool("no-skill") {
        // Kept unless --force, like every other file: a team may have
        // tailored it. `apic skill install` is the refresh.
        let done = write_skill(
            dir,
            &skill_dirs(),
            &[apic_skills::NAME.to_string()],
            SkillWrite { create: true, overwrite: force, user: false },
        )?;
        written.extend(done.written);
        skipped.extend(done.skipped);
    }
    if !f.bool("no-mcp") {
        // The entry runs apic mcp from the project itself, so it needs no
        // --dir. The rest of an existing file is kept, and so is a
        // different apic entry unless --force: a team may have pointed it
        // at another directory or environment.
        let done = write_mcp_config(dir, &[MCP_CONFIG_FILE.to_string()], &["mcp".to_string()], force)?;
        written.extend(done.written);
        skipped.extend(done.skipped);
    }
    if a.g.json {
        #[derive(Serialize)]
        struct Out<'a> {
            out: &'a str,
            env: &'a str,
            #[serde(serialize_with = "crate::shared::null_when_empty")]
            written: &'a [String],
            #[serde(skip_serializing_if = "<[String]>::is_empty")]
            skipped: &'a [String],
        }
        return a.write_json(&Out { out: dir, env: &env_name, written: &written, skipped: &skipped });
    }
    let t = theme();
    for f in &written {
        wln!(a.stdout, "{} {f}", t.ok.render("wrote"));
    }
    for f in &skipped {
        wln!(
            a.stdout,
            "{}  {f} {}",
            t.warn.render("kept"),
            t.dim.render("(use --force to overwrite)")
        );
    }
    w!(a.stdout, "\n{}\n", t.bold.render("next:"));
    if dir != "." {
        wln!(a.stdout, "  cd {dir}");
    }
    wln!(
        a.stdout,
        "  edit api.http, then:  {}",
        t.dim.render("apic list · apic run ping · apic ui")
    );
    Ok(())
}

/// The first line of a scaffolded apic.yaml: the YAML language server
/// (and so VS Code, JetBrains and Neovim with it) picks up the published
/// schema for completion and validation.
const SCHEMA_MODELINE: &str = "# yaml-language-server: $schema=https://apic.sh/schemas/apic.schema.json";

/// Switches the response history on in a scaffolded apic.yaml: apic
/// history, apic history diff and apic select read it, and an agent uses
/// select to look at a response again (or at the rest of a body that
/// --body-limit cut) without sending the request again.
const HISTORY_STANZA: &str = "# Keep the last 10 responses of each request under .apic/history
# (gitignored, mode 0600) for apic history, history diff and select.
# A response body can hold personal data or a token: set 0 to keep none.
history: 10
";

/// Writes the starter files, skipping ones that exist unless force is
/// set. The .gitignore is appended to rather than replaced. Returns the
/// files written and the files kept.
fn write_init_project(
    dir: &str,
    base_url: &str,
    env_name: &str,
    force: bool,
) -> Result<(Vec<String>, Vec<String>), CliError> {
    apic_gocompat::mkdir_all(Path::new(&fp::join(dir, "features"))).map_err(CliError::other)?;
    let (mut written, mut skipped) = (Vec::new(), Vec::new());
    let files: [(String, String, u32); 5] = [
        (
            "apic.yaml".to_string(),
            format!("{SCHEMA_MODELINE}\n# Default environment when --env is not given.\nenv: {env_name}\n{HISTORY_STANZA}"),
            0o644,
        ),
        (
            "http-client.env.json".to_string(),
            format!(
                "{{\n  \"$shared\": {{}},\n  {}: {{\n    \"baseUrl\": {}\n  }}\n}}\n",
                quote(env_name),
                quote(base_url)
            ),
            0o644,
        ),
        (
            "http-client.private.env.json".to_string(),
            format!("{{\n  {}: {{\n    \"apiKey\": \"change-me\"\n  }}\n}}\n", quote(env_name)),
            0o600,
        ),
        ("api.http".to_string(), INIT_HTTP.to_string(), 0o644),
        (fp::join("features", "smoke.feature"), INIT_FEATURE.to_string(), 0o644),
    ];
    for (name, content, mode) in &files {
        let target = fp::join(dir, name);
        if !force && std::fs::metadata(&target).is_ok() {
            skipped.push(target);
            continue;
        }
        write_file(&target, content.as_bytes(), *mode).map_err(CliError::other)?;
        written.push(target);
    }
    ensure_gitignore(dir, &mut written)?;
    Ok((written, skipped))
}

fn ensure_gitignore(dir: &str, written: &mut Vec<String>) -> CliResult {
    let target = fp::join(dir, ".gitignore");
    // .env belongs here too: it is read as an environment file and its
    // values are marked secret, so it is a credential file like the
    // private env file.
    let lines = ["http-client.private.env.json", ".env", ".apic/"];
    let existing = match std::fs::read(&target) {
        Ok(data) => String::from_utf8_lossy(&data).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(CliError::other(apic_gocompat::path_error(
                "open",
                Path::new(&target),
                &e,
            )))
        }
    };
    let add: Vec<&str> = lines.iter().copied().filter(|l| !existing.contains(l)).collect();
    if add.is_empty() {
        return Ok(());
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&add.join("\n"));
    content.push('\n');
    // Non-secret scaffolding. The private env file is written 0600 above.
    write_file(&target, content.as_bytes(), 0o644).map_err(CliError::other)?;
    written.push(target);
    Ok(())
}

const INIT_HTTP: &str = "### Is the API up?
# A request apic can run right away: apic run ping
# @name ping
# @step the API is up
# @assert status == 200
GET {{baseUrl}}/health
Accept: application/json

### Fetch something that needs the key from http-client.private.env.json
# @name get-thing
# @description Fetch one thing by id
# @step I fetch thing {id}
# @assert status == 200
# @capture thingName = body.$.name
GET {{baseUrl}}/things/{{id}}
X-Api-Key: {{apiKey}}
Accept: application/json
";

const INIT_FEATURE: &str = "Feature: Smoke
  The requests live in api.http; the phrases used here are declared on
  them with \"# @step\".

  Scenario: The API answers
    When the API is up
    Then the response status is 200
";
