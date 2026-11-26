//! The right pane: the tab strip and what each tab shows.

use std::fmt::Write as _;

use apic_httpfile::RequestKey;
use apic_output::style;
use apic_snippet::LANGUAGES;

use crate::ui::{is_cached_token, Model, Tab, TAB_NAMES};

fn width(s: &str) -> i32 {
    style::string_width(s) as i32
}

impl Model {
    /// Draws the tab strip above the viewport, with the scroll position
    /// pushed to the right when the pane holds more than it can show.
    pub(crate) fn render_tabs(&self, width_: i32) -> String {
        let t = &self.theme;
        let mut line = String::new();
        for (i, name) in TAB_NAMES.iter().enumerate() {
            let label = format!(" {} {name} ", i + 1);
            if i == self.tab.index() {
                line.push_str(&t.accent.render(&label));
            } else {
                line.push_str(&t.dim.render(&label));
            }
        }
        let pos = self.scroll_position();
        // The request's name is a reminder, not a control: it shows whole
        // or not at all, never as a stub beside the scroll position.
        if let Some(sel) = &self.selected {
            let name = format!("  {}", t.dim.render(&style::truncate(&sel.id(), 30, "…")));
            let mut room = width_ - width(&line) - width(&name);
            if !pos.is_empty() {
                room -= width(&pos) + 1;
            }
            if room >= 0 {
                line.push_str(&name);
            }
        }
        if pos.is_empty() {
            return style::truncate(&line, width_, "…");
        }
        let pos = t.dim.render(&pos);
        let room = width_ - width(&pos) - 1;
        if room < 1 {
            return style::truncate(&line, width_, "…");
        }
        let line = style::truncate(&line, room, "…");
        let gap = (room - width(&line) + 1) as usize;
        format!("{line}{}{pos}", " ".repeat(gap))
    }

    /// Says where the viewport sits in content taller than the pane, and
    /// nothing at all when everything already fits.
    fn scroll_position(&self) -> String {
        let span = self.vp.lines.len() as i32 - self.vp.height;
        if self.vp.height <= 0 || span <= 0 {
            return String::new();
        }
        if self.vp.offset <= 0 {
            return "top ↓".to_string();
        }
        if self.vp.at_bottom() {
            return "↑ end".to_string();
        }
        format!("↑ {}% ↓", self.vp.offset * 100 / span)
    }

    /// Returns the content of the current tab.
    pub(crate) fn render_pane(&mut self, width_: i32) -> String {
        if self.show_help {
            return self.render_help();
        }
        if self.confirm_clear {
            return self.theme.warn.render(&format!(
                "clear the session for {}? (y/n)",
                apic_gocompat::quote(self.env_label())
            ));
        }
        if self.selected.is_none() && self.tab != Tab::Session {
            return self.theme.dim.render("no request selected");
        }
        match self.tab {
            Tab::Preview => self.render_preview(),
            Tab::Response => self.render_response(width_),
            Tab::Checks => self.render_checks(width_),
            Tab::History => self.render_history(width_),
            Tab::Session => self.render_session(width_),
        }
    }

    pub(crate) fn env_label(&self) -> &str {
        if self.env.is_empty() {
            "default"
        } else {
            &self.env
        }
    }

    fn selected_key(&self) -> Option<RequestKey> {
        self.selected.as_ref().map(RequestKey::new)
    }

    fn render_preview(&self) -> String {
        let (Some(sel), Some(key)) = (&self.selected, self.selected_key()) else {
            return String::new();
        };
        match self.descs.get(&key) {
            Some(d) => apic_output::describe(&self.theme, d, &sel.headers),
            None => self.theme.dim.render("…"),
        }
    }

    /// Draws a labelled divider across the pane: "── request ─────". The
    /// response tab shows what was sent and what came back in one scroll,
    /// and the rules are what tell the two apart.
    fn rule(&self, label: &str, width_: i32) -> String {
        let t = &self.theme;
        let head = format!("{}{} ", t.dim.render("── "), t.bold.render(label));
        let fill = (width_ - width(&head)).max(0) as usize;
        format!("{head}{}\n", t.dim.render(&"─".repeat(fill)))
    }

    fn render_response(&self, width_: i32) -> String {
        let t = &self.theme;
        let Some(key) = self.selected_key() else {
            return String::new();
        };
        let res = self.results.get(&key);
        if let Some(err) = self.errs.get(&key) {
            let mut b = String::new();
            if let Some(res) = res {
                b.push_str(&self.rule("request", width_));
                b.push_str(&apic_output::request_line(t, res));
                b.push_str("\n\n");
                b.push_str(&self.rule("response", width_));
            }
            let _ = writeln!(b, "{}{err}", t.fail.render("✗ "));
            return b;
        }
        let Some(res) = res else {
            return t
                .dim
                .render("not run yet · press enter to send it, f to run the whole file");
        };
        if self.code_lang > 0 {
            let lang = LANGUAGES[self.code_lang - 1];
            let next = if self.code_lang < LANGUAGES.len() {
                format!("c shows {}", LANGUAGES[self.code_lang])
            } else {
                "c hides this".to_string()
            };
            return format!(
                "{}{}\n\n{}\n\n{}",
                t.bold.render(lang),
                t.dim
                    .render(&format!(" · {}/{}", self.code_lang, LANGUAGES.len())),
                self.code_for(lang),
                t.dim.render(&next)
            );
        }
        let mut b = String::new();
        b.push_str(&self.rule("request", width_));
        b.push_str(&apic_output::request_line(t, res));
        b.push('\n');
        if self.show_headers {
            b.push_str(&apic_output::request_detail(t, res));
        }
        b.push('\n');
        b.push_str(&self.rule("response", width_));
        b.push_str(&apic_output::status_line(t, res));
        if res.response.is_none() {
            return b;
        }
        // The timings are one long line: break it between words, where
        // the viewport's own wrap would cut one in half. Each line is
        // styled on its own, so the continuation stays dim.
        let timings = style::strip(&apic_output::timings(t, res));
        let timings = timings.trim();
        if !timings.is_empty() {
            for line in style::wordwrap(timings, width_, "").split('\n') {
                b.push_str(&t.dim.render(line));
                b.push('\n');
            }
        }
        if self.show_headers {
            b.push_str(&apic_output::response_headers(t, res));
        } else {
            b.push_str(&t.dim.render("H shows the headers and the body sent"));
            b.push('\n');
        }
        let body = res.display_raw_body();
        if !body.is_empty() {
            let _ = writeln!(b, "\n{}", apic_output::body_or_summary(t, res, &body));
        }
        let summary = apic_output::checks(t, res, (width_ - 4).max(0) as usize, false);
        if !summary.is_empty() {
            b.push('\n');
            b.push_str(&self.rule("checks", width_));
            b.push_str(&summary);
        }
        b
    }

