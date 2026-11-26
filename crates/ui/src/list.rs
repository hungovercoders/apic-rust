//! The left pane: the rows, the cursor, the filter, and how a row is
//! drawn.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use apic_httpfile::{Request, RequestKey};
use apic_output::style;

use crate::ui::Model;

/// One row of the left pane: a file heading (`req` is `None`) or a
/// request.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub(crate) file: String,
    pub(crate) req: Option<Arc<Request>>,
}

pub(crate) fn build_items(reqs: &[Arc<Request>]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut last = "";
    for r in reqs {
        if r.file.path != last {
            items.push(Item {
                file: r.file.path.clone(),
                req: None,
            });
            last = &r.file.path;
        }
        items.push(Item {
            file: r.file.path.clone(),
            req: Some(r.clone()),
        });
    }
    items
}

impl Item {
    fn matches(&self, filter: &str) -> bool {
        if filter.is_empty() {
            return true;
        }
        let f = filter.to_lowercase();
        let Some(r) = &self.req else {
            return false;
        };
        r.id().to_lowercase().contains(&f)
            || r.url.to_lowercase().contains(&f)
            || r.description.to_lowercase().contains(&f)
            || r.method.to_lowercase().contains(&f)
            || r.file.path.to_lowercase().contains(&f)
    }

    fn is(&self, req: &Arc<Request>) -> bool {
        self.req.as_ref().is_some_and(|r| Arc::ptr_eq(r, req))
    }
}

/// Blank columns between a row's text and its right-hand badge.
const ROW_GAP: i32 = 2;
/// The narrowest description budget worth spending a row on.
const MIN_DESC: i32 = 10;
/// The method column fits DELETE; OPTIONS widens it for the whole list.
const MIN_METH: usize = 6;

/// Counts how the requests of one file have fared so far.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Rollup {
    total: usize,
    pass: usize,
    fail: usize,
}

fn width(s: &str) -> i32 {
    style::string_width(s) as i32
}

/// Pads left so that `right` ends at `width`, and drops `right` when the
/// two cannot both fit.
pub(crate) fn align_right(left: String, right: &str, total: i32) -> String {
    if right.is_empty() {
        return left;
    }
    let gap = total - width(&left) - width(right);
    if gap < 1 {
        return left;
    }
    format!("{left}{}{right}", " ".repeat(gap as usize))
}

