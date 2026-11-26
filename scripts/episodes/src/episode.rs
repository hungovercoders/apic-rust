//! An episode's shot list: how it is read, checked and turned into the
//! text files that ship with the video.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use apic_gocompat::quote;
use regex::Regex;
use serde::{Deserialize, Deserializer};

use crate::video::{yt_time, zoom_window};

/// Where the episodes and the description template live, relative to the
/// repository root.
pub(crate) const EPISODES_DIR: &str = "docs/learn/episodes";
pub(crate) const TEMPLATE: &str = "docs/learn/_episode-template.md";

/// A scalar as the text it was written as: a number or a switch where
/// text is expected is that text, and nothing is the empty string.
#[derive(Deserialize)]
#[serde(untagged)]
enum Scalar {
    Text(String),
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Nothing(()),
}

impl Scalar {
    fn into_text(self) -> String {
        match self {
            Scalar::Text(s) => s,
            Scalar::Bool(b) => b.to_string(),
            Scalar::Int(n) => n.to_string(),
            Scalar::Uint(n) => n.to_string(),
            Scalar::Float(f) => apic_gocompat::format_float_g(f),
            Scalar::Nothing(()) => String::new(),
        }
    }
}

fn text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Scalar::deserialize(d)?.into_text())
}

fn texts<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let list = Option::<Vec<Scalar>>::deserialize(d)?.unwrap_or_default();
    Ok(list.into_iter().map(Scalar::into_text).collect())
}

fn rows<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<String>>, D::Error> {
    let list = Option::<Vec<Option<Vec<Scalar>>>>::deserialize(d)?.unwrap_or_default();
    Ok(list
        .into_iter()
        .map(|row| {
            row.unwrap_or_default()
                .into_iter()
                .map(Scalar::into_text)
                .collect()
        })
        .collect())
}

/// A member that may be written with nothing after it.
fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One lesson's episode: the shots in order, what the voice says over
/// each, and the cards between the terminal clips. It is read from
/// docs/learn/episodes/NN.yaml; the lesson page supplies the title and the
/// slug.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Episode {
    #[serde(deserialize_with = "or_default")]
    pub lesson: i64,
    #[serde(deserialize_with = "text")]
    pub summary: String,
    #[serde(deserialize_with = "or_default")]
    pub project: bool,
    #[serde(deserialize_with = "or_default")]
    pub thumbnail: Thumbnail,
    #[serde(deserialize_with = "or_default")]
    pub shots: Vec<Shot>,

    /// "01"
    #[serde(skip)]
    pub name: String,
    /// "01-first-request"
    #[serde(skip)]
    pub slug: String,
    /// The page title without its "Lesson N: " prefix.
    #[serde(skip)]
    pub title: String,
}

/// What the YouTube thumbnail shows beside the logo and the episode
/// number: a few words of the title, and a frame from a clip.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Thumbnail {
    #[serde(deserialize_with = "text")]
    pub words: String,
    #[serde(deserialize_with = "text")]
    pub clip: String,
}

/// One piece of the episode: a card, a terminal clip or a zoom on the last
/// frame of the previous clip. Its lines are what the voice says over it,
/// one caption each; they set how long the shot runs.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Shot {
    #[serde(deserialize_with = "text")]
    pub chapter: String,
    #[serde(deserialize_with = "texts")]
    pub say: Vec<String>,
    pub card: Option<Card>,
    #[serde(deserialize_with = "text")]
    pub clip: String,
    #[serde(deserialize_with = "text")]
    pub tape: String,
    #[serde(deserialize_with = "or_default")]
    pub wide: bool,
    #[serde(deserialize_with = "or_default")]
    pub setup: Vec<Setup>,
    pub zoom: Option<Zoom>,
}

/// A full-frame slide rendered from HTML: a series label and gradient
/// title (the title and next-lesson cards), or a heading with a command
/// grid, a table or a block of text, and a footer line. Backticks in text,
/// table cells and the footer mark highlighted spans.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Card {
    #[serde(deserialize_with = "text")]
    pub series: String,
    #[serde(deserialize_with = "text")]
    pub title: String,
    #[serde(deserialize_with = "text")]
    pub heading: String,
    #[serde(deserialize_with = "or_default")]
    pub commands: Vec<CardCommand>,
    #[serde(deserialize_with = "rows")]
    pub table: Vec<Vec<String>>,
    #[serde(deserialize_with = "text")]
    pub text: String,
    #[serde(deserialize_with = "text")]
    pub foot: String,
}

/// One cell of a card's command grid.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CardCommand {
    #[serde(deserialize_with = "text")]
    pub label: String,
    #[serde(deserialize_with = "text")]
    pub command: String,
}

