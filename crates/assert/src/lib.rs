//! Parses and evaluates `# @assert` expressions: `<selector> <op> <value>`,
//! `<selector> exists` / `not exists`, a predicate such as
//! `<selector> isInteger` or `not isEmpty`, `<selector> length <op> <n>`
//! and `<selector> matchesSchema <file>`.
//!
//! A resolved schema is shared as an `Arc<Resolved>`: it is immutable once
//! built, and every assertion naming the same file validates against the
//! same one.

#![forbid(unsafe_code)]

mod assert;
mod schema;

pub use apic_selector::Number;
pub use assert::{
    eval, eval_with, parse, parse_number, redact, schemas, Expr, Options, Result, SchemaLoader, OPERATORS, PREDICATES,
};
pub use schema::Resolved;

#[cfg(test)]
mod assert_tests;
#[cfg(test)]
mod schema_tests;
