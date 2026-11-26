//! Styled text for the terminal: colours, attributes, padding, borders and
//! layout, written as SGR escape sequences. It is apic's own small layer,
//! shared by the CLI renderers, the terminal UI and the screenshot
//! generator, so all three draw the same thing.
//!
//! A [`Style`] is a value: every setter returns a new style, and
//! [`Style::render`] turns text into styled text according to the colour
//! profile in force ([`color_profile`]). Under `Profile::Ascii` no escape
//! sequence is written at all, though padding, borders and alignment still
//! apply.
//!
//! Nothing here asks the terminal anything: the profile comes from the
//! environment, and the background is taken to be dark unless
//! [`set_has_dark_background`] says otherwise.
//!
//! The sequences are a contract: `\x1b[1;38;5;212mtext\x1b[0m`, attributes
//! first (bold, italic, underline, reverse, blink, faint), then foreground,
//! then background, one opening and one reset per line. The UI's tests and
//! the screenshots compare them byte for byte, and
//! `testdata/style_golden.json` pins them.

mod ansi;
mod border;
mod color;
mod layout;
mod parser;
mod profile;

pub use ansi::{
    cut, first_grapheme_cluster, hardwrap, string_width, strip, truncate, truncate_left, wordwrap,
    wrap,
};
pub use border::{
    ascii_border, block_border, double_border, hidden_border, inner_half_block_border,
    markdown_border, normal_border, outer_half_block_border, rounded_border, thick_border, Border,
};
pub use color::Color;
pub use layout::{
    height, join_horizontal, join_vertical, place, place_horizontal, place_vertical, size, width,
    Position,
};
pub use profile::{
    color_profile, detect_color_profile, env_color_profile, has_dark_background, set_color_profile,
    set_has_dark_background, with_color_profile, Profile,
};

use layout::{get_lines, spaces};

/// The width a tab is turned into unless [`Style::tab_width`] says
/// otherwise.
const TAB_WIDTH_DEFAULT: i32 = 4;

/// How text is to be drawn. Build one with [`Style::new`] and the setters,
/// keep it, and call [`Style::render`] as often as needed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strikethrough: Option<bool>,
    reverse: Option<bool>,
    blink: Option<bool>,
    faint: Option<bool>,
    underline_spaces: Option<bool>,
    strikethrough_spaces: Option<bool>,
    color_whitespace: Option<bool>,
    foreground: Option<Color>,
    background: Option<Color>,
    width: Option<i32>,
    height: Option<i32>,
    max_width: Option<i32>,
    max_height: Option<i32>,
    align_horizontal: Option<Position>,
    align_vertical: Option<Position>,
    padding: [Option<i32>; 4],
    margin: [Option<i32>; 4],
    margin_background: Option<Color>,
    border_style: Option<Border>,
    border_sides: [Option<bool>; 4],
    border_foreground: [Option<Color>; 4],
    border_background: [Option<Color>; 4],
    inline: Option<bool>,
    tab_width: Option<i32>,
}

// Sides are kept in the order of the CSS shorthands.
const TOP: usize = 0;
const RIGHT: usize = 1;
const BOTTOM: usize = 2;
const LEFT: usize = 3;

/// Reads a one to four value shorthand as top, right, bottom, left: one
/// value is every side; two are vertical then horizontal; three are top,
/// horizontal, bottom; four go clockwise from the top. Any other count is
/// nothing.
fn which_sides<T: Clone>(v: &[T]) -> Option<[T; 4]> {
    let c = |i: usize| v[i].clone();
    match v.len() {
        1 => Some([c(0), c(0), c(0), c(0)]),
        2 => Some([c(0), c(1), c(0), c(1)]),
        3 => Some([c(0), c(1), c(2), c(1)]),
        4 => Some([c(0), c(1), c(2), c(3)]),
        _ => None,
    }
}

/// The escape sequences of one style under one profile.
#[derive(Clone, Debug)]
struct Sgr {
    profile: Profile,
    styles: Vec<String>,
}

impl Sgr {
    fn new(profile: Profile) -> Sgr {
        Sgr {
            profile,
            styles: Vec::new(),
        }
    }

    fn push(&mut self, seq: &str) {
        self.styles.push(seq.to_string());
    }

    fn color(&mut self, c: &Color, background: bool) {
        if let Some(c) = c.resolve(self.profile) {
            self.styles.push(c.sequence(background));
        }
    }

    fn styled(&self, s: &str) -> String {
        if self.profile == Profile::Ascii || self.styles.is_empty() {
            return s.to_string();
        }
        let seq = self.styles.join(";");
        if seq.is_empty() {
            return s.to_string();
        }
        format!("\x1b[{seq}m{s}\x1b[0m")
    }
}

