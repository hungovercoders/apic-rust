use std::sync::{Arc, Mutex};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // difftest <dir> <format> <tags> <stop> <redact> [paths...]
    let project = apic_project::load(&args[1]).expect("project");
    let mut cfg = apic_bdd::Config::new(project.clone());
    cfg.env = if project.config.env.is_empty() {
        String::new()
    } else {
        project.config.env.clone()
    };
    cfg.redact = args[5] == "1";
    let mut opts = apic_bdd::Options::new(cfg);
    opts.format = args[2].clone();
    opts.tags = args[3].clone();
    opts.stop_on_failure = args[4] == "1";
    opts.no_colors = std::env::var("COLORS").is_err();
    opts.paths = args[6..].to_vec();
    opts.output = Some(Arc::new(Mutex::new(std::io::stdout())));
    let (code, err) = apic_bdd::run(&apic_httpc::CancelToken::new(), &opts);
    if let Some(e) = err {
        eprintln!("error: {e}");
    }
    std::process::exit(code);
}
