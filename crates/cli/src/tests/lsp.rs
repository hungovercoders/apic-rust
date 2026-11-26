use apic_httpc::CancelToken;

use super::{execute_with, s, tmp};

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

#[test]
fn lsp_command_serves_over_stdio() {
    let dir = tmp();
    let input = frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":null,"capabilities":{}}}"#)
        + &frame(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#)
        + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#);
    let (code, out, stderr) = execute_with(&CancelToken::new(), Some(&input), &["-C", &s(&dir), "lsp"]);
    assert_eq!(code, 0, "exit {code}: {stderr}");
    assert!(
        out.contains(r#""name":"apic""#)
            && out.contains(r#""hoverProvider":true"#)
            && out.contains(r#""id":2,"result":null"#),
        "stdout:\n{out}"
    );
}

#[test]
fn lsp_command_fails_without_shutdown() {
    let input = frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    let (code, _, stderr) = execute_with(&CancelToken::new(), Some(&input), &["lsp"]);
    assert!(code == 1 && stderr.contains("without a shutdown"), "exit {code}: {stderr}");
}
