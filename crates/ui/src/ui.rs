//! The model: the whole UI state, how a message changes it, and how it
//! draws the screen.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use apic_gocompat::Time;
use apic_httpc::CancelToken;
use apic_httpfile::{Request, RequestKey};
use apic_output::style::{self, Position};
use apic_output::Theme;
use apic_runner::{Description, Runner};
use apic_session::Cookie;

use crate::history::HistView;
use crate::keys::KeyMap;
use crate::list::{build_items, Item};
use crate::program::{after, Cmd, Msg};
use crate::viewport::{Spinner, Viewport};

/// Rebuilds the runner for an environment, on an environment change or a
/// reload.
pub type NewRunner = Box<dyn FnMut(&str) -> Result<Arc<Runner>, apic_runner::Error>>;

/// Everything the CLI hands the UI; tests build it directly.
pub struct Config {
    /// The project root shown in the status bar.
    pub root: String,
    /// The initial environment.
    pub env: String,
    /// The initial runner.
    pub runner: Arc<Runner>,
    /// Rebuilds the runner on an environment change or a reload.
    pub new_runner: Option<NewRunner>,
    /// The command for the o key; empty disables it.
    pub editor: String,
    /// --redact.
    pub redact: bool,
    /// Shows a demo badge in the status bar.
    pub demo: bool,
    pub theme: Theme,
}

impl Config {
    /// A configuration around a runner, with everything else left out:
    /// no root, the runner's environment, no way to rebuild the runner, no
    /// editor, and the default theme.
    pub fn new(runner: Arc<Runner>) -> Config {
        Config {
            root: String::new(),
            env: String::new(),
            runner,
            new_runner: None,
            editor: String::new(),
            redact: false,
            demo: false,
            theme: Theme::default(),
        }
    }
}

pub(crate) const MIN_WIDTH: i32 = 70;
pub(crate) const MIN_HEIGHT: i32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Preview,
    Response,
    Checks,
    Session,
    History,
}

pub(crate) const TABS: [Tab; 5] = [
    Tab::Preview,
    Tab::Response,
    Tab::Checks,
    Tab::Session,
    Tab::History,
];

pub(crate) const TAB_NAMES: [&str; 5] = ["preview", "response", "checks", "session", "history"];

impl Tab {
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// The run in flight, if any.
pub(crate) struct RunState {
    pub(crate) id: u64,
    pub(crate) reqs: Vec<Arc<Request>>,
    pub(crate) idx: usize,
    pub(crate) flow: bool,
    /// When the run began; `None` leaves the clock out of the status bar.
    pub(crate) started: Option<Instant>,
    pub(crate) cancel: CancelToken,
}

impl RunState {
    pub(crate) fn current(&self) -> &Arc<Request> {
        &self.reqs[self.idx]
    }
}

/// The whole UI state.
pub struct Model {
    pub(crate) cfg: Config,
    pub(crate) theme: Theme,
    pub(crate) runner: Arc<Runner>,
    pub(crate) envs: Vec<String>,
    pub(crate) env: String,

    pub(crate) items: Vec<Item>,
    pub(crate) cursor: usize,
    pub(crate) filter: String,
    pub(crate) filtering: bool,

    pub(crate) descs: HashMap<RequestKey, Description>,
    pub(crate) results: HashMap<RequestKey, Arc<apic_runner::Result>>,
    pub(crate) errs: HashMap<RequestKey, String>,
    /// Moves on whenever a description, a result or an error changes, so
    /// the pane knows to draw its content again.
    pub(crate) data_seq: u64,
    pub(crate) session: BTreeMap<String, String>,
    pub(crate) cookies: Vec<Cookie>,
    /// The history tab's view; `hist_seq` moves on after every run and
    /// reload, so the next draw reads the history again.
    pub(crate) hist: Option<HistView>,
    pub(crate) hist_seq: u64,

    pub(crate) inflight: Option<RunState>,
    pub(crate) run_seq: u64,

