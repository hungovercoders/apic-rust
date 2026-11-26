//! apic import: scaffold .http files from an OpenAPI 3 document, a
//! Postman collection or a curl command.

use std::collections::HashSet;

use apic_project::path as fp;
use apic_runner::Code;
use serde::Serialize;

use crate::io::{w, wln};
use crate::root::{fail, App, CliResult, Invocation};
use crate::shared::{read_file, write_file};
use crate::spec::{ArgRule, CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new(
        "import <openapi.yaml|openapi.json|collection.postman.json> | --curl <command>",
        "Scaffold .http files from an OpenAPI 3 document, a Postman collection or a curl command",
    )
    .long(
        "Generate .http files from an OpenAPI 3 document (one file per tag, a named
request per operation, example bodies from the schemas, an env file with the
server URL) or from a Postman v2.1 collection (folders become files, requests
become named requests, variables become env files, and the simple pm.test
checks become # @assert and # @capture lines). The format is detected from
the file. Existing files are left alone unless --force is given.

--curl turns one curl command into a request block: printed, or appended
to the file --into names (created if needed). --curl - reads the command
from stdin, so a copied command can be piped in.",
    )
    .example(
        r#"  apic import openapi.yaml -o api --env-name dev
  apic import collection.postman.json -o api --postman-env staging.postman_environment.json
  apic import --curl 'curl -X POST https://api.example.com/todos -H "Content-Type: application/json" -d "{}"' --into todos.http --name create-todo
  pbpaste | apic import --curl - --into todos.http"#,
    )
    .args(ArgRule::Max(1))
    .flag(FlagSpec::string("out", ".", "directory to write .http files into").short('o'))
    .flag(FlagSpec::string(
        "env-name",
        "dev",
        "environment name for the generated http-client.env.json (OpenAPI)",
    ))
    .flag(FlagSpec::string_array(
        "postman-env",
        "Postman environment export to turn into an environment (repeatable)",
    ))
    .flag(FlagSpec::bool("force", "overwrite existing files"))
    .flag(FlagSpec::string("curl", "", "a curl command to turn into a request (- reads stdin)"))
    .flag(FlagSpec::string(
        "into",
        "",
        "append the request to this .http file (relative to the project root), creating it if needed",
    ))
    .flag(FlagSpec::string("name", "", "request name (default: from the method and path)"))
    .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let f = &inv.flags;
    let (out, env_name, curl, into, name) = (
        f.string("out"),
        f.string("env-name"),
        f.string("curl"),
        f.string("into"),
        f.string("name"),
    );
    let force = f.bool("force");
    let postman_envs = f.strings("postman-env");
    let args = &inv.args;
    if !curl.is_empty() {
        if !args.is_empty() {
            return Err(fail(Code::Flag, "--curl takes the command, not a file"));
        }
        return a.import_curl(curl, &into, name);
    }
    if args.len() != 1 {
        return Err(fail(Code::Flag, "import needs a file to read, or --curl"));
    }
    if !into.is_empty() || !name.is_empty() {
        return Err(fail(Code::Flag, "--into and --name go with --curl"));
    }
    let data = read_file(&args[0]).map_err(|e| fail(Code::File, e))?;
    if apic_postman::looks_like_collection(&data) {
        let res = apic_postman::import(
            &args[0],
            apic_postman::Options {
                out_dir: out,
                env_files: postman_envs,
                force,
            },
        )
        .map_err(|e| fail(Code::Import, e.to_string()))?;
        if a.g.json {
            return a.write_json(&res);
        }
        a.print_import(
            &res.files,
            &res.skipped,
            &res.env_file,
            &res.base_url,
            res.requests,
        );
        for u in &res.unsupported {
            wln!(a.stdout, "note  {}: {}: {}", u.request, u.what, u.reason);
        }
        return Ok(());
    }
    if apic_postman::looks_like_environment(&data) {
        return Err(fail(
            Code::Flag,
            format!(
                "{} is a Postman environment; pass the collection and give environments with --postman-env",
                args[0]
            ),
        ));
    }
    if !postman_envs.is_empty() {
        return Err(fail(
            Code::Flag,
            "--postman-env goes with a Postman collection, not an OpenAPI document",
        ));
    }
    let res = apic_openapi::import(
        &args[0],
        apic_openapi::Options {
            out_dir: out,
            env_name,
            force,
        },
    )
    .map_err(|e| fail(Code::Import, e.to_string()))?;
    if a.g.json {
        return a.write_json(&res);
    }
    a.print_import(
        &res.files,
        &res.skipped,
        &res.env_file,
        &res.base_url,
        res.requests,
    );
    Ok(())
}

