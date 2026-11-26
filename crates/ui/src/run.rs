//! What the keys do: moving, filtering, running requests and flows,
//! switching environment, reloading and clearing the session.

use std::sync::{Arc, PoisonError};
use std::time::Instant;

use apic_httpc::CancelToken;
use apic_httpfile::{Request, RequestKey};
use apic_snippet::LANGUAGES;

use crate::keys::Binding;
use crate::program::{async_cmd, batch, quit, Cmd, Msg};
use crate::term::{Key, KeyType};
use crate::ui::{Model, RunState, Tab, TABS};

/// One finished request (a single run, or one step of a flow).
pub struct ResultMsg {
    pub(crate) run_id: u64,
    pub(crate) req: Arc<Request>,
    pub(crate) res: Option<apic_runner::Result>,
    pub(crate) err: Option<apic_runner::Error>,
}

/// What one binding does in the main view.
#[derive(Clone, Copy)]
enum Action {
    Quit,
    Help,
    Escape,
    Down,
    Up,
    Top,
    Bottom,
    Filter,
    Run,
    RunFile,
    RunAll,
    NextTab,
    PrevTab,
    SetTab(Tab),
    Headers,
    Code,
    Open,
    Env,
    PrevEnv,
    Reload,
    Clear,
    LineUp,
    LineDown,
    PageUp,
    PageDown,
}

impl Model {
    pub(crate) fn handle_key(&mut self, msg: Key) -> Option<Cmd> {
        if self.show_help {
            return self.help_key(msg);
        }
        if self.confirm_clear {
            return self.confirm_key(msg);
        }
        if self.filtering {
            return self.filter_key(msg);
        }
        let action = self
            .key_actions()
            .into_iter()
            .find(|(b, _)| b.matches(msg))
            .map(|(_, action)| action)?;
        self.act(action)
    }

    /// Closes the help overlay on esc, ? or q and swallows the rest.
    fn help_key(&mut self, msg: Key) -> Option<Cmd> {
        let k = &self.keys;
        if k.esc.matches(msg) || k.help.matches(msg) || k.quit.matches(msg) {
            self.show_help = false;
        }
        None
    }

    /// Answers the clear-session prompt: y clears, anything else cancels.
    fn confirm_key(&mut self, msg: Key) -> Option<Cmd> {
        self.confirm_clear = false;
        let name = msg.to_string();
        if name == "y" || name == "Y" {
            return self.clear_session();
        }
        None
    }

    /// Edits the filter while it is being typed. The arrows move the
    /// cursor through the matches, as a fuzzy finder does, so a letter is
    /// never needed for that. Only a change to the text puts the cursor
    /// back on the first match; enter, and any key the prompt has no use
    /// for, leave it where it is.
    fn filter_key(&mut self, msg: Key) -> Option<Cmd> {
        match msg.kind {
            KeyType::Enter => self.filtering = false,
            KeyType::Up => self.move_by(-1),
            KeyType::Down => self.move_by(1),
            KeyType::CtrlC => return self.quit(),
            KeyType::Esc => {
                self.filtering = false;
                self.filter.clear();
                self.reset_cursor();
            }
            KeyType::Backspace => {
                self.filter.pop();
                self.reset_cursor();
            }
            KeyType::Rune => {
                self.filter.push(msg.rune);
                self.reset_cursor();
            }
            _ => {}
        }
        None
    }

    /// Puts the cursor on the first row the filter leaves visible.
    fn reset_cursor(&mut self) {
        self.cursor = 0;
        self.clamp_cursor();
        self.selected = self.selected_req();
    }

    /// Lists the main view's bindings in the order they are tried; the
    /// first that matches the key wins.
    fn key_actions(&self) -> Vec<(Binding, Action)> {
        let k = &self.keys;
        vec![
            (k.quit, Action::Quit),
            (k.help, Action::Help),
            (k.esc, Action::Escape),
            (k.down, Action::Down),
            (k.up, Action::Up),
            (k.top, Action::Top),
            (k.bottom, Action::Bottom),
            (k.filter, Action::Filter),
            (k.run, Action::Run),
            (k.run_file, Action::RunFile),
            (k.run_all, Action::RunAll),
            (k.next_tab, Action::NextTab),
            (k.prev_tab, Action::PrevTab),
            (k.tab1, Action::SetTab(Tab::Preview)),
            (k.tab2, Action::SetTab(Tab::Response)),
            (k.tab3, Action::SetTab(Tab::Checks)),
            (k.tab4, Action::SetTab(Tab::Session)),
            (k.tab5, Action::SetTab(Tab::History)),
            (k.headers, Action::Headers),
            (k.curl, Action::Code),
            (k.open, Action::Open),
            (k.env, Action::Env),
            (k.prev_env, Action::PrevEnv),
            (k.reload, Action::Reload),
            (k.clear, Action::Clear),
            (k.line_up, Action::LineUp),
            (k.line_down, Action::LineDown),
            (k.page_up, Action::PageUp),
            (k.page_down, Action::PageDown),
        ]
    }