/// State a clip needs before it is recorded: a shell snippet, or runnable
/// blocks of the lesson page (1-based, in page order), optionally with a
/// text substitution applied to them first.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Setup {
    #[serde(deserialize_with = "text")]
    pub sh: String,
    #[serde(deserialize_with = "or_default")]
    pub blocks: Vec<i64>,
    #[serde(deserialize_with = "texts")]
    pub replace: Vec<String>,
}

/// Names the terminal cell (1-based column and row) the push-in on the
/// previous clip's last frame must keep in view.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Zoom {
    #[serde(deserialize_with = "or_default")]
    pub col: i64,
    #[serde(deserialize_with = "or_default")]
    pub row: i64,
}

impl Shot {
    /// "card", "clip" or "zoom", or "" when the shot is none or more than
    /// one of them.
    pub fn kind(&self) -> &'static str {
        let kinds: Vec<&'static str> = [
            (self.card.is_some(), "card"),
            (!self.clip.is_empty(), "clip"),
            (self.zoom.is_some(), "zoom"),
        ]
        .into_iter()
        .filter(|(is, _)| *is)
        .map(|(_, kind)| kind)
        .collect();
        if kinds.len() == 1 {
            kinds[0]
        } else {
            ""
        }
    }
}

static RE_TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^title: "Lesson [0-9]+: (.+)"$"#).expect("a valid pattern"));

/// Reads one episode and fills in what the lesson page supplies.
pub(crate) fn load(root: &Path, path: &Path) -> Result<Episode, String> {
    let shown = path.display();
    let data = std::fs::read(path).map_err(|err| apic_gocompat::path_error("open", path, &err))?;
    let mut ep: Episode =
        serde_yaml_ng::from_slice(&data).map_err(|err| format!("{shown}: yaml: {err}"))?;
    ep.name = path
        .file_name()
        .map(|n| n.to_string_lossy().trim_end_matches(".yaml").to_string())
        .unwrap_or_default();
    let page = apic_lessons::page(root, &ep.name).map_err(|err| format!("{shown}: {err}"))?;
    ep.slug = page
        .file_name()
        .map(|n| n.to_string_lossy().trim_end_matches(".md").to_string())
        .unwrap_or_default();
    let src = std::fs::read(&page).map_err(|err| apic_gocompat::path_error("open", &page, &err))?;
    let src = String::from_utf8_lossy(&src);
    let Some(m) = RE_TITLE.captures(&src) else {
        return Err(format!(
            "{}: no \"Lesson N: title\" in the front matter",
            page.display()
        ));
    };
    ep.title = m[1].to_string();
    ep.validate().map_err(|err| format!("{shown}: {err}"))?;
    // A setup step naming a block the lesson does not have, or text that
    // is not in it, would otherwise fail minutes into a recording.
    let blocks = apic_lessons::blocks(root, &ep.name).map_err(|err| err.to_string())?;
    for s in &ep.shots {
        for st in &s.setup {
            st.step(&ep.name)
                .code(&blocks)
                .map_err(|err| format!("{shown}: clip {}: {err}", s.clip))?;
        }
    }
    Ok(ep)
}

/// Reads every episode, in lesson order.
pub(crate) fn load_all(root: &Path) -> Result<Vec<Episode>, String> {
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(root.join(EPISODES_DIR)) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(".yaml"))
            .collect(),
        // A directory that is not there holds no episodes.
        Err(_) => Vec::new(),
    };
    paths.sort();
    paths.iter().map(|p| load(root, p)).collect()
}

/// What a clip may be called: it is spliced into file names.
static CLIP_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("a valid pattern"));