impl App {
    /// Turns a curl command into a request block.
    fn import_curl(&mut self, mut command: String, into: &str, mut name: String) -> CliResult {
        if command == "-" {
            let data = self.stdin.read_all().map_err(|e| {
                fail(
                    Code::File,
                    format!(
                        "reading the command from stdin: {}",
                        apic_gocompat::io_error_text(&e)
                    ),
                )
            })?;
            command = String::from_utf8_lossy(&data).into_owned();
        }
        let mut req = apic_curlimport::parse(&command)
            .map_err(|e| fail(Code::Import, format!("curl: {e}")))?;
        // The project's baseUrl, when there is one, replaces the host.
        let mut base_url = String::new();
        if let Ok(r) = self.new_runner() {
            for v in r.env_vars() {
                if v.name == "baseUrl" && !v.missing {
                    base_url = v.value;
                }
            }
        }
        if name.is_empty() {
            name = apic_curlimport::name(&req);
        }
        let mut target = String::new();
        let mut existing = String::new();
        if !into.is_empty() {
            target = if fp::is_abs(into) {
                into.to_string()
            } else {
                fp::join(&self.g.dir, into)
            };
            match std::fs::read(&target) {
                Ok(data) => existing = String::from_utf8_lossy(&data).into_owned(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(fail(
                        Code::Import,
                        apic_gocompat::path_error("open", std::path::Path::new(&target), &e),
                    ))
                }
            }
            // A name already in the file gets a suffix rather than a
            // duplicate.
            let (f, _) = apic_httpfile::parse(into, &existing);
            let taken: HashSet<&str> = f.requests.iter().map(|r| r.name.as_str()).collect();
            let base = name.clone();
            let mut i = 2;
            while taken.contains(name.as_str()) {
                name = format!("{base}-{i}");
                i += 1;
            }
        }
        if !req.body.is_empty() && apic_curlimport::splits_block(&req.body) {
            // The parser would read this body as a new block or a file
            // reference, so it goes into a side file the request points at.
            if target.is_empty() {
                req.warnings.push("the body starts a new block or looks like a file reference as written; use --into so it can go into a side file".to_string());
            } else {
                let side = format!("{name}.body.txt");
                let side_path = fp::join(&fp::dir(&target), &side);
                write_file(&side_path, req.body.as_bytes(), 0o644)
                    .map_err(|e| fail(Code::Import, e))?;
                req.body = String::new();
                req.body_file = format!("./{side}");
                wln!(self.stdout, "wrote {}", fp::join(&fp::dir(into), &side));
            }
        }
        let block = apic_curlimport::render(&req, &name, &base_url);
        if !target.is_empty() {
            let text = if existing.is_empty() {
                block.clone()
            } else {
                format!("{}\n\n{block}", existing.trim_end_matches('\n'))
            };
            write_file(&target, text.as_bytes(), 0o644).map_err(|e| fail(Code::Import, e))?;
        }
        if self.g.json {
            #[derive(Serialize)]
            struct Out<'a> {
                #[serde(skip_serializing_if = "str::is_empty")]
                file: &'a str,
                name: &'a str,
                request: &'a str,
                warnings: &'a [String],
            }
            return self.write_json(&Out {
                file: into,
                name: &name,
                request: &block,
                warnings: &req.warnings,
            });
        }
        if target.is_empty() {
            w!(self.stdout, "{block}");
        } else {
            wln!(self.stdout, "added {name} to {into}");
        }
        for warning in &req.warnings {
            wln!(self.stderr, "note: {warning}");
        }
        Ok(())
    }

    fn print_import(
        &self,
        files: &[String],
        skipped: &[String],
        env_file: &str,
        base_url: &str,
        requests: usize,
    ) {
        for f in files {
            if f == env_file {
                continue;
            }
            wln!(self.stdout, "wrote {f}");
        }
        for f in skipped {
            wln!(self.stdout, "kept  {f} (use --force to overwrite)");
        }
        if !env_file.is_empty() {
            if base_url.is_empty() {
                wln!(self.stdout, "wrote {env_file}");
            } else {
                wln!(self.stdout, "wrote {env_file} (baseUrl = {base_url})");
            }
        }
        wln!(
            self.stdout,
            "{requests} request(s) generated; run `apic list` to see them"
        );
    }
}