const BOLD: &str = "1";
const FAINT: &str = "2";
const ITALIC: &str = "3";
const UNDERLINE: &str = "4";
const BLINK: &str = "5";
const REVERSE: &str = "7";
const CROSS_OUT: &str = "9";

fn set_color(c: &Option<Color>) -> Option<&Color> {
    c.as_ref().filter(|c| **c != Color::None)
}

impl Style {
    /// A style that changes nothing.
    pub fn new() -> Style {
        Style::default()
    }

    /// Bold text.
    pub fn bold(mut self, v: bool) -> Style {
        self.bold = Some(v);
        self
    }

    /// Italic text.
    pub fn italic(mut self, v: bool) -> Style {
        self.italic = Some(v);
        self
    }

    /// Underlined text. Spaces between words are underlined too unless
    /// [`Style::underline_spaces`] turns that off.
    pub fn underline(mut self, v: bool) -> Style {
        self.underline = Some(v);
        self
    }

    /// Struck-through text.
    pub fn strikethrough(mut self, v: bool) -> Style {
        self.strikethrough = Some(v);
        self
    }

    /// Foreground and background swapped.
    pub fn reverse(mut self, v: bool) -> Style {
        self.reverse = Some(v);
        self
    }

    /// Blinking text.
    pub fn blink(mut self, v: bool) -> Style {
        self.blink = Some(v);
        self
    }

    /// Dimmed text.
    pub fn faint(mut self, v: bool) -> Style {
        self.faint = Some(v);
        self
    }

    /// Whether the spaces between words are underlined.
    pub fn underline_spaces(mut self, v: bool) -> Style {
        self.underline_spaces = Some(v);
        self
    }

    /// Whether the spaces between words are struck through.
    pub fn strikethrough_spaces(mut self, v: bool) -> Style {
        self.strikethrough_spaces = Some(v);
        self
    }

    /// Whether the background colour also fills the padding. It does
    /// unless this is turned off.
    pub fn color_whitespace(mut self, v: bool) -> Style {
        self.color_whitespace = Some(v);
        self
    }

    /// The text colour.
    pub fn foreground(mut self, c: impl Into<Color>) -> Style {
        self.foreground = Some(c.into());
        self
    }

    /// The background colour.
    pub fn background(mut self, c: impl Into<Color>) -> Style {
        self.background = Some(c.into());
        self
    }

    /// The width of the block before borders and margins. Text is wrapped
    /// to it and shorter lines are padded to it.
    pub fn width(mut self, n: i32) -> Style {
        self.width = Some(n);
        self
    }

    /// The least height of the block before borders and margins.
    pub fn height(mut self, n: i32) -> Style {
        self.height = Some(n);
        self
    }

    /// The most cells a rendered line may take; longer ones are cut.
    pub fn max_width(mut self, n: i32) -> Style {
        self.max_width = Some(n);
        self
    }

    /// The most lines the rendered block may take; the rest are dropped.
    pub fn max_height(mut self, n: i32) -> Style {
        self.max_height = Some(n);
        self
    }

    /// Horizontal alignment of the lines of a block.
    pub fn align_horizontal(mut self, p: Position) -> Style {
        self.align_horizontal = Some(p);
        self
    }

    /// Vertical alignment of the text in a block with a height.
    pub fn align_vertical(mut self, p: Position) -> Style {
        self.align_vertical = Some(p);
        self
    }

    /// Horizontal alignment, and with a second value vertical alignment.
    pub fn align(mut self, p: &[Position]) -> Style {
        if let Some(h) = p.first() {
            self.align_horizontal = Some(*h);
        }
        if let Some(v) = p.get(1) {
            self.align_vertical = Some(*v);
        }
        self
    }

    /// Padding inside the border, as a shorthand of one to four values:
    /// all sides; vertical, horizontal; top, horizontal, bottom; or top,
    /// right, bottom, left.
    pub fn padding(mut self, v: &[i32]) -> Style {
        if let Some(sides) = which_sides(v) {
            self.padding = sides.map(Some);
        }
        self
    }

    pub fn padding_top(mut self, n: i32) -> Style {
        self.padding[TOP] = Some(n);
        self
    }

    pub fn padding_right(mut self, n: i32) -> Style {
        self.padding[RIGHT] = Some(n);
        self
    }

    pub fn padding_bottom(mut self, n: i32) -> Style {
        self.padding[BOTTOM] = Some(n);
        self
    }

    pub fn padding_left(mut self, n: i32) -> Style {
        self.padding[LEFT] = Some(n);
        self
    }