impl Episode {
    /// Checks the shot list for what would otherwise fail halfway through
    /// a recording: a shot that is not exactly one kind, a zoom with no
    /// clip before it or on a cell the push-in cannot keep in frame, a
    /// line the caption renderer would misread.
    pub fn validate(&self) -> Result<(), String> {
        let mut errs: Vec<String> = Vec::new();
        if self.lesson < 1 {
            errs.push("lesson must be a positive number".to_string());
        }
        if format!("{:02}", self.lesson) != self.name {
            errs.push(format!(
                "lesson {} does not match the file name {}",
                self.lesson, self.name
            ));
        }
        if self.summary.trim().is_empty() {
            errs.push("summary (the first line of the description) is empty".to_string());
        }
        match self.shots.first() {
            None => errs.push("no shots".to_string()),
            Some(first) => {
                if !first.card.as_ref().is_some_and(|c| !c.title.is_empty()) {
                    errs.push("the first shot is the title card (a card with a series label and a title), as the episode template says".to_string());
                }
            }
        }
        // Each clip seen so far, and whether it is wide.
        let mut clips: HashMap<&str, bool> = HashMap::new();
        let mut last_wide = false;
        for (i, s) in self.shots.iter().enumerate() {
            let at = format!("shot {}", i + 1);
            if s.chapter.is_empty() {
                errs.push(format!("{at}: no chapter"));
            }
            if s.say.is_empty() {
                errs.push(format!("{at}: nothing to say"));
            }
            for line in &s.say {
                if line.contains(['{', '}', '\\', '\n']) {
                    errs.push(format!("{at}: a line holds a brace, backslash or newline, which the caption renderer would misread: {}", quote(line)));
                }
            }
            match s.kind() {
                "card" => errs.extend(s.validate_card(&at)),
                "clip" => {
                    errs.extend(s.validate_clip(&at, clips.contains_key(s.clip.as_str())));
                    clips.insert(&s.clip, s.wide);
                    last_wide = s.wide;
                }
                "zoom" => errs.extend(s.validate_zoom(&at, !clips.is_empty(), last_wide)),
                _ => errs.push(format!("{at}: a shot is exactly one of card, clip or zoom")),
            }
        }
        match clips.get(self.thumbnail.clip.as_str()) {
            Some(wide) if !self.thumbnail.words.is_empty() => {
                if *wide {
                    errs.push(format!("thumbnail: clip {} is wide; the thumbnail cuts the standard terminal out of its frame", quote(&self.thumbnail.clip)));
                }
            }
            _ => errs.push("thumbnail needs words and the name of a clip".to_string()),
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("\n"))
        }
    }

    /// Every line of the episode in cue order, so cue n is `lines()[n-1]`.
    pub fn lines(&self) -> Vec<String> {
        self.shots.iter().flat_map(|s| s.say.clone()).collect()
    }
}

impl Shot {
    fn belongs_to_a_clip(&self) -> bool {
        !self.tape.is_empty() || self.wide || !self.setup.is_empty()
    }

    fn validate_card(&self, at: &str) -> Vec<String> {
        let mut errs = Vec::new();
        if self
            .card
            .as_ref()
            .is_some_and(|c| c.title.is_empty() && c.heading.is_empty())
        {
            errs.push(format!("{at}: a card needs a title or a heading"));
        }
        if self.belongs_to_a_clip() {
            errs.push(format!("{at}: tape, wide and setup belong to a clip"));
        }
        errs
    }

    fn validate_clip(&self, at: &str, taken: bool) -> Vec<String> {
        let mut errs = Vec::new();
        if !CLIP_NAME.is_match(&self.clip) {
            errs.push(format!(
                "{at}: clip name {} must be lower-case letters, digits and hyphens",
                quote(&self.clip)
            ));
        }
        if taken {
            errs.push(format!(
                "{at}: another clip is already named {}",
                quote(&self.clip)
            ));
        }
        if self.tape.trim().is_empty() {
            errs.push(format!("{at}: clip {} has no tape", quote(&self.clip)));
        }
        for st in &self.setup {
            if st.sh.is_empty() == st.blocks.is_empty() {
                errs.push(format!(
                    "{at}: a setup step is a shell snippet or lesson blocks, not both or neither"
                ));
            }
            if !st.replace.is_empty() && st.replace.len() != 2 {
                errs.push(format!("{at}: replace is [old, new]"));
            }
        }
        errs
    }

    fn validate_zoom(&self, at: &str, after_clip: bool, after_wide: bool) -> Vec<String> {
        let mut errs = Vec::new();
        if !after_clip {
            errs.push(format!("{at}: a zoom needs a clip before it"));
        }
        if after_wide {
            errs.push(format!("{at}: the clip before a zoom cannot be wide; the push-in assumes the standard terminal"));
        }
        if self.belongs_to_a_clip() {
            errs.push(format!("{at}: tape, wide and setup belong to a clip"));
        }
        let (cols, rows) = zoom_window();
        if let Some(z) = &self.zoom {
            if z.col > cols || z.row > rows || z.col < 1 || z.row < 1 {
                errs.push(format!(
                    "{at}: zoom target ({},{}) is outside the {cols}x{rows} window the push-in shows",
                    z.col, z.row
                ));
            }
        }
        errs
    }
}

impl Setup {
    /// The setup step as the shared recorder runs it.
    pub fn step(&self, lesson: &str) -> record::Step {
        let mut st = record::Step {
            sh: self.sh.clone(),
            lesson: lesson.to_string(),
            blocks: self.blocks.clone(),
            ..record::Step::default()
        };
        if let [old, new] = self.replace.as_slice() {
            st.replace = [old.clone(), new.clone()];
        }
        st
    }
}

/// How long a line takes to say at about 150 words a minute, with a beat
/// after it: the template says to pause for longer than feels natural,
/// because viewers read slower than you talk.
pub(crate) fn say_time(line: &str) -> f64 {
    let words = line.split_whitespace().count() as f64;
    (words / 2.5 + 0.9).max(2.4)
}

/// How long each line takes to say.
pub(crate) fn say_times(lines: &[String]) -> Vec<f64> {
    lines.iter().map(|l| say_time(l)).collect()
}

