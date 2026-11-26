//! Turns a terminal frame (lines of text carrying SGR escapes) into a
//! standalone SVG drawn inside a window, so a screenshot in the docs is the
//! bytes apic really writes to a terminal rather than a drawing of them.

use std::fmt::Write as _;
use std::time::Duration;

use apic_output::style::{string_width, strip};

// Geometry of the window, in SVG user units. The character advance is the
// one DejaVu Sans Mono uses at this size, and every character is placed on
// it explicitly, so the grid holds whichever monospace font the reader has.
const FONT_SIZE: f64 = 13.0;
const CELL_W: f64 = FONT_SIZE * 0.60129;
const LINE_H: f64 = 18.0;
const PAD_X: f64 = 18.0;
const BAR_H: f64 = 38.0;
/// The baseline of the first line of text.
const FIRST_BASE: f64 = 60.0;
const BOTTOM_MARGIN: f64 = 14.0;

// Window colours, matching the docs theme.
const WINDOW_BG: &str = "#11131a";
const BAR_BG: &str = "#1b1e28";
const TITLE_FG: &str = "#8b93a7";
const DEFAULT_FG: &str = "#d6dae4";
const FAINT_OPACITY: &str = "0.55";

/// The SGR state in force for a run of characters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Style {
    /// Empty means the default.
    fg: String,
    bg: String,
    bold: bool,
    faint: bool,
    underline: bool,
}

/// A stretch of characters sharing one style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Run {
    /// The starting column.
    col: usize,
    /// The columns it occupies.
    width: usize,
    text: String,
    style: Style,
}

/// Renders `frame` (a whole terminal screen, lines separated by `\n`) as
/// an SVG window titled with the command that produced it.
#[cfg(test)]
pub fn svg(title: &str, frame: &str) -> String {
    animated_svg(
        title,
        &[Frame {
            text: frame.to_string(),
            ..Frame::default()
        }],
    )
}

/// One screen of an animation: what the terminal showed, how long to hold
/// it, and a caption drawn in the title bar (the key that was pressed,
/// say). A zero `hold` means the frame is static.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub text: String,
    pub hold: Duration,
    pub caption: String,
}