    /// Margin outside the border, with the shorthand of
    /// [`Style::padding`].
    pub fn margin(mut self, v: &[i32]) -> Style {
        if let Some(sides) = which_sides(v) {
            self.margin = sides.map(Some);
        }
        self
    }

    pub fn margin_top(mut self, n: i32) -> Style {
        self.margin[TOP] = Some(n);
        self
    }

    pub fn margin_right(mut self, n: i32) -> Style {
        self.margin[RIGHT] = Some(n);
        self
    }

    pub fn margin_bottom(mut self, n: i32) -> Style {
        self.margin[BOTTOM] = Some(n);
        self
    }

    pub fn margin_left(mut self, n: i32) -> Style {
        self.margin[LEFT] = Some(n);
        self
    }

    /// The background colour of the margin.
    pub fn margin_background(mut self, c: impl Into<Color>) -> Style {
        self.margin_background = Some(c.into());
        self
    }

    /// A border, and which sides have one, with the shorthand of
    /// [`Style::padding`]; an empty `sides` is every side.
    pub fn border(mut self, b: Border, sides: &[bool]) -> Style {
        self.border_style = Some(b);
        self.border_sides = which_sides(sides).unwrap_or([true; 4]).map(Some);
        self
    }

    /// The characters of the border, leaving which sides have one as it
    /// is: every side when none was turned on or off.
    pub fn border_style(mut self, b: Border) -> Style {
        self.border_style = Some(b);
        self
    }

    pub fn border_top(mut self, v: bool) -> Style {
        self.border_sides[TOP] = Some(v);
        self
    }

    pub fn border_right(mut self, v: bool) -> Style {
        self.border_sides[RIGHT] = Some(v);
        self
    }

    pub fn border_bottom(mut self, v: bool) -> Style {
        self.border_sides[BOTTOM] = Some(v);
        self
    }

    pub fn border_left(mut self, v: bool) -> Style {
        self.border_sides[LEFT] = Some(v);
        self
    }

    /// The colour of the border, on every side.
    pub fn border_foreground(mut self, c: impl Into<Color>) -> Style {
        let c = c.into();
        self.border_foreground = [(); 4].map(|()| Some(c.clone()));
        self
    }

    /// The colour of the border side by side, with the shorthand of
    /// [`Style::padding`].
    pub fn border_foreground_sides(mut self, c: &[Color]) -> Style {
        if let Some(sides) = which_sides(c) {
            self.border_foreground = sides.map(Some);
        }
        self
    }

    /// The background colour of the border, on every side.
    pub fn border_background(mut self, c: impl Into<Color>) -> Style {
        let c = c.into();
        self.border_background = [(); 4].map(|()| Some(c.clone()));
        self
    }

    /// The background colour of the border side by side, with the
    /// shorthand of [`Style::padding`].
    pub fn border_background_sides(mut self, c: &[Color]) -> Style {
        if let Some(sides) = which_sides(c) {
            self.border_background = sides.map(Some);
        }
        self
    }

    /// Renders on one line: newlines are removed, and padding, borders
    /// and margins are left out.
    pub fn inline(mut self, v: bool) -> Style {
        self.inline = Some(v);
        self
    }

    /// How many spaces a tab becomes: four by default, none for 0, and -1
    /// leaves tabs alone.
    pub fn tab_width(mut self, n: i32) -> Style {
        self.tab_width = Some(n.max(-1));
        self
    }

    fn is_unset(&self) -> bool {
        *self == Style::default()
    }

    fn convert_tabs(&self, s: &str) -> String {
        match self.tab_width.unwrap_or(TAB_WIDTH_DEFAULT) {
            -1 => s.to_string(),
            0 => s.replace('\t', ""),
            n => s.replace('\t', &spaces(i64::from(n))),
        }
    }

    /// The width the margins, the border and the padding take, left and
    /// right together.
    pub fn horizontal_frame_size(&self) -> i32 {
        let pad = |i: usize| self.padding[i].unwrap_or(0);
        let margin = |i: usize| self.margin[i].unwrap_or(0);
        let (_, right, _, left) = self.sides();
        let b = self.border_style.clone().unwrap_or_default();
        let edge = |parts: [&str; 3]| parts.iter().map(|p| max_rune_width(p)).max().unwrap_or(0);
        let mut n = pad(LEFT) + pad(RIGHT) + margin(LEFT) + margin(RIGHT);
        if left {
            n += edge([&b.top_left, &b.left, &b.bottom_left]) as i32;
        }
        if right {
            n += edge([&b.top_right, &b.right, &b.bottom_right]) as i32;
        }
        n
    }

