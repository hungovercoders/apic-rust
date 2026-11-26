//! The picture. Terminal clips are recorded at the template's shape, 100
//! columns by 31 lines at font size 16, which VHS draws at 1150x720 with a
//! 60px pad; most of the pad is cropped and the rest scaled up onto a
//! 1080p canvas, leaving a band at the bottom for the captions. VHS at
//! that font size reports 105 columns for 1200px, which gives the cell
//! size.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use apic_gocompat::format_float_g;

use crate::episode::Cue;

pub(crate) const FPS: i64 = 30;
pub(crate) const CANVAS_W: i64 = 1920;
pub(crate) const CANVAS_H: i64 = 1080;
pub(crate) const TERM_W: i64 = 1150;
pub(crate) const TERM_H: i64 = 720;
// apic list is 188 columns wide and 31 lines long; this holds it
// unwrapped, with the command and prompt around it.
pub(crate) const WIDE_W: i64 = 2100;
pub(crate) const WIDE_H: i64 = 800;
const PAD: i64 = 60;
const CROP: i64 = 40;
const TERM_SCALE: f64 = 1.5;
const TERM_Y: i64 = 16;
const ZOOM_MAX: f64 = 1.7;
pub(crate) const BG: &str = "#1e1e2e";

const CELL_W: f64 = (1200 - 2 * PAD) as f64 / 105.0;
const CELL_H: f64 = 600.0 / 31.0;

/// Where a clip lands on the canvas: its scaled size and offset, as
/// (width, height, x, y).
fn term_box() -> (i64, i64, i64, i64) {
    let w = ((TERM_W - 2 * CROP) as f64 * TERM_SCALE) as i64 / 2 * 2;
    let h = ((TERM_H - 2 * CROP) as f64 * TERM_SCALE) as i64 / 2 * 2;
    (w, h, (CANVAS_W - w) / 2, TERM_Y)
}

/// How many terminal cells the push-in shows at full zoom, as (columns,
/// rows).
pub(crate) fn zoom_window() -> (i64, i64) {
    (
        ((CANVAS_W as f64 / ZOOM_MAX - 30.0) / (CELL_W * TERM_SCALE)) as i64,
        ((CANVAS_H as f64 / ZOOM_MAX - 30.0) / (CELL_H * TERM_SCALE)) as i64,
    )
}

/// Every clip's preamble; the body follows it.
pub(crate) fn tape_head(output: &str, width: i64, height: i64) -> String {
    format!(
        "Output {output}
Set Shell \"bash\"
Set FontSize 16
Set Width {width}
Set Height {height}
Set Theme \"Catppuccin Mocha\"
Set TypingSpeed 40ms
Set Framerate 30
Set Padding 60
"
    )
}

/// A path as an argument.
fn arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub(crate) fn ffmpeg(args: &[String]) -> Result<(), String> {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status();
    let failed = match status {
        Ok(status) if status.success() => return Ok(()),
        Ok(status) => record::exit_text(status),
        Err(err) => err.to_string(),
    };
    Err(format!("ffmpeg {}: {failed}", args.join(" ")))
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// A media file's length in seconds.
pub(crate) fn duration(path: &Path) -> Result<f64, String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let failed = match out {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let text = text.trim();
            return text
                .parse::<f64>()
                .map_err(|_| format!("ffprobe {}: {text:?} is not a length", path.display()));
        }
        Ok(out) => record::exit_text(out.status),
        Err(err) => err.to_string(),
    };
    Err(format!("ffprobe {}: {failed}", path.display()))
}

fn encode_args() -> Vec<String> {
    strings(&[
        "-r",
        &FPS.to_string(),
        "-c:v",
        "libx264",
        "-preset",
        "medium",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        "-an",
    ])
}

/// The arguments for an encode: what is particular to it, the shared
/// encoding settings, then where it goes.
fn encode(first: Vec<String>, dst: &Path) -> Vec<String> {
    let mut args = first;
    args.extend(encode_args());
    args.push(arg(dst));
    args
}

/// The filter that puts a terminal recording on the canvas and holds its
/// last frame for `hold` seconds.
fn clip_filter(hold: f64, wide: bool) -> String {
    let (cw, ch, sw, sh, x, y);
    if wide {
        (cw, ch) = (WIDE_W - 2 * CROP, WIDE_H - 2 * CROP);
        sw = CANVAS_W - 80;
        sh = (ch as f64 * sw as f64 / cw as f64) as i64 / 2 * 2;
        (x, y) = (40, 170);
    } else {
        (cw, ch) = (TERM_W - 2 * CROP, TERM_H - 2 * CROP);
        (sw, sh, x, y) = term_box();
    }
    format!("fps={FPS},crop={cw}:{ch}:{CROP}:{CROP},scale={sw}:{sh}:flags=lanczos,pad={CANVAS_W}:{CANVAS_H}:{x}:{y}:color={BG},tpad=stop_mode=clone:stop_duration={hold:.3}")
}

