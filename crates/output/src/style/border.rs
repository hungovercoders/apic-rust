//! The characters a border is drawn with.

/// The parts of a border. The `middle*` parts are the junctions a table
/// needs; a box around a block uses the sides and the corners.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Border {
    pub top: String,
    pub bottom: String,
    pub left: String,
    pub right: String,
    pub top_left: String,
    pub top_right: String,
    pub bottom_left: String,
    pub bottom_right: String,
    pub middle_left: String,
    pub middle_right: String,
    pub middle: String,
    pub middle_top: String,
    pub middle_bottom: String,
}

/// Builds a border from its thirteen parts, in the order of the fields.
fn border(parts: [&str; 13]) -> Border {
    let [top, bottom, left, right, top_left, top_right, bottom_left, bottom_right, middle_left, middle_right, middle, middle_top, middle_bottom] =
        parts.map(str::to_string);
    Border {
        top,
        bottom,
        left,
        right,
        top_left,
        top_right,
        bottom_left,
        bottom_right,
        middle_left,
        middle_right,
        middle,
        middle_top,
        middle_bottom,
    }
}

/// A border of thin lines with square corners.
pub fn normal_border() -> Border {
    border([
        "─", "─", "│", "│", "┌", "┐", "└", "┘", "├", "┤", "┼", "┬", "┴",
    ])
}

/// A border of thin lines with rounded corners.
pub fn rounded_border() -> Border {
    border([
        "─", "─", "│", "│", "╭", "╮", "╰", "╯", "├", "┤", "┼", "┬", "┴",
    ])
}

/// A border of full blocks.
pub fn block_border() -> Border {
    border(["█"; 13])
}

/// A border of half blocks that sits outside the frame.
pub fn outer_half_block_border() -> Border {
    border(["▀", "▄", "▌", "▐", "▛", "▜", "▙", "▟", "", "", "", "", ""])
}

/// A border of half blocks that sits inside the frame.
pub fn inner_half_block_border() -> Border {
    border(["▄", "▀", "▐", "▌", "▗", "▖", "▝", "▘", "", "", "", "", ""])
}

/// A border of thick lines.
pub fn thick_border() -> Border {
    border([
        "━", "━", "┃", "┃", "┏", "┓", "┗", "┛", "┣", "┫", "╋", "┳", "┻",
    ])
}

/// A border of double lines.
pub fn double_border() -> Border {
    border([
        "═", "═", "║", "║", "╔", "╗", "╚", "╝", "╠", "╣", "╬", "╦", "╩",
    ])
}

/// A border of spaces: it takes the room of one without drawing it, and
/// still shows a background colour.
pub fn hidden_border() -> Border {
    border([" "; 13])
}

/// A border in the style of a Markdown table.
pub fn markdown_border() -> Border {
    border([
        "-", "-", "|", "|", "|", "|", "|", "|", "|", "|", "|", "|", "|",
    ])
}

/// A border of ASCII characters.
pub fn ascii_border() -> Border {
    border([
        "-", "-", "|", "|", "+", "+", "+", "+", "+", "+", "+", "+", "+",
    ])
}
