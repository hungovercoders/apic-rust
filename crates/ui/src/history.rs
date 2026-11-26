//! The history tab.

use std::fmt::Write as _;
use std::path::Path;

use apic_history::{Change, Entry, Store};

use crate::ui::Model;

/// The history tab's content for one request and environment, read from
/// disk once and kept until a run, a reload or another request or
/// environment changes what it would show.
#[derive(Clone, Debug, Default)]
pub(crate) struct HistView {
    key: String,
    entries: Vec<Entry>,
    /// The indexes of the two entries compared: the older, then the newer.
    from: usize,
    to: usize,
    changes: Vec<Change>,
    err: Option<String>,
}

impl Model {
    /// Returns the selected request's history, reading it when the cached
    /// view is stale.
    fn history_view(&mut self) -> HistView {
        let Some(sel) = self.selected.clone() else {
            return HistView::default();
        };
        let cache = format!("{}|{}|{}", sel.id(), self.env, self.hist_seq);
        if let Some(hist) = &self.hist {
            if hist.key == cache {
                return hist.clone();
            }
        }
        let v = self.read_history(cache, &sel);
        self.hist = Some(v.clone());
        v
    }

    fn read_history(&self, cache: String, sel: &apic_httpfile::Request) -> HistView {
        let p = &self.runner.project;
        let s = Store::new(Path::new(&p.root), p.config.history);
        let mut v = HistView {
            key: cache,
            ..HistView::default()
        };
        if sel.name.is_empty() {
            return v;
        }
        let key = apic_runner::history_key(p, sel);
        match s.list(&self.env, &key) {
            Ok(entries) => v.entries = entries,
            Err(e) => v.err = Some(e.to_string()),
        }
        if v.err.is_some() || v.entries.len() < 2 {
            return v;
        }
        let from = match s.get(&self.env, &key, 2) {
            Ok(e) => e,
            Err(e) => {
                v.err = Some(e.to_string());
                return v;
            }
        };
        let to = match s.get(&self.env, &key, 1) {
            Ok(e) => e,
            Err(e) => {
                v.err = Some(e.to_string());
                return v;
            }
        };
        v.from = from.index;
        v.to = to.index;
        match apic_history::compare(
            from.result.as_deref().unwrap_or(""),
            to.result.as_deref().unwrap_or(""),
        ) {
            Ok(changes) => v.changes = changes,
            Err(e) => v.err = Some(e),
        }
        v
    }

    pub(crate) fn render_history(&mut self, width: i32) -> String {
        let t = self.theme.clone();
        let p = self.runner.project.clone();
        let Some(sel) = self.selected.clone() else {
            return String::new();
        };
        let mut b = String::new();
        let mut head = format!("history · {}", self.env_label());
        if p.config.history > 0 {
            head.push_str(
                &t.dim
                    .render(&format!(" · keeps the last {}", p.config.history)),
            );
        }
        let _ = writeln!(b, "{}", t.bold.render(&head));
        if sel.name.is_empty() {
            let _ = writeln!(
                b,
                "{}",
                t.dim
                    .render("history keeps named requests only · give this one a # @name")
            );
            return b;
        }
        let v = self.history_view();
        if let Some(err) = &v.err {
            let _ = writeln!(b, "{}{err}", t.fail.render("✗ "));
            return b;
        }
        if self.runner.opts.no_session {
            let _ = writeln!(
                b,
                "{}",
                t.dim.render("--no-session: this run records nothing")
            );
        } else if p.config.history <= 0 {
            let _ = writeln!(
                b,
                "{}",
                t.dim.render(
                    "history is off · set history: 20 in apic.yaml to keep the last 20 responses"
                )
            );
        }
        if v.entries.is_empty() {
            let _ = writeln!(
                b,
                "{}",
                t.dim
                    .render("nothing recorded yet · press enter to send it")
            );
            return b;
        }
        b.push_str(&apic_output::history_entries(&t, &v.entries));
        if v.entries.len() < 2 {
            let _ = writeln!(b, "\n{}", t.dim.render("run it again to see what changes"));
            return b;
        }
        let _ = writeln!(
            b,
            "\n{}{}",
            t.bold.render("what changed"),
            t.dim.render(&format!(" · #{} → #{}", v.from, v.to))
        );
        if v.changes.is_empty() {
            let _ = writeln!(b, "{}", t.dim.render("no changes in status or body"));
            return b;
        }
        b.push_str(&apic_output::changes(
            &t,
            &v.changes,
            (width - 4).max(0) as usize,
        ));
        let _ = writeln!(
            b,
            "{}",
            t.dim.render(&format!(
                "apic history diff {} for the same in a terminal",
                apic_runner::history_key(&p, &sel)
            ))
        );
        b
    }
}
