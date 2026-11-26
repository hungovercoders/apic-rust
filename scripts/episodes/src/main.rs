//! Builds the course's YouTube episodes. Each lesson's shot list in
//! docs/learn/episodes/NN.yaml names the cards, the terminal clips (a VHS
//! tape each, recorded against the demo API from a scratch directory
//! holding the files the lesson has created by then) and the push-ins,
//! with the lines the voice says over each shot. Those lines set the
//! timing: a shot runs as long as they take to say, or as long as its
//! clip, whichever is longer, so a voice recorded afterwards to the
//! captions fits the cut. Given a voice directory (-voice) holding one
//! recorded clip per line, the shots are timed to the recording instead
//! and the clips go on the audio track, so the cut fits the voice rather
//! than the other way round. For each episode it writes, under
//! dist/episodes: the captioned MP4 with a silent track where the voice
//! goes, a clean MP4 without captions, their voiced copies when there is a
//! voice, the captions as SRT, the YouTube title, chapters and description
//! (from docs/learn/_episode-template.md), the narration script and the
//! thumbnail. Run it with `task episodes` (vhs, ttyd, ffmpeg and a
//! chromium on PATH); `task episodes:check` validates every shot list and
//! tape with vhs alone, recording nothing; `task episodes:voice` muxes a
//! narration recorded in one take onto an episode already built.

#![forbid(unsafe_code)]

mod cards;
mod episode;
#[cfg(test)]
mod tests;
mod video;
mod voice;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use apic_gocompat::FlagSet;
use apic_lessons::Block;

use cards::{find_chromium, render_card, render_thumb, screenshot};
use episode::{chapter_list, load_all, say_times, spread, Chapter, Cue, Episode, Shot};
use video::{
    ass, burn, compose_card, compose_clip, compose_zoom, concat, crop_term, duration, last_frame,
    srt, tape_head, with_silence, yt_time, CANVAS_H, CANVAS_W, TERM_H, TERM_W, WIDE_H, WIDE_W,
};
use voice::{load_voice, mux, with_voice, Voice, VOICE_PAD};

fn main() {
    let mut flags = FlagSet::new("episodes");
    flags.bool(
        "check",
        false,
        "validate every episode's shot list and tapes with vhs and build nothing",
    );
    flags.string(
        "only",
        "",
        "comma-separated episode names to build, e.g. 01,02 (default: all)",
    );
    flags.string(
        "out",
        "dist/episodes",
        "directory to write each episode into, under its name",
    );
    flags.bool(
        "keep",
        false,
        "keep each episode's scratch directory for inspection",
    );
    flags.string(
        "version",
        "dev",
        "version to stamp into the apic binary the episodes show",
    );
    flags.string("voice", "", "directory of recorded lines, one clip per cue under each episode's name (NN/001.wav and up); an episode with one is timed to it and written voiced as well");
    flags.string("mux", "", "a narration recorded in one take to the script's timestamps: write the voiced cuts of the one episode named by -only, already built under -out, and build nothing");
    flags.parse_env();
    let root = Path::new(".");
    let all = load_all(root).unwrap_or_else(|err| fail(&err));
    let eps =
        record::pick(&all, |e| e.name.clone(), flags.get("only")).unwrap_or_else(|err| fail(&err));
    let (out, voice, mux_file) = (
        Path::new(flags.get("out")),
        flags.get("voice"),
        flags.get("mux"),
    );
    if !mux_file.is_empty() {
        if eps.len() != 1 {
            fail("-mux takes one episode; name it with -only");
        }
        if !record::on_path("ffmpeg") {
            fail("ffmpeg is not on PATH; muxing needs it");
        }
        if let Err(err) = mux(&eps[0], out, Path::new(mux_file)) {
            fail(&err);
        }
        return;
    }
    if flags.get_bool("check") {
        let mut stdout = std::io::stdout();
        let mut n = 0;
        if !voice.is_empty() {
            n = check_voice(&eps, voice, &mut stdout);
        }
        n += check(&eps, &mut stdout);
        if n > 0 {
            fail(&format!("{n} of {} episodes are invalid", eps.len()));
        }
        return;
    }
    let keep = flags.get_bool("keep");
    if let Err(err) = build(root, &all, &eps, out, flags.get("version"), voice, keep) {
        fail(&err);
    }
}

