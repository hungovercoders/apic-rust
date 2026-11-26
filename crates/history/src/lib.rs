//! Keeps the last few responses of each named request, per environment, so
//! "what did this return yesterday?" has an answer. It is off unless
//! apic.yaml sets `history: N`. Entries live under
//! `.apic/history/<env>/<request>/`, one JSON file each, beside the session
//! and under the same rules: the directory ignores itself and the files
//! are 0600, since a response may hold data nobody wants in a commit.
//!
//! Results travel as JSON text (`&str` in, `String` out), kept exactly as
//! the run printed them.

#![forbid(unsafe_code)]

mod diff;
mod history;

pub use apic_gocompat::Time;
pub use diff::{compare, Change, ADDED, CHANGED, REMOVED};
pub use history::{Entry, Error, RangeError, Recorded, Store, DEFAULT_ENV, DIR};

#[cfg(test)]
mod history_tests;
