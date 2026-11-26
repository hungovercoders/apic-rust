//! The key bindings and the help overlay's grouping of them.

use crate::term::Key;

/// A set of key names with a help entry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Binding {
    keys: &'static [&'static str],
    /// Shown in help, e.g. "↑/k".
    pub(crate) name: &'static str,
    pub(crate) help: &'static str,
}

const fn bind(name: &'static str, help: &'static str, keys: &'static [&'static str]) -> Binding {
    Binding { keys, name, help }
}

impl Binding {
    /// Reports whether `k` triggers the binding.
    pub(crate) fn matches(&self, k: Key) -> bool {
        let name = k.to_string();
        self.keys.contains(&name.as_str())
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct KeyMap {
    pub(crate) up: Binding,
    pub(crate) down: Binding,
    pub(crate) top: Binding,
    pub(crate) bottom: Binding,
    pub(crate) run: Binding,
    pub(crate) run_file: Binding,
    pub(crate) run_all: Binding,
    pub(crate) filter: Binding,
    pub(crate) next_tab: Binding,
    pub(crate) prev_tab: Binding,
    pub(crate) tab1: Binding,
    pub(crate) tab2: Binding,
    pub(crate) tab3: Binding,
    pub(crate) tab4: Binding,
    pub(crate) tab5: Binding,
    pub(crate) headers: Binding,
    pub(crate) curl: Binding,
    pub(crate) open: Binding,
    pub(crate) env: Binding,
    pub(crate) prev_env: Binding,
    pub(crate) reload: Binding,
    pub(crate) clear: Binding,
    pub(crate) line_up: Binding,
    pub(crate) line_down: Binding,
    pub(crate) page_up: Binding,
    pub(crate) page_down: Binding,
    pub(crate) esc: Binding,
    pub(crate) help: Binding,
    pub(crate) quit: Binding,
}

impl KeyMap {
    pub(crate) fn new() -> KeyMap {
        KeyMap {
            up: bind("↑/k", "up", &["k", "up"]),
            down: bind("↓/j", "down", &["j", "down"]),
            top: bind("g", "first request", &["g", "home"]),
            bottom: bind("G", "last request", &["G", "end"]),
            run: bind("enter", "run selected", &["enter"]),
            run_file: bind("f", "run the file as a flow", &["f"]),
            run_all: bind("a", "run everything", &["a"]),
            filter: bind("/", "filter requests", &["/"]),
            next_tab: bind("tab/l", "next tab", &["tab", "l", "right"]),
            prev_tab: bind("shift+tab/h", "previous tab", &["shift+tab", "h", "left"]),
            tab1: bind("1", "preview tab", &["1"]),
            tab2: bind("2", "response tab", &["2"]),
            tab3: bind("3", "checks tab", &["3"]),
            tab4: bind("4", "session tab", &["4"]),
            tab5: bind("5", "history tab", &["5"]),
            headers: bind("H", "toggle headers", &["H"]),
            curl: bind("c", "show as code: curl, httpie … go, then hide", &["c"]),
            open: bind("o", "open in $EDITOR", &["o"]),
            env: bind("e", "next environment", &["e"]),
            prev_env: bind("E", "previous environment", &["E"]),
            reload: bind("r", "reload the project", &["r"]),
            clear: bind("x", "clear the session", &["x"]),
            line_up: bind("K", "scroll the pane a line up", &["K"]),
            line_down: bind("J", "scroll the pane a line down", &["J"]),
            page_up: bind("pgup/ctrl+u", "scroll the pane up", &["pgup", "ctrl+u"]),
            page_down: bind("pgdn/ctrl+d", "scroll the pane down", &["pgdown", "ctrl+d"]),
            esc: bind("esc", "cancel, or go back", &["esc"]),
            help: bind("?", "this help", &["?"]),
            quit: bind("q", "quit", &["q", "ctrl+c"]),
        }
    }

    /// Groups the bindings for the help overlay.
    pub(crate) fn columns(&self) -> Vec<Vec<Binding>> {
        let k = self;
        vec![
            vec![k.up, k.down, k.top, k.bottom, k.filter],
            vec![
                k.run, k.run_file, k.run_all, k.esc, k.env, k.prev_env, k.reload, k.open,
            ],
            vec![
                k.next_tab, k.prev_tab, k.tab1, k.tab2, k.tab3, k.tab4, k.tab5,
            ],
            vec![k.line_up, k.line_down, k.page_up, k.page_down],
            vec![k.headers, k.curl, k.clear, k.help, k.quit],
        ]
    }
}
