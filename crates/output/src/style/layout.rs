//! Measuring blocks of text and putting them side by side, one above the
//! other, or inside a larger box.

use super::ansi::string_width;

/// A position along an axis, from 0 (the left, the top) to 1 (the right,
/// the bottom).
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Position(pub f64);

impl Position {
    pub const TOP: Position = Position(0.0);
    pub const BOTTOM: Position = Position(1.0);
    pub const CENTER: Position = Position(0.5);
    pub const LEFT: Position = Position(0.0);
    pub const RIGHT: Position = Position(1.0);

    fn value(self) -> f64 {
        self.0.clamp(0.0, 1.0)
    }
}

/// `n` spaces; none when `n` is not positive.
pub(crate) fn spaces(n: i64) -> String {
    " ".repeat(usize::try_from(n).unwrap_or(0))
}

/// The lines of a block and the width of the widest.
pub(crate) fn get_lines(s: &str) -> (Vec<&str>, usize) {
    let lines: Vec<&str> = s.split('\n').collect();
    let widest = lines.iter().map(|l| string_width(l)).max().unwrap_or(0);
    (lines, widest)
}

/// The width in cells of the widest line of a block, escape sequences not
/// counted.
pub fn width(s: &str) -> usize {
    get_lines(s).1
}

/// The number of lines of a block.
pub fn height(s: &str) -> usize {
    s.matches('\n').count() + 1
}

/// The width and the height of a block.
pub fn size(s: &str) -> (usize, usize) {
    (width(s), height(s))
}

/// How a gap of `n` is shared out at `pos`: what goes before, what after.
fn split_gap(n: usize, pos: Position) -> (usize, usize) {
    let split = (n as f64 * pos.value()).round() as usize;
    let before = n - split;
    (n - before, before)
}

/// Puts blocks side by side. Each block is padded to its own width, and a
/// block shorter than the tallest is placed along the vertical axis at
/// `pos`: `Position::TOP`, `Position::BOTTOM`, or somewhere between.
pub fn join_horizontal(pos: Position, blocks: &[&str]) -> String {
    match blocks {
        [] => return String::new(),
        [one] => return (*one).to_string(),
        _ => {}
    }
    let mut columns: Vec<(Vec<&str>, usize)> = blocks.iter().map(|b| get_lines(b)).collect();
    let max_height = columns.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    for (lines, _) in &mut columns {
        if lines.len() >= max_height {
            continue;
        }
        let n = max_height - lines.len();
        let (above, below) = if pos == Position::TOP {
            (0, n)
        } else if pos == Position::BOTTOM {
            (n, 0)
        } else {
            let split = (n as f64 * pos.value()).round() as usize;
            let top = n - split;
            // What is not left above goes below.
            (n - top, top)
        };
        let mut padded = vec![""; above];
        padded.append(lines);
        padded.extend(std::iter::repeat_n("", below));
        *lines = padded;
    }
    let mut b = String::new();
    for i in 0..max_height {
        for (lines, widest) in &columns {
            b.push_str(lines[i]);
            b.push_str(&spaces(*widest as i64 - string_width(lines[i]) as i64));
        }
        if i < max_height - 1 {
            b.push('\n');
        }
    }
    b
}

/// Puts blocks one above the other. Every line is padded to the width of
/// the widest, and placed along the horizontal axis at `pos`:
/// `Position::LEFT`, `Position::RIGHT`, or somewhere between.
pub fn join_vertical(pos: Position, blocks: &[&str]) -> String {
    match blocks {
        [] => return String::new(),
        [one] => return (*one).to_string(),
        _ => {}
    }
    let columns: Vec<(Vec<&str>, usize)> = blocks.iter().map(|b| get_lines(b)).collect();
    let max_width = columns.iter().map(|(_, w)| *w).max().unwrap_or(0);
    let mut b = String::new();
    for (i, (lines, _)) in columns.iter().enumerate() {
        for (j, line) in lines.iter().enumerate() {
            let w = max_width.saturating_sub(string_width(line));
            if pos == Position::LEFT {
                b.push_str(line);
                b.push_str(&" ".repeat(w));
            } else if pos == Position::RIGHT {
                b.push_str(&" ".repeat(w));
                b.push_str(line);
            } else if w < 1 {
                b.push_str(line);
            } else {
                let (left, right) = split_gap(w, pos);
                b.push_str(&" ".repeat(left));
                b.push_str(line);
                b.push_str(&" ".repeat(right));
            }
            if !(i == columns.len() - 1 && j == lines.len() - 1) {
                b.push('\n');
            }
        }
    }
    b
}

/// Places a block in a box of `width` by `height` cells, padding with
/// spaces. A block larger than the box is returned as it is.
pub fn place(width: i32, height: i32, h_pos: Position, v_pos: Position, s: &str) -> String {
    place_vertical(height, v_pos, &place_horizontal(width, h_pos, s))
}

/// Places a block in a box `width` cells wide, padding with spaces.
pub fn place_horizontal(width: i32, pos: Position, s: &str) -> String {
    let (lines, content_width) = get_lines(s);
    let gap = i64::from(width) - content_width as i64;
    if gap <= 0 {
        return s.to_string();
    }
    let gap = gap as usize;
    let mut b = String::new();
    for (i, l) in lines.iter().enumerate() {
        // Is this line shorter than the longest line?
        let short = content_width.saturating_sub(string_width(l));
        if pos == Position::LEFT {
            b.push_str(l);
            b.push_str(&" ".repeat(gap + short));
        } else if pos == Position::RIGHT {
            b.push_str(&" ".repeat(gap + short));
            b.push_str(l);
        } else {
            let total = gap + short;
            let split = (total as f64 * pos.value()).round() as usize;
            let left = total - split;
            let right = total - left;
            b.push_str(&" ".repeat(left));
            b.push_str(l);
            b.push_str(&" ".repeat(right));
        }
        if i < lines.len() - 1 {
            b.push('\n');
        }
    }
    b
}

/// Places a block in a box `height` lines tall, padding with lines of
/// spaces as wide as the block.
pub fn place_vertical(height: i32, pos: Position, s: &str) -> String {
    let content_height = s.matches('\n').count() + 1;
    let gap = i64::from(height) - content_height as i64;
    if gap <= 0 {
        return s.to_string();
    }
    let gap = gap as usize;
    let empty_line = " ".repeat(width(s));
    let mut b = String::new();
    if pos == Position::TOP {
        b.push_str(s);
        b.push('\n');
        for i in 0..gap {
            b.push_str(&empty_line);
            if i < gap - 1 {
                b.push('\n');
            }
        }
    } else if pos == Position::BOTTOM {
        b.push_str(&format!("{empty_line}\n").repeat(gap));
        b.push_str(s);
    } else {
        let split = (gap as f64 * pos.value()).round() as usize;
        let top = gap - split;
        let bottom = gap - top;
        b.push_str(&format!("{empty_line}\n").repeat(top));
        b.push_str(s);
        for _ in 0..bottom {
            b.push('\n');
            b.push_str(&empty_line);
        }
    }
    b
}