/// Reads each episode's recorded lines, if it has any, and returns how
/// many episodes have a recording that does not match their lines. It
/// needs ffprobe for the clips' lengths.
fn check_voice(eps: &[Episode], dir: &str, w: &mut dyn Write) -> usize {
    if !record::on_path("ffprobe") {
        let _ = writeln!(
            w,
            "ffprobe is not on PATH; checking a voice directory needs it"
        );
        return eps.len();
    }
    let mut failed = 0;
    for ep in eps {
        match load_voice(dir, ep) {
            Err(err) => {
                let _ = writeln!(w, "{err}");
                failed += 1;
            }
            Ok(None) => {
                let _ = writeln!(w, "{}: no voice under {dir}", ep.name);
            }
            Ok(Some(v)) => {
                let _ = writeln!(
                    w,
                    "{}: voice ok ({} lines, {})",
                    ep.name,
                    v.clips.len(),
                    yt_time(v.total())
                );
            }
        }
    }
    failed
}

fn fail(err: &str) -> ! {
    eprintln!("episodes: {err}");
    std::process::exit(1);
}

/// Validates the shot lists (loading already did) and every clip's tape
/// with vhs, and returns how many episodes failed.
fn check(eps: &[Episode], w: &mut dyn Write) -> usize {
    if !record::on_path("vhs") {
        let _ = writeln!(w, "{VHS_HINT}");
        return eps.len();
    }
    let dir = match tempfile::Builder::new()
        .prefix("apic-episodes-check-")
        .tempdir()
    {
        Ok(dir) => dir,
        Err(err) => {
            let _ = writeln!(w, "{err}");
            return eps.len();
        }
    };
    let mut failed = 0;
    for ep in eps {
        let mut ok = true;
        for (i, s) in ep.shots.iter().enumerate() {
            if s.kind() != "clip" {
                continue;
            }
            let tape = dir.path().join(format!("clip-{i:02}-{}.tape", s.clip));
            if let Err(err) = write_tape(&tape, s) {
                let _ = writeln!(w, "{err}");
                ok = false;
                continue;
            }
            let (out, result) =
                record::combined_output(Command::new("vhs").arg("validate").arg(&tape));
            if let Err(err) = result {
                let _ = write!(
                    w,
                    "{}: clip {}: {err}\n{}",
                    ep.name,
                    s.clip,
                    String::from_utf8_lossy(&out)
                );
                ok = false;
            }
        }
        if ok {
            let _ = writeln!(w, "{}: ok ({} shots)", ep.name, ep.shots.len());
        } else {
            failed += 1;
        }
    }
    failed
}

/// The text of a clip's tape: the shared preamble, then its body. `name`
/// is the tape's file name, which names the recording too.
fn tape_text(name: &str, s: &Shot) -> String {
    let (w, h) = if s.wide {
        (WIDE_W, WIDE_H)
    } else {
        (TERM_W, TERM_H)
    };
    let mp4 = format!("{}.mp4", name.trim_end_matches(".tape"));
    format!("{}{}\n", tape_head(&mp4, w, h), s.tape.trim())
}

/// Writes a clip's tape.
fn write_tape(path: &Path, s: &Shot) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::fs::write(path, tape_text(&name, s))
        .map_err(|err| apic_gocompat::path_error("open", path, &err))
}

const VHS_HINT: &str = "vhs is not on PATH; checking needs vhs, building vhs, ttyd and ffmpeg (https://github.com/charmbracelet/vhs#installation) and a chromium";