/// Renders frames as one SVG window that cycles through them with SMIL
/// timing, which browsers run even inside an `<img>`, so the README and
/// the docs get a moving picture with no GIF, no video and no script. It
/// loops: a single pass clocked from page load would often be over before
/// a reader scrolled to it. The style block inside the SVG shows only the
/// last frame under prefers-reduced-motion for browsers that evaluate the
/// media query inside an image document (`!important` is what beats a
/// SMIL-animated opacity); the pages do not rely on that and serve the
/// poster through a `<picture>` instead (see `main.rs`). A viewer with no
/// SMIL at all sees the first frame. With a single frame the output is a
/// plain static screenshot.
pub fn animated_svg(title: &str, frames: &[Frame]) -> String {
    let mut cols = 0;
    let mut rows = 0;
    let mut parsed: Vec<Vec<Vec<Run>>> = Vec::with_capacity(frames.len());
    for fr in frames {
        let lines: Vec<&str> = fr.text.trim_end_matches('\n').split('\n').collect();
        rows = rows.max(lines.len());
        let mut screen = Vec::with_capacity(lines.len());
        for line in lines {
            screen.push(split(line));
            // The full width, trailing spaces and all: a UI frame pads
            // every line to the terminal width, and the window has to be
            // that wide even though blank runs are never drawn.
            cols = cols.max(string_width(&strip(line)));
        }
        parsed.push(screen);
    }
    cols = cols.max(string_width(title) + 8);
    let width = round_to(2.0 * PAD_X + cols as f64 * CELL_W);
    let height = round_to(FIRST_BASE + (rows as f64 - 1.0) * LINE_H + BOTTOM_MARGIN);

    let total: Duration = frames.iter().map(|fr| fr.hold).sum();
    let animated = frames.len() > 1 && !total.is_zero();

    let mut b = String::new();
    let _ = writeln!(
        b,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" \
         font-family=\"ui-monospace,SFMono-Regular,Menlo,Consolas,monospace\" \
         font-size=\"{FONT_SIZE}\">"
    );
    if animated {
        b.push_str(
            "<style>@media (prefers-reduced-motion: reduce) { .frame { display: none } \
             .frame.rest { display: inline; opacity: 1 !important } }</style>\n",
        );
    }
    let _ = writeln!(
        b,
        "<rect width=\"{width}\" height=\"{height}\" rx=\"10\" fill=\"{WINDOW_BG}\"/>"
    );
    let _ = writeln!(
        b,
        "<rect width=\"{width}\" height=\"{BAR_H}\" rx=\"10\" fill=\"{BAR_BG}\"/>"
    );
    let _ = writeln!(
        b,
        "<rect y=\"{}\" width=\"{width}\" height=\"10\" fill=\"{BAR_BG}\"/>",
        BAR_H - 10.0
    );
    for (i, c) in ["#ff5f57", "#febc2e", "#28c840"].iter().enumerate() {
        let _ = writeln!(
            b,
            "<circle cx=\"{}\" cy=\"19\" r=\"5.5\" fill=\"{c}\"/>",
            18 + i * 17
        );
    }
    let _ = writeln!(
        b,
        "<text x=\"{}\" y=\"23.5\" fill=\"{TITLE_FG}\" text-anchor=\"middle\" font-size=\"12\">{}</text>",
        width as f64 / 2.0,
        escape(title)
    );

    let mut at = Duration::ZERO;
    let last = frames.len().saturating_sub(1);
    for (f, fr) in frames.iter().enumerate() {
        if animated {
            // Each frame is a group whose opacity steps to 1 for its slot
            // of the loop and back to 0, with discrete timing so nothing
            // fades. keyTimes are fractions of the whole loop.
            // Every keyTimes list starts at 0 and ends at 1, which every
            // reading of the SMIL spec accepts; the value at 1 repeats the
            // last slot so the loop's final instant draws the right frame.
            let from = at.as_secs_f64() / total.as_secs_f64();
            let to = (at + fr.hold).as_secs_f64() / total.as_secs_f64();
            let (values, key_times, initial) = if f == 0 {
                ("1;0;0", format!("0;{};1", frac(to)), "1")
            } else if f == last {
                ("0;1;1", format!("0;{};1", frac(from)), "0")
            } else {
                ("0;1;0;0", format!("0;{};{};1", frac(from), frac(to)), "0")
            };
            let class = if f == last { "frame rest" } else { "frame" };
            let _ = writeln!(
                b,
                "<g class=\"{class}\" opacity=\"{initial}\"><animate attributeName=\"opacity\" \
                 calcMode=\"discrete\" values=\"{values}\" keyTimes=\"{key_times}\" dur=\"{}s\" \
                 repeatCount=\"indefinite\"/>",
                frac(total.as_secs_f64())
            );
            at += fr.hold;
        }
        if !fr.caption.is_empty() {
            let _ = writeln!(
                b,
                "<text x=\"{}\" y=\"23.5\" fill=\"{TITLE_FG}\" text-anchor=\"end\" font-size=\"12\">{}</text>",
                width as f64 - PAD_X,
                escape(&fr.caption)
            );
        }
        for (i, runs) in parsed[f].iter().enumerate() {
            let y = FIRST_BASE + i as f64 * LINE_H;
            for r in runs {
                let s = r.render(y);
                if !s.is_empty() {
                    b.push_str(&s);
                    b.push('\n');
                }
            }
        }
        if animated {
            b.push_str("</g>\n");
        }
    }
    b.push_str("</svg>\n");
    b
}

/// Formats a fraction of the loop, or a duration in seconds, to four
/// decimals with trailing zeros dropped: precise to a millisecond over a
/// short loop, and readable in a diff.
fn frac(f: f64) -> String {
    let s = format!("{f:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        return "0".to_string();
    }
    s.to_string()
}