    pub(crate) selected: Option<Arc<Request>>,
    pub(crate) tab: Tab,
    pub(crate) show_headers: bool,
    /// 0 hides the code view; n shows `apic_snippet::LANGUAGES[n-1]`.
    pub(crate) code_lang: usize,
    pub(crate) vp: Viewport,
    /// `pane_id` of the content in the viewport.
    pane_key: String,
    /// `pane_topic` of that content; a change scrolls back to the top.
    pane_shown: String,

    pub(crate) width: i32,
    pub(crate) height: i32,
    /// The clock the session tab reads; tests set it.
    pub(crate) now: Box<dyn Fn() -> Time>,
    pub(crate) spin: Spinner,
    pub(crate) keys: KeyMap,
    pub(crate) show_help: bool,
    pub(crate) confirm_clear: bool,
    pub(crate) status: String,
    pub(crate) status_seq: u64,
    pub(crate) last_summary: String,
    pub(crate) quitting: bool,
}

/// One segment of the bottom bar. `drop` says what goes first when the
/// bar is wider than the terminal: the highest number is dropped first,
/// and zero is never dropped.
struct StatusPart {
    text: String,
    drop: u8,
}

/// The divider between the panes with a blank column either side, so
/// neither pane's text runs into the line.
const GUTTER: i32 = 3;

impl Model {
    /// Builds the model from a config.
    pub fn new(cfg: Config) -> Model {
        let mut m = Model {
            theme: cfg.theme.clone(),
            runner: cfg.runner.clone(),
            env: cfg.env.clone(),
            cfg,
            envs: Vec::new(),
            items: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filtering: false,
            descs: HashMap::new(),
            results: HashMap::new(),
            errs: HashMap::new(),
            data_seq: 0,
            session: BTreeMap::new(),
            cookies: Vec::new(),
            hist: None,
            hist_seq: 0,
            inflight: None,
            run_seq: 0,
            selected: None,
            tab: Tab::Preview,
            show_headers: false,
            code_lang: 0,
            vp: Viewport::default(),
            pane_key: String::new(),
            pane_shown: String::new(),
            width: 0,
            height: 0,
            now: Box::new(Time::now),
            spin: Spinner::new(),
            keys: KeyMap::new(),
            show_help: false,
            confirm_clear: false,
            status: String::new(),
            status_seq: 0,
            last_summary: String::new(),
            quitting: false,
        };
        m.rebuild("");
        m
    }

    /// Refreshes everything derived from the runner: the request rows,
    /// environments, descriptions and the session snapshot. `select_id`
    /// keeps the cursor on the same request across a reload when possible.
    pub(crate) fn rebuild(&mut self, select_id: &str) {
        self.envs = self.runner.envs.names();
        if self.env.is_empty() {
            self.env = self.runner.opts.env.clone();
        }
        self.items = build_items(&self.runner.project.requests());
        self.results.clear();
        self.errs.clear();
        self.selected = None;
        self.refresh_descs();
        self.refresh_session();
        self.hist_seq += 1;
        self.cursor = 0;
        self.clamp_cursor();
        if !select_id.is_empty() {
            let at = self
                .visible()
                .iter()
                .position(|it| it.req.as_ref().is_some_and(|r| r.id() == select_id));
            if let Some(i) = at {
                self.cursor = i;
            }
        }
        self.selected = self.selected_req();
        self.pane_key.clear();
        self.pane_shown.clear();
    }

    pub(crate) fn refresh_descs(&mut self) {
        self.data_seq += 1;
        self.descs = self
            .items
            .iter()
            .filter_map(|it| it.req.as_ref())
            .map(|req| (RequestKey::new(req), self.runner.describe(req)))
            .collect();
    }

    pub(crate) fn refresh_session(&mut self) {
        self.session = BTreeMap::new();
        self.cookies = match &self.runner.jar {
            Some(jar) => jar.cookies(&self.env),
            None => Vec::new(),
        };
        if let Some(session) = &self.runner.session {
            self.session = session
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .vars(&self.env);
        }
    }

