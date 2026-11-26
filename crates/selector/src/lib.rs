//! Extracts values from an HTTP response using the small selector language
//! shared by `# @capture` and `# @assert`:
//!
//! ```text
//! status                 response status code
//! statusText             e.g. "OK"
//! header.<name>          first value of a response header (case-insensitive)
//! header.<name>.#        how many values it has; header.<name>[1] the second
//! cookie.<name>          value of a cookie the response set (Set-Cookie)
//! body                   raw body
//! body.$                 whole body as JSON
//! body.$.<path>          JSONPath: body.$.items[0].id, body.$..id,
//!                        body.$.items[?(@.done == true)].id, [-1], [1:3],
//!                        .# and .length; see the path module
//! duration               round-trip time in milliseconds
//! ```
//!
//! The response is plain data ([`Response`], with [`Headers`]): the crate
//! knows nothing of how the request was sent.

#![forbid(unsafe_code)]

mod json;
mod number;
mod path;
mod regexp;
mod selector;

pub use number::Number;
pub use path::{leading, parse_path, Path};
pub use regexp::compile_regex;
pub use selector::{check, select, select_value, Error, Headers, Kind, Response, Value};

#[cfg(test)]
mod path_tests;
#[cfg(test)]
mod selector_tests;