impl Run {
    /// Draws one run: its background, if it has one, then the characters
    /// at their exact column positions.
    fn render(&self, y: f64) -> String {
        let blank = self.text.trim().is_empty();
        if blank && self.style.bg.is_empty() {
            return String::new(); // nothing to draw, and the window background shows through
        }
        let mut b = String::new();
        if !self.style.bg.is_empty() {
            let _ = write!(
                b,
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{LINE_H}\" fill=\"{}\"/>",
                num(PAD_X + self.col as f64 * CELL_W),
                num(y - FONT_SIZE),
                num(self.width as f64 * CELL_W),
                self.style.bg
            );
            if blank {
                return b;
            }
            b.push('\n');
        }
        let mut xs = Vec::with_capacity(self.text.len());
        let mut col = self.col;
        let mut buf = [0u8; 4];
        for c in self.text.chars() {
            xs.push(num(PAD_X + col as f64 * CELL_W));
            col += string_width(c.encode_utf8(&mut buf));
        }
        let fill = if self.style.fg.is_empty() {
            DEFAULT_FG
        } else {
            &self.style.fg
        };
        let _ = write!(
            b,
            "<text x=\"{}\" y=\"{}\" fill=\"{fill}\"",
            xs.join(" "),
            num(y)
        );
        if self.style.bold {
            b.push_str(" font-weight=\"bold\"");
        }
        if self.style.faint {
            let _ = write!(b, " opacity=\"{FAINT_OPACITY}\"");
        }
        if self.style.underline {
            b.push_str(" text-decoration=\"underline\"");
        }
        let _ = write!(b, " xml:space=\"preserve\">{}</text>", escape(&self.text));
        b
    }
}

/// Breaks one line into runs of equal style, applying every SGR escape it
/// meets and skipping any other escape sequence.
fn split(line: &str) -> Vec<Run> {
    fn flush(out: &mut Vec<Run>, cur: &mut Option<Run>) {
        if let Some(run) = cur.take() {
            if !run.text.is_empty() {
                out.push(run);
            }
        }
    }

    let mut out = Vec::new();
    let mut cur: Option<Run> = None;
    let mut st = Style::default();
    let mut col = 0;
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut buf = [0u8; 4];
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            let (n, params) = escape_at(&line[i..]);
            if n == 0 {
                i += 1; // a stray escape byte
                continue;
            }
            if let Some(params) = params {
                let next = apply(st.clone(), &params);
                if next != st {
                    flush(&mut out, &mut cur);
                    st = next;
                }
            }
            i += n;
            continue;
        }
        let Some(c) = line[i..].chars().next() else {
            break;
        };
        i += c.len_utf8();
        let w = string_width(c.encode_utf8(&mut buf));
        let run = cur.get_or_insert_with(|| Run {
            col,
            style: st.clone(),
            ..Run::default()
        });
        run.text.push(c);
        run.width += w;
        col += w;
    }
    flush(&mut out, &mut cur);
    out
}

/// Measures the escape sequence at the start of `s`, and returns its
/// parameters when it is an SGR (`...m`) sequence. A length of zero means
/// there is no sequence to skip.
fn escape_at(s: &str) -> (usize, Option<Vec<&str>>) {
    let b = s.as_bytes();
    if b.len() < 2 || b[0] != 0x1b {
        return (0, None);
    }
    if b[1] != b'[' {
        // A two-byte escape the frame never uses; the whole of the
        // character after the escape byte goes with it.
        let n = s[1..].chars().next().map_or(1, char::len_utf8);
        return (1 + n, None);
    }
    for i in 2..b.len() {
        if (0x40..=0x7e).contains(&b[i]) {
            if b[i] != b'm' {
                return (i + 1, None);
            }
            return (i + 1, Some(s[2..i].split(';').collect()));
        }
    }
    (b.len(), None)
}

/// Folds one SGR sequence into the style in force.
fn apply(mut st: Style, params: &[&str]) -> Style {
    if params.is_empty() || (params.len() == 1 && params[0].is_empty()) {
        return Style::default();
    }
    let mut i = 0;
    while i < params.len() {
        if let Ok(p) = params[i].parse::<i64>() {
            match p {
                0 => st = Style::default(),
                1 => st.bold = true,
                2 => st.faint = true,
                4 => st.underline = true,
                22 => {
                    st.bold = false;
                    st.faint = false;
                }
                24 => st.underline = false,
                39 => st.fg.clear(),
                49 => st.bg.clear(),
                30..=37 => st.fg = palette(p - 30),
                90..=97 => st.fg = palette(p - 90 + 8),
                40..=47 => st.bg = palette(p - 40),
                100..=107 => st.bg = palette(p - 100 + 8),
                38 | 48 => {
                    let (c, used) = extended(&params[i + 1..]);
                    if !c.is_empty() {
                        if p == 38 {
                            st.fg = c;
                        } else {
                            st.bg = c;
                        }
                    }
                    i += used;
                }
                _ => {}
            }
        }
        i += 1;
    }
    st
}

