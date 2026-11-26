//! Replays scripted sessions over the demo project and compares every
//! screen, byte for byte, with the screens recorded for the same script.
//!
//! A script (`testdata/golden/*.txt`) is a list of steps, one per line:
//! `size W H`, `key a b c` (press each, settling what it starts), `type
//! text`, `watch k` (press, with a screen after every request that
//! lands), `tour` and `sweep` (every request under the tabs, with
//! headers and as code). Every step but the last two ends with a screen.
//! The recording (`*.out`) starts with the port its server had and then
//! holds each screen after a line of `\x1e`, the step's number and the
//! step.
//!
//! What a run cannot repeat is pinned before the model sees it: round
//! trip times, the timing breakdown and the date header are set from the
//! request's name. The server's port differs from run to run, so its
//! digits are matched place by place.
//!
//! `UI_SCRIPT`, `UI_OUT` and `UI_PROFILE` (`ascii`, `ansi`, `ansi256`,
//! `truecolor`) with `cargo test -p apic-ui dump_script -- --ignored`
//! write the screens of any script to a file, in the recording's format.

use std::fmt::Write as _;

use apic_output::style::{set_has_dark_background, with_color_profile, Profile};
use apic_runner::{Options, Timings};

use super::{build_runner, demo_project, kp};
use crate::{Cmd, Config, Model, Msg};

fn normalise(res: &mut apic_runner::Result) {
    for d in &mut res.deps {
        normalise(d);
    }
    let n = res.request.method.len() + res.request.name.len();
    let Some(response) = &mut res.response else {
        return;
    };
    let d = (n * 37 % 250 + 3) as i64;
    response.duration_ms = d;
    if response.timings.is_some() {
        response.timings = Some(Timings {
            dns_ms: 0,
            connect_ms: 1,
            tls_ms: 0,
            ttfb_ms: d,
            total_ms: d,
            reused: n % 2 == 0,
        });
    }
    for (k, v) in &mut response.headers {
        if k.eq_ignore_ascii_case("date") {
            *v = "Thu, 26 Nov 2026 10:00:00 GMT".to_string();
        }
    }
}

struct Harness {
    m: Model,
    out: String,
    n: usize,
}

impl Harness {
    /// Takes the run clock out of the status bar.
    fn calm(&mut self) {
        if let Some(rs) = &mut self.m.inflight {
            rs.started = None;
        }
    }

    fn settle(&mut self, mut cmd: Option<Cmd>, watch: Option<&str>) {
        for _ in 0..1000 {
            let Some(run) = cmd.take() else {
                return;
            };
            match run() {
                None | Some(Msg::Quit | Msg::Delay(..) | Msg::Exec(_)) => return,
                Some(Msg::Async(work)) => {
                    let mut out = work();
                    if let Msg::Result(rm) = &mut out {
                        if let Some(res) = &mut rm.res {
                            normalise(res);
                        }
                    }
                    cmd = self.m.update(out);
                    self.calm();
                    if let Some(label) = watch {
                        self.snap(label);
                    }
                }
                Some(Msg::Cmd(next)) => cmd = Some(next),
                Some(Msg::Batch(mut cmds)) => {
                    let Some(last) = cmds.pop() else {
                        return;
                    };
                    for c in cmds {
                        self.settle(Some(c), watch);
                    }
                    cmd = Some(last);
                }
                Some(msg) => cmd = self.m.update(msg),
            }
        }
    }

    fn press(&mut self, name: &str, watch: Option<&str>) {
        let cmd = self.m.update(Msg::Key(kp(name)));
        self.calm();
        self.settle(cmd, watch);
    }

    fn snap(&mut self, label: &str) {
        self.n += 1;
        let view = self.m.view();
        let _ = writeln!(self.out, "\x1e{} {label}\n{view}", self.n);
    }

    fn selected(&self) -> String {
        self.m.selected.as_ref().map(|r| r.id()).unwrap_or_default()
    }

    fn sweep(&mut self, history: bool) {
        self.press("g", None);
        for _ in 0..self.m.all_requests().len() + 1 {
            let id = self.selected();
            for tab in ["1", "2", "3", "4", "5"] {
                if tab == "5" && !history {
                    continue;
                }
                self.press(tab, None);
                self.snap(&format!("sweep {id} tab {tab}"));
                if tab == "2" {
                    self.press("H", None);
                    self.snap(&format!("sweep {id} tab {tab} headers"));
                    self.press("H", None);
                }
            }
            self.press("1", None);
            for k in 0..7 {
                self.press("c", None);
                self.snap(&format!("sweep {id} code {}", k + 1));
            }
            self.press("J", None);
            self.press("ctrl+d", None);
            self.snap(&format!("sweep {id} scrolled"));
            self.press("j", None);
        }
    }

    fn tour(&mut self) {
        self.press("g", None);
        for i in 0..self.m.all_requests().len() {
            let id = self.selected();
            for tab in ["1", "2", "3"] {
                self.press(tab, None);
                self.snap(&format!("tour {id} tab {tab}"));
            }
            self.press("H", None);
            self.snap(&format!("tour {id} headers"));
            self.press("H", None);
            for _ in 0..=i % 6 {
                self.press("c", None);
            }
            self.snap(&format!("tour {id} code"));
            self.press("esc", None);
            self.press("1", None);
            self.press("j", None);
        }
    }

