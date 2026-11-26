use std::path::Path;

use apic_httpfile::KNOWN_DIRECTIVES;

/// Keeps the VS Code extension's highlighting in step with the parser: the
/// directives its grammar marks as known must be exactly
/// `KNOWN_DIRECTIVES`, so the editor never shows a directive as valid that
/// `apic validate` reports as unknown, or the reverse.
#[test]
fn grammar_matches_known_directives() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../editors/vscode/syntaxes/apic-directives.injection.json");
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("skipped: no extension grammar at {}: {err}", path.display());
            return;
        }
    };
    let grammar: serde_json::Value = serde_json::from_str(&data).unwrap();
    let pattern = grammar["patterns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["name"] == "meta.directive.apic")
        .filter_map(|p| p["match"].as_str())
        .next_back()
        .unwrap_or("");
    assert!(
        !pattern.is_empty(),
        "grammar has no meta.directive.apic pattern"
    );
    // The pattern lists the directives as `(@(?:a|b|c))`.
    let start = pattern
        .find("(@(?:")
        .unwrap_or_else(|| panic!("cannot find the directive alternation in {pattern:?}"));
    let rest = &pattern[start + "(@(?:".len()..];
    let end = rest
        .find("))")
        .filter(|end| !rest[..*end].contains(')'))
        .unwrap_or_else(|| panic!("cannot find the directive alternation in {pattern:?}"));
    let mut got: Vec<&str> = rest[..end].split('|').collect();
    got.sort_unstable();
    let mut want: Vec<&str> = KNOWN_DIRECTIVES.iter().map(|(k, _)| *k).collect();
    want.sort_unstable();
    assert_eq!(
        got,
        want,
        "the extension grammar lists {got:?}\nbut the parser knows {want:?}\nkeep {} in step with KNOWN_DIRECTIVES",
        path.display()
    );
}
