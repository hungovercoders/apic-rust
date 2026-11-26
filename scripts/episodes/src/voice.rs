//! A recorded voice. Without one, a shot runs as long as its lines take to
//! say at a reading pace, and the narration is recorded afterwards to the
//! script's timestamps. With one, the recording comes first and the cut is
//! timed to it: a directory holds one clip per line, under the episode's
//! name and numbered like the SRT's cues (three digits, so the first line
//! of lesson 4 is 001 under 04, in any format ffmpeg reads), which suits
//! recording line by line from the narration script and a text-to-speech
//! tool that writes one file a line. Each shot then runs as long as its
//! clips plus a beat after each, or as long as its terminal clip,
//! whichever is longer, and the clips land on the audio track at their
//! cues' starts.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use apic_gocompat::quote;

use crate::episode::{Cue, Episode};
use crate::video::{duration, ffmpeg};

/// The beat left after each recorded line.
pub(crate) const VOICE_PAD: f64 = 0.7;

/// An episode's narration as one clip per line, in cue order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Voice {
    /// Each line's clip.
    pub clips: Vec<PathBuf>,
    /// And how long it runs.
    pub secs: Vec<f64>,
}

/// Reads the clips for one episode from `dir`/NN, measuring each with
/// ffprobe.
pub(crate) fn load_voice(dir: &str, ep: &Episode) -> Result<Option<Voice>, String> {
    load_voice_with(dir, ep, &duration)
}

/// Reads the clips for one episode from `dir`/NN: every file named by a
/// cue number, in whatever format, since `probe` (a clip's length in
/// seconds; tests stand in for ffprobe) decides what ffmpeg can read and
/// fails on what it cannot. No such directory means no voice, and the
/// episode builds silent as it would without one; a directory with a clip
/// missing, two for one cue, or one past the last line fails naming the
/// cue and its line, so a half-recorded lesson is obvious before anything
/// is rendered.
pub(crate) fn load_voice_with(
    dir: &str,
    ep: &Episode,
    probe: &dyn Fn(&Path) -> Result<f64, String>,
) -> Result<Option<Voice>, String> {
    if dir.is_empty() {
        return Ok(None);
    }
    let ep_dir = Path::new(dir).join(&ep.name);
    let mut names: Vec<String> = match std::fs::read_dir(&ep_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| !e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(apic_gocompat::path_error("open", &ep_dir, &err)),
    };
    names.sort();
    let lines = ep.lines();
    let shown = ep_dir.display();
    let mut by_cue: BTreeMap<usize, String> = BTreeMap::new();
    let mut errs: Vec<String> = Vec::new();
    for name in names {
        let stem = match name.rfind('.') {
            Some(dot) => &name[..dot],
            None => name.as_str(),
        };
        let Some(n) = stem.parse::<usize>().ok().filter(|n| *n >= 1) else {
            continue;
        };
        if let Some(prev) = by_cue.get(&n) {
            errs.push(format!(
                "{shown}: two clips for cue {n:03}, {prev} and {name}"
            ));
        } else if n > lines.len() {
            errs.push(format!(
                "{shown}: {name} is cue {n:03}, past the last line; the episode has {}",
                lines.len()
            ));
        } else {
            by_cue.insert(n, name);
        }
    }
    let mut v = Voice::default();
    for (i, line) in lines.iter().enumerate() {
        let Some(name) = by_cue.get(&(i + 1)) else {
            errs.push(format!(
                "{shown}: no clip {:03} for the line {}",
                i + 1,
                quote(line)
            ));
            continue;
        };
        let p = ep_dir.join(name);
        match probe(&p) {
            Ok(secs) => {
                v.clips.push(p);
                v.secs.push(secs);
            }
            Err(err) => errs.push(err),
        }
    }
    if !errs.is_empty() {
        return Err(errs.join("\n"));
    }
    Ok(Some(v))
}

impl Voice {
    /// How long the recorded lines run with their beats.
    pub fn total(&self) -> f64 {
        self.secs.iter().map(|s| s + VOICE_PAD).sum()
    }
}

