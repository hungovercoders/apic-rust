//! The language server on stdin and stdout, as `apic lsp` runs it, for
//! comparing sessions against another build.
//!
//! ```text
//! cargo run -p apic-lsp --example lsp_stdio -- [--env NAME] [--root DIR] [--version V]
//! ```

use std::time::Duration;

use apic_lsp::{serve, Error, Options};
use apic_output::style;

fn main() {
    let mut opts = Options {
        version: "dev".to_string(),
        debounce: Duration::from_millis(150),
        ..Options::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--env" => opts.env = value,
            "--root" => opts.root = value,
            "--version" => opts.version = value,
            other => {
                eprintln!("unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    // The protocol owns stdout; reports written to the client's log carry
    // no colour codes.
    style::set_color_profile(style::Profile::Ascii);
    apic_runner::set_version(&opts.version);
    let cancel = apic_httpc::CancelToken::new();
    match serve(&cancel, std::io::stdin(), std::io::stdout(), opts) {
        Ok(()) | Err(Error::Cancelled) => {}
        Err(Error::NoShutdown) => {
            eprintln!("{}", Error::NoShutdown);
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    }
}