    /// The height the margins, the border and the padding take, top and
    /// bottom together.
    pub fn vertical_frame_size(&self) -> i32 {
        let pad = |i: usize| self.padding[i].unwrap_or(0);
        let margin = |i: usize| self.margin[i].unwrap_or(0);
        let (top, _, bottom, _) = self.sides();
        let b = self.border_style.clone().unwrap_or_default();
        let edge = |parts: [&str; 3]| parts.iter().map(|p| max_rune_width(p)).max().unwrap_or(0);
        let mut n = pad(TOP) + pad(BOTTOM) + margin(TOP) + margin(BOTTOM);
        if top {
            n += edge([&b.top_left, &b.top, &b.top_right]) as i32;
        }
        if bottom {
            n += edge([&b.bottom_left, &b.bottom, &b.bottom_right]) as i32;
        }
        n
    }

    /// Which sides have a border: top, right, bottom, left.
    fn sides(&self) -> (bool, bool, bool, bool) {
        let has_style = self
            .border_style
            .as_ref()
            .is_some_and(|b| *b != Border::default());
        if has_style && self.border_sides.iter().all(Option::is_none) {
            // A border with no side turned on or off is on every side.
            return (true, true, true, true);
        }
        let side = |i: usize| self.border_sides[i].unwrap_or(false);
        (side(TOP), side(RIGHT), side(BOTTOM), side(LEFT))
    }

    /// Renders `s` in this style, under the colour profile in force.
    pub fn render(&self, s: &str) -> String {
        self.render_with(color_profile(), s)
    }

    /// Renders `s` in this style under the profile given.
    pub fn render_with(&self, p: Profile, s: &str) -> String {
        if self.is_unset() {
            return self.convert_tabs(s);
        }

        let flag = |v: Option<bool>| v.unwrap_or(false);
        let bold = flag(self.bold);
        let italic = flag(self.italic);
        let underline = flag(self.underline);
        let strikethrough = flag(self.strikethrough);
        let reverse = flag(self.reverse);
        let blink = flag(self.blink);
        let faint = flag(self.faint);

        let fg = set_color(&self.foreground);
        let bg = set_color(&self.background);

        let width = self.width.unwrap_or(0);
        let height = self.height.unwrap_or(0);
        let horizontal_align = self.align_horizontal.unwrap_or(Position::LEFT);
        let vertical_align = self.align_vertical.unwrap_or(Position::TOP);

        let pad = |i: usize| self.padding[i].unwrap_or(0);

        let color_whitespace = self.color_whitespace.unwrap_or(true);
        let inline = flag(self.inline);
        let max_width = self.max_width.unwrap_or(0);
        let max_height = self.max_height.unwrap_or(0);

        let underline_spaces =
            flag(self.underline_spaces) || (underline && self.underline_spaces.unwrap_or(true));
        let strikethrough_spaces = flag(self.strikethrough_spaces)
            || (strikethrough && self.strikethrough_spaces.unwrap_or(true));

        // Is white space (padding, and the room beside short lines) styled
        // apart from the text?
        let style_whitespace = reverse;

        // Are the spaces between words styled apart from the words?
        let use_space_styler = (underline && !underline_spaces)
            || (strikethrough && !strikethrough_spaces)
            || underline_spaces
            || strikethrough_spaces;

        let mut te = Sgr::new(p);
        let mut te_space = Sgr::new(p);
        let mut te_whitespace = Sgr::new(p);

        if bold {
            te.push(BOLD);
        }
        if italic {
            te.push(ITALIC);
        }
        if underline {
            te.push(UNDERLINE);
        }
        if reverse {
            te_whitespace.push(REVERSE);
            te.push(REVERSE);
        }
        if blink {
            te.push(BLINK);
        }
        if faint {
            te.push(FAINT);
        }

        if let Some(fg) = fg {
            te.color(fg, false);
            if style_whitespace {
                te_whitespace.color(fg, false);
            }
            if use_space_styler {
                te_space.color(fg, false);
            }
        }

        if let Some(bg) = bg {
            te.color(bg, true);
            if color_whitespace {
                te_whitespace.color(bg, true);
            }
            if use_space_styler {
                te_space.color(bg, true);
            }
        }

        // Underline is written a second time after the colours; terminals
        // do not mind, and the sequences have always read this way.
        if underline {
            te.push(UNDERLINE);
        }
        if strikethrough {
            te.push(CROSS_OUT);
        }

        if underline_spaces {
            te_space.push(UNDERLINE);
        }
        if strikethrough_spaces {
            te_space.push(CROSS_OUT);
        }

        // Tabs become spaces, and carriage returns go: they make a mess of
        // a rendered block.
        let mut str = self.convert_tabs(s).replace("\r\n", "\n");

        if inline {
            str = str.replace('\n', "");
        }

        // Word wrap
        if !inline && width > 0 {
            let wrap_at = width - pad(LEFT) - pad(RIGHT);
            str = cell_wrap(&str, wrap_at);
        }

        // The text itself, line by line
        {
            let mut b = String::with_capacity(str.len() + 16);
            let lines: Vec<&str> = str.split('\n').collect();
            for (i, line) in lines.iter().enumerate() {
                if use_space_styler {
                    // Spaces get a style of their own.
                    let mut buf = [0u8; 4];
                    for r in line.chars() {
                        let r = &*r.encode_utf8(&mut buf);
                        if r.chars().all(char::is_whitespace) {
                            b.push_str(&te_space.styled(r));
                        } else {
                            b.push_str(&te.styled(r));
                        }
                    }
                } else {
                    b.push_str(&te.styled(line));
                }
                if i != lines.len() - 1 {
                    b.push('\n');
                }
            }
            str = b;
        }

        let whitespace = (color_whitespace || style_whitespace).then_some(&te_whitespace);

        // Padding
        if !inline {
            if pad(LEFT) > 0 {
                str = pad_lines(&str, -pad(LEFT), whitespace);
            }
            if pad(RIGHT) > 0 {
                str = pad_lines(&str, pad(RIGHT), whitespace);
            }
            if pad(TOP) > 0 {
                str = "\n".repeat(pad(TOP) as usize) + &str;
            }
            if pad(BOTTOM) > 0 {
                str.push_str(&"\n".repeat(pad(BOTTOM) as usize));
            }
        }

        // Height
        if height > 0 {
            str = align_text_vertical(&str, vertical_align, height);
        }

        // Alignment. This also pads short lines so that every line is as
        // wide as the widest, which is why it runs for any block of more
        // than one line.
        if str.contains('\n') || width != 0 {
            str = align_text_horizontal(&str, horizontal_align, width, whitespace);
        }

        if !inline {
            str = self.apply_border(&str, p);
            str = self.apply_margins(&str, p);
        }

        // Cut to the most width allowed
        if max_width > 0 {
            str = str
                .split('\n')
                .map(|l| truncate(l, max_width, ""))
                .collect::<Vec<_>>()
                .join("\n");
        }

        // Cut to the most height allowed
        if max_height > 0 {
            let lines: Vec<&str> = str.split('\n').collect();
            let n = (max_height as usize).min(lines.len());
            str = lines[..n].join("\n");
        }

        str
    }