    /// Handles one message and returns any follow-up work.
    pub fn update(&mut self, msg: Msg) -> Option<Cmd> {
        match msg {
            Msg::Size { width, height } => {
                self.width = width;
                self.height = height;
                self.resized();
            }
            Msg::Tick => {
                if self.inflight.is_some() {
                    // The spinner shows in the list and the status bar,
                    // both redrawn every frame.
                    self.spin.tick();
                }
            }
            Msg::Result(msg) => return self.handle_result(*msg),
            Msg::EditorDone(err) => {
                return match err {
                    Some(err) => self.set_status(&format!("editor: {err}")),
                    None => self.reload(),
                };
            }
            Msg::ClearStatus(seq) => {
                if seq == self.status_seq {
                    self.status.clear();
                }
            }
            Msg::Key(key) => return self.handle_key(key),
            _ => {}
        }
        None
    }

    /// Renders the whole screen.
    pub fn view(&mut self) -> String {
        if self.quitting {
            return String::new();
        }
        if self.width == 0 || self.height == 0 {
            return "loading…".to_string();
        }
        if self.width < MIN_WIDTH || self.height < MIN_HEIGHT {
            return format!(
                "terminal too small: apic ui needs {MIN_WIDTH}x{MIN_HEIGHT}, have {}x{}",
                self.width, self.height
            );
        }
        self.sync_pane();
        let (left_w, right_w) = self.pane_widths();
        let content_h = self.height - 1;
        let left = self.render_list(left_w, content_h);
        let right = format!("{}\n{}", self.render_tabs(right_w), self.vp.view());
        let sep = format!(" {} \n", self.theme.dim.render("│")).repeat(content_h as usize);
        let sep = sep.trim_end_matches('\n');
        let body = style::join_horizontal(Position::TOP, &[&left, sep, &right]);
        format!("{body}\n{}", self.render_status())
    }

    /// Shares the screen out: the list takes two fifths, within bounds
    /// that keep an id and its status readable on a narrow terminal and
    /// leave a wide one's extra columns to the response.
    fn pane_widths(&self) -> (i32, i32) {
        let left = (self.width * 2 / 5).clamp(28, 52);
        (left, self.width - left - GUTTER)
    }

    fn resized(&mut self) {
        if self.width < MIN_WIDTH || self.height < MIN_HEIGHT {
            return;
        }
        let (_, right_w) = self.pane_widths();
        self.vp.width = right_w;
        self.vp.height = self.height - 2;
        self.pane_key.clear();
    }

    /// Refreshes the viewport content when what it shows has changed. The
    /// scroll position survives a re-render of the same subject (a result
    /// arriving, a resize) and only resets when the pane turns to
    /// something else.
    fn sync_pane(&mut self) {
        let key = self.pane_id();
        if key == self.pane_key {
            return;
        }
        let topic = self.pane_topic();
        let changed = topic != self.pane_shown;
        self.pane_key = key;
        self.pane_shown = topic;
        let content = self.render_pane(self.vp.width);
        self.vp
            .set_content(&style::hardwrap(&content, self.vp.width, true));
        if changed {
            self.vp.goto_top();
        }
    }

    fn pane_req_id(&self) -> String {
        self.selected.as_ref().map(|r| r.id()).unwrap_or_default()
    }

    /// Identifies what the pane is about, ignoring content that changes
    /// underneath it. Switching request, tab or overlay changes the topic;
    /// a result landing on the request already shown does not.
    fn pane_topic(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.pane_req_id(),
            self.tab.index(),
            self.code_lang,
            self.show_help,
            self.confirm_clear
        )
    }