    fn act(&mut self, action: Action) -> Option<Cmd> {
        let tabs = TABS.len();
        match action {
            Action::Quit => return self.quit(),
            Action::Help => self.show_help = true,
            Action::Escape => return self.escape(),
            Action::Down => self.move_by(1),
            Action::Up => self.move_by(-1),
            Action::Top => self.move_to(true),
            Action::Bottom => self.move_to(false),
            Action::Filter => self.filtering = true,
            Action::Run => return self.run_selected(),
            Action::RunFile => return self.run_flow(self.file_requests()),
            Action::RunAll => return self.run_flow(self.all_requests()),
            Action::NextTab => self.tab = TABS[(self.tab.index() + 1) % tabs],
            Action::PrevTab => self.tab = TABS[(self.tab.index() + tabs - 1) % tabs],
            Action::SetTab(tab) => self.tab = tab,
            Action::Headers => {
                self.show_headers = !self.show_headers;
                self.tab = Tab::Response;
            }
            Action::Code => {
                self.code_lang = (self.code_lang + 1) % (LANGUAGES.len() + 1);
                self.tab = Tab::Response;
            }
            Action::Open => return self.open_editor(),
            Action::Env => return self.cycle_env(1),
            Action::PrevEnv => return self.cycle_env(-1),
            Action::Reload => return self.reload(),
            Action::Clear => return self.ask_clear(),
            Action::LineUp => self.vp.scroll(-1),
            Action::LineDown => self.vp.scroll(1),
            Action::PageUp => self.vp.half_page_up(),
            Action::PageDown => self.vp.half_page_down(),
        }
        None
    }

    /// Cancels a run, else clears the filter, else closes the snippet.
    fn escape(&mut self) -> Option<Cmd> {
        if self.inflight.is_some() {
            self.cancel_run();
            return self.set_status("run cancelled");
        }
        if !self.filter.is_empty() {
            self.filter.clear();
            self.clamp_cursor();
            self.selected = self.selected_req();
        } else if self.code_lang > 0 {
            self.code_lang = 0;
        }
        None
    }

    fn run_selected(&mut self) -> Option<Cmd> {
        let sel = self.selected_req()?;
        self.start_run(vec![sel], false)
    }

    fn run_flow(&mut self, reqs: Vec<Arc<Request>>) -> Option<Cmd> {
        if reqs.is_empty() {
            return None;
        }
        self.start_run(reqs, true)
    }

    /// Opens the clear-session prompt when there is a session to clear
    /// and no run using it.
    fn ask_clear(&mut self) -> Option<Cmd> {
        if self.inflight.is_some() {
            return self.set_status("busy: wait for the run to finish");
        }
        if self.runner.session.is_none() {
            return self.set_status("session disabled by --no-session");
        }
        self.tab = Tab::Session;
        self.confirm_clear = true;
        None
    }

    pub(crate) fn quit(&mut self) -> Option<Cmd> {
        self.cancel_run();
        self.quitting = true;
        Some(Box::new(quit))
    }

    /// Begins a run of one or more requests; only one run at a time.
    fn start_run(&mut self, reqs: Vec<Arc<Request>>, flow: bool) -> Option<Cmd> {
        if self.inflight.is_some() {
            return self.set_status("busy: a run is in progress (esc cancels)");
        }
        self.run_seq += 1;
        for r in &reqs {
            let key = RequestKey::new(r);
            self.results.remove(&key);
            self.errs.remove(&key);
        }
        self.touch();
        let first = reqs[0].clone();
        // A flow runs the whole file, filter or not. Its rows light up one
        // by one only if they are on screen, so a filter that hides some
        // of them goes now rather than leaving the cursor behind the pane.
        let hidden = !self.filter.is_empty() && !self.all_visible(&reqs);
        self.inflight = Some(RunState {
            id: self.run_seq,
            reqs,
            idx: 0,
            flow,
            started: Some(Instant::now()),
            cancel: CancelToken::new(),
        });
        self.code_lang = 0;
        self.tab = Tab::Response;
        let mut cleared = None;
        if hidden {
            self.filter.clear();
            self.filtering = false;
            cleared = self.set_status("filter cleared: the run includes requests it hid");
        }
        self.focus(&first);
        let step = self.step_cmd()?;
        match cleared {
            Some(cleared) => Some(batch(vec![cleared, step])),
            None => Some(step),
        }
    }

    /// Sends the current request of the run in flight. The request goes
    /// out on a thread of its own so the UI keeps redrawing while it is
    /// in flight.
    fn step_cmd(&self) -> Option<Cmd> {
        let rs = self.inflight.as_ref()?;
        let req = rs.current().clone();
        let (run_id, cancel, runner) = (rs.id, rs.cancel.clone(), self.runner.clone());
        Some(async_cmd(move || {
            let (res, err) = runner.run(&cancel, &req);
            Msg::Result(Box::new(ResultMsg {
                run_id,
                req,
                res,
                err,
            }))
        }))
    }

    pub(crate) fn cancel_run(&mut self) {
        let Some(rs) = self.inflight.take() else {
            return;
        };
        rs.cancel.cancel();
        self.run_seq += 1;
    }