/// Reads the tail of a 38/48 sequence: `;5;n` for a palette index and
/// `;2;r;g;b` for a direct colour. Returns the colour, empty when there is
/// none, and how many parameters it took.
fn extended(rest: &[&str]) -> (String, usize) {
    let Some(kind) = rest.first() else {
        return (String::new(), 0);
    };
    match *kind {
        "5" => {
            if rest.len() < 2 {
                return (String::new(), rest.len());
            }
            match rest[1].parse::<i64>() {
                Ok(n) => (palette(n), 2),
                Err(_) => (String::new(), 2),
            }
        }
        "2" => {
            if rest.len() < 4 {
                return (String::new(), rest.len());
            }
            let mut rgb = [0i64; 3];
            for (i, v) in rgb.iter_mut().enumerate() {
                match rest[i + 1].parse::<i64>() {
                    Ok(n) => *v = n.clamp(0, 255),
                    Err(_) => return (String::new(), 4),
                }
            }
            (format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]), 4)
        }
        _ => (String::new(), 1),
    }
}

/// The low end of the palette: the colours a terminal theme picks for
/// itself. These match the window background above.
const BASE16: [&str; 16] = [
    "#2a2e3a", "#ff5f57", "#28c840", "#febc2e", "#5aa9f8", "#d67bff", "#4de0e0", "#d6dae4",
    "#555b6b", "#ff8a84", "#5ff08a", "#ffd479", "#8ec6ff", "#e6a6ff", "#8ff0f0", "#ffffff",
];

/// Maps an xterm-256 index to a hex colour: sixteen themed colours, then
/// the 6×6×6 cube, then the greyscale ramp.
fn palette(n: i64) -> String {
    match n {
        0..=15 => BASE16[n as usize].to_string(),
        16..=231 => {
            const LEVELS: [i64; 6] = [0, 95, 135, 175, 215, 255];
            let n = (n - 16) as usize;
            format!(
                "#{:02x}{:02x}{:02x}",
                LEVELS[n / 36],
                LEVELS[(n / 6) % 6],
                LEVELS[n % 6]
            )
        }
        232..=255 => {
            let v = 8 + (n - 232) * 10;
            format!("#{v:02x}{v:02x}{v:02x}")
        }
        _ => DEFAULT_FG.to_string(),
    }
}

/// Formats a coordinate the way the rest of the file does: one decimal,
/// with a trailing `.0` kept so columns line up in a diff.
fn num(f: f64) -> String {
    format!("{f:.1}")
}

fn round_to(f: f64) -> i64 {
    (f + 0.5) as i64
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_groups_characters_by_style() {
        let runs = split("a \x1b[1;38;5;212mbc\x1b[0m\x1b[2K d");
        let got: Vec<(usize, usize, &str, &str, bool)> = runs
            .iter()
            .map(|r| {
                (
                    r.col,
                    r.width,
                    r.text.as_str(),
                    r.style.fg.as_str(),
                    r.style.bold,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (0, 2, "a ", "", false),
                (2, 2, "bc", "#ff87d7", true),
                (4, 2, " d", "", false),
            ]
        );
    }

    #[test]
    fn extended_colours() {
        assert_eq!(
            apply(Style::default(), &["48", "2", "300", "0", "16"]).bg,
            "#ff0010"
        );
        assert_eq!(apply(Style::default(), &["38", "5", "240"]).fg, "#585858");
        assert_eq!(apply(Style::default(), &["38", "5", "999"]).fg, DEFAULT_FG);
        assert_eq!(apply(Style::default(), &["92"]).fg, "#5ff08a");
    }

    #[test]
    fn frac_drops_trailing_zeros() {
        assert_eq!(frac(0.0), "0");
        assert_eq!(frac(0.25), "0.25");
        assert_eq!(frac(1.0 / 3.0), "0.3333");
        assert_eq!(frac(20.0), "20");
    }

    #[test]
    fn a_static_frame_is_a_plain_window() {
        let out = svg("apic <run>", "\x1b[32mok\x1b[0m\n");
        assert!(out.starts_with(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"177\" height=\"74\" viewBox=\"0 0 177 74\" "
        ));
        assert!(out.contains(">apic &lt;run&gt;</text>"));
        assert!(out.contains(
            "<text x=\"18.0 25.8\" y=\"60.0\" fill=\"#28c840\" xml:space=\"preserve\">ok</text>\n</svg>\n"
        ));
        assert!(!out.contains("<animate"));
    }
}
