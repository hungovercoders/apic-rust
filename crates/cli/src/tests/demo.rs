use super::execute;

#[test]
fn demo_rejects_zero_port() {
    let (code, _, stderr) = execute(&["demo", "--port", "0"]);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(stderr.contains("--port must be between 1 and 65535"), "unexpected stderr: {stderr}");
}