    fn apply_border(&self, s: &str, p: Profile) -> String {
        let Some(mut border) = self.border_style.clone() else {
            return s.to_string();
        };
        let (has_top, has_right, has_bottom, has_left) = self.sides();
        if border == Border::default() || !(has_top || has_right || has_bottom || has_left) {
            return s.to_string();
        }

        let (lines, mut width) = get_lines(s);

        if has_left {
            if border.left.is_empty() {
                border.left = " ".to_string();
            }
            width += max_rune_width(&border.left);
        }
        if has_right && border.right.is_empty() {
            border.right = " ".to_string();
        }

        // A corner that is to be drawn but has no character is a space.
        let fill = |s: &mut String| {
            if s.is_empty() {
                *s = " ".to_string();
            }
        };
        if has_top && has_left {
            fill(&mut border.top_left);
        }
        if has_top && has_right {
            fill(&mut border.top_right);
        }
        if has_bottom && has_left {
            fill(&mut border.bottom_left);
        }
        if has_bottom && has_right {
            fill(&mut border.bottom_right);
        }

        // A corner is drawn only where both of its sides are.
        if has_top {
            if !has_left {
                border.top_left.clear();
            }
            if !has_right {
                border.top_right.clear();
            }
        }
        if has_bottom {
            if !has_left {
                border.bottom_left.clear();
            }
            if !has_right {
                border.bottom_right.clear();
            }
        }

        // A corner is one character.
        let first = |s: &str| s.chars().next().map(String::from).unwrap_or_default();
        border.top_left = first(&border.top_left);
        border.top_right = first(&border.top_right);
        border.bottom_right = first(&border.bottom_right);
        border.bottom_left = first(&border.bottom_left);

        let style_border = |text: &str, side: usize| -> String {
            let fg = set_color(&self.border_foreground[side]);
            let bg = set_color(&self.border_background[side]);
            if fg.is_none() && bg.is_none() {
                return text.to_string();
            }
            // Border colours are written whatever the profile says about
            // attributes: only a profile without colour leaves them out,
            // by resolving each colour to nothing.
            let mut sgr = Sgr::new(Profile::Ansi);
            if let Some(fg) = fg {
                if let Some(c) = fg.resolve(p) {
                    sgr.styles.push(c.sequence(false));
                }
            }
            if let Some(bg) = bg {
                if let Some(c) = bg.resolve(p) {
                    sgr.styles.push(c.sequence(true));
                }
            }
            sgr.styled(text)
        };

        let mut out = String::new();

        if has_top {
            let top =
                render_horizontal_edge(&border.top_left, &border.top, &border.top_right, width);
            out.push_str(&style_border(&top, TOP));
            out.push('\n');
        }

        let left_runes: Vec<char> = border.left.chars().collect();
        let right_runes: Vec<char> = border.right.chars().collect();
        let (mut left_index, mut right_index) = (0, 0);

        for (i, l) in lines.iter().enumerate() {
            if has_left {
                let r = left_runes[left_index].to_string();
                left_index = (left_index + 1) % left_runes.len();
                out.push_str(&style_border(&r, LEFT));
            }
            out.push_str(l);
            if has_right {
                let r = right_runes[right_index].to_string();
                right_index = (right_index + 1) % right_runes.len();
                out.push_str(&style_border(&r, RIGHT));
            }
            if i < lines.len() - 1 {
                out.push('\n');
            }
        }

        if has_bottom {
            let bottom = render_horizontal_edge(
                &border.bottom_left,
                &border.bottom,
                &border.bottom_right,
                width,
            );
            out.push('\n');
            out.push_str(&style_border(&bottom, BOTTOM));
        }

        out
    }

