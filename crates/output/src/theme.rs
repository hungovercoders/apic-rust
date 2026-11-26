use std::collections::HashMap;

use crate::style::Style;

/// Every style apic uses for human output, shared by the CLI renderers and
/// the terminal UI so both look the same. Colours collapse to plain text
/// automatically when the colour profile is `Profile::Ascii` (--no-color,
/// NO_COLOR, --json, or output that is not a terminal).
#[derive(Clone, Debug)]
pub struct Theme {
    pub bold: Style,
    pub dim: Style,
    pub ok: Style,
    pub warn: Style,
    pub fail: Style,
    pub url: Style,
    pub header: Style,
    pub capture: Style,
    pub accent: Style,

    // JSON syntax highlighting.
    pub key: Style,
    pub str: Style,
    pub num: Style,
    pub lit: Style,

    methods: HashMap<&'static str, Style>,
    other: Style,
}

impl Default for Theme {
    /// The theme apic ships with.
    fn default() -> Theme {
        let c = |n: &str| Style::new().foreground(n);
        Theme {
            bold: Style::new().bold(true),
            dim: Style::new().faint(true),
            ok: c("42").bold(true),
            warn: c("214").bold(true),
            fail: c("203").bold(true),
            url: c("39"),
            header: c("141"),
            capture: c("81"),
            accent: c("212").bold(true),
            key: c("81"),
            str: c("114"),
            num: c("221"),
            lit: c("176"),
            methods: HashMap::from([
                ("GET", c("39").bold(true)),
                ("HEAD", c("39").bold(true)),
                ("OPTIONS", c("39").bold(true)),
                ("POST", c("42").bold(true)),
                ("PUT", c("214").bold(true)),
                ("PATCH", c("214").bold(true)),
                ("DELETE", c("203").bold(true)),
            ]),
            other: c("176").bold(true),
        }
    }
}

impl Theme {
    /// Renders an HTTP method in its colour.
    pub fn method(&self, m: &str) -> String {
        self.methods.get(m).unwrap_or(&self.other).render(m)
    }

    /// Picks the style for a status class.
    fn status_style(&self, code: i64) -> &Style {
        if code < 300 {
            &self.ok
        } else if code < 400 {
            &self.warn
        } else {
            &self.fail
        }
    }

    /// Renders "200 OK" coloured by status class.
    pub fn status(&self, code: i64, text: &str) -> String {
        self.status_style(code).render(&format!("{code} {text}"))
    }

    /// Renders the status number on its own, coloured by class, for the
    /// places too narrow for the reason phrase.
    pub fn status_code(&self, code: i64) -> String {
        self.status_style(code).render(&code.to_string())
    }

    /// Renders a duration in milliseconds, coloured by how long it took.
    pub fn latency(&self, ms: i64) -> String {
        let s = format!("{ms} ms");
        if ms < 200 {
            self.ok.render(&s)
        } else if ms < 1000 {
            self.warn.render(&s)
        } else {
            self.fail.render(&s)
        }
    }

    /// Renders a pass or fail tick.
    pub fn mark(&self, pass: bool) -> String {
        if pass {
            self.ok.render("✓")
        } else {
            self.fail.render("✗")
        }
    }
}
