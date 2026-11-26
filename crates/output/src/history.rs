use std::fmt::Write as _;

use apic_gocompat::{format_time, Time};
use apic_history::{Change, Entry, ADDED, REMOVED};

use crate::render::{size, truncate};
use crate::theme::Theme;

/// How history entries show their time: local, to the second.
pub fn history_time(at: Time) -> String {
    format_time(apic_runner::local_time(at), "", "2006-01-02 15:04:05")
}

/// Renders a request's history, newest first: one row per entry with its
/// number, time, status, duration and size.
pub fn history_entries(t: &Theme, entries: &[Entry]) -> String {
    let mut b = String::new();
    for e in entries {
        let mark = if e.ok {
            t.ok.render("✓")
        } else {
            t.fail.render("✗")
        };
        let _ = writeln!(
            b,
            "  {} {}  {}  {} {} {} {} {}",
            t.dim.render(&format!("#{:<2}", e.index)),
            history_time(e.time),
            mark,
            t.status(e.status, &e.status_text),
            t.dim.render("·"),
            t.latency(e.duration_ms),
            t.dim.render("·"),
            t.dim.render(&size(usize::try_from(e.size).unwrap_or(0))),
        );
    }
    b
}

/// Names one entry: "#2 (2026-10-06 10:00:01, 200)".
pub fn history_entry(e: &Entry) -> String {
    format!("#{} ({}, {})", e.index, history_time(e.time), e.status)
}

/// Renders the differences between two responses, one per line: ~ for a
/// changed value, + for an added one, - for a removed one. Values are cut
/// at `width`.
pub fn changes(t: &Theme, changes: &[Change], width: usize) -> String {
    let width = if width == 0 { 80 } else { width };
    let mut b = String::new();
    for c in changes {
        let from = truncate(c.from.as_deref().unwrap_or(""), width);
        let to = truncate(c.to.as_deref().unwrap_or(""), width);
        if c.op == ADDED {
            let _ = writeln!(b, "{} {}: {}", t.ok.render("+"), c.path, to);
        } else if c.op == REMOVED {
            let _ = writeln!(b, "{} {}: {}", t.fail.render("-"), c.path, from);
        } else if from.is_empty() && to.is_empty() {
            let _ = writeln!(
                b,
                "{} {} {}",
                t.warn.render("~"),
                c.path,
                t.dim.render("differs")
            );
        } else {
            let _ = writeln!(
                b,
                "{} {}: {} {} {}",
                t.warn.render("~"),
                c.path,
                from,
                t.dim.render("→"),
                to
            );
        }
    }
    b
}