    /// Renders the selected request as code in `lang`.
    fn code_for(&self, lang: &str) -> String {
        let t = &self.theme;
        if self.inflight.is_some() {
            return t.dim.render("(available once the run finishes)");
        }
        let (Some(r), Some(key)) = (&self.selected, self.selected_key()) else {
            return String::new();
        };
        let resolved = match self.runner.resolve(r) {
            Ok(resolved) => resolved,
            Err(err) => return format!("{}{err}", t.fail.render("✗ ")),
        };
        let code = match apic_snippet::render(lang, &resolved, self.cfg.redact) {
            Ok(code) => code,
            Err(err) => return format!("{}{err}", t.fail.render("✗ ")),
        };
        if self.descs.get(&key).is_some_and(|d| !d.ready) {
            return format!(
                "{}\n\n{code}",
                t.warn
                    .render("some variables are missing; the code below is incomplete")
            );
        }
        code
    }

    fn render_checks(&self, width_: i32) -> String {
        let t = &self.theme;
        let Some(key) = self.selected_key() else {
            return String::new();
        };
        let Some(res) = self.results.get(&key) else {
            if let Some(err) = self.errs.get(&key) {
                return format!("{}{err}", t.fail.render("✗ "));
            }
            let mut b = String::new();
            b.push_str(
                &t.dim
                    .render("not run yet · these are the checks it declares"),
            );
            b.push('\n');
            if let Some(d) = self.descs.get(&key) {
                for a in &d.asserts {
                    let _ = write!(b, "\n  {a}");
                }
                for c in &d.captures {
                    let _ = write!(b, "\n  {} {c}", t.capture.render("↳"));
                }
            }
            return b;
        };
        let mut b = String::new();
        b.push_str(&self.rule("request", width_));
        b.push_str(&apic_output::request_line(t, res));
        b.push_str("\n\n");
        b.push_str(&self.rule("response", width_));
        b.push_str(&apic_output::status_line(t, res));
        b.push('\n');
        let mut checks = apic_output::checks(t, res, (width_ - 4).max(0) as usize, true);
        if checks.is_empty() {
            checks = format!(
                "{}\n",
                t.dim.render("no assertions or captures on this request")
            );
        }
        b.push_str(&self.rule("checks", width_));
        b.push_str(&checks);
        b
    }

    fn render_session(&self, width_: i32) -> String {
        let t = &self.theme;
        let mut b = String::new();
        let _ = writeln!(
            b,
            "{}",
            t.bold.render(&format!("session · {}", self.env_label()))
        );
        if self.runner.session.is_none() {
            let _ = writeln!(b, "{}", t.dim.render("session disabled by --no-session"));
            return b;
        }
        if self.session.is_empty() && self.cookies.is_empty() {
            let _ = writeln!(
                b,
                "{}",
                t.dim
                    .render("nothing captured yet · run a request with # @capture")
            );
            return b;
        }
        let now = (self.now)();
        for (k, v) in &self.session {
            let v = if is_cached_token(k) {
                apic_auth::describe_cached(v, now)
            } else if self.cfg.redact {
                "***".to_string()
            } else {
                let room = width_ - k.len() as i32 - 8;
                apic_output::truncate(v, room.max(0) as usize)
            };
            let _ = writeln!(b, "  {} {k} = {v}", t.capture.render("↳"));
        }
        for c in &self.cookies {
            let _ = writeln!(
                b,
                "  {} cookie {} = *** {}",
                t.capture.render("↳"),
                c.name,
                t.dim.render(&format!(
                    "({}{} · {})",
                    c.domain,
                    c.path,
                    c.expiry_text(now)
                ))
            );
        }
        let _ = writeln!(
            b,
            "\n{}",
            t.dim.render("x clears these values for this environment")
        );
        b
    }

    fn render_help(&self) -> String {
        let t = &self.theme;
        let mut b = String::new();
        let _ = write!(
            b,
            "{}{}\n\n",
            t.accent.render("apic is epic"),
            t.dim.render(" · keys")
        );
        for col in self.keys.columns() {
            for k in col {
                let _ = writeln!(b, "  {:<13} {}", t.bold.render(k.name), k.help);
            }
            b.push('\n');
        }
        b.push_str(&t.dim.render("esc or ? closes this"));
        b
    }
}
