//! The o key: opening the selected request in the user's editor.

use std::process::Command;

use crate::program::{Cmd, ExecMsg, Msg};
use crate::ui::Model;

/// Builds the command that opens `file` at `line` in `editor`. Editors
/// that understand +line get it; VS Code and its relatives get -g
/// file:line, and --wait unless the user already passes it, since they
/// otherwise return at once and the reload that follows sees no edit.
pub(crate) fn editor_command(editor: &str, file: &str, line: usize) -> Result<Command, String> {
    let parts: Vec<&str> = editor.split_whitespace().collect();
    let Some(program) = parts.first() else {
        return Err("set $EDITOR (or $VISUAL) to open files".to_string());
    };
    let mut args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
    let base = apic_project::path::base(program);
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    match base {
        "code" | "code-insiders" | "codium" | "cursor" => {
            if !args.iter().any(|a| a == "--wait" || a == "-w") {
                args.push("--wait".to_string());
            }
            args.push("-g".to_string());
            args.push(format!("{file}:{line}"));
        }
        "notepad" => args.push(file.to_string()),
        _ => {
            args.push(format!("+{line}"));
            args.push(file.to_string());
        }
    }
    let mut cmd = Command::new(program);
    cmd.args(args);
    Ok(cmd)
}

impl Model {
    /// Suspends the UI, opens the selected request's file at its line,
    /// and reloads the project when the editor exits.
    pub(crate) fn open_editor(&mut self) -> Option<Cmd> {
        let sel = self.selected_req()?;
        if self.inflight.is_some() {
            return self.set_status("busy: wait for the run to finish");
        }
        let path = apic_project::path::join(&self.runner.project.root, &sel.file.path);
        let cmd = match editor_command(&self.cfg.editor, &path, sel.line) {
            Ok(cmd) => cmd,
            Err(err) => return self.set_status(&err),
        };
        Some(Box::new(move || {
            Some(Msg::Exec(ExecMsg {
                cmd,
                done: Some(Box::new(Msg::EditorDone)),
            }))
        }))
    }
}