/// One caption: when it starts and ends, the line, and the recorded clip
/// of it when the episode has a voice.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub audio: Option<PathBuf>,
}

/// Lays a shot's lines over its length: each in proportion to the time it
/// needs (as said at a reading pace, or as recorded), stretched when the
/// clip runs longer than the narration. A line's time is never shrunk, so
/// a recorded clip always fits inside its cue.
pub(crate) fn spread(start: f64, secs: f64, lines: &[String], times: &[f64]) -> Vec<Cue> {
    let want: f64 = times.iter().sum();
    let mut t = start;
    lines
        .iter()
        .zip(times)
        .map(|(l, time)| {
            let d = time * secs / want;
            let cue = Cue {
                start: t,
                end: t + d,
                text: l.clone(),
                audio: None,
            };
            t += d;
            cue
        })
        .collect()
}

/// A YouTube chapter: where it starts and its name.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Chapter {
    pub start: f64,
    pub title: String,
}

/// The chapters as YouTube's list reads them, one a line.
pub(crate) fn chapter_list(chapters: &[Chapter]) -> String {
    let mut b = String::new();
    for c in chapters {
        let _ = writeln!(b, "{} {}", yt_time(c.start), c.title);
    }
    b
}

/// Replaces each placeholder with its value in one pass over `text`, so a
/// value that happens to hold a placeholder is left as it is.
fn fill(text: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while let Some(c) = rest.chars().next() {
        for (old, new) in pairs {
            if let Some(after) = rest.strip_prefix(old) {
                out.push_str(new);
                rest = after;
                continue 'scan;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

impl Episode {
    /// Fills the template's description in for this episode.
    pub fn description(
        &self,
        root: &Path,
        chapters: &[Chapter],
        prev: Option<&Episode>,
    ) -> Result<String, String> {
        let path = root.join(TEMPLATE);
        let data = std::fs::read_to_string(&path)
            .map_err(|err| apic_gocompat::path_error("open", &path, &err))?;
        let body = data
            .split_once("```text\n")
            .and_then(|(_, rest)| rest.split_once("\n```"))
            .map(|(body, _)| body)
            .ok_or_else(|| format!("{TEMPLATE} has no ```text block to fill"))?;
        let (head, after) = body
            .split_once("Chapters\n")
            .and_then(|(head, tail)| Some((head, tail.split_once("\n\nLinks")?.1)))
            .ok_or_else(|| {
                format!("{TEMPLATE}: the description's Chapters block is not where it was")
            })?;
        let list = chapter_list(chapters);
        let body = format!(
            "{head}Chapters\n{}\n\nLinks{after}",
            list.strip_suffix('\n').unwrap_or(&list)
        );
        let previous = match prev {
            Some(prev) => format!("https://apic.sh/learn/{}/", prev.slug),
            None => "this is the first one".to_string(),
        };
        let out = fill(
            &body,
            &[
                (
                    "<One sentence: what you can do after this episode.>",
                    self.summary.trim(),
                ),
                ("<N>", &self.lesson.to_string()),
                ("<NN-slug>", &self.slug),
                (r#"<link or "this is the first one">"#, &previous),
                (r#"<link or "coming soon">"#, "coming soon"),
            ],
        );
        if let Some(at) = out.find('<') {
            return Err(format!(
                "the template has a placeholder this generator does not fill: {}",
                &out[at..]
            ));
        }
        Ok(format!("{out}\n"))
    }

    /// The YouTube title, per the template.
    pub fn video_title(&self) -> String {
        format!("From zero to apic #{}: {}", self.lesson, self.title)
    }

    /// The script to read: every line with its cue number and the time it
    /// starts, under its chapter.
    pub fn narration(&self, cues: &[Cue], chapters: &[Chapter], total: f64) -> String {
        let name = &self.name;
        let mut b = String::new();
        let _ = write!(b, "# {}\n\n", self.video_title());
        let _ = writeln!(b, "Narration script. Runs {}. Each line is one caption in `{name}.srt`, numbered as there; the timestamp is where it starts in `{name}-clean.mp4`, which has a silent track the voice replaces. Either say each line as it appears, in one take, and the cut stays in sync; or record one clip per line, named by its number (001 and up) under a directory named {name}, and rebuild with that directory as the voice, which times the cut to the recording.", yt_time(total));
        let mut ci = 0;
        for (i, c) in cues.iter().enumerate() {
            while ci < chapters.len() && chapters[ci].start <= c.start + 0.01 {
                let _ = write!(
                    b,
                    "\n## {} {}\n\n",
                    yt_time(chapters[ci].start),
                    chapters[ci].title
                );
                ci += 1;
            }
            let _ = writeln!(b, "- **{}** {:03} · {}", yt_time(c.start), i + 1, c.text);
        }
        b
    }
}
