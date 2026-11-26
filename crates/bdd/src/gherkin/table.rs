//! The parser's state table: for each state, the tokens it accepts in the
//! order they are tried, what each does to the tree under construction,
//! and the state that follows.

use super::parser::{
    Action::{Build as B, End as E, Start as S},
    RuleType as R, State, TokenType as K, Transition as T,
};

/// One entry per parser state; state 42 is the end of the document.
#[rustfmt::skip]
pub(super) static STATES: [State; 55] = [
    // 0: Start
    State {
        expected: "#EOF, #Language, #TagLine, #FeatureLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[B], next: 42 },
            T { token: K::Language, lookahead: None, actions: &[S(R::Feature), S(R::FeatureHeader), B], next: 1 },
            T { token: K::TagLine, lookahead: None, actions: &[S(R::Feature), S(R::FeatureHeader), S(R::Tags), B], next: 2 },
            T { token: K::FeatureLine, lookahead: None, actions: &[S(R::Feature), S(R::FeatureHeader), B], next: 3 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 0 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 0 },
        ],
    },
    // 1: GherkinDocument:0>Feature:0>FeatureHeader:0>#Language:0
    State {
        expected: "#TagLine, #FeatureLine, #Comment, #Empty",
        transitions: &[
            T { token: K::TagLine, lookahead: None, actions: &[S(R::Tags), B], next: 2 },
            T { token: K::FeatureLine, lookahead: None, actions: &[B], next: 3 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 1 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 1 },
        ],
    },
    // 2: GherkinDocument:0>Feature:0>FeatureHeader:1>Tags:0>#TagLine:0
    State {
        expected: "#TagLine, #FeatureLine, #Comment, #Empty",
        transitions: &[
            T { token: K::TagLine, lookahead: None, actions: &[B], next: 2 },
            T { token: K::FeatureLine, lookahead: None, actions: &[E, B], next: 3 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 2 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 2 },
        ],
    },
    // 3: GherkinDocument:0>Feature:0>FeatureHeader:2>#FeatureLine:0
    State {
        expected: "#EOF, #Empty, #Comment, #BackgroundLine, #TagLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, B], next: 42 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 3 },
            T { token: K::Comment, lookahead: None, actions: &[S(R::Description), B], next: 4 },
            T { token: K::BackgroundLine, lookahead: None, actions: &[E, S(R::Background), B], next: 5 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[S(R::Description), B], next: 4 },
        ],
    },
    // 4: GherkinDocument:0>Feature:0>FeatureHeader:3>DescriptionHelper:1>Description:0>__alt1:0>#Other:0
    State {
        expected: "#EOF, #Comment, #BackgroundLine, #TagLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, B], next: 42 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 4 },
            T { token: K::BackgroundLine, lookahead: None, actions: &[E, E, S(R::Background), B], next: 5 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 4 },
        ],
    },
    // 5: GherkinDocument:0>Feature:1>Background:0>#BackgroundLine:0
    State {
        expected: "#EOF, #Empty, #Comment, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, B], next: 42 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 5 },
            T { token: K::Comment, lookahead: None, actions: &[S(R::Description), B], next: 6 },
            T { token: K::StepLine, lookahead: None, actions: &[S(R::Step), B], next: 7 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[S(R::Description), B], next: 6 },
        ],
    },
    // 6: GherkinDocument:0>Feature:1>Background:1>DescriptionHelper:1>Description:0>__alt1:0>#Other:0
    State {
        expected: "#EOF, #Comment, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, B], next: 42 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 6 },
            T { token: K::StepLine, lookahead: None, actions: &[E, S(R::Step), B], next: 7 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 6 },
        ],
    },
    // 7: GherkinDocument:0>Feature:1>Background:2>Step:0>#StepLine:0
    State {
        expected: "#EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[S(R::DataTable), B], next: 8 },
            T { token: K::DocStringSeparator, lookahead: None, actions: &[S(R::DocString), B], next: 52 },
            T { token: K::StepLine, lookahead: None, actions: &[E, S(R::Step), B], next: 7 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 7 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 7 },
        ],
    },
    // 8: GherkinDocument:0>Feature:1>Background:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:0>DataTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 8 },
            T { token: K::DocStringSeparator, lookahead: None, actions: &[E, S(R::DocString), B], next: 9 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 7 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 8 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 8 },
        ],
    },
    // 9: GherkinDocument:0>Feature:1>Background:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:1>DocString:0>#DocStringSeparator:0
    State {
        expected: "#DocStringSeparator, #Other",
        transitions: &[
            T { token: K::DocStringSeparator, lookahead: None, actions: &[B], next: 10 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 9 },
        ],
    },
    // 10: GherkinDocument:0>Feature:1>Background:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:1>DocString:2>#DocStringSeparator:0
    State {
        expected: "#EOF, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 7 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 10 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 10 },
        ],
    },
    // 11: GherkinDocument:0>Feature:2>ScenarioDefinition:0>Tags:0>#TagLine:0
    State {
        expected: "#TagLine, #ScenarioLine, #Comment, #Empty",
        transitions: &[
            T { token: K::TagLine, lookahead: None, actions: &[B], next: 11 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, S(R::Scenario), B], next: 12 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 11 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 11 },
        ],
    },
    // 12: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:0>#ScenarioLine:0
    State {
        expected: "#EOF, #Empty, #Comment, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, B], next: 42 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 12 },
            T { token: K::Comment, lookahead: None, actions: &[S(R::Description), B], next: 13 },
            T { token: K::StepLine, lookahead: None, actions: &[S(R::Step), B], next: 14 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[S(R::Description), B], next: 13 },
        ],
    },
    // 13: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:1>DescriptionHelper:1>Description:0>__alt1:0>#Other:0
    State {
        expected: "#EOF, #Comment, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 13 },
            T { token: K::StepLine, lookahead: None, actions: &[E, S(R::Step), B], next: 14 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 13 },
        ],
    },
    // 14: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:2>Step:0>#StepLine:0
    State {
        expected: "#EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[S(R::DataTable), B], next: 15 },
            T { token: K::DocStringSeparator, lookahead: None, actions: &[S(R::DocString), B], next: 49 },
            T { token: K::StepLine, lookahead: None, actions: &[E, S(R::Step), B], next: 14 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 14 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 14 },
        ],
    },
    // 15: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:0>DataTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 15 },
            T { token: K::DocStringSeparator, lookahead: None, actions: &[E, S(R::DocString), B], next: 16 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 14 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 15 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 15 },
        ],
    },
    // 16: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:1>DocString:0>#DocStringSeparator:0
    State {
        expected: "#DocStringSeparator, #Other",
        transitions: &[
            T { token: K::DocStringSeparator, lookahead: None, actions: &[B], next: 17 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 16 },
        ],
    },
    // 17: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:1>DocString:2>#DocStringSeparator:0
    State {
        expected: "#EOF, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 14 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 17 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 17 },
        ],
    },
    // 18: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:3>ExamplesDefinition:0>Tags:0>#TagLine:0
    State {
        expected: "#TagLine, #ExamplesLine, #Comment, #Empty",
        transitions: &[
            T { token: K::TagLine, lookahead: None, actions: &[B], next: 18 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, S(R::Examples), B], next: 19 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 18 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 18 },
        ],
    },
    // 19: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:3>ExamplesDefinition:1>Examples:0>#ExamplesLine:0
    State {
        expected: "#EOF, #Empty, #Comment, #TableRow, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 19 },
            T { token: K::Comment, lookahead: None, actions: &[S(R::Description), B], next: 20 },
            T { token: K::TableRow, lookahead: None, actions: &[S(R::ExamplesTable), B], next: 21 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[S(R::Description), B], next: 20 },
        ],
    },
    // 20: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:3>ExamplesDefinition:1>Examples:1>DescriptionHelper:1>Description:0>__alt1:0>#Other:0
    State {
        expected: "#EOF, #Comment, #TableRow, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, B], next: 42 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 20 },
            T { token: K::TableRow, lookahead: None, actions: &[E, S(R::ExamplesTable), B], next: 21 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 20 },
        ],
    },
    // 21: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:3>ExamplesDefinition:1>Examples:2>ExamplesTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 21 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 21 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 21 },
        ],
    },
    // 22: GherkinDocument:0>Feature:3>Rule:0>RuleHeader:0>Tags:0>#TagLine:0
    State {
        expected: "#TagLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::TagLine, lookahead: None, actions: &[B], next: 22 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 22 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 22 },
        ],
    },
    // 23: GherkinDocument:0>Feature:3>Rule:0>RuleHeader:1>#RuleLine:0
    State {
        expected: "#EOF, #Empty, #Comment, #BackgroundLine, #TagLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, B], next: 42 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[S(R::Description), B], next: 24 },
            T { token: K::BackgroundLine, lookahead: None, actions: &[E, S(R::Background), B], next: 25 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[S(R::Description), B], next: 24 },
        ],
    },
    // 24: GherkinDocument:0>Feature:3>Rule:0>RuleHeader:2>DescriptionHelper:1>Description:0>__alt1:0>#Other:0
    State {
        expected: "#EOF, #Comment, #BackgroundLine, #TagLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 24 },
            T { token: K::BackgroundLine, lookahead: None, actions: &[E, E, S(R::Background), B], next: 25 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 24 },
        ],
    },
    // 25: GherkinDocument:0>Feature:3>Rule:1>Background:0>#BackgroundLine:0
    State {
        expected: "#EOF, #Empty, #Comment, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, B], next: 42 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 25 },
            T { token: K::Comment, lookahead: None, actions: &[S(R::Description), B], next: 26 },
            T { token: K::StepLine, lookahead: None, actions: &[S(R::Step), B], next: 27 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[S(R::Description), B], next: 26 },
        ],
    },
    // 26: GherkinDocument:0>Feature:3>Rule:1>Background:1>DescriptionHelper:1>Description:0>__alt1:0>#Other:0
    State {
        expected: "#EOF, #Comment, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 26 },
            T { token: K::StepLine, lookahead: None, actions: &[E, S(R::Step), B], next: 27 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 26 },
        ],
    },
    // 27: GherkinDocument:0>Feature:3>Rule:1>Background:2>Step:0>#StepLine:0
    State {
        expected: "#EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[S(R::DataTable), B], next: 28 },
            T { token: K::DocStringSeparator, lookahead: None, actions: &[S(R::DocString), B], next: 46 },
            T { token: K::StepLine, lookahead: None, actions: &[E, S(R::Step), B], next: 27 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 27 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 27 },
        ],
    },
    // 28: GherkinDocument:0>Feature:3>Rule:1>Background:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:0>DataTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 28 },
            T { token: K::DocStringSeparator, lookahead: None, actions: &[E, S(R::DocString), B], next: 29 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 27 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 28 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 28 },
        ],
    },
    // 29: GherkinDocument:0>Feature:3>Rule:1>Background:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:1>DocString:0>#DocStringSeparator:0
    State {
        expected: "#DocStringSeparator, #Other",
        transitions: &[
            T { token: K::DocStringSeparator, lookahead: None, actions: &[B], next: 30 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 29 },
        ],
    },
    // 30: GherkinDocument:0>Feature:3>Rule:1>Background:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:1>DocString:2>#DocStringSeparator:0
    State {
        expected: "#EOF, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 27 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 30 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 30 },
        ],
    },
    // 31: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:0>Tags:0>#TagLine:0
    State {
        expected: "#TagLine, #ScenarioLine, #Comment, #Empty",
        transitions: &[
            T { token: K::TagLine, lookahead: None, actions: &[B], next: 31 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, S(R::Scenario), B], next: 32 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 31 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 31 },
        ],
    },
    // 32: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:0>#ScenarioLine:0
    State {
        expected: "#EOF, #Empty, #Comment, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 32 },
            T { token: K::Comment, lookahead: None, actions: &[S(R::Description), B], next: 33 },
            T { token: K::StepLine, lookahead: None, actions: &[S(R::Step), B], next: 34 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[S(R::Description), B], next: 33 },
        ],
    },
    // 33: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:1>DescriptionHelper:1>Description:0>__alt1:0>#Other:0
    State {
        expected: "#EOF, #Comment, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 33 },
            T { token: K::StepLine, lookahead: None, actions: &[E, S(R::Step), B], next: 34 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 33 },
        ],
    },
    // 34: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:2>Step:0>#StepLine:0
    State {
        expected: "#EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[S(R::DataTable), B], next: 35 },
            T { token: K::DocStringSeparator, lookahead: None, actions: &[S(R::DocString), B], next: 43 },
            T { token: K::StepLine, lookahead: None, actions: &[E, S(R::Step), B], next: 34 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 34 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 34 },
        ],
    },
    // 35: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:0>DataTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #DocStringSeparator, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 35 },
            T { token: K::DocStringSeparator, lookahead: None, actions: &[E, S(R::DocString), B], next: 36 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 34 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 35 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 35 },
        ],
    },
    // 36: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:1>DocString:0>#DocStringSeparator:0
    State {
        expected: "#DocStringSeparator, #Other",
        transitions: &[
            T { token: K::DocStringSeparator, lookahead: None, actions: &[B], next: 37 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 36 },
        ],
    },
    // 37: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:0>DataTableAndMaybeDocString:1>DocString:2>#DocStringSeparator:0
    State {
        expected: "#EOF, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, B], next: 42 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 34 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 37 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 37 },
        ],
    },
    // 38: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:3>ExamplesDefinition:0>Tags:0>#TagLine:0
    State {
        expected: "#TagLine, #ExamplesLine, #Comment, #Empty",
        transitions: &[
            T { token: K::TagLine, lookahead: None, actions: &[B], next: 38 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, S(R::Examples), B], next: 39 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 38 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 38 },
        ],
    },
    // 39: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:3>ExamplesDefinition:1>Examples:0>#ExamplesLine:0
    State {
        expected: "#EOF, #Empty, #Comment, #TableRow, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, B], next: 42 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 39 },
            T { token: K::Comment, lookahead: None, actions: &[S(R::Description), B], next: 40 },
            T { token: K::TableRow, lookahead: None, actions: &[S(R::ExamplesTable), B], next: 41 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[S(R::Description), B], next: 40 },
        ],
    },
    // 40: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:3>ExamplesDefinition:1>Examples:1>DescriptionHelper:1>Description:0>__alt1:0>#Other:0
    State {
        expected: "#EOF, #Comment, #TableRow, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Other",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, E, B], next: 42 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 40 },
            T { token: K::TableRow, lookahead: None, actions: &[E, S(R::ExamplesTable), B], next: 41 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 40 },
        ],
    },
    // 41: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:3>ExamplesDefinition:1>Examples:2>ExamplesTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 41 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 41 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 41 },
        ],
    },
    State { expected: "", transitions: &[] },
    // 43: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:0>DocString:0>#DocStringSeparator:0
    State {
        expected: "#DocStringSeparator, #Other",
        transitions: &[
            T { token: K::DocStringSeparator, lookahead: None, actions: &[B], next: 44 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 43 },
        ],
    },
    // 44: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:0>DocString:2>#DocStringSeparator:0
    State {
        expected: "#EOF, #TableRow, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[E, S(R::DataTable), B], next: 45 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 34 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 44 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 44 },
        ],
    },
    // 45: GherkinDocument:0>Feature:3>Rule:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:1>DataTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 45 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 34 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 38 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 39 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 45 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 45 },
        ],
    },
    // 46: GherkinDocument:0>Feature:3>Rule:1>Background:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:0>DocString:0>#DocStringSeparator:0
    State {
        expected: "#DocStringSeparator, #Other",
        transitions: &[
            T { token: K::DocStringSeparator, lookahead: None, actions: &[B], next: 47 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 46 },
        ],
    },
    // 47: GherkinDocument:0>Feature:3>Rule:1>Background:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:0>DocString:2>#DocStringSeparator:0
    State {
        expected: "#EOF, #TableRow, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[E, S(R::DataTable), B], next: 48 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 27 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 47 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 47 },
        ],
    },
    // 48: GherkinDocument:0>Feature:3>Rule:1>Background:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:1>DataTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 48 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 27 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 31 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 32 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 48 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 48 },
        ],
    },
    // 49: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:0>DocString:0>#DocStringSeparator:0
    State {
        expected: "#DocStringSeparator, #Other",
        transitions: &[
            T { token: K::DocStringSeparator, lookahead: None, actions: &[B], next: 50 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 49 },
        ],
    },
    // 50: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:0>DocString:2>#DocStringSeparator:0
    State {
        expected: "#EOF, #TableRow, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[E, S(R::DataTable), B], next: 51 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 14 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 50 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 50 },
        ],
    },
    // 51: GherkinDocument:0>Feature:2>ScenarioDefinition:1>Scenario:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:1>DataTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #StepLine, #TagLine, #ExamplesLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 51 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 14 },
            T { token: K::TagLine, lookahead: Some(1), actions: &[E, E, S(R::ExamplesDefinition), S(R::Tags), B], next: 18 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ExamplesLine, lookahead: None, actions: &[E, E, S(R::ExamplesDefinition), S(R::Examples), B], next: 19 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 51 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 51 },
        ],
    },
    // 52: GherkinDocument:0>Feature:1>Background:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:0>DocString:0>#DocStringSeparator:0
    State {
        expected: "#DocStringSeparator, #Other",
        transitions: &[
            T { token: K::DocStringSeparator, lookahead: None, actions: &[B], next: 53 },
            T { token: K::Other, lookahead: None, actions: &[B], next: 52 },
        ],
    },
    // 53: GherkinDocument:0>Feature:1>Background:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:0>DocString:2>#DocStringSeparator:0
    State {
        expected: "#EOF, #TableRow, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[E, S(R::DataTable), B], next: 54 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 7 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 53 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 53 },
        ],
    },
    // 54: GherkinDocument:0>Feature:1>Background:2>Step:1>StepArg:0>__alt0:1>DocStringAndMaybeDataTable:1>DataTable:0>#TableRow:0
    State {
        expected: "#EOF, #TableRow, #StepLine, #TagLine, #ScenarioLine, #RuleLine, #Comment, #Empty",
        transitions: &[
            T { token: K::Eof, lookahead: None, actions: &[E, E, E, E, B], next: 42 },
            T { token: K::TableRow, lookahead: None, actions: &[B], next: 54 },
            T { token: K::StepLine, lookahead: None, actions: &[E, E, S(R::Step), B], next: 7 },
            T { token: K::TagLine, lookahead: Some(0), actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Tags), B], next: 11 },
            T { token: K::TagLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), S(R::Tags), B], next: 22 },
            T { token: K::ScenarioLine, lookahead: None, actions: &[E, E, E, S(R::ScenarioDefinition), S(R::Scenario), B], next: 12 },
            T { token: K::RuleLine, lookahead: None, actions: &[E, E, E, S(R::Rule), S(R::RuleHeader), B], next: 23 },
            T { token: K::Comment, lookahead: None, actions: &[B], next: 54 },
            T { token: K::Empty, lookahead: None, actions: &[B], next: 54 },
        ],
    },
];
