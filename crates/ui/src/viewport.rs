//! The scrollable window the right pane shows its content through, and
//! the spinner.

/// A scrollable window over pre-wrapped lines.
#[derive(Debug, Default)]
pub(crate) struct Viewport {
    pub(crate) lines: Vec<String>,
    pub(crate) offset: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

impl Viewport {
    pub(crate) fn set_content(&mut self, s: &str) {
        self.lines = s.split('\n').map(str::to_string).collect();
        self.clamp();
    }

    fn len(&self) -> i32 {
        self.lines.len() as i32
    }

    fn clamp(&mut self) {
        let max = (self.len() - self.height).max(0);
        if self.offset > max {
            self.offset = max;
        }
        if self.offset < 0 {
            self.offset = 0;
        }
    }

    pub(crate) fn goto_top(&mut self) {
        self.offset = 0;
    }

    pub(crate) fn scroll(&mut self, n: i32) {
        self.offset += n;
        self.clamp();
    }

    pub(crate) fn half_page_up(&mut self) {
        self.scroll(-self.height / 2);
    }

    pub(crate) fn half_page_down(&mut self) {
        self.scroll(self.height / 2);
    }

    /// Reports whether the last line is visible.
    pub(crate) fn at_bottom(&self) -> bool {
        self.offset >= self.len() - self.height
    }

    /// Returns exactly `height` lines, padded with blanks.
    pub(crate) fn view(&mut self) -> String {
        if self.height <= 0 {
            return String::new();
        }
        self.clamp();
        let end = (self.offset + self.height).min(self.len());
        let mut out: Vec<&str> = Vec::with_capacity(self.height as usize);
        if self.offset < end {
            out.extend(
                self.lines[self.offset as usize..end as usize]
                    .iter()
                    .map(String::as_str),
            );
        }
        while (out.len() as i32) < self.height {
            out.push("");
        }
        out.join("\n")
    }
}

/// A small braille spinner advanced by the program's ticker.
#[derive(Debug)]
pub(crate) struct Spinner {
    pub(crate) frames: &'static [&'static str],
    i: usize,
}

impl Spinner {
    pub(crate) fn new() -> Spinner {
        Spinner {
            frames: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            i: 0,
        }
    }

    pub(crate) fn tick(&mut self) {
        self.i = (self.i + 1) % self.frames.len();
    }

    pub(crate) fn view(&self) -> &'static str {
        self.frames[self.i]
    }
}
