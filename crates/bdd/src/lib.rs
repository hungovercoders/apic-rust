//! Runs Gherkin feature files against a project's .http requests with a
//! fixed step vocabulary; the Gherkin reader and the scenario runner are
//! part of the crate.
//!
//! [`run`] takes [`Options`] (a [`Config`] plus what to run and how to
//! report it) and returns an exit code with the typed error, if any, that
//! decided it. [`run_summary`] runs with the cucumber report in memory and
//! returns its [`Summary`]. A run is cancelled through the
//! [`apic_httpc::CancelToken`] it is given. The project is shared as an
//! `Arc<Project>`; report and note destinations are [`Output`] handles
//! (`Arc<Mutex<dyn Write + Send>>`), so a caller keeps its own handle on
//! what it passes in.

#![forbid(unsafe_code)]

mod colors;
mod engine;
mod fmt;
mod gherkin;
mod jsonmatch;
mod mask;
mod phrases;
mod state;
mod steps;
mod suite;
mod summary;
mod tags;

pub use phrases::phrases;
pub use state::{Config, Output};
pub use steps::{VocabularyEntry, VOCABULARY};
pub use suite::{
    feature_files, run, run_summary, Feature, Options, EXIT_FAILED, EXIT_PASSED, EXIT_TRANSPORT,
    EXIT_USAGE, FORMATS,
};
pub use summary::{summarize, Failure, Summary};

#[cfg(test)]
mod tests;
