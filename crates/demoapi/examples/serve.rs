fn main() {
    let port = std::env::args().nth(1).unwrap();
    let listener = apic_httpd::Listener::bind(&format!("127.0.0.1:{port}")).unwrap();
    apic_httpd::serve(
        listener,
        apic_demoapi::new(),
        &apic_httpd::CancelToken::new(),
    );
}