/// Makes the picked episodes, out of all of them (the description links
/// each to the one before): one apic binary, then each from its own
/// scratch directory into `out`/NN, timed to its recorded lines under the
/// voice directory when it has them.
fn build(
    root: &Path,
    all: &[Episode],
    eps: &[Episode],
    out: &Path,
    version: &str,
    voice: &str,
    keep: bool,
) -> Result<(), String> {
    for tool in ["vhs", "ttyd", "ffmpeg", "ffprobe"] {
        if !record::on_path(tool) {
            return Err(format!("{tool} is not on PATH; building needs vhs, ttyd and ffmpeg (https://github.com/charmbracelet/vhs#installation) and a chromium"));
        }
    }
    let chromium = find_chromium()?;
    let bin = tempfile::Builder::new()
        .prefix("apic-episodes-bin-")
        .tempdir()
        .map_err(|err| err.to_string())?;
    record::build_apic(root, bin.path(), Some(version))?;
    // A recording that does not match its lines fails here, before any
    // episode spends minutes rendering.
    let mut voices: HashMap<&str, Option<Voice>> = HashMap::new();
    for ep in eps {
        voices.insert(&ep.name, load_voice(voice, ep)?);
    }
    for ep in eps {
        let prev = all.iter().rfind(|e| e.lesson == ep.lesson - 1);
        let voice = voices.remove(ep.name.as_str()).flatten();
        build_one(
            root,
            bin.path(),
            &chromium,
            ep,
            prev,
            voice,
            &out.join(&ep.name),
            keep,
        )
        .map_err(|err| format!("{}: {err}", ep.name))?;
    }
    Ok(())
}

/// One episode in progress: where it is made and written, the demo API
/// once it is up, and the cut so far.
struct Build<'a> {
    root: &'a Path,
    chromium: &'a Path,
    work: PathBuf,
    out: &'a Path,
    ep: &'a Episode,
    /// The lesson's runnable blocks, for the setups.
    blocks: Vec<Block>,
    env: Vec<String>,
    /// The demo API, once this process serves it.
    server: Option<apic_httpd::Server>,
    segments: Vec<PathBuf>,
    cues: Vec<Cue>,
    chapters: Vec<Chapter>,
    t: f64,
    /// The newest clip's segment, for a zoom.
    last_clip: PathBuf,
    /// Each clip's segment, for the thumbnail.
    clip_seg: HashMap<String, PathBuf>,
    /// The recorded lines, if there are any.
    voice: Option<Voice>,
    /// How many lines the shots so far have used.
    cue: usize,
}

/// Records, renders and cuts one episode.
#[allow(clippy::too_many_arguments)]
fn build_one(
    root: &Path,
    bin: &Path,
    chromium: &Path,
    ep: &Episode,
    prev: Option<&Episode>,
    voice: Option<Voice>,
    out: &Path,
    keep: bool,
) -> Result<(), String> {
    let io = |err: std::io::Error| err.to_string();
    let scratch = tempfile::Builder::new()
        .prefix(&format!("apic-episodes-{}-", ep.name))
        .tempdir()
        .map_err(io)?;
    let work = if keep {
        scratch.keep()
    } else {
        scratch.path().to_path_buf()
    };
    for d in [work.join("home"), work.join("cards"), out.to_path_buf()] {
        std::fs::create_dir_all(d).map_err(io)?;
    }
    let blocks = apic_lessons::blocks(root, &ep.name).map_err(|err| err.to_string())?;
    let mut env = record::environment(bin, &work);
    env.extend(cargo_caches());
    if let Some(voice) = &voice {
        println!(
            "{}: timing the cut to {} recorded lines ({})",
            ep.name,
            voice.clips.len(),
            yt_time(voice.total())
        );
    }
    let mut b = Build {
        root,
        chromium,
        work: work.clone(),
        out,
        ep,
        blocks,
        env,
        server: None,
        segments: Vec::new(),
        cues: Vec::new(),
        chapters: Vec::new(),
        t: 0.0,
        last_clip: PathBuf::new(),
        clip_seg: HashMap::new(),
        voice,
        cue: 0,
    };
    if ep.project {
        apic_demoapi::write_project(
            &work.join("apic-demo").to_string_lossy(),
            record::PORT,
            true,
        )?;
        b.server = Some(record::serve()?);
    }
    b.render_cards()?;
    for (i, s) in ep.shots.iter().enumerate() {
        b.shot(i, s)?;
    }
    b.write(prev)?;
    println!("{}: wrote {} ({})", ep.name, out.display(), yt_time(b.t));
    if keep {
        println!("{}: scratch directory kept at {}", ep.name, work.display());
    }
    Ok(())
}