/// Puts a terminal recording on the canvas and holds its last frame for
/// `hold` seconds, so the shot lasts as long as its narration.
pub(crate) fn compose_clip(src: &Path, dst: &Path, hold: f64, wide: bool) -> Result<(), String> {
    ffmpeg(&encode(
        vec!["-i".into(), arg(src), "-vf".into(), clip_filter(hold, wide)],
        dst,
    ))
}

/// Writes a clip's final frame as a PNG.
pub(crate) fn last_frame(src: &Path, png: &Path) -> Result<(), String> {
    let mut args = strings(&["-sseof", "-0.1", "-i"]);
    args.push(arg(src));
    args.extend(strings(&["-frames:v", "1", "-update", "1"]));
    args.push(arg(png));
    ffmpeg(&args)
}

/// The push-in's filter and how many frames it runs for.
fn zoom_filter(secs: f64) -> (String, i64) {
    let (_, _, tx, ty) = term_box();
    let px = tx as f64 + (PAD - CROP) as f64 * TERM_SCALE - 30.0;
    let py = ty as f64 + (PAD - CROP) as f64 * TERM_SCALE - 30.0;
    let n = (secs * FPS as f64) as i64;
    let ramp = FPS * 2;
    let max = format_float_g(ZOOM_MAX);
    let z = format!("min(1+({max}-1)*on/{ramp},{max})");
    (
        format!("zoompan=z='{z}':x='{px:.1}*(1-1/zoom)/(1-1/{max})':y='{py:.1}*(1-1/zoom)/(1-1/{max})':d={n}:s={CANVAS_W}x{CANVAS_H}:fps={FPS}"),
        n,
    )
}

/// Pushes in on a canvas-sized frame over two seconds and holds. The
/// window is anchored at the top-left of the terminal's content, so the
/// command line stays in frame and the zoom reads as a push-in, not a
/// search; the episode's validation checks the target cell fits the
/// window.
pub(crate) fn compose_zoom(frame: &Path, dst: &Path, secs: f64) -> Result<(), String> {
    let (vf, n) = zoom_filter(secs);
    ffmpeg(&encode(
        vec![
            "-loop".into(),
            "1".into(),
            "-i".into(),
            arg(frame),
            "-vf".into(),
            vf,
            "-frames:v".into(),
            n.to_string(),
        ],
        dst,
    ))
}

/// Shows a rendered card for `secs`.
pub(crate) fn compose_card(png: &Path, dst: &Path, secs: f64) -> Result<(), String> {
    ffmpeg(&encode(
        vec![
            "-loop".into(),
            "1".into(),
            "-framerate".into(),
            FPS.to_string(),
            "-i".into(),
            arg(png),
            "-t".into(),
            format!("{secs:.3}"),
            "-vf".into(),
            format!("fps={FPS},format=yuv420p"),
        ],
        dst,
    ))
}

/// Joins same-encoded segments without re-encoding.
pub(crate) fn concat(
    list: &[std::path::PathBuf],
    list_file: &Path,
    dst: &Path,
) -> Result<(), String> {
    let mut b = String::new();
    for p in list {
        let _ = writeln!(b, "file '{}'", p.display());
    }
    std::fs::write(list_file, b).map_err(|err| err.to_string())?;
    let mut args = strings(&["-f", "concat", "-safe", "0", "-i"]);
    args.push(arg(list_file));
    args.extend(strings(&["-c", "copy"]));
    args.push(arg(dst));
    ffmpeg(&args)
}

/// Adds a silent stereo track, so a voice recorded to the captions drops
/// straight in and players treat the file as having audio.
pub(crate) fn with_silence(src: &Path, dst: &Path) -> Result<(), String> {
    let mut args = vec!["-i".to_string(), arg(src)];
    args.extend(strings(&[
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=48000:cl=stereo",
        "-shortest",
        "-c:v",
        "copy",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        "-movflags",
        "+faststart",
    ]));
    args.push(arg(dst));
    ffmpeg(&args)
}