/// The arguments that put the recorded lines on a cut's audio track.
fn voice_args(src: &Path, cues: &[Cue], dst: &Path) -> Result<Vec<String>, String> {
    let mut args = vec!["-i".to_string(), src.to_string_lossy().into_owned()];
    let (mut filter, mut mix) = (String::new(), String::new());
    let mut n = 0;
    for c in cues {
        let Some(audio) = &c.audio else { continue };
        n += 1;
        args.extend(["-i".to_string(), audio.to_string_lossy().into_owned()]);
        let _ = write!(
            filter,
            "[{n}:a]aformat=sample_rates=48000:channel_layouts=stereo,adelay={}:all=1[a{n}];",
            (c.start * 1000.0).round() as i64
        );
        let _ = write!(mix, "[a{n}]");
    }
    if n == 0 {
        return Err("no recorded lines to mix".to_string());
    }
    let _ = write!(
        filter,
        "{mix}amix=inputs={n}:normalize=0:dropout_transition=0,apad[voice]"
    );
    args.push("-filter_complex".to_string());
    args.push(filter);
    args.extend(
        [
            "-map",
            "0:v",
            "-map",
            "[voice]",
            "-shortest",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-movflags",
            "+faststart",
        ]
        .map(str::to_string),
    );
    args.push(dst.to_string_lossy().into_owned());
    Ok(args)
}

/// Puts the recorded lines on the audio track of a cut, each at its cue's
/// start, and writes the result. The clips are brought to one format and
/// mixed without scaling, so each plays at the level it was recorded; the
/// track is padded to the picture.
pub(crate) fn with_voice(src: &Path, cues: &[Cue], dst: &Path) -> Result<(), String> {
    ffmpeg(&voice_args(src, cues, dst)?)
}

/// Replaces a cut's audio with a narration recorded in one take to the
/// script's timestamps, padded to the picture so a take that ends early
/// does not cut the video short.
pub(crate) fn mux_track(video: &Path, audio: &Path, dst: &Path) -> Result<(), String> {
    let mut args = vec![
        "-i".to_string(),
        video.to_string_lossy().into_owned(),
        "-i".to_string(),
        audio.to_string_lossy().into_owned(),
    ];
    args.extend(
        [
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-af",
            "aformat=sample_rates=48000:channel_layouts=stereo,apad",
            "-shortest",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-movflags",
            "+faststart",
        ]
        .map(str::to_string),
    );
    args.push(dst.to_string_lossy().into_owned());
    ffmpeg(&args)
}

