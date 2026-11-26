//! Code lenses, formatting and the commands the lenses run.

use std::sync::Arc;

use apic_httpfile::Request;
use apic_output::Theme;
use apic_runner::{Description, Runner, MASKED};
use serde_json::{json, Value};

use crate::complete::{body_key, json_body};
use crate::jsonrpc::{to_json, RpcError, CODE_INVALID_PARAMS};
use crate::protocol::{
    uri_to_path, CodeLens, Command, ExecuteCommandParams, Node, Position, Range, ShowMessageParams,
    TextEdit, MESSAGE_ERROR, MESSAGE_INFO, MESSAGE_LOG, MESSAGE_WARNING,
};
use crate::server::{
    file_of, target, Handled, Reply, Server, COMMAND_CURL, COMMAND_DESCRIBE, COMMAND_RUN,
    COMMAND_VALIDATE,
};

impl Server {
    /// Puts Run, Describe and curl above every request, as commands the
    /// server itself executes, so a client with no apic-specific UI still
    /// gets them. `None` for a document that is not a file.
    pub(crate) fn code_lenses(&self, uri: &str) -> Option<Vec<CodeLens>> {
        let path = uri_to_path(uri)?;
        let mut st = self.lock();
        let ps = st.project_for(&path);
        let mut out = Vec::new();
        let Some(f) = file_of(&ps, &path) else {
            return Some(out);
        };
        for r in &f.requests {
            let line = r.line as i64 - 1;
            let at = Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 0 },
            };
            let args = vec![json!(uri), json!(target(r))];
            let mut run = "▶ Run".to_string();
            if !r.name.is_empty() {
                run.push(' ');
                run.push_str(&r.name);
            }
            for (title, command) in [
                (run.as_str(), COMMAND_RUN),
                ("Describe", COMMAND_DESCRIBE),
                ("curl", COMMAND_CURL),
            ] {
                out.push(CodeLens {
                    range: at,
                    command: Some(Command {
                        title: title.to_string(),
                        command: command.to_string(),
                        arguments: args.clone(),
                    }),
                });
            }
        }
        Some(out)
    }

    /// Rewrites a document in apic fmt's canonical form, as one edit over
    /// the whole text; no edits when it is already canonical.
    pub(crate) fn format(&self, uri: &str) -> Option<Vec<TextEdit>> {
        let path = uri_to_path(uri)?;
        let st = self.lock();
        let text = st.text(&path);
        let formatted = apic_httpfile::format(&text);
        if formatted == text {
            return Some(Vec::new());
        }
        Some(vec![TextEdit {
            range: Range {
                start: Position::default(),
                end: st.units.end_of(&text),
            },
            new_text: formatted,
        }])
    }

    /// Runs a code lens command. Run sends the request, on its own thread
    /// so the editor keeps its completions meanwhile, and answers with
    /// the result `apic run --json` prints; Describe answers with what
    /// `apic describe --json` prints, and curl with the command. Each
    /// also shows a one-line summary and writes the full text report to
    /// the client's log.
    pub(crate) fn execute(self: &Arc<Self>, id: Option<&str>, p: &ExecuteCommandParams) -> Handled {
        if p.command == COMMAND_VALIDATE {
            self.validate_workspace();
            return Ok(Reply::Null);
        }
        let invalid = |msg: String| RpcError::new(CODE_INVALID_PARAMS, msg);
        if p.arguments.len() < 2 {
            return Err(invalid(format!(
                "{} takes a document URI and a request target",
                p.command
            )));
        }
        let text = |v: &Node| match v.as_str() {
            Some(s) => Some(s.to_string()),
            None if v.is_null() => Some(String::new()),
            None => None,
        };
        let (Some(uri), Some(tgt)) = (text(&p.arguments[0]), text(&p.arguments[1])) else {
            return Err(invalid(format!(
                "{} takes a document URI and a request target, both strings",
                p.command
            )));
        };
        let Some(path) = uri_to_path(&uri) else {
            return Err(invalid(format!("not a file URI: {uri}")));
        };
        if ![COMMAND_RUN, COMMAND_DESCRIBE, COMMAND_CURL].contains(&p.command.as_str()) {
            return Err(invalid(format!("unknown command {}", p.command)));
        }
        self.settle(&uri);
        let (ps, env) = {
            let mut st = self.lock();
            let ps = st.project_for(&path);
            let env = st.env_for(&ps.root);
            (ps, env)
        };
        if let Some(err) = &ps.err {
            return Err(RpcError::failed(err));
        }
        let Some(project) = &ps.p else {
            return Err(RpcError::failed(""));
        };
        let reqs = project.resolve(&tgt).map_err(RpcError::failed)?;
        if reqs.len() != 1 {
            return Err(RpcError::failed(format!(
                "{tgt} names {} requests",
                reqs.len()
            )));
        }
        let req = &reqs[0];
        match p.command.as_str() {
            COMMAND_DESCRIBE => {
                let r = self.lock().runner_for(&ps).map_err(RpcError::failed)?;
                let mut d = r.describe(req);
                for v in &mut d.variables {
                    if v.secret {
                        v.value = MASKED.to_string();
                    }
                }
                self.log(&apic_output::describe(&Theme::default(), &d, &req.headers));
                self.show(MESSAGE_INFO, &describe_summary(req, &d));
                return Ok(Reply::of(&d));
            }
            COMMAND_CURL => {
                let r = self.lock().runner_for(&ps).map_err(RpcError::failed)?;
                let d = r.describe(req);
                if !d.ready {
                    return Err(RpcError::failed(describe_summary(req, &d)));
                }
                let resolved = r.resolve(req).map_err(RpcError::failed)?;
                // Redacted, as `apic curl --redact` prints it: credentials
                // become shell placeholders and other values are masked,
                // since the command lands in a notification and in the
                // editor's log file. `apic curl <target>` in a terminal
                // prints it with the values.
                let code =
                    apic_snippet::render("curl", &resolved, true).map_err(RpcError::failed)?;
                self.log(&code);
                self.show(MESSAGE_INFO, &code);
                return Ok(Reply::of(&code));
            }
            _ => {}
        }
        // The run itself: a real runner, with the session and the history,
        // as `apic run` has. It refuses a project with errors, as the CLI
        // does.
        let opts = apic_runner::Options {
            env,
            ..apic_runner::Options::default()
        };
        let r = Runner::new(Arc::clone(project), opts).map_err(RpcError::failed)?;
        self.start_run(id, Arc::new(r), Arc::clone(req), ps.root.clone());
        Ok(Reply::Deferred)
    }

    /// Sends the request on its own thread and answers the executeCommand
    /// request `id` when it is done.
    fn start_run(
        self: &Arc<Self>,
        id: Option<&str>,
        r: Arc<Runner>,
        req: Arc<Request>,
        root: String,
    ) {
        let s = Arc::clone(self);
        let id = id.map(str::to_string);
        self.spawn(move || {
            let (res, err) = r.run(&s.run_cancel, &req);
            if let Some(err) = err {
                s.show(MESSAGE_ERROR, &format!("✗ {}: {err}", req.id()));
                let _ = s.conn.reply(id.as_deref(), Err(RpcError::failed(err)));
                return;
            }
            let res = res.unwrap_or_default();
            s.remember(&r, &root, &res);
            {
                // The run changed the session: describe and complete afresh.
                let prefix = format!("{root}\0");
                s.lock().runners.retain(|k, _| !k.starts_with(&prefix));
            }
            let mut b: Vec<u8> = Vec::new();
            apic_output::human(&mut b, &res, false);
            s.log(&String::from_utf8_lossy(&b));
            let kind = if res.ok {
                MESSAGE_INFO
            } else {
                MESSAGE_WARNING
            };
            s.show(kind, &run_summary(&res));
            let _ = s.conn.reply(id.as_deref(), Ok(Some(to_json(&res))));
        });
    }

    /// Keeps the JSON bodies a run received, for body.$ completion, the
    /// request's and those its # @ref ran first.
    fn remember(&self, r: &Runner, root: &str, res: &apic_runner::Result) {
        fn walk(
            bodies: &mut std::collections::HashMap<String, Value>,
            r: &Runner,
            root: &str,
            res: &apic_runner::Result,
        ) {
            if let (Some(req), Some(response)) = (res.req(), &res.response) {
                if !req.name.is_empty() {
                    if let Some(body) = json_body(&response.body) {
                        let key = apic_runner::history_key(&r.project, req);
                        bodies.insert(body_key(root, &r.opts.env, &key), body);
                    }
                }
            }
            for d in &res.deps {
                walk(bodies, r, root, d);
            }
        }
        walk(&mut self.lock().bodies, r, root, res);
    }

    /// Puts a message in front of the user.
    pub(crate) fn show(&self, kind: i64, msg: &str) {
        let _ = self.conn.notify(
            "window/showMessage",
            &ShowMessageParams {
                kind,
                message: msg.to_string(),
            },
        );
    }

    /// Writes to the client's output for the server.
    pub(crate) fn log(&self, msg: &str) {
        let _ = self.conn.notify(
            "window/logMessage",
            &ShowMessageParams {
                kind: MESSAGE_LOG,
                message: msg.trim_end_matches('\n').to_string(),
            },
        );
    }
}

/// The one line a run leaves in the editor: "✓ login · 200 OK · 87 ms",
/// or what failed.
fn run_summary(res: &apic_runner::Result) -> String {
    let mut name = res.request.name.clone();
    if name.is_empty() {
        name = format!("{}:{}", res.request.file, res.request.line);
    }
    let mark = if res.ok { "✓" } else { "✗" };
    let mut line = format!("{mark} {name}");
    if let Some(r) = &res.response {
        line.push_str(&format!(
            " · {} {} · {} ms",
            r.status, r.status_text, r.duration_ms
        ));
    }
    if !res.ok {
        for a in res.display_asserts() {
            if !a.pass {
                return format!("{line} · {}", a.expr);
            }
        }
        if let Some(e) = res.errors.first() {
            return format!("{line} · {e}");
        }
    }
    line
}

fn describe_summary(req: &Request, d: &Description) -> String {
    if d.ready {
        return format!("{}: ready to send", req.id());
    }
    let missing: Vec<&str> = d
        .variables
        .iter()
        .filter(|v| v.missing)
        .map(|v| v.name.as_str())
        .collect();
    format!("{}: missing {}", req.id(), missing.join(", "))
}
