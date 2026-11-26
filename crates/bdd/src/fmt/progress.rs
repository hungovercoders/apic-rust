//! The `progress` format: one character per step, seventy to a row, then
//! the failed steps in full.

use super::{s, summary};
use crate::colors::{black_bold, cyan, green, red, red_bold, yellow, Out};
use crate::engine::{id_number, Formatter, Status, Storage};
use crate::gherkin::{Pickle, PickleStep};

const STEPS_PER_ROW: usize = 70;

#[derive(Default)]
pub(super) struct Progress {
    steps: usize,
}

impl Formatter for Progress {
    fn step(&mut self, st: &Storage, out: &mut Out<'_>, _pickle: &Pickle, step: &PickleStep) {
        let Some(result) = st.step_result(&step.id) else {
            return;
        };
        out.print(&match result.status {
            Status::Passed => green("."),
            Status::Skipped => cyan("-"),
            Status::Failed => red("F"),
            Status::Undefined => yellow("U"),
            Status::Ambiguous => yellow("A"),
        });
        self.steps += 1;
        if self.steps % STEPS_PER_ROW == 0 {
            out.print(&format!(" {}\n", self.steps));
        }
    }

    fn summary(&mut self, st: &Storage, out: &mut Out<'_>) {
        let left = self.steps % STEPS_PER_ROW;
        if left != 0 {
            if self.steps > STEPS_PER_ROW {
                out.print(&format!(
                    "{} {}\n",
                    s((STEPS_PER_ROW - left) as isize),
                    self.steps
                ));
            } else {
                out.print(&format!(" {}\n", self.steps));
            }
        }
        let mut lines: Vec<String> = Vec::new();
        let mut failed = st.step_results_by_status(Status::Failed);
        failed.sort_by_key(|r| id_number(&r.step_id));
        for sr in failed {
            let found = st.pickle(&sr.pickle_id).and_then(|pickle| {
                let step = st.pickle_step(&sr.step_id)?;
                let feature = st.feature(&pickle.uri)?;
                let scenario = feature.doc.find_scenario(&pickle.ast_node_ids[0])?;
                let ast_step = feature.doc.find_step(&step.ast_node_id)?;
                Some((pickle, step, scenario, ast_step))
            });
            let Some((pickle, step, scenario, ast_step)) = found else {
                continue;
            };
            let scenario_desc = format!("{}: {}", scenario.keyword, pickle.name);
            let scenario_line = format!("{}:{}", pickle.uri, scenario.line);
            let step_desc = format!("{} {}", ast_step.keyword.trim(), step.text);
            let step_line = format!("{}:{}", pickle.uri, ast_step.line);
            lines.push(format!(
                "{}{}{}",
                s(2),
                red(&scenario_desc),
                black_bold(&format!(" # {scenario_line}"))
            ));
            lines.push(format!(
                "{}{}{}",
                s(4),
                red(&step_desc),
                black_bold(&format!(" # {step_line}"))
            ));
            lines.push(format!(
                "{}{}{}",
                s(6),
                red("Error: "),
                red_bold(sr.err.as_deref().unwrap_or("<nil>"))
            ));
            lines.push(String::new());
        }
        if !lines.is_empty() {
            out.println(&format!("\n\n--- {}\n", red("Failed steps:")));
            out.print(&lines.join("\n"));
        }
        out.println("");
        summary(st, out);
    }
}