    fn step(&mut self, line: &str) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields[0] {
            "size" => {
                let n = |s: &str| s.parse::<i32>().expect("a size");
                self.m.resize(n(fields[1]), n(fields[2]));
            }
            "key" => {
                for k in &fields[1..] {
                    self.press(k, None);
                }
            }
            "type" => {
                for r in fields[1..].join(" ").chars() {
                    self.press(&r.to_string(), None);
                }
            }
            "watch" => self.press(fields[1], Some(&format!("{line} (landing)"))),
            "sweep" => return self.sweep(fields.last() != Some(&"nohistory")),
            "tour" => return self.tour(),
            _ => panic!("bad script line {line:?}"),
        }
        self.snap(line);
    }
}

/// Plays a script over a fresh demo project and returns the recording.
fn replay(script: &str, profile: Profile, redact: bool) -> String {
    set_has_dark_background(true);
    with_color_profile(profile, || {
        let (dir, srv) = demo_project();
        let opts = Options {
            redact,
            ..Options::default()
        };
        let runner = build_runner(dir.path(), &opts, "local").expect("runner");
        let mut cfg = Config::new(runner);
        cfg.root = "apic-demo".to_string();
        cfg.env = "local".to_string();
        cfg.editor = "vim".to_string();
        cfg.redact = redact;
        cfg.demo = true;
        let root = dir.path().to_path_buf();
        cfg.new_runner = Some(Box::new(move |env| build_runner(&root, &opts, env)));
        let mut h = Harness {
            m: Model::new(cfg),
            out: format!("port {}\n", srv.addr().port()),
            n: 0,
        };
        for line in script.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            h.step(line);
        }
        h.out
    })
}

/// Splits a recording into its port and its screens, each with its
/// label.
fn frames(recording: &str) -> (Vec<char>, Vec<(&str, &str)>) {
    let (head, rest) = recording.split_once('\n').expect("a port line");
    let port = head.strip_prefix("port ").expect("a port line");
    let frames = rest
        .split('\x1e')
        .skip(1)
        .map(|f| f.split_once('\n').expect("a label line"))
        .collect();
    (port.chars().collect(), frames)
}

/// Hides what the demo API counts by the clock: the digits of its
/// uptime.
fn mask_uptime(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.split_inclusive('\n') {
        match line.split_once("uptime_seconds") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str("uptime_seconds");
                out.extend(
                    after
                        .chars()
                        .map(|c| if c.is_ascii_digit() { '#' } else { c }),
                );
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Reports whether two screens are the same but for the digits of the
/// servers' ports, which may differ only place by place.
fn same_screen(want: &str, want_port: &[char], got: &str, got_port: &[char]) -> bool {
    let (want, got) = (mask_uptime(want), mask_uptime(got));
    if want.chars().count() != got.chars().count() {
        return false;
    }
    want.chars().zip(got.chars()).all(|(w, g)| {
        w == g
            || want_port
                .iter()
                .zip(got_port)
                .any(|(wp, gp)| *wp == w && *gp == g)
    })
}

fn compare(name: &str, recording: &str, replayed: &str) {
    let (want_port, want) = frames(recording);
    let (got_port, got) = frames(replayed);
    assert_eq!(
        want_port.len(),
        got_port.len(),
        "{name}: the test server's port has to be as long as the recording's"
    );
    let mut wrong = 0;
    for (i, ((want_label, want_view), (got_label, got_view))) in want.iter().zip(&got).enumerate() {
        if want_label != got_label || !same_screen(want_view, &want_port, got_view, &got_port) {
            wrong += 1;
            if wrong <= 3 {
                eprintln!(
                    "{name}: screen {} differs\n--- recorded: {want_label}\n{want_view}\n--- drawn: {got_label}\n{got_view}",
                    i + 1
                );
            }
        }
    }
    assert_eq!(wrong, 0, "{name}: {wrong} screens differ");
    assert_eq!(want.len(), got.len(), "{name}: the number of screens");
}

fn golden(name: &str) -> (String, String) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/golden");
    let read = |ext: &str| {
        let path = format!("{dir}/{name}.{ext}");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    };
    (read("txt"), read("out"))
}

#[test]
fn screens_match_the_recording() {
    let (script, recording) = golden("golden");
    compare(
        "golden",
        &recording,
        &replay(&script, Profile::Ascii, false),
    );
}

#[test]
fn coloured_screens_match_the_recording() {
    let (script, recording) = golden("colour");
    compare(
        "colour",
        &recording,
        &replay(&script, Profile::TrueColor, false),
    );
}

#[test]
#[ignore = "writes the screens of the script in UI_SCRIPT to UI_OUT"]
fn dump_script() {
    let script = std::env::var("UI_SCRIPT").expect("UI_SCRIPT");
    let out = std::env::var("UI_OUT").expect("UI_OUT");
    let profile = match std::env::var("UI_PROFILE").as_deref() {
        Ok("truecolor") => Profile::TrueColor,
        Ok("ansi256") => Profile::Ansi256,
        Ok("ansi") => Profile::Ansi,
        _ => Profile::Ascii,
    };
    let script = std::fs::read_to_string(script).expect("read the script");
    let redact = std::env::var("UI_REDACT").is_ok();
    std::fs::write(out, replay(&script, profile, redact)).expect("write the screens");
}
