use std::path::{Path, PathBuf};

use crate::episode::{
    load_all, say_time, say_times, spread, Card, Chapter, Cue, Episode, Shot, Thumbnail, Zoom,
    EPISODES_DIR,
};
use crate::video::{ass_time, srt_time, yt_time};
use crate::voice::VOICE_PAD;
use crate::{cargo_caches, check_voice, tape_text};

fn root() -> PathBuf {
    PathBuf::from(format!("{}/../..", env!("CARGO_MANIFEST_DIR")))
}

fn say(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

/// Every episode loads and validates against the lesson page it is for;
/// loading validates, so this is the check CI gets without vhs.
#[test]
fn episodes_load() {
    let eps = load_all(&root()).unwrap();
    assert!(!eps.is_empty(), "no episodes in {EPISODES_DIR}");
    for ep in &eps {
        assert!(
            !ep.title.is_empty() && !ep.slug.is_empty(),
            "{}: no title or slug from the lesson page",
            ep.name
        );
    }
    let names: Vec<&str> = eps.iter().map(|e| e.name.as_str()).collect();
    assert!(names.windows(2).all(|w| w[0] < w[1]), "{names:?}");
}

/// Every setup step's lesson blocks exist, and its substitution names text
/// that is in them.
#[test]
fn setup_names_real_blocks() {
    for ep in load_all(&root()).unwrap() {
        let blocks = apic_lessons::blocks(root(), &ep.name).unwrap();
        for s in &ep.shots {
            for st in &s.setup {
                let code = st
                    .step(&ep.name)
                    .code(&blocks)
                    .unwrap_or_else(|err| panic!("{}: clip {}: {err}", ep.name, s.clip));
                assert!(
                    st.replace.len() != 2 || !code.contains(&st.replace[0]),
                    "{}: clip {}: the substitution of {:?} left some behind",
                    ep.name,
                    s.clip,
                    st.replace[0]
                );
            }
        }
    }
}

fn card(series: &str, title: &str, heading: &str) -> Option<Card> {
    Some(Card {
        series: series.to_string(),
        title: title.to_string(),
        heading: heading.to_string(),
        ..Card::default()
    })
}

fn good() -> Episode {
    Episode {
        lesson: 1,
        name: "01".to_string(),
        summary: "s".to_string(),
        thumbnail: Thumbnail {
            words: "w".to_string(),
            clip: "a".to_string(),
        },
        shots: vec![
            Shot {
                chapter: "c".to_string(),
                say: say(&["hi"]),
                card: card("s", "t", ""),
                ..Shot::default()
            },
            Shot {
                chapter: "c".to_string(),
                say: say(&["hi"]),
                clip: "a".to_string(),
                tape: r#"Type "x" Enter"#.to_string(),
                ..Shot::default()
            },
            Shot {
                chapter: "c".to_string(),
                say: say(&["hi"]),
                zoom: Some(Zoom { col: 1, row: 1 }),
                ..Shot::default()
            },
        ],
        ..Episode::default()
    }
}

/// Validation catches an episode that does not open on its title card, a
/// shot that is no kind or two, a zoom before any clip or after a wide
/// one, a thumbnail naming no clip or a wide one, a clip name that is not
/// a file name, and a caption the renderer would misread.
#[test]
fn validate() {
    if let Err(err) = good().validate() {
        panic!("a good episode fails: {err}");
    }
    type Break = fn(&mut Episode);
    let cases: [(&str, Break); 14] = [
        ("no title first", |e| {
            e.shots.remove(0);
        }),
        ("heading first", |e| e.shots[0].card = card("", "", "h")),
        ("no kind", |e| e.shots[1].clip.clear()),
        ("two kinds", |e| e.shots[1].card = card("", "", "h")),
        ("zoom first", |e| {
            e.shots.remove(1);
        }),
        ("zoom off window", |e| {
            e.shots[2].zoom = Some(Zoom { col: 500, row: 1 });
        }),
        ("thumbnail clip", |e| e.thumbnail.clip = "nope".to_string()),
        ("brace in caption", |e| e.shots[1].say = say(&["{{user}}"])),
        ("name mismatch", |e| e.lesson = 2),
        ("empty tape", |e| e.shots[1].tape = " ".to_string()),
        ("zoom with a tape", |e| {
            e.shots[2].tape = r#"Type "x""#.to_string();
        }),
        ("zoom after wide", |e| {
            e.shots[1].wide = true;
            e.thumbnail.clip = "b".to_string();
            e.shots.push(Shot {
                chapter: "c".to_string(),
                say: say(&["hi"]),
                clip: "b".to_string(),
                tape: "Enter".to_string(),
                ..Shot::default()
            });
        }),
        ("wide thumbnail", |e| {
            e.shots[1].wide = true;
            e.shots[2] = Shot {
                chapter: "c".to_string(),
                say: say(&["hi"]),
                card: card("", "", "h"),
                ..Shot::default()
            };
        }),
        ("clip name", |e| {
            e.shots[1].clip = "a/b".to_string();
            e.thumbnail.clip = "a/b".to_string();
        }),
    ];
    for (name, break_it) in cases {
        let mut e = good();
        break_it(&mut e);
        assert!(e.validate().is_err(), "{name}: validation passed");
    }
    // Every problem is reported, one a line.
    let mut e = good();
    e.lesson = 0;
    e.summary = " ".to_string();
    let err = e.validate().unwrap_err();
    assert_eq!(
        err,
        "lesson must be a positive number\nlesson 0 does not match the file name 01\nsummary (the first line of the description) is empty"
    );
}

/// A shot list is read strictly: a key the generator does not know is a
/// mistake, a scalar is taken as the text it was written as, and a key
/// with nothing after it is unset.
#[test]
fn shot_lists_are_read_strictly() {
    let ep: Episode = serde_yaml_ng::from_str(
        "lesson: 3\nsummary: 42\nthumbnail:\n  words: w\n  clip: a\nshots:\n  - chapter: c\n    say: [one, 2]\n    card:\n      title: t\n      table:\n        - [a, 1]\n      foot:\n  - chapter: c\n    zoom: { col: 2, row: 3 }\n    setup:\n",
    )
    .unwrap();
    assert_eq!(ep.lesson, 3);
    assert_eq!(ep.summary, "42");
    assert_eq!(ep.shots[0].say, ["one", "2"]);
    let first = ep.shots[0].card.as_ref().unwrap();
    assert_eq!(first.table, [["a", "1"]]);
    assert_eq!(first.foot, "");
    assert_eq!(ep.shots[0].kind(), "card");
    assert_eq!(ep.shots[1].zoom, Some(Zoom { col: 2, row: 3 }));
    assert!(ep.shots[1].setup.is_empty());
    assert!(serde_yaml_ng::from_str::<Episode>("lesson: 1\nsumary: typo\n").is_err());
    assert!(serde_yaml_ng::from_str::<Episode>("shots:\n  - chapter: c\n    tap: x\n").is_err());
}

/// The timing formats, and that a shot never runs shorter than its lines.
#[test]
fn times() {
    for (t, srt, ass, yt) in [
        (0.0, "00:00:00,000", "0:00:00.00", "0:00"),
        (61.5, "00:01:01,500", "0:01:01.50", "1:01"),
        (3599.999, "00:59:59,999", "1:00:00.00", "59:59"),
    ] {
        assert_eq!(srt_time(t), srt, "srt_time({t})");
        assert_eq!(ass_time(t), ass, "ass_time({t})");
        assert_eq!(yt_time(t), yt, "yt_time({t})");
    }
    assert_eq!(
        say_time("Now send it."),
        2.4,
        "a short line should take the minimum"
    );
    let lines = say(&["one two three four five", "six"]);
    let cues = spread(10.0, 20.0, &lines, &say_times(&lines));
    assert!(
        cues.len() == 2 && cues[0].start == 10.0 && cues[1].end > 29.99 && cues[1].end < 30.01,
        "spread did not fill the shot: {cues:?}"
    );
    // Recorded times: a 1s and a 3s clip over an 8s shot keep their ratio,
    // and each cue is at least as long as its clip.
    let cues = spread(0.0, 8.0, &lines, &[1.0 + VOICE_PAD, 3.0 + VOICE_PAD]);
    assert!(
        cues[0].end >= 1.0
            && cues[1].start == cues[0].end
            && cues[1].end - cues[1].start >= 3.0
            && cues[1].end > 7.99
            && cues[1].end < 8.01,
        "spread over recorded times: {cues:?}"
    );
}

/// The narration script numbers each line as the SRT does.
#[test]
fn narration_numbers_cues() {
    let ep = Episode {
        lesson: 4,
        name: "04".to_string(),
        title: "T".to_string(),
        ..Episode::default()
    };
    let cue = |start: f64, text: &str| Cue {
        start,
        end: start + 2.0,
        text: text.to_string(),
        audio: None,
    };
    let chapters = [Chapter {
        start: 0.0,
        title: "Why".to_string(),
    }];
    let got = ep.narration(&[cue(0.0, "one"), cue(2.0, "two")], &chapters, 4.0);
    for want in [
        "# From zero to apic #4: T\n\nNarration script. Runs 0:04. ",
        "## 0:00 Why",
        "- **0:00** 001 · one",
        "- **0:02** 002 · two",
    ] {
        assert!(got.contains(want), "narration lacks {want:?}:\n{got}");
    }
}

/// The description fills every placeholder of the template, and the
/// template still has the block the generator fills.
#[test]
fn description() {
    let eps = load_all(&root()).unwrap();
    let chapters = [
        Chapter {
            start: 0.0,
            title: "Why".to_string(),
        },
        Chapter {
            start: 30.2,
            title: "Install".to_string(),
        },
    ];
    let mut prev: Option<&Episode> = None;
    for ep in &eps {
        let d = ep
            .description(&root(), &chapters, prev)
            .unwrap_or_else(|err| panic!("{}: {err}", ep.name));
        let previous = match prev {
            Some(prev) => format!("⬅ Previous: https://apic.sh/learn/{}/\n", prev.slug),
            None => "⬅ Previous: this is the first one\n".to_string(),
        };
        for want in [
            ep.summary.trim().to_string(),
            format!("https://apic.sh/learn/{}/", ep.slug),
            "0:00 Why\n0:30 Install\n".to_string(),
            format!("lesson {} of", ep.name.trim_start_matches('0')),
            previous,
        ] {
            assert!(
                d.contains(&want),
                "{}: description lacks {want:?}:\n{d}",
                ep.name
            );
        }
        prev = Some(ep);
    }
}

/// A clip's tape is the shared preamble at the clip's size, naming the
/// recording after the tape, then the clip's own lines.
#[test]
fn tape_text_for_a_clip() {
    let mut shot = Shot {
        clip: "a".to_string(),
        tape: "Type \"x\" Enter\n\n".to_string(),
        ..Shot::default()
    };
    let text = tape_text("clip-01-a.tape", &shot);
    assert!(
        text.starts_with("Output clip-01-a.mp4\nSet Shell \"bash\"\nSet FontSize 16\nSet Width 1150\nSet Height 720\n"),
        "{text}"
    );
    assert!(
        text.ends_with("Set Padding 60\nType \"x\" Enter\n"),
        "{text}"
    );
    shot.wide = true;
    assert!(tape_text("clip-01-a.tape", &shot).contains("Set Width 2100\nSet Height 800\n"));
}

/// Checking a voice needs ffprobe, and says so for every episode when it
/// is missing; with it, an episode without a recording is not a failure.
#[test]
fn check_voice_reports() {
    let eps = [good()];
    let dir = tempfile::tempdir().unwrap();
    let mut out: Vec<u8> = Vec::new();
    let failed = check_voice(&eps, &dir.path().to_string_lossy(), &mut out);
    let text = String::from_utf8(out).unwrap();
    if record::on_path("ffprobe") {
        assert_eq!(failed, 0, "{text}");
        assert!(text.starts_with("01: no voice under "), "{text}");
    } else {
        assert_eq!(failed, 1);
        assert_eq!(
            text,
            "ffprobe is not on PATH; checking a voice directory needs it\n"
        );
    }
}

/// The recording keeps the machine's cargo and rustup directories, by
/// their own variables.
#[test]
fn cargo_caches_are_named() {
    for kv in cargo_caches() {
        let (name, value) = kv.split_once('=').unwrap();
        assert!(matches!(name, "CARGO_HOME" | "RUSTUP_HOME"), "{kv}");
        assert!(Path::new(value).is_absolute(), "{kv}");
    }
}
