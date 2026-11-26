//! Reads Gherkin feature files: the document tree with the line of every
//! node, and the pickles (one runnable scenario each, outlines expanded and
//! backgrounds prepended) compiled from it.
//!
//! Every node that a report refers to carries an id drawn from a counter
//! the caller supplies. The order the ids are handed out in is part of the
//! behaviour: reports list step results in id order.

mod dialects;
mod parser;
mod pickles;
mod table;

pub(crate) use parser::{parse, ParseErrors};
pub(crate) use pickles::pickles;

/// A parsed feature file.
#[derive(Debug, Default, Clone)]
pub(crate) struct Document {
    pub(crate) feature: Option<Feature>,
    pub(crate) comments: Vec<Comment>,
}

/// A `#` line anywhere in the file.
#[derive(Debug, Clone)]
pub(crate) struct Comment {
    pub(crate) line: usize,
    pub(crate) text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Tag {
    pub(crate) line: usize,
    pub(crate) name: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Feature {
    pub(crate) line: usize,
    pub(crate) tags: Vec<Tag>,
    pub(crate) keyword: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) children: Vec<FeatureChild>,
}

#[derive(Debug, Clone)]
pub(crate) enum FeatureChild {
    Background(Background),
    Scenario(Scenario),
    Rule(Rule),
}

#[derive(Debug, Clone)]
pub(crate) struct Rule {
    pub(crate) tags: Vec<Tag>,
    pub(crate) children: Vec<RuleChild>,
}

#[derive(Debug, Clone)]
pub(crate) enum RuleChild {
    Background(Background),
    Scenario(Scenario),
}

#[derive(Debug, Clone)]
pub(crate) struct Background {
    pub(crate) keyword: String,
    pub(crate) name: String,
    pub(crate) steps: Vec<Step>,
}

#[derive(Debug, Clone)]
pub(crate) struct Scenario {
    pub(crate) id: String,
    pub(crate) line: usize,
    pub(crate) tags: Vec<Tag>,
    pub(crate) keyword: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) steps: Vec<Step>,
    pub(crate) examples: Vec<Examples>,
}

#[derive(Debug, Clone)]
pub(crate) struct Examples {
    pub(crate) line: usize,
    pub(crate) tags: Vec<Tag>,
    pub(crate) keyword: String,
    pub(crate) name: String,
    pub(crate) table_header: Option<TableRow>,
    pub(crate) table_body: Vec<TableRow>,
}

#[derive(Debug, Clone)]
pub(crate) struct TableRow {
    pub(crate) id: String,
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) cells: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Step {
    pub(crate) id: String,
    pub(crate) line: usize,
    /// As written, with its trailing blank: `Given `.
    pub(crate) keyword: String,
    pub(crate) text: String,
    pub(crate) data_table: Option<DataTable>,
    pub(crate) doc_string: Option<DocString>,
}

#[derive(Debug, Clone)]
pub(crate) struct DataTable {
    pub(crate) rows: Vec<TableRow>,
}

#[derive(Debug, Clone)]
pub(crate) struct DocString {
    pub(crate) line: usize,
    /// What follows the opening delimiter on its line, as written.
    pub(crate) media_type: String,
    pub(crate) content: String,
    pub(crate) delimiter: String,
}

/// One runnable scenario: a plain scenario, or one row of an outline.
#[derive(Debug, Clone)]
pub(crate) struct Pickle {
    pub(crate) id: String,
    pub(crate) uri: String,
    pub(crate) name: String,
    pub(crate) steps: Vec<PickleStep>,
    /// Tag names with their `@`, inherited ones first.
    pub(crate) tags: Vec<String>,
    /// The scenario's id, then the example row's for an outline.
    pub(crate) ast_node_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PickleStep {
    pub(crate) id: String,
    pub(crate) text: String,
    /// The step's id in the document.
    pub(crate) ast_node_id: String,
    pub(crate) argument: Option<PickleArgument>,
}

/// What follows a step: a table, a doc string, or (rarely) both.
#[derive(Debug, Clone, Default)]
pub(crate) struct PickleArgument {
    pub(crate) doc_string: Option<PickleDocString>,
    pub(crate) data_table: Option<Table>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PickleDocString {
    pub(crate) media_type: String,
    pub(crate) content: String,
}

/// A step's data table, cell values only.
#[derive(Debug, Clone, Default)]
pub(crate) struct Table {
    pub(crate) rows: Vec<Vec<String>>,
}

impl Document {
    fn children(&self) -> &[FeatureChild] {
        self.feature.as_ref().map_or(&[], |f| &f.children)
    }

