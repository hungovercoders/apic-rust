use super::*;

fn counter() -> impl FnMut() -> String {
    let mut next = 0;
    move || {
        let id = next.to_string();
        next += 1;
        id
    }
}

fn parse_ok(src: &str) -> (Document, Vec<Pickle>) {
    let mut new_id = counter();
    let doc = parse(src.as_bytes(), &mut new_id).expect("a valid feature");
    let pickles = pickles(&doc, "x.feature", &mut new_id);
    (doc, pickles)
}

const SAMPLE: &str = r#"# a comment
@feat
Feature: Sample
  Lines of
    description.

  Background: setup
    Given a start

  @one @two # trailing
  Scenario: Plain
    When a table:
      | a  | b\|c |
      | \n | d\\  |
    Then a doc:
      """json
        indented
      \"\"\"
      """

  Scenario Outline: Row <n>
    When I use <n>

    @ex
    Examples: named
      | n |
      | 1 |
      | 2 |
"#;

#[test]
fn document_and_ids() {
    let (doc, pickles) = parse_ok(SAMPLE);
    assert_eq!(doc.comments.len(), 1);
    assert_eq!(
        (doc.comments[0].line, doc.comments[0].text.as_str()),
        (1, "# a comment")
    );
    let feature = doc.feature.as_ref().unwrap();
    assert_eq!(feature.name, "Sample");
    assert_eq!(feature.description, "  Lines of\n    description.");
    // Ids are drawn as nodes are completed: steps, then their owner, the
    // feature's own tags last.
    let FeatureChild::Background(bg) = &feature.children[0] else {
        panic!("a background first");
    };
    assert_eq!(bg.steps[0].id, "0");
    let FeatureChild::Scenario(plain) = &feature.children[1] else {
        panic!("a scenario");
    };
    let table = plain.steps[0].data_table.as_ref().unwrap();
    assert_eq!(table.rows[0].cells, ["a", "b|c"]);
    assert_eq!(table.rows[1].cells, ["\n", "d\\"]);
    assert_eq!(table.rows[0].id, "2");
    assert_eq!(plain.steps[0].id, "4");
    let doc_string = plain.steps[1].doc_string.as_ref().unwrap();
    assert_eq!(doc_string.media_type, "json");
    assert_eq!(doc_string.content, "  indented\n\"\"\"");
    assert_eq!(plain.steps[1].id, "5");
    let tags: Vec<&str> = plain.tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tags, ["@one", "@two"]);
    assert_eq!(plain.id, "8");
    let FeatureChild::Scenario(outline) = &feature.children[2] else {
        panic!("an outline");
    };
    assert_eq!(outline.keyword, "Scenario Outline");
    assert_eq!(outline.examples[0].name, "named");
    assert_eq!(outline.examples[0].table_body.len(), 2);

    // Pickles: the background's steps first, outlines once per row.
    assert_eq!(pickles.len(), 3);
    assert_eq!(pickles[0].steps.len(), 3);
    assert_eq!(pickles[0].steps[0].id, "17");
    assert_eq!(pickles[0].id, "20");
    assert_eq!(pickles[0].tags, ["@feat", "@one", "@two"]);
    assert_eq!(pickles[1].name, "Row 1");
    assert_eq!(pickles[1].steps[1].text, "I use 1");
    assert_eq!(pickles[2].tags, ["@feat", "@ex"]);
    assert_eq!(pickles[2].ast_node_ids.len(), 2);
}

#[test]
fn other_languages_and_rules() {
    let (doc, pickles) = parse_ok(
        "# language: fr\nFonctionnalité: F\n  Règle: R\n    Contexte:\n      Soit a\n    Scénario: S\n      Quand b\n      * c\n",
    );
    let feature = doc.feature.as_ref().unwrap();
    assert_eq!(feature.keyword, "Fonctionnalité");
    assert_eq!(pickles.len(), 1);
    let texts: Vec<&str> = pickles[0].steps.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["a", "b", "c"]);
    let scenario = doc.find_scenario(&pickles[0].ast_node_ids[0]).unwrap();
    assert!(doc.find_rule(&scenario.id).is_some());
    assert_eq!(doc.find_background(&scenario.id).unwrap().steps.len(), 1);
    assert_eq!(
        doc.find_step(&pickles[0].steps[2].ast_node_id)
            .unwrap()
            .keyword,
        "* "
    );
}

#[test]
fn empty_documents() {
    let (doc, pickles) = parse_ok("");
    assert!(doc.feature.is_none() && pickles.is_empty());
    let (doc, pickles) = parse_ok("Feature: only\n\n  Scenario: empty\n");
    assert_eq!(doc.feature.unwrap().children.len(), 1);
    assert!(pickles[0].steps.is_empty());
}

#[test]
fn errors_carry_positions() {
    let errors = |src: &str| {
        parse(src.as_bytes(), &mut String::new)
            .expect_err("a broken feature")
            .to_string()
    };
    assert_eq!(
        errors("Feature: x\n  Scenario: s\n    Given a\nFeature: y\n"),
        "Parser errors:\n(4:1): expected: #EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty, got 'Feature: y'"
    );
    assert_eq!(
        errors("Feature: x\n  Scenario: s\n    Given a\n  Garbage here\n    | a | b |\n    | c |\n@tag with space\n  Scenario: t\n"),
        "Parser errors:\n(4:3): expected: #EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty, got '  Garbage here'\n(6:5): inconsistent cell count within the table\n(7:1): A tag may not contain whitespace"
    );
    assert_eq!(
        errors("# language: zz\nFeature: y\n"),
        "Parser errors:\n(1:1): Language not supported: zz"
    );
    assert_eq!(
        errors("Feature: x\n  @lonely\n"),
        "Parser errors:\n(3:0): unexpected end of file, expected: #TagLine, #RuleLine, #Comment, #Empty"
    );
}