/// Renders the captions into the picture.
pub(crate) fn burn(src: &Path, ass: &Path, dst: &Path) -> Result<(), String> {
    let mut args = vec![
        "-i".to_string(),
        arg(src),
        "-vf".to_string(),
        format!("ass={}", ass.display()),
    ];
    args.extend(strings(&[
        "-c:v",
        "libx264",
        "-preset",
        "medium",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "copy",
        "-movflags",
        "+faststart",
    ]));
    args.push(arg(dst));
    ffmpeg(&args)
}

/// Cuts the terminal out of a composed segment's last frame, for the
/// thumbnail.
pub(crate) fn crop_term(src: &Path, png: &Path) -> Result<(), String> {
    let (w, h, x, y) = term_box();
    let mut args = strings(&["-sseof", "-0.1", "-i"]);
    args.push(arg(src));
    args.extend(strings(&["-frames:v", "1", "-update", "1", "-vf"]));
    args.push(format!("crop={w}:{h}:{x}:{y}"));
    args.push(arg(png));
    ffmpeg(&args)
}

pub(crate) fn srt_time(t: f64) -> String {
    let ms = (t * 1000.0).round() as i64;
    let (h, ms) = (ms / 3_600_000, ms % 3_600_000);
    let (m, ms) = (ms / 60_000, ms % 60_000);
    let (s, ms) = (ms / 1000, ms % 1000);
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

pub(crate) fn ass_time(t: f64) -> String {
    let cs = (t * 100.0).round() as i64;
    let (h, cs) = (cs / 360_000, cs % 360_000);
    let (m, cs) = (cs / 6000, cs % 6000);
    let (s, cs) = (cs / 100, cs % 100);
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

/// The m:ss YouTube's chapter list reads.
pub(crate) fn yt_time(t: f64) -> String {
    let s = t as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The captions as a sidecar file.
pub(crate) fn srt(cues: &[Cue]) -> String {
    let mut b = String::new();
    for (i, c) in cues.iter().enumerate() {
        let _ = write!(
            b,
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_time(c.start),
            srt_time(c.end),
            c.text
        );
    }
    b
}

/// The captions styled for burning in: Inter at the bottom of the canvas,
/// in the docs theme's text colour on a translucent box of the background
/// colour, so a caption stays readable over a push-in whose output reaches
/// the bottom of the frame.
pub(crate) fn ass(cues: &[Cue]) -> String {
    let mut b = String::new();
    b.push_str(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nWrapStyle: 0\n\n",
    );
    b.push_str("[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    b.push_str("Style: Cap,Inter,40,&H00F4D6CD,&H00FFFFFF,&H502E1E1E,&H502E1E1E,0,0,0,0,100,100,0,0,3,14,0,2,140,140,34,1\n\n");
    b.push_str("[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    for c in cues {
        let _ = writeln!(
            b,
            "Dialogue: 0,{},{},Cap,,0,0,0,,{}",
            ass_time(c.start),
            ass_time(c.end),
            c.text
        );
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The geometry the filters are built from, and the filters as ffmpeg
    /// is handed them.
    #[test]
    fn geometry_and_filters() {
        assert_eq!(term_box(), (1604, 960, 158, 16));
        assert_eq!(zoom_window(), (71, 20));
        assert_eq!(
            clip_filter(1.5, false),
            "fps=30,crop=1070:640:40:40,scale=1604:960:flags=lanczos,pad=1920:1080:158:16:color=#1e1e2e,tpad=stop_mode=clone:stop_duration=1.500"
        );
        assert_eq!(
            clip_filter(0.0, true),
            "fps=30,crop=2020:720:40:40,scale=1840:654:flags=lanczos,pad=1920:1080:40:170:color=#1e1e2e,tpad=stop_mode=clone:stop_duration=0.000"
        );
        assert_eq!(
            zoom_filter(4.0),
            (
                "zoompan=z='min(1+(1.7-1)*on/60,1.7)':x='158.0*(1-1/zoom)/(1-1/1.7)':y='16.0*(1-1/zoom)/(1-1/1.7)':d=120:s=1920x1080:fps=30".to_string(),
                120
            )
        );
        assert!(tape_head("a.mp4", 1150, 720)
            .starts_with("Output a.mp4\nSet Shell \"bash\"\nSet FontSize 16\nSet Width 1150\n"));
    }

    /// The captions in both forms.
    #[test]
    fn caption_files() {
        let cues = [Cue {
            start: 0.0,
            end: 2.5,
            text: "one".to_string(),
            audio: None,
        }];
        assert_eq!(srt(&cues), "1\n00:00:00,000 --> 00:00:02,500\none\n\n");
        assert!(ass(&cues).ends_with("Dialogue: 0,0:00:00.00,0:00:02.50,Cap,,0,0,0,,one\n"));
    }
}