    /// The rule a scenario belongs to, if it is inside one.
    pub(crate) fn find_rule(&self, scenario_id: &str) -> Option<&Rule> {
        self.children().iter().find_map(|child| match child {
            FeatureChild::Rule(rule)
                if rule
                    .children
                    .iter()
                    .any(|c| matches!(c, RuleChild::Scenario(sc) if sc.id == scenario_id)) =>
            {
                Some(rule)
            }
            _ => None,
        })
    }

    /// The scenario with this id, in the feature or in one of its rules.
    pub(crate) fn find_scenario(&self, scenario_id: &str) -> Option<&Scenario> {
        for child in self.children() {
            match child {
                FeatureChild::Scenario(sc) if sc.id == scenario_id => return Some(sc),
                FeatureChild::Rule(rule) => {
                    for c in &rule.children {
                        if let RuleChild::Scenario(sc) = c {
                            if sc.id == scenario_id {
                                return Some(sc);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The background in force for a scenario: the last one declared
    /// before it.
    pub(crate) fn find_background(&self, scenario_id: &str) -> Option<&Background> {
        let mut bg = None;
        for child in self.children() {
            match child {
                FeatureChild::Background(b) => bg = Some(b),
                FeatureChild::Scenario(sc) => {
                    if sc.id == scenario_id {
                        return bg;
                    }
                }
                FeatureChild::Rule(rule) => {
                    for c in &rule.children {
                        match c {
                            RuleChild::Background(b) => bg = Some(b),
                            RuleChild::Scenario(sc) => {
                                if sc.id == scenario_id {
                                    return bg;
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn scenarios(&self) -> impl Iterator<Item = &Scenario> {
        self.children().iter().flat_map(|child| {
            let list: Vec<&Scenario> = match child {
                FeatureChild::Scenario(sc) => vec![sc],
                FeatureChild::Rule(rule) => rule
                    .children
                    .iter()
                    .filter_map(|c| match c {
                        RuleChild::Scenario(sc) => Some(sc),
                        RuleChild::Background(_) => None,
                    })
                    .collect(),
                FeatureChild::Background(_) => Vec::new(),
            };
            list
        })
    }

    /// The examples table and row an outline's pickle was made from.
    pub(crate) fn find_example(&self, row_id: &str) -> Option<(&Examples, &TableRow)> {
        for sc in self.scenarios() {
            for example in &sc.examples {
                for row in &example.table_body {
                    if row.id == row_id {
                        return Some((example, row));
                    }
                }
            }
        }
        None
    }

    /// The step with this id, wherever it is declared.
    pub(crate) fn find_step(&self, step_id: &str) -> Option<&Step> {
        let in_steps = |steps: &'_ [Step]| steps.iter().position(|s| s.id == step_id);
        for child in self.children() {
            match child {
                FeatureChild::Rule(rule) => {
                    for c in &rule.children {
                        let steps = match c {
                            RuleChild::Scenario(sc) => &sc.steps,
                            RuleChild::Background(bg) => &bg.steps,
                        };
                        if let Some(i) = in_steps(steps) {
                            return Some(&steps[i]);
                        }
                    }
                }
                FeatureChild::Scenario(sc) => {
                    if let Some(i) = in_steps(&sc.steps) {
                        return Some(&sc.steps[i]);
                    }
                }
                FeatureChild::Background(bg) => {
                    if let Some(i) = in_steps(&bg.steps) {
                        return Some(&bg.steps[i]);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests;
