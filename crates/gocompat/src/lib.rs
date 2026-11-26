//! The text formats of apic's output contract: durations such as `1.5s`
//! and `2m0s`, quoted strings such as `"a\tb"`, and floating-point numbers in their shortest
//! form. They live in one crate so every package prints the same thing.
//!
//! Add a new helper in its own module file and re-export it here.

mod duration;
mod flag;
mod float;
mod ioerror;
mod json;
mod jsontext;
mod mkdirall;
mod quote;
mod time;
mod timeformat;

pub use duration::{format_duration, parse_duration};
pub use flag::{FlagError, FlagSet};
pub use float::{format_float_g, format_float_json};
pub use ioerror::{io_error_text, path_error};
pub use json::{escape_html, escape_line_separators};
pub use jsontext::{check_json, compact_json, indent_json, json_string};
pub use mkdirall::mkdir_all;
pub use quote::quote;
pub use time::{days_in_month, Civil, Time};
pub use timeformat::{format_time, RFC1123, RFC3339};