    pub(crate) fn handle_result(&mut self, msg: ResultMsg) -> Option<Cmd> {
        let rs = self.inflight.as_mut()?;
        if msg.run_id != rs.id {
            return None;
        }
        rs.idx += 1;
        let more = rs.idx < rs.reqs.len();
        let key = RequestKey::new(&msg.req);
        if let Some(res) = msg.res {
            let res = Arc::new(res);
            self.results.insert(key.clone(), res.clone());
            self.store_deps(&res);
        }
        if let Some(err) = msg.err {
            self.errs.insert(key, err.to_string());
        }
        self.touch();
        self.focus(&msg.req);
        if more {
            return self.step_cmd();
        }
        if let Some(rs) = self.inflight.take() {
            self.finish_run(&rs);
        }
        None
    }

    /// Records the results of the requests `# @ref` ran first, so their
    /// rows show what happened to them too.
    fn store_deps(&mut self, res: &apic_runner::Result) {
        for dep in &res.deps {
            if let Some(req) = dep.req() {
                let key = RequestKey::new(req);
                self.results.insert(key.clone(), Arc::new(dep.clone()));
                self.errs.remove(&key);
            }
            self.store_deps(dep);
        }
    }

    /// Refreshes derived state and the summary once a run completes.
    fn finish_run(&mut self, rs: &RunState) {
        self.refresh_descs();
        self.refresh_session();
        self.hist_seq += 1;
        let mut results: Vec<apic_runner::Result> = Vec::new();
        for r in &rs.reqs {
            let key = RequestKey::new(r);
            if let Some(res) = self.results.get(&key) {
                results.push((**res).clone());
            } else if let Some(err) = self.errs.get(&key) {
                let mut res = apic_runner::Result::default();
                res.request.name = r.name.clone();
                res.request.file = r.file.path.clone();
                res.request.line = r.line;
                res.request.method = r.method.clone();
                res.request.url = r.url.clone();
                res.errors = vec![err.clone()];
                results.push(res);
            }
        }
        if !rs.flow && results.len() == 1 {
            let res = &results[0];
            let id = rs.reqs[0].id();
            if self.errs.contains_key(&RequestKey::new(&rs.reqs[0])) {
                self.last_summary = self.theme.fail.render(&format!("✗ {id}"));
            } else if let Some(response) = &res.response {
                self.last_summary = format!(
                    "{} {id} {} {}",
                    self.theme.mark(res.ok),
                    self.theme.status_code(response.status),
                    self.theme
                        .dim
                        .render(&format!("{} ms", response.duration_ms))
                );
            }
            return;
        }
        self.last_summary = apic_output::summary_line(&self.theme, &results)
            .trim()
            .to_string();
    }

    /// Switches to the environment `delta` places along the list (1 for
    /// the next, -1 for the previous, wrapping) and rebuilds the runner.
    fn cycle_env(&mut self, delta: isize) -> Option<Cmd> {
        if self.inflight.is_some() {
            return self.set_status("busy: wait for the run to finish");
        }
        if self.envs.len() < 2 {
            return match self.envs.first() {
                None => self.set_status("no environments: create http-client.env.json"),
                Some(only) => {
                    let status = format!("only one environment: {only}");
                    self.set_status(&status)
                }
            };
        }
        let n = self.envs.len() as isize;
        let next = match self.envs.iter().position(|e| *e == self.env) {
            Some(i) => self.envs[((i as isize + delta + n) % n) as usize].clone(),
            None => self.envs[0].clone(),
        };
        let status = format!("env {next}");
        self.switch_runner(next, &status)
    }

    /// Re-reads the project from disk.
    pub(crate) fn reload(&mut self) -> Option<Cmd> {
        if self.inflight.is_some() {
            return self.set_status("busy: wait for the run to finish");
        }
        self.switch_runner(self.env.clone(), "project reloaded")
    }

    fn switch_runner(&mut self, env: String, status: &str) -> Option<Cmd> {
        let Some(new_runner) = self.cfg.new_runner.as_mut() else {
            return self.set_status("cannot rebuild the runner here");
        };
        let runner = match new_runner(&env) {
            Ok(runner) => runner,
            Err(err) => return self.set_status(&err.to_string()),
        };
        let sel = self.selected_req().map(|s| s.id()).unwrap_or_default();
        self.runner = runner;
        self.env = env;
        self.last_summary.clear();
        self.rebuild(&sel);
        self.set_status(status)
    }

    fn clear_session(&mut self) -> Option<Cmd> {
        let session = self.runner.session.clone()?;
        let saved = {
            let mut store = session.lock().unwrap_or_else(PoisonError::into_inner);
            store.clear(&self.env);
            store.save()
        };
        if let Err(err) = saved {
            return self.set_status(&format!("session: {err}"));
        }
        if let Some(jar) = self.runner.jar.clone() {
            jar.clear(&self.env);
            if let Err(err) = jar.save() {
                return self.set_status(&format!("cookies: {err}"));
            }
        }
        self.refresh_session();
        self.refresh_descs();
        let status = format!("session cleared for {}", self.env_label());
        self.set_status(&status)
    }
}
