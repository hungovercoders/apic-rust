//! apic mcp: serve the project's requests to AI agents, and register the
//! server in a project's MCP configuration.

use std::path::Path;

use apic_project::path as fp;
use apic_runner::Code;
use serde_json::{json, Map, Value};

use crate::io::wln;
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::{plain_json, write_file};
use crate::skill::{confined_path, write_root, Written};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "mcp [--http <host:port> [--token <bearer>]]",
        "Serve the project's requests to AI agents over MCP (stdio, or HTTP)",
    )
    .long(
        "Start a Model Context Protocol server on stdin/stdout exposing the tools
list_requests, describe_request, run_request, run_file, run_features,
list_environments, clear_session, validate_project and curl_request, plus
every .http file as a resource.

Register it with your agent, for example:
  claude mcp add api -- apic mcp --dir ./api --env dev

--http serves the streamable HTTP transport instead, for an agent on
another machine or in a container. Bind the loopback interface, or set
--token (or APIC_MCP_TOKEN): the server runs the project's requests with
the project's credentials, and refuses to listen on any other interface
without a bearer token guarding it.",
    )
    .example(
        "  apic mcp --dir ./api --env dev
  apic mcp --http 127.0.0.1:8765
  APIC_MCP_TOKEN=$(openssl rand -hex 16) apic mcp --http 0.0.0.0:8765",
    )
    .args(ArgRule::None)
    .flag(FlagSpec::string(
        "http",
        "",
        "serve the streamable HTTP transport on this host:port instead of stdio",
    ))
    .flag(FlagSpec::string(
        "token",
        "",
        "bearer token clients must send (default $APIC_MCP_TOKEN); required off the loopback interface",
    ))
    .runs(run)
    .subcommand(
        CommandSpec::new(
            "install [dir]",
            "Register the server in the project's .mcp.json for the agents that read it",
        )
        .long(
            r#"install writes an entry that runs "apic mcp" into <dir>/.mcp.json, the
project-scoped MCP configuration Claude Code reads from the directory it
starts in; dir is the current directory unless given. The entry names the
project (-C) relative to dir when the two differ, and the environment
(--env) when one is given, so an agent gets the same tools "claude mcp
add" would register, from a file every clone of the repository shares.
Other servers in the file are kept; an entry already named "api" is
replaced, and one already holding the same text is left alone. Cursor
reads the same shape from .cursor/mcp.json: --to names the file. Commit
the result."#,
        )
        .example(
            "  apic mcp install                          # .mcp.json in the current directory
  apic mcp install -C api --env dev         # the entry runs apic mcp --dir api --env dev
  apic mcp install --to .cursor/mcp.json    # Cursor",
        )
        .args(ArgRule::Max(1))
        .flag(FlagSpec::string_array(
            "to",
            "file under dir to write instead of .mcp.json (repeatable)",
        ))
        .runs(install),
    )
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let http_addr = inv.flags.string("http");
    let mut token = inv.flags.string("token");
    let cfg = apic_mcp::Config {
        dir: a.g.dir.clone(),
        env: a.g.env.clone(),
        version: crate::version().to_string(),
    };
    if !http_addr.is_empty() {
        if token.is_empty() {
            token = std::env::var("APIC_MCP_TOKEN").unwrap_or_default();
        }
        let stderr = a.stderr.clone();
        let guard = if token.is_empty() {
            "no token: loopback only"
        } else {
            "bearer token required"
        };
        let ready = move |bound: &str| {
            wln!(stderr, "apic mcp listening on http://{bound} ({guard})");
        };
        return apic_mcp::serve_http(&inv.cancel, cfg, &http_addr, &token, Some(&ready))
            .map_err(|e| fail(Code::Server, e.to_string()));
    }
    match apic_mcp::serve(&inv.cancel, cfg) {
        Ok(()) => Ok(()),
        // Ctrl-C or SIGTERM: the server was told to stop.
        Err(e) if e.is_cancelled() || e.to_string() == "context canceled" => Ok(()),
        // The client closed the pipe: normal shutdown.
        Err(e) if e.to_string().ends_with("EOF") => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// The project-scoped MCP configuration Claude Code reads from the
/// directory it starts in, in the `{"mcpServers": {…}}` shape Cursor
/// (.cursor/mcp.json), Windsurf and the other clients accept too. `apic
/// mcp install` and `apic init` register the server there under
/// [`MCP_SERVER_NAME`], the name the docs' `claude mcp add api` example
/// uses.
pub(crate) const MCP_CONFIG_FILE: &str = ".mcp.json";
const MCP_SERVER_NAME: &str = "api";

fn install(a: &mut App, inv: &Invocation) -> CliResult {
    let dir = inv.args.first().map_or(".", String::as_str);
    let into = inv.flags.strings("to");
    let files = if into.is_empty() {
        vec![MCP_CONFIG_FILE.to_string()]
    } else {
        into
    };
    let server_args = mcp_server_args(dir, &a.g.dir, &a.g.env)?;
    let done = write_mcp_config(dir, &files, &server_args, true)?;
    a.print_written(&done)
}

/// The argument list the registered entry runs apic with: mcp, then
/// --dir with the project as a path relative to the directory the
/// configuration lives in (where the agent starts) when the two differ,
/// in forward slashes so the committed file works on every platform, then
/// --env when one is set.
pub(crate) fn mcp_server_args(
    config_dir: &str,
    project_dir: &str,
    env: &str,
) -> Result<Vec<String>, CliError> {
    let mut args = vec!["mcp".to_string()];
    let abs = |p: &str| fp::abs(p).map_err(|e| fail(Code::File, apic_gocompat::io_error_text(&e)));
    let abs_config = abs(config_dir)?;
    let abs_project = abs(project_dir)?;
    // Another drive: only an absolute path reaches it.
    let rel = fp::rel(&abs_config, &abs_project).unwrap_or(abs_project);
    if rel != "." {
        args.push("--dir".to_string());
        args.push(fp::to_slash(&rel));
    }
    if !env.is_empty() {
        args.push("--env".to_string());
        args.push(env.to_string());
    }
    Ok(args)
}

/// Puts the server entry into each of files under dir, merging with what
/// a file holds: its other servers and top-level keys stay and the apic
/// entry is set. A file already holding the same entry is reported as
/// unchanged; one holding a different apic entry is overwritten, or with
/// overwrite false kept and reported as skipped, as `apic init` keeps
/// every file a user may have tailored. An empty file is an empty
/// document; one that is not a JSON object, or whose mcpServers is not
/// one, is an error rather than overwritten. Every path is confined to
/// dir, as the skill's are to the project.
pub(crate) fn write_mcp_config(
    dir: &str,
    files: &[String],
    server_args: &[String],
    overwrite: bool,
) -> Result<Written, CliError> {
    let abs_dir = write_root(
        dir,
        false,
        "the configuration is written into an existing project (apic init creates one)",
    )?;
    let entry = json!({"command": "apic", "args": server_args});
    let mut out = Written::default();
    for name in files {
        let (shown, target) = confined_path(dir, &abs_dir, name, "the configuration")?;
        let mut doc: Map<String, Value> = Map::new();
        match std::fs::read(&target) {
            Ok(existing) if !existing.trim_ascii().is_empty() => {
                doc = read_object(&existing).map_err(|why| {
                    fail(Code::File, format!("{shown} is not a JSON object: {why}"))
                })?;
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let why = apic_gocompat::path_error("open", Path::new(&target), &e);
                return Err(fail(Code::File, format!("{shown}: {why}")));
            }
        }
        let mut servers = match doc.remove("mcpServers") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(servers)) => servers,
            Some(_) => {
                return Err(fail(
                    Code::File,
                    format!("{shown}: mcpServers is not an object, so the entry has nowhere to go; nothing was written"),
                ))
            }
        };
        if servers
            .get(MCP_SERVER_NAME)
            .is_some_and(|have| plain_json(have) == plain_json(&entry))
        {
            out.unchanged.push(shown);
            continue;
        }
        if servers.contains_key(MCP_SERVER_NAME) && !overwrite {
            out.skipped.push(shown);
            continue;
        }
        servers.insert(MCP_SERVER_NAME.to_string(), entry.clone());
        doc.insert("mcpServers".to_string(), Value::Object(servers));
        let text = apic_gocompat::indent_json(&plain_json(&Value::Object(doc)), "", "  ") + "\n";
        apic_gocompat::mkdir_all(Path::new(&fp::dir(&target))).map_err(|e| fail(Code::File, e))?;
        write_file(&target, text.as_bytes(), 0o644).map_err(|e| fail(Code::File, e))?;
        out.written.push(shown);
    }
    Ok(out)
}

/// Reads a JSON object; the error says why the text is not one.
fn read_object(data: &[u8]) -> Result<Map<String, Value>, String> {
    apic_gocompat::check_json(data)?;
    let kind = match serde_json::from_slice::<Value>(data).map_err(|e| e.to_string())? {
        Value::Object(doc) => return Ok(doc),
        Value::Null => return Err("it holds null".to_string()),
        Value::Array(_) => "array",
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
    };
    Err(format!(
        "json: cannot unmarshal {kind} into Go value of type map[string]interface {{}}"
    ))
}