impl Build<'_> {
    /// Draws every card shot to a PNG under the scratch directory.
    fn render_cards(&self) -> Result<(), String> {
        let cards = self.work.join("cards");
        for (i, s) in self.ep.shots.iter().enumerate() {
            let Some(card) = s.card.as_ref().filter(|_| s.kind() == "card") else {
                continue;
            };
            let html = cards.join(format!("{i:02}.html"));
            render_card(card, &html)?;
            screenshot(
                self.chromium,
                &html,
                &cards.join(format!("{i:02}.png")),
                CANVAS_W as u32,
                CANVAS_H as u32,
            )?;
        }
        println!("{}: cards rendered", self.ep.name);
        Ok(())
    }

    /// Makes the segment for one shot and adds it to the cut: its captions
    /// spread over its length, and a chapter when the chapter changes. The
    /// shot runs as long as its lines need, as said at a reading pace or
    /// as recorded, with a beat after each.
    fn shot(&mut self, i: usize, s: &Shot) -> Result<(), String> {
        let seg = self.work.join(format!("seg-{i:02}.mp4"));
        let mut times = say_times(&s.say);
        if let Some(voice) = &self.voice {
            for (j, time) in times.iter_mut().enumerate() {
                *time = voice.secs[self.cue + j] + VOICE_PAD;
            }
        }
        let want: f64 = times.iter().sum();
        match s.kind() {
            "clip" => self.record_clip(i, s, &seg, want)?,
            "card" => compose_card(
                &self.work.join("cards").join(format!("{i:02}.png")),
                &seg,
                want,
            )?,
            "zoom" => {
                let frame = self.work.join(format!("frame-{i:02}.png"));
                last_frame(&self.last_clip, &frame)?;
                compose_zoom(&frame, &seg, want)?;
            }
            _ => {}
        }
        let secs = duration(&seg)?;
        let mut cues = spread(self.t, secs, &s.say, &times);
        if let Some(voice) = &self.voice {
            for (j, cue) in cues.iter_mut().enumerate() {
                cue.audio = Some(voice.clips[self.cue + j].clone());
            }
        }
        self.cues.extend(cues);
        self.cue += s.say.len();
        if self.chapters.last().is_none_or(|c| c.title != s.chapter) {
            self.chapters.push(Chapter {
                start: self.t,
                title: s.chapter.clone(),
            });
        }
        self.segments.push(seg);
        self.t += secs;
        println!(
            "{}: {i:02} {:<5} {secs:6.1}s -> {}",
            self.ep.name,
            s.kind(),
            yt_time(self.t)
        );
        Ok(())
    }

    /// Runs a clip's setup, records its tape with vhs and puts the
    /// recording on the canvas, held to the length of its narration.
    fn record_clip(&mut self, i: usize, s: &Shot, seg: &Path, want: f64) -> Result<(), String> {
        self.setup(s)?;
        // An episode that does not start with the project has a clip that
        // runs `apic demo` on camera; vhs ends that server with the shell,
        // so from the next clip on this process serves the API.
        if self.work.join("apic-demo").exists() && self.server.is_none() {
            self.server = Some(record::serve()?);
        }
        let name = format!("clip-{i:02}-{}.tape", s.clip);
        let tape = self.work.join(&name);
        write_tape(&tape, s)?;
        println!("{}: recording {name}", self.ep.name);
        let mut vhs = Command::new("vhs");
        vhs.arg(&name)
            .current_dir(&self.work)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        record::set_env(&mut vhs, &self.env);
        match vhs.status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                return Err(format!(
                    "clip {}: vhs: {}",
                    s.clip,
                    record::exit_text(status)
                ))
            }
            Err(err) => return Err(format!("clip {}: vhs: {err}", s.clip)),
        }
        let mp4 = self.work.join(format!("clip-{i:02}-{}.mp4", s.clip));
        let clip_len = duration(&mp4)?;
        compose_clip(&mp4, seg, (want - clip_len).max(0.0), s.wide)?;
        self.last_clip = seg.to_path_buf();
        self.clip_seg.insert(s.clip.clone(), seg.to_path_buf());
        Ok(())
    }

    /// Runs a clip's setup steps in the scratch directory, off camera. A
    /// step sees the repository root as EPISODE_REPO, for a lesson that
    /// works on the example projects rather than the demo; the clips do
    /// not.
    fn setup(&self, s: &Shot) -> Result<(), String> {
        let root = std::path::absolute(self.root).map_err(|err| err.to_string())?;
        let mut env = self.env.clone();
        env.push(format!("EPISODE_REPO={}", root.display()));
        for st in &s.setup {
            let code = st.step(&self.ep.name).code(&self.blocks)?;
            record::shell(&self.work, &env, &code)
                .map_err(|err| format!("clip {}: {err}", s.clip))?;
        }
        Ok(())
    }

    /// Joins the segments into the clean and captioned cuts and writes
    /// everything else the episode ships with.
    fn write(&self, prev: Option<&Episode>) -> Result<(), String> {
        let ep = self.ep;
        let name = &ep.name;
        let all = self.work.join("all.mp4");
        let clean = self.out.join(format!("{name}-clean.mp4"));
        concat(&self.segments, &self.work.join("segments.txt"), &all)?;
        with_silence(&all, &clean)?;
        let captions = self.work.join("captions.ass");
        std::fs::write(&captions, ass(&self.cues)).map_err(|err| err.to_string())?;
        burn(&clean, &captions, &self.out.join(format!("{name}.mp4")))?;
        if self.voice.is_some() {
            let voiced = self.out.join(format!("{name}-clean-voiced.mp4"));
            with_voice(&all, &self.cues, &voiced)?;
            burn(
                &voiced,
                &captions,
                &self.out.join(format!("{name}-voiced.mp4")),
            )?;
        }
        let shot = self.work.join("shot.png");
        let thumb = self.work.join("thumb.html");
        let framed = self
            .clip_seg
            .get(&ep.thumbnail.clip)
            .cloned()
            .unwrap_or_default();
        crop_term(&framed, &shot)?;
        render_thumb(ep, &shot, &thumb)?;
        screenshot(
            self.chromium,
            &thumb,
            &self.out.join(format!("{name}-thumbnail.png")),
            1280,
            720,
        )?;
        let description = ep.description(self.root, &self.chapters, prev)?;
        for (suffix, text) in [
            (".srt", srt(&self.cues)),
            ("-title.txt", format!("{}\n", ep.video_title())),
            ("-chapters.txt", chapter_list(&self.chapters)),
            ("-description.md", description),
            (
                "-narration.md",
                ep.narration(&self.cues, &self.chapters, self.t),
            ),
        ] {
            std::fs::write(self.out.join(format!("{name}{suffix}")), text)
                .map_err(|err| err.to_string())?;
        }
        Ok(())
    }
}

/// Keeps the recording machine's cargo and rustup directories in force
/// under the recording's own HOME, so a clip that builds or tests apic in
/// a clone (lesson 13) finds its toolchain and the crates already
/// downloaded: the recording's environment has no proxy to fetch them
/// through.
fn cargo_caches() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")]
        .iter()
        .filter_map(|(name, dir)| {
            let value = std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| (!home.is_empty()).then(|| format!("{home}/{dir}")))?;
            Some(format!("{name}={value}"))
        })
        .collect()
}