/// Writes the voiced cuts of an already built episode from one narration
/// file: NN-clean-voiced.mp4 and NN-voiced.mp4 beside the silent
/// NN-clean.mp4 and NN.mp4 under `out`/NN.
pub(crate) fn mux(ep: &Episode, out: &Path, audio: &Path) -> Result<(), String> {
    let dir = out.join(&ep.name);
    let name = &ep.name;
    for cut in ["-clean", ""] {
        let src = dir.join(format!("{name}{cut}.mp4"));
        if let Err(err) = std::fs::metadata(&src) {
            return Err(format!(
                "{} is not built; run the build first: {}",
                src.display(),
                apic_gocompat::path_error("stat", &src, &err)
            ));
        }
        mux_track(&src, audio, &dir.join(format!("{name}{cut}-voiced.mp4")))?;
    }
    println!(
        "{name}: wrote {name}-clean-voiced.mp4 and {name}-voiced.mp4 in {}",
        dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episode::Shot;

    fn shot(say: &[&str]) -> Shot {
        Shot {
            say: say.iter().map(|s| s.to_string()).collect(),
            ..Shot::default()
        }
    }

    fn base(p: &Path) -> String {
        p.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// A voice directory is read by cue number, whatever the clip format,
    /// and a missing, duplicated or surplus clip is named with its line;
    /// files not named by a number are left alone, and no directory is no
    /// voice.
    #[test]
    fn load_voice_by_cue() {
        let ep = Episode {
            name: "04".to_string(),
            shots: vec![shot(&["first line", "second line"]), shot(&["third line"])],
            ..Episode::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let dir_arg = dir.path().to_string_lossy().into_owned();
        let probe = |_: &Path| -> Result<f64, String> { Ok(1.5) };
        let load = |dir: &str| load_voice_with(dir, &ep, &probe);
        assert_eq!(load(&dir_arg), Ok(None), "no directory should be no voice");
        assert_eq!(load(""), Ok(None), "no -voice should be no voice");
        let ep_dir = dir.path().join("04");
        std::fs::create_dir_all(&ep_dir).unwrap();
        let write = |name: &str| std::fs::write(ep_dir.join(name), "x").unwrap();
        write("001.wav");
        write("002.flac");
        write("notes.txt");
        let err = load(&dir_arg).unwrap_err();
        assert!(
            err.contains("no clip 003") && err.contains("third line"),
            "a missing clip should name its cue and line, got {err}"
        );
        write("003.aiff");
        let v = load(&dir_arg).unwrap().expect("a voice");
        let total = v.total();
        assert!(
            v.clips.len() == 3
                && base(&v.clips[1]) == "002.flac"
                && base(&v.clips[2]) == "003.aiff"
                && v.secs[2] == 1.5
                && total > 6.59
                && total < 6.61,
            "voice: {v:?} (total {total})"
        );
        write("004.m4a");
        let err = load(&dir_arg).unwrap_err();
        assert!(
            err.contains("past the last line"),
            "a surplus clip should fail, got {err}"
        );
        std::fs::remove_file(ep_dir.join("004.m4a")).unwrap();
        write("2.wav");
        let err = load(&dir_arg).unwrap_err();
        assert!(
            err.contains("two clips for cue 002"),
            "two clips for one cue should fail, got {err}"
        );
    }

    /// The recorded lines are delayed to their cues and mixed as they
    /// were recorded; a cut with none has nothing to mix.
    #[test]
    fn voice_arguments() {
        let cue = |start: f64, audio: Option<&str>| Cue {
            start,
            end: start + 1.0,
            text: "x".to_string(),
            audio: audio.map(PathBuf::from),
        };
        let cues = [
            cue(0.2, Some("001.wav")),
            cue(1.0, None),
            cue(1.5, Some("002.wav")),
        ];
        let args = voice_args(Path::new("cut.mp4"), &cues, Path::new("out.mp4")).unwrap();
        assert_eq!(
            args[..6],
            ["-i", "cut.mp4", "-i", "001.wav", "-i", "002.wav"]
        );
        assert_eq!(
            args[7],
            "[1:a]aformat=sample_rates=48000:channel_layouts=stereo,adelay=200:all=1[a1];[2:a]aformat=sample_rates=48000:channel_layouts=stereo,adelay=1500:all=1[a2];[a1][a2]amix=inputs=2:normalize=0:dropout_transition=0,apad[voice]"
        );
        assert_eq!(args.last().map(String::as_str), Some("out.mp4"));
        assert_eq!(
            voice_args(Path::new("cut.mp4"), &cues[1..2], Path::new("out.mp4")),
            Err("no recorded lines to mix".to_string())
        );
    }

    /// With ffmpeg on hand, recorded lines land on a cut's audio track at
    /// their cues and the track runs the length of the picture; a one-take
    /// narration muxes the same way.
    #[test]
    fn voice_tracks() {
        if !record::on_path("ffmpeg") || !record::on_path("ffprobe") {
            return; // ffmpeg is not on PATH
        }
        let dir = tempfile::tempdir().unwrap();
        let strings =
            |args: &[&str]| -> Vec<String> { args.iter().map(|a| a.to_string()).collect() };
        let video = dir.path().join("cut.mp4");
        let mut args = strings(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=10",
            "-t",
            "3",
            "-pix_fmt",
            "yuv420p",
        ]);
        args.push(video.to_string_lossy().into_owned());
        ffmpeg(&args).unwrap();
        let clip = |name: &str, secs: &str| -> PathBuf {
            let p = dir.path().join(name);
            let mut args = strings(&[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=22050",
                "-t",
                secs,
            ]);
            args.push(p.to_string_lossy().into_owned());
            ffmpeg(&args).unwrap();
            p
        };
        let cue = |start: f64, text: &str, audio: PathBuf| Cue {
            start,
            end: 0.0,
            text: text.to_string(),
            audio: Some(audio),
        };
        let cues = [
            cue(0.2, "a", clip("001.wav", "0.5")),
            cue(1.5, "b", clip("002.wav", "0.5")),
        ];
        let voiced = dir.path().join("voiced.mp4");
        with_voice(&video, &cues, &voiced).unwrap();
        let secs = duration(&voiced).unwrap();
        assert!(
            (2.9..=3.2).contains(&secs),
            "the voiced cut runs {secs}, want the picture's 3s"
        );
        let streams = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_entries",
                "stream=index",
                "-of",
                "csv=p=0",
            ])
            .arg(&voiced)
            .output()
            .unwrap();
        let n = String::from_utf8_lossy(&streams.stdout)
            .split_whitespace()
            .count();
        assert_eq!(n, 1, "the voiced cut has {n} audio streams, want 1");
        let muxed = dir.path().join("muxed.mp4");
        mux_track(&video, &clip("take.wav", "1"), &muxed).unwrap();
        let secs = duration(&muxed).unwrap();
        assert!(
            (2.9..=3.2).contains(&secs),
            "the muxed cut runs {secs}, want the picture's 3s, not the take's 1s"
        );
    }
}