    fn pane_id(&self) -> String {
        let clock = if self.live_clock() {
            (self.now)().unix()
        } else {
            0
        };
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.pane_req_id(),
            self.tab.index(),
            self.show_headers,
            self.code_lang,
            self.show_help,
            self.confirm_clear,
            self.data_seq,
            self.session.len(),
            self.cookies.len(),
            self.hist_seq,
            self.vp.width,
            self.vp.height,
            clock
        )
    }

    /// Reports whether the pane shows something that changes with the
    /// clock: the session tab, when it holds a cached token whose expiry
    /// it counts down.
    fn live_clock(&self) -> bool {
        if self.tab != Tab::Session || self.show_help || self.confirm_clear {
            return false;
        }
        self.session.keys().any(|k| is_cached_token(k))
    }

    /// Reports whether the screen changes on its own: a run is in flight,
    /// or the pane counts a token's expiry down. The program ticks only
    /// then.
    pub fn live(&self) -> bool {
        self.inflight.is_some() || self.live_clock()
    }

    /// Draws the bottom bar. On a narrow terminal it gives up the project
    /// path, then the last run's summary, then the environment, so the key
    /// hint and anything in flight stay in view rather than being cut off
    /// at the right.
    fn render_status(&self) -> String {
        let t = &self.theme;
        let part = |text: String, drop: u8| StatusPart { text, drop };
        let mut parts = vec![part(t.accent.render("apic"), 0)];
        if self.cfg.demo {
            parts.push(part(t.warn.render("demo"), 0));
        }
        if self.cfg.redact {
            parts.push(part(t.warn.render("redact"), 0));
        }
        parts.push(part(
            t.dim.render(&apic_output::truncate(&self.cfg.root, 30)),
            3,
        ));
        let env = if self.env.is_empty() {
            "(none)"
        } else {
            &self.env
        };
        parts.push(part(format!("env={}", t.bold.render(env)), 1));
        if let Some(rs) = &self.inflight {
            let mut running = format!(
                "{} running {}",
                t.accent.render(self.spin.view()),
                rs.current().id()
            );
            if rs.flow {
                running.push_str(&format!(" ({}/{})", rs.idx + 1, rs.reqs.len()));
            }
            if let Some(started) = rs.started {
                running.push(' ');
                running.push_str(&t.dim.render(&short_duration(started.elapsed())));
            }
            parts.push(part(running, 0));
        } else if !self.last_summary.is_empty() {
            parts.push(part(format!("last: {}", self.last_summary), 2));
        }
        if !self.status.is_empty() {
            parts.push(part(t.warn.render(&self.status), 0));
        }
        parts.push(part(t.dim.render("? help · q quit"), 0));
        let sep = t.dim.render(" · ");
        let join = |parts: &[StatusPart]| {
            parts
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join(&sep)
        };
        let mut line = join(&parts);
        while style::string_width(&line) as i32 > self.width {
            let mut at = None;
            let mut drop = 0;
            for (i, p) in parts.iter().enumerate() {
                if p.drop > drop {
                    at = Some(i);
                    drop = p.drop;
                }
            }
            let Some(at) = at else {
                break;
            };
            parts.remove(at);
            line = join(&parts);
        }
        style::truncate(&line, self.width, "…")
    }

    /// Shows a transient message in the status bar for a few seconds.
    pub(crate) fn set_status(&mut self, s: &str) -> Option<Cmd> {
        self.status = s.to_string();
        self.status_seq += 1;
        Some(after(
            Duration::from_secs(4),
            Msg::ClearStatus(self.status_seq),
        ))
    }

    /// Returns the request under the cursor (for tests).
    pub fn selected(&self) -> Option<Arc<Request>> {
        self.selected_req()
    }

    /// Returns the current environment (for tests).
    pub fn env(&self) -> &str {
        &self.env
    }

    /// Reports whether a run is in flight (for tests).
    pub fn running(&self) -> bool {
        self.inflight.is_some()
    }

    /// Returns the recorded result for a request (for tests).
    pub fn result(&self, req: &Arc<Request>) -> Option<&apic_runner::Result> {
        self.results.get(&RequestKey::new(req)).map(Arc::as_ref)
    }

    /// Returns the transient status text (for tests).
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Says a description, a result or an error changed, so the pane
    /// draws its content again.
    pub(crate) fn touch(&mut self) {
        self.data_seq += 1;
    }
}

/// Reports whether a session key holds a cached token, which the session
/// tab describes rather than shows.
pub(crate) fn is_cached_token(key: &str) -> bool {
    key.starts_with("$oauth2:") || key.starts_with("$exec:")
}

/// Renders a run clock: milliseconds under a second, then tenths, so the
/// status bar keeps a steady width while a request is out.
pub(crate) fn short_duration(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        return format!("{}ms", d.as_millis());
    }
    format!("{:.1}s", d.as_secs_f64())
}
