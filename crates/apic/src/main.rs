//! Command apic runs .http request files for humans and AI agents.

fn main() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    std::process::exit(apic_cli::main(&args));
}
