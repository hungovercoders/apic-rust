//! Small helpers that reproduce the text formats apic's output contract was
//! first written against: durations such as `1.5s` and `2m0s`, quoted
//! strings such as `"a\tb"`, and floating-point numbers in their shortest
//! form. They live in one crate so every package prints the same thing.
//!
//! Add a new helper in its own module file and re-export it here.

mod duration;
mod float;
mod quote;

pub use duration::{format_duration, parse_duration};
pub use float::{format_float_g, format_float_json};
pub use quote::quote;
