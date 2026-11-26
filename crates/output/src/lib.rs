//! Renders run results for humans (coloured, readable) and for machines
//! (one JSON object per result). The string-returning renderers in
//! `render` are shared with the terminal UI; the functions in `output`
//! are the writer wrappers the CLI uses.
//!
//! [`style`] is the layer underneath: styles, colours and the colour
//! profile, layout, and the functions that measure and cut text carrying
//! escape sequences. The UI and the screenshot generator build on it too.
//!
//! Results are borrowed (`&apic_runner::Result`) and a flow is a slice of
//! them; nothing here keeps one. A [`Theme`] is cheap to build and to
//! clone: make one with `Theme::default()` and pass it by reference.

#![forbid(unsafe_code)]

mod history;
mod jsonhl;
mod output;
mod render;
pub mod style;
mod theme;

pub use history::{changes, history_entries, history_entry, history_time};
pub use jsonhl::highlight_json;
pub use output::{body, deps, human, json, json_nested, summary};
pub use render::{
    attempt, body_or_summary, checks, describe, indent, mask, readiness, render_body,
    request_detail, request_line, response_headers, result, section, size, status_line,
    summary_line, summary_table, timings, truncate, variables, Options,
};
pub use theme::Theme;

#[cfg(test)]
mod output_tests;
