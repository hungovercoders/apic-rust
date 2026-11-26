fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cfg = apic_mcp::Config {
        dir: args[1].clone(),
        env: args[2].clone(),
        version: "dev".to_string(),
    };
    let cancel = apic_httpc::CancelToken::new();
    let result = if args.len() > 3 {
        apic_mcp::serve_http(
            &cancel,
            cfg,
            &args[3],
            args.get(4).map_or("", String::as_str),
            Some(&|a| eprintln!("listening {a}")),
        )
    } else {
        apic_mcp::serve(&cancel, cfg)
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
}