impl Model {
    /// Returns the rows after filtering; file headings stay only when a
    /// request under them matches.
    pub(crate) fn visible(&self) -> Vec<Item> {
        if self.filter.is_empty() {
            return self.items.clone();
        }
        let mut out = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            if it.req.is_none() {
                let any = self.items[i + 1..]
                    .iter()
                    .take_while(|sub| sub.req.is_some())
                    .any(|sub| sub.matches(&self.filter));
                if any {
                    out.push(it.clone());
                }
                continue;
            }
            if it.matches(&self.filter) {
                out.push(it.clone());
            }
        }
        out
    }

    pub(crate) fn selected_req(&self) -> Option<Arc<Request>> {
        self.visible()
            .get(self.cursor)
            .and_then(|it| it.req.clone())
    }

    /// Keeps the cursor on a request row.
    pub(crate) fn clamp_cursor(&mut self) {
        let v = self.visible();
        if v.is_empty() {
            self.cursor = 0;
            return;
        }
        if self.cursor >= v.len() {
            self.cursor = v.len() - 1;
        }
        while self.cursor < v.len() && v[self.cursor].req.is_none() {
            self.cursor += 1;
        }
        if self.cursor >= v.len() {
            self.cursor = v.len() - 1;
            while self.cursor > 0 && v[self.cursor].req.is_none() {
                self.cursor -= 1;
            }
        }
    }

    pub(crate) fn move_by(&mut self, delta: isize) {
        let v = self.visible();
        let mut i = self.cursor as isize;
        loop {
            i += delta;
            if i < 0 || i >= v.len() as isize {
                return;
            }
            if let Some(req) = &v[i as usize].req {
                self.cursor = i as usize;
                self.selected = Some(req.clone());
                return;
            }
        }
    }

    pub(crate) fn move_to(&mut self, first: bool) {
        let v = self.visible();
        if first {
            self.cursor = 0;
            self.clamp_cursor();
        } else {
            self.cursor = v.len().saturating_sub(1);
            while self.cursor > 0 && v[self.cursor].req.is_none() {
                self.cursor -= 1;
            }
        }
        self.selected = self.selected_req();
    }

    /// Puts the cursor on `req` when the list shows it, and in every case
    /// keeps the pane on the request under the cursor: the cursor is the
    /// one source of what is selected, so enter, o and the right pane
    /// never disagree.
    pub(crate) fn focus(&mut self, req: &Arc<Request>) {
        if let Some(i) = self.visible().iter().position(|it| it.is(req)) {
            self.cursor = i;
        }
        self.selected = self.selected_req();
    }

    /// Reports whether the filter leaves every one of `reqs` on screen.
    pub(crate) fn all_visible(&self, reqs: &[Arc<Request>]) -> bool {
        let shown: HashSet<RequestKey> = self
            .visible()
            .iter()
            .filter_map(|it| it.req.as_ref().map(RequestKey::new))
            .collect();
        reqs.iter().all(|r| shown.contains(&RequestKey::new(r)))
    }

    /// Returns every request in the file of the selected request.
    pub(crate) fn file_requests(&self) -> Vec<Arc<Request>> {
        let Some(sel) = self.selected_req() else {
            return Vec::new();
        };
        self.items
            .iter()
            .filter(|it| it.file == sel.file.path)
            .filter_map(|it| it.req.clone())
            .filter(|r| !r.disabled())
            .collect()
    }

    pub(crate) fn all_requests(&self) -> Vec<Arc<Request>> {
        self.items
            .iter()
            .filter_map(|it| it.req.clone())
            .filter(|r| !r.disabled())
            .collect()
    }

    /// Draws the left pane: a title or the filter prompt, then a window
    /// of rows around the cursor.
    pub(crate) fn render_list(&self, width_: i32, height: i32) -> String {
        let t = &self.theme;
        let v = self.visible();
        let count = v.iter().filter(|it| it.req.is_some()).count();
        let mut title = t.bold.render(&format!("requests ({count})"));
        if self.filtering {
            title = format!(
                "{}{}{}",
                t.accent.render("/"),
                self.filter,
                t.accent.render("▏")
            );
        } else if !self.filter.is_empty() {
            title.push_str(&t.dim.render(&format!(" /{}", self.filter)));
        }
        let rows = (height - 1).max(1) as usize;
        let start = if self.cursor >= rows {
            self.cursor - rows + 1
        } else {
            0
        };
        if !self.filtering {
            title = align_right(
                style::truncate(&title, width_, "…"),
                &self.render_hidden(start, start + rows, v.len()),
                width_,
            );
        }
        let mut lines = vec![style::truncate(&title, width_, "…")];
        if v.is_empty() {
            if self.items.is_empty() {
                // Nothing to browse at all: say what gives this directory
                // some requests, the way apic list does.
                for l in [
                    "  no .http files here",
                    "  apic init scaffolds a project;",
                    "  apic ui --demo shows a working one",
                ] {
                    lines.push(t.dim.render(l));
                }
            } else {
                lines.push(t.dim.render("  no requests match"));
            }
        }
        let rolls = self.rollups(&v);
        for (i, it) in v.iter().enumerate().skip(start).take(rows) {
            lines.push(style::truncate(
                &self.render_row(it, i == self.cursor, width_, &rolls),
                width_,
                "…",
            ));
        }
        while (lines.len() as i32) < height {
            lines.push(String::new());
        }
        for line in &mut lines {
            let w = width(line);
            if w < width_ {
                line.push_str(&" ".repeat((width_ - w) as usize));
            }
        }
        lines.join("\n")
    }

    /// Counts the rows above and below the window, so a list taller than
    /// the pane says so the way the tab strip does for the pane.
    fn render_hidden(&self, start: usize, end: usize, total: usize) -> String {
        let mut parts = Vec::new();
        if start > 0 {
            parts.push(format!("↑{start}"));
        }
        if end < total {
            parts.push(format!("↓{}", total - end));
        }
        if parts.is_empty() {
            return String::new();
        }
        self.theme.dim.render(&parts.join(" "))
    }

    /// Draws one row: a file heading with its rollup, or a request with
    /// its mark, method, id, description and, once it has run, the status
    /// and how long it took, pushed to the right edge.
    fn render_row(
        &self,
        it: &Item,
        cursor: bool,
        width_: i32,
        rolls: &HashMap<String, Rollup>,
    ) -> String {
        let t = &self.theme;
        let Some(r) = &it.req else {
            let roll = rolls.get(&it.file).copied().unwrap_or_default();
            return align_right(t.accent.render(&it.file), &self.render_rollup(roll), width_);
        };
        let prefix = if cursor {
            t.accent.render("▸ ")
        } else {
            "  ".to_string()
        };
        let mut id = r.id();
        if cursor {
            id = t.bold.render(&id);
        } else if r.disabled() {
            // Kept out of runs of the file and of everything; r still sends it.
            id = t.dim.render(&id);
        }
        let method = format!("{:<w$}", r.method, w = self.method_width());
        let mut head = format!("{prefix}{} {} {id}", self.row_mark(r), t.method(&method));

        let (badge, room) = self.row_layout(r, width(&head), width_);
        if !r.description.is_empty() && room >= MIN_DESC {
            // Truncate adds an ellipsis of its own, and the description is
            // preceded by a space, so two columns of the budget are spoken for.
            head.push(' ');
            head.push_str(
                &t.dim
                    .render(&apic_output::truncate(&r.description, (room - 2) as usize)),
            );
        }
        align_right(head, &badge, width_)
    }

    /// The method column: wide enough for the longest method in the
    /// project, so an OPTIONS request does not push its id out of line.
    fn method_width(&self) -> usize {
        self.items
            .iter()
            .filter_map(|it| it.req.as_ref())
            .map(|r| r.method.len())
            .fold(MIN_METH, usize::max)
    }

    /// Decides how much of a row survives its width. A description that
    /// would be cut to a stub is worth less than the timing beside it, and
    /// both are worth less than the status code, so the row sheds them in
    /// that order. It returns the badge to draw and the columns left for
    /// everything between the id and it.
    fn row_layout(&self, r: &Arc<Request>, head_w: i32, width_: i32) -> (String, i32) {
        let desc = if r.description.is_empty() {
            0
        } else {
            MIN_DESC
        };
        let full = self.row_badge(r, false);
        let short = self.row_badge(r, true);
        for (badge, desc) in [
            (full.clone(), desc),
            (full, 0),
            (short, 0),
            (String::new(), 0),
        ] {
            let room = width_ - head_w - width(&badge) - ROW_GAP;
            if room >= desc {
                return (badge, room);
            }
        }
        (String::new(), width_ - head_w)
    }

    fn is_current(&self, r: &Arc<Request>) -> bool {
        self.inflight
            .as_ref()
            .is_some_and(|rs| Arc::ptr_eq(rs.current(), r))
    }

    /// The outcome of a request once it has run: the status code and its
    /// round trip, or "error" when no response came back at all. The
    /// short form drops the timing, for a pane with no room for it.
    fn row_badge(&self, r: &Arc<Request>, short: bool) -> String {
        let t = &self.theme;
        if self.is_current(r) {
            return String::new(); // the spinner in the mark says everything
        }
        let key = RequestKey::new(r);
        let res = self.results.get(&key);
        let Some(response) = res.and_then(|res| res.response.as_ref()) else {
            if self.errs.contains_key(&key) || res.is_some() {
                return t.fail.render(if short { "err" } else { "error" });
            }
            return String::new();
        };
        let code = t.status_code(response.status);
        if short {
            return code;
        }
        let timing = format!("{}ms", response.duration_ms);
        format!(
            "{code} {}",
            t.dim
                .render(&format!("{timing:>w$}", w = self.timing_width()))
        )
    }

    /// The timing column: as wide as the slowest round trip in the list,
    /// so the status codes to its left stay in one column whatever each
    /// request took.
    fn timing_width(&self) -> usize {
        self.results
            .values()
            .filter_map(|res| res.response.as_ref())
            .map(|response| format!("{}ms", response.duration_ms).len())
            .max()
            .unwrap_or(0)
    }

    /// Tallies the visible rows per file, so a heading can summarise the
    /// same requests the filter left on screen.
    fn rollups(&self, v: &[Item]) -> HashMap<String, Rollup> {
        let mut out: HashMap<String, Rollup> = HashMap::new();
        for it in v {
            let Some(req) = &it.req else {
                continue;
            };
            let key = RequestKey::new(req);
            let r = out.entry(it.file.clone()).or_default();
            r.total += 1;
            if self.errs.contains_key(&key) {
                r.fail += 1;
            } else if let Some(res) = self.results.get(&key) {
                if res.ok {
                    r.pass += 1;
                } else {
                    r.fail += 1;
                }
            }
        }
        out
    }

    /// The right-hand side of a file heading: how many requests it holds,
    /// and how many have passed or failed once any of them has run.
    fn render_rollup(&self, r: Rollup) -> String {
        let t = &self.theme;
        if r.total == 0 {
            return String::new();
        }
        if r.pass == 0 && r.fail == 0 {
            return t.dim.render(&r.total.to_string());
        }
        let mut parts = Vec::new();
        if r.pass > 0 {
            parts.push(t.ok.render(&format!("✓{}", r.pass)));
        }
        if r.fail > 0 {
            parts.push(t.fail.render(&format!("✗{}", r.fail)));
        }
        let rest = r.total - r.pass - r.fail;
        if rest > 0 {
            parts.push(t.dim.render(&format!("·{rest}")));
        }
        parts.join(" ")
    }

    /// The one-character state of a request: running, passed, failed,
    /// disabled, ready or not ready.
    fn row_mark(&self, r: &Arc<Request>) -> String {
        let t = &self.theme;
        if self.is_current(r) {
            return t.accent.render(self.spin.view());
        }
        let key = RequestKey::new(r);
        if self.errs.contains_key(&key) {
            return t.fail.render("✗");
        }
        if let Some(res) = self.results.get(&key) {
            return t.mark(res.ok);
        }
        if r.disabled() {
            return t.dim.render("-");
        }
        if self.descs.get(&key).is_some_and(|d| !d.ready) {
            return t.warn.render("○");
        }
        t.dim.render("●")
    }
}
