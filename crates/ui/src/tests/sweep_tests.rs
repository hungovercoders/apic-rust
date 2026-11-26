use apic_output::style;
use apic_runner::Options;

use super::{ascii, new_fixture, Fixture};
use crate::ui::{MIN_HEIGHT, MIN_WIDTH};

/// The terminals the sweep draws at: the minimum, one more than it in
/// each direction, the common ones, and a big one. Small sizes are where
/// a pane overflows; big ones are where padding goes wrong.
const SWEEP_SIZES: [(i32, i32); 6] = [
    (MIN_WIDTH, MIN_HEIGHT),
    (MIN_WIDTH + 1, MIN_HEIGHT + 1),
    (80, 24),
    (100, 30),
    (140, 45),
    (220, 70),
];

struct Sweep {
    f: Fixture,
    frames: usize,
    problems: Vec<String>,
}

impl Sweep {
    fn check(&mut self, what: &str, w: i32, h: i32) {
        self.frames += 1;
        let v = self.f.view();
        let lines: Vec<&str> = v.split('\n').collect();
        if lines.len() as i32 != h {
            self.problems.push(format!(
                "{w}x{h} {what}: {} lines, want {h}\n{v}",
                lines.len()
            ));
            return;
        }
        for (i, l) in lines.iter().enumerate() {
            let lw = style::string_width(l) as i32;
            if lw > w {
                self.problems
                    .push(format!("{w}x{h} {what}: line {} is {lw} wide\n{l}", i + 1));
            }
        }
        let last = lines[lines.len() - 1];
        if !last.contains("? help") {
            self.problems.push(format!(
                "{w}x{h} {what}: the status bar lost its key hint:\n{last}"
            ));
        }
        let shown = self.f.m.selected.as_ref().map(|r| r.id());
        let under = self.f.m.selected_req().map(|r| r.id());
        if shown != under {
            self.problems.push(format!(
                "{w}x{h} {what}: the pane shows {shown:?} while the cursor is on {under:?}"
            ));
        }
    }
}

// Visits every request, every tab with and without headers, every code
// snippet language, the help overlay and the filter, at each size in
// SWEEP_SIZES, before any run and after running the whole project, and
// fails on a frame with the wrong number of lines or a line wider than the
// terminal. A scenario test checks one screen; this checks that no screen
// has quietly stopped fitting.
#[test]
fn every_pane_fits_at_every_size() {
    ascii(|| {
        let mut s = Sweep {
            f: new_fixture(Options::default()),
            frames: 0,
            problems: Vec::new(),
        };
        let requests = s.f.m.all_requests().len();
        assert!(
            requests >= 10,
            "the demo project has {requests} requests; the sweep expects more"
        );
        for phase in ["before any run", "after running everything"] {
            if phase == "after running everything" {
                let cmd = s.f.press(&["a"]);
                s.f.drain(cmd);
            }
            for (w, h) in SWEEP_SIZES {
                s.f.m.resize(w, h);
                s.f.press(&["g"]);
                for _ in 0..requests {
                    let id = s.f.selected_id();
                    for tab in ["1", "2", "3", "4", "5"] {
                        s.f.press(&[tab]);
                        s.check(&format!("{phase} {id} tab {tab}"), w, h);
                        s.f.press(&["H"]);
                        s.check(&format!("{phase} {id} tab {tab} with headers"), w, h);
                        s.f.press(&["H"]);
                    }
                    s.f.press(&["1"]);
                    for _ in 0..8 {
                        s.f.press(&["c"]);
                        s.check(&format!("{phase} {id} code snippet"), w, h);
                    }
                    s.f.press(&["j"]);
                }
                s.f.press(&["?"]);
                s.check(&format!("{phase} help"), w, h);
                s.f.press(&["esc"]);
                s.f.press(&["/"]);
                s.f.press(&["t"]);
                s.check(&format!("{phase} filter"), w, h);
                s.f.press(&["esc"]);
                s.f.press(&["esc"]);
            }
        }
        assert!(
            s.problems.is_empty(),
            "{} frames do not fit, the first few:\n{}",
            s.problems.len(),
            s.problems[..s.problems.len().min(5)].join("\n\n")
        );
        assert!(
            s.frames >= 1000,
            "only {} frames checked; the sweep has lost its coverage",
            s.frames
        );
        s.f.m.resize(MIN_WIDTH - 1, MIN_HEIGHT);
        let v = s.f.view();
        assert!(
            v.contains("terminal too small"),
            "one column under the minimum should refuse to draw:\n{v}"
        );
    });
}