    fn apply_margins(&self, s: &str, p: Profile) -> String {
        let margin = |i: usize| self.margin[i].unwrap_or(0);

        // The margin's colour is written like a border's: by the colour
        // alone.
        let mut styler = Sgr::new(Profile::Ansi);
        if let Some(bg) = set_color(&self.margin_background) {
            if let Some(c) = bg.resolve(p) {
                styler.styles.push(c.sequence(true));
            }
        }

        let mut str = pad_lines(s, -margin(LEFT), Some(&styler));
        str = pad_lines(&str, margin(RIGHT), Some(&styler));

        let spaces = " ".repeat(width(&str));
        if margin(TOP) > 0 {
            str = styler.styled(&format!("{spaces}\n").repeat(margin(TOP) as usize)) + &str;
        }
        if margin(BOTTOM) > 0 {
            str.push_str(&styler.styled(&format!("\n{spaces}").repeat(margin(BOTTOM) as usize)));
        }
        str
    }
}

/// Pads every line of a block: on the right for a positive `n`, on the
/// left for a negative one.
fn pad_lines(s: &str, n: i32, style: Option<&Sgr>) -> String {
    if n == 0 {
        return s.to_string();
    }
    let mut sp = spaces(i64::from(n.unsigned_abs()));
    if let Some(style) = style {
        sp = style.styled(&sp);
    }
    s.split('\n')
        .map(|l| {
            if n > 0 {
                format!("{l}{sp}")
            } else {
                format!("{sp}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Aligns the lines of a block, padding each to the width of the widest,
/// or to `width` when that is more.
fn align_text_horizontal(s: &str, pos: Position, width: i32, style: Option<&Sgr>) -> String {
    let (lines, widest) = get_lines(s);
    let styled = |n: i64| -> String {
        let sp = spaces(n);
        match style {
            Some(style) => style.styled(&sp),
            None => sp,
        }
    };
    let mut b = String::new();
    for (i, l) in lines.iter().enumerate() {
        let line_width = string_width(l) as i64;
        // Short of the widest line, then short of the width asked for.
        let mut short = widest as i64 - line_width;
        short += (i64::from(width) - (short + line_width)).max(0);
        if short > 0 {
            if pos == Position::RIGHT {
                b.push_str(&styled(short));
                b.push_str(l);
            } else if pos == Position::CENTER {
                // Not every line has a centre.
                let left = short / 2;
                let right = left + short % 2;
                b.push_str(&styled(left));
                b.push_str(l);
                b.push_str(&styled(right));
            } else {
                b.push_str(l);
                b.push_str(&styled(short));
            }
        } else {
            b.push_str(l);
        }
        if i < lines.len() - 1 {
            b.push('\n');
        }
    }
    b
}

fn align_text_vertical(s: &str, pos: Position, height: i32) -> String {
    let str_height = s.matches('\n').count() + 1;
    let height = height as usize;
    if height < str_height {
        return s.to_string();
    }
    let gap = height - str_height;
    if pos == Position::TOP {
        return s.to_string() + &"\n".repeat(gap);
    }
    if pos == Position::CENTER {
        let top = gap / 2;
        let bottom = gap - top;
        return "\n".repeat(top) + s + &"\n".repeat(bottom);
    }
    if pos == Position::BOTTOM {
        return "\n".repeat(gap) + s;
    }
    s.to_string()
}

/// The top or the bottom of a border.
fn render_horizontal_edge(left: &str, middle: &str, right: &str, width: usize) -> String {
    let middle = if middle.is_empty() { " " } else { middle };
    let left_width = string_width(left);
    let right_width = string_width(right);
    let runes: Vec<char> = middle.chars().collect();
    let mut j = 0;
    let mut out = String::from(left);
    let mut i = left_width + right_width;
    while i < width + right_width {
        out.push(runes[j]);
        j = (j + 1) % runes.len();
        i += string_width(&runes[j].to_string()).max(1);
    }
    out.push_str(right);
    out
}

/// The width of the widest cluster of a border part.
fn max_rune_width(s: &str) -> usize {
    let mut rest = s;
    let mut widest = 0;
    while !rest.is_empty() {
        let (cluster, w) = first_grapheme_cluster(rest);
        widest = widest.max(w);
        rest = &rest[cluster.len().max(1).min(rest.len())..];
    }
    widest
}

/// What the wrapping of a block has to remember about the SGR sequences
/// it has passed: which attributes and colours are on, so that they can
/// be switched off at the end of a line and on again at the start of the
/// next, and whether a space would show (so must not be dropped at a
/// break).
#[derive(Clone, Default)]
struct Pen {
    /// One bit per attribute, numbered by its SGR parameter: 1 bold,
    /// 2 faint, 3 italic, 5 and 6 blink, 7 reverse, 8 conceal, 9 struck
    /// through.
    attrs: u16,
    underline: bool,
    /// The parameters that select the colour, as `38;5;212` or `31`.
    fg: Option<String>,
    bg: Option<String>,
}

const REVERSE_ATTR: u16 = 1 << 7;

impl Pen {
    fn is_empty(&self) -> bool {
        self.attrs == 0 && !self.underline && self.fg.is_none() && self.bg.is_none()
    }

    /// Whether a space written with this pen shows nothing.
    fn is_blank(&self) -> bool {
        self.attrs & REVERSE_ATTR == 0 && self.bg.is_none() && !self.underline
    }

    /// The sequence that switches this pen on.
    fn sequence(&self) -> String {
        let mut parts: Vec<String> = (1..=9)
            .filter(|n| self.attrs & (1 << n) != 0)
            .map(|n| n.to_string())
            .collect();
        if self.underline {
            parts.push("4;4".to_string());
        }
        parts.extend(self.fg.clone());
        parts.extend(self.bg.clone());
        format!("\x1b[{}m", parts.join(";"))
    }

    /// Reads the parameters of an SGR sequence.
    fn read(&mut self, seq: &str) {
        let params = &seq[2..seq.len() - 1];
        let mut it = params.split(';');
        while let Some(param) = it.next() {
            let mut sub = param.split(':');
            let code = sub.next().unwrap_or("").parse::<u16>().unwrap_or(0);
            match code {
                0 => *self = Pen::default(),
                1..=3 | 5..=9 => self.attrs |= 1 << code,
                4 => self.underline = !param.ends_with(":0"),
                22 => self.attrs &= !(1 << 1 | 1 << 2),
                23 => self.attrs &= !(1 << 3),
                24 => self.underline = false,
                25 => self.attrs &= !(1 << 5 | 1 << 6),
                27 => self.attrs &= !REVERSE_ATTR,
                28 => self.attrs &= !(1 << 8),
                29 => self.attrs &= !(1 << 9),
                30..=37 | 90..=97 => self.fg = Some(code.to_string()),
                39 => self.fg = None,
                40..=47 | 100..=107 => self.bg = Some(code.to_string()),
                49 => self.bg = None,
                38 | 48 => {
                    // An extended colour takes its own parameters along,
                    // after semicolons or after colons.
                    let mut parts = vec![code.to_string()];
                    if param.contains(':') {
                        parts.extend(sub.filter(|p| !p.is_empty()).map(str::to_string));
                    } else {
                        let kind = it.next().unwrap_or("");
                        parts.push(kind.to_string());
                        let n = match kind {
                            "5" => 1,
                            "2" => 3,
                            _ => 0,
                        };
                        parts.extend(it.by_ref().take(n).map(str::to_string));
                    }
                    let colour = Some(parts.join(";"));
                    if code == 38 {
                        self.fg = colour;
                    } else {
                        self.bg = colour;
                    }
                }
                _ => {}
            }
        }
    }
}

/// The next piece of `s`: an escape sequence or a control character
/// (width 0), or a grapheme cluster with its width.
fn next_piece(s: &str) -> (&str, usize) {
    let b = s.as_bytes();
    if b[0] != 0x1b {
        return first_grapheme_cluster(s);
    }
    let string_end = |from: usize| -> usize {
        // A string ends at BEL or at ESC \.
        let mut i = from;
        while i < b.len() {
            if b[i] == 0x07 {
                return i + 1;
            }
            if b[i] == 0x1b {
                return if b.get(i + 1) == Some(&b'\\') {
                    i + 2
                } else {
                    i
                };
            }
            i += 1;
        }
        i
    };
    let end = match b.get(1) {
        Some(b'[') => {
            let mut i = 2;
            while i < b.len() && (0x20..=0x3f).contains(&b[i]) {
                i += 1;
            }
            if i < b.len() && (0x40..=0x7e).contains(&b[i]) {
                i + 1
            } else {
                i
            }
        }
        Some(b']' | b'P' | b'X' | b'^' | b'_') => string_end(2),
        Some(c) if (0x20..=0x7e).contains(c) => {
            let mut i = 1;
            while i < b.len() && (0x20..=0x2f).contains(&b[i]) {
                i += 1;
            }
            (i + 1).min(b.len())
        }
        _ => 1,
    };
    let mut end = end.max(1);
    while !s.is_char_boundary(end) {
        end += 1;
    }
    (&s[..end], 0)
}

/// The state of [`cell_wrap`]: the text written, the word and the white
/// space waiting to be placed, and the pens they were written with.
#[derive(Default)]
struct CellWrap {
    limit: usize,
    buf: String,
    word: String,
    space: String,
    /// The pen at this point of the text, and the one the last word was
    /// written with.
    pen: Pen,
    cur_pen: Pen,
    cur_width: usize,
    word_len: usize,
}

impl CellWrap {
    fn add_space(&mut self) {
        self.cur_width += self.space.len();
        self.buf.push_str(&self.space);
        self.space.clear();
    }

    fn add_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        self.cur_pen = self.pen.clone();
        self.add_space();
        self.cur_width += self.word_len;
        self.buf.push_str(&self.word);
        self.word.clear();
        self.word_len = 0;
    }

    fn add_newline(&mut self) {
        if !self.cur_pen.is_empty() {
            self.buf.push_str("\x1b[m");
        }
        self.buf.push('\n');
        if !self.cur_pen.is_empty() {
            self.buf.push_str(&self.cur_pen.sequence());
        }
        self.cur_width = 0;
        self.space.clear();
    }

    /// Writes the pending white space when no word follows it, unless it
    /// would overflow the line.
    fn flush_space(&mut self) {
        if self.word_len == 0 {
            if self.cur_width + self.space.len() > self.limit {
                self.cur_width = 0;
            } else {
                self.buf.push_str(&self.space);
            }
            self.space.clear();
        }
    }
}

/// Wraps a block for a style with a width: at spaces and hyphens where a
/// line allows, inside a word where it does not. White space at a break is
/// dropped, and a style switched on before a break is switched off at the
/// end of the line and on again at the start of the next.
fn cell_wrap(s: &str, limit: i32) -> String {
    if s.is_empty() {
        return String::new();
    }
    if limit < 1 {
        return s.to_string();
    }
    let mut w = CellWrap {
        limit: limit as usize,
        ..CellWrap::default()
    };
    let mut rest = s;
    while !rest.is_empty() {
        let (seq, width) = next_piece(rest);
        rest = &rest[seq.len()..];
        if width == 0 {
            if seq == "\t" {
                w.add_word();
                w.space.push_str(seq);
            } else if seq == "\n" {
                w.flush_space();
                w.add_word();
                w.add_newline();
            } else {
                if seq.starts_with("\x1b[") && seq.ends_with('m') {
                    w.pen.read(seq);
                }
                w.word.push_str(seq);
            }
            continue;
        }
        if seq == " " && w.pen.is_blank() {
            w.add_word();
            w.space.push(' ');
            continue;
        }
        if seq == "-" {
            w.add_space();
            if w.cur_width + w.word_len + width <= w.limit {
                w.add_word();
                w.buf.push_str(seq);
                w.cur_width += width;
                continue;
            }
        }
        if w.word_len + width > w.limit {
            // The word alone is wider than a line: break it.
            w.add_word();
        }
        w.word.push_str(seq);
        w.word_len += width;
        if w.cur_width + w.word_len + w.space.len() > w.limit {
            w.add_newline();
        }
    }
    w.flush_space();
    w.add_word();
    if !w.cur_pen.is_empty() {
        w.buf.push_str("\x1b[m");
    }
    w.buf
}

#[cfg(test)]
mod tests;
