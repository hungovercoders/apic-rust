use regex::Regex;

use super::execute;

/// The order `apic fmt --help` lists is the order the formatter writes,
/// so a directive added to the formatter cannot go missing from the help.
#[test]
fn fmt_help_lists_the_formatter_order() {
    let (_, out, _) = execute(&["fmt", "--help"]);
    let re =
        Regex::new(r"(?s)directives in a fixed order \((.*?), then the rest as written\)").unwrap();
    let m = re
        .captures(&out)
        .unwrap_or_else(|| panic!("fmt help has no directive order:\n{out}"));
    let got = m[1].split_whitespace().collect::<Vec<_>>().join(" ");
    let want = apic_httpfile::directive_order().join(", ");
    assert_eq!(got, want, "fmt help order");
}
