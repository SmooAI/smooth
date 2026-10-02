//! The window's state and what it does, toolkit-free: engine events in,
//! keymap actions, keystrokes, attach/resize bookkeeping, and the New Session
//! / close / approval flows. Every rule comes from `smooth-flow-client` or
//! this crate's pure modules (`layout`, `sheet`, `field`, `frames`).
//!
//! The GPUI `Workspace` wraps a [`Core`] and only adds focus, font metrics,
//! repaints and the threads HTTP runs on; `tests/e2e.rs` drives a `Core`
//! directly against a real daemon. Nothing here may depend on GPUI.

use std::collections::{BTreeMap, HashMap, HashSet};

use smooth_flow_client::attention::{self, Approval, Attention};
use smooth_flow_client::close::{self, Scope};
use smooth_flow_client::harness::Harness;
use smooth_flow_client::keymap::{Action, Keymap, Platform};
use smooth_flow_client::pane::{Direction, PaneId, Rect};
use smooth_flow_client::surfaces::Surfaces;
use smooth_flow_client::{fleet, Session};

use crate::discovery::Endpoint;
use crate::frames::{self, Decision, Inbound, NewSession};
use crate::http::{self, Inferred, RepoList};
use crate::keys;
use crate::layout::{self, CellMetrics};
use crate::net::{Event, Outbox};
use crate::sheet::{Effect, Sheet};
use crate::terminal::TerminalModel;

/// Connection state, as the sidebar footer shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Connected,
    Offline(String),
}

/// Where a session we asked the engine to start should open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenIn {
    /// The focused pane when it's empty, else a new tab (New Session).
    Here,
    /// A new tab (New Shell Here).
    NewTab,
}

/// What a confirmation dialog's buttons do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    /// Close the view (pane or tab); the session keeps running.
    Close {
        whole_tab: bool,
    },
    /// End the session, then close the view.
    EndAndClose {
        session: String,
        whole_tab: bool,
    },
    Kill {
        session: String,
        resume: bool,
    },
}

/// A modal question. Cancel is always offered and is the default (Enter/Esc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub title: String,
    pub message: String,
    pub buttons: Vec<(String, Confirmed)>,
    /// Offer "Don't ask again" (the close confirmation setting).
    pub offers_dont_ask: bool,
}

/// An HTTP read the New Session sheet asked for (`GET /api/flow/repos` or
/// `/api/flow/infer`). It blocks, so the GUI runs it on a background thread;
/// hand the answer to [`Core::loaded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch {
    endpoint: Endpoint,
    effect: Effect,
}

/// A [`Fetch`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    Repos { generation: u64, result: Result<RepoList, String> },
    Inferred { generation: u64, result: Result<Inferred, String> },
}

impl Fetch {
    /// Do the request (blocking).
    #[must_use]
    pub fn run(self) -> Loaded {
        match self.effect {
            Effect::Search { query, generation } => Loaded::Repos {
                generation,
                result: http::repos(&self.endpoint, &query),
            },
            Effect::Infer { cwd, generation } => Loaded::Inferred {
                generation,
                result: http::infer(&self.endpoint, cwd.as_deref()),
            },
            // Only the two reads become fetches (see `Core::run_effects`).
            Effect::Start(_) | Effect::Cancel => Loaded::Repos {
                generation: 0,
                result: Err("not a fetch".into()),
            },
        }
    }
}

/// A pane on screen: its frame in whole pixels and the grid that fits it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneFrame {
    pub pane: PaneId,
    pub rect: Rect,
    pub grid: (usize, usize),
}

pub struct Core {
    out: Outbox,
    pub connection: Connection,
    pub machine: String,
    pub endpoint: Option<Endpoint>,
    pub order: Vec<String>,
    pub sessions: HashMap<String, Session>,
    pub attention: HashMap<String, Attention>,
    pub harnesses: Vec<Harness>,
    pub surfaces: Surfaces,
    /// Session → the size it is attached at on this connection.
    attached: HashMap<String, (usize, usize)>,
    pub terminals: HashMap<String, TerminalModel>,
    pub home: String,
    pub keymap: Keymap,
    pub sheet: Option<Sheet>,
    pub dialog: Option<Dialog>,
    pending_open: Option<OpenIn>,
    pub sidebar_visible: bool,
    /// Ask before closing a view of a running session (spec §5, §11).
    confirm_close: bool,
    /// The pane area as last laid out, for directional focus.
    pub pane_area: Rect,
    /// The last engine error or "not yet" note, shown in the footer.
    pub notice: Option<String>,
}

impl Core {
    /// `home` is this machine's `$HOME`, used for titles until the daemon
    /// says its own in `flow.hello`.
    #[must_use]
    pub fn new(out: Outbox, keymap: Keymap, home: String) -> Self {
        Self {
            out,
            connection: Connection::Connecting,
            machine: String::new(),
            endpoint: None,
            order: Vec::new(),
            sessions: HashMap::new(),
            attention: HashMap::new(),
            harnesses: Vec::new(),
            surfaces: Surfaces::new(),
            attached: HashMap::new(),
            terminals: HashMap::new(),
            home,
            keymap,
            sheet: None,
            dialog: None,
            pending_open: None,
            sidebar_visible: true,
            confirm_close: true,
            pane_area: Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            notice: None,
        }
    }

    #[must_use]
    pub fn ordered(&self) -> Vec<Session> {
        self.order.iter().filter_map(|id| self.sessions.get(id).cloned()).collect()
    }

    /// The fleet in sidebar order (grouped), for Focus Session 1–9.
    fn sidebar_order(&self) -> Vec<String> {
        fleet::grouped(&self.ordered()).into_iter().flat_map(|g| g.sessions).collect()
    }

    /// The chord bound to `action`, as this platform writes it ("" if none).
    #[must_use]
    pub fn chord_hint(&self, action: Action) -> String {
        self.keymap.chord(action).map(|c| c.display(Platform::current())).unwrap_or_default()
    }

    /// The text a pane shows instead of a terminal, or `None` when it shows
    /// its session's terminal. A session that ended says so, and when the
    /// engine said why (`launch failed: tmux not found …`, th-9f6814) the
    /// reason is shown. It replaces a blank terminal, so a failed launch is
    /// never just an empty pane.
    #[must_use]
    pub fn pane_hint(&self, pane: PaneId) -> Option<String> {
        let Some(id) = self.surfaces.session_of(pane) else {
            return Some(if self.sessions.is_empty() {
                format!("No sessions yet — New Session ({}).", self.chord_hint(Action::NewSession))
            } else {
                "Empty pane — pick a session in the sidebar.".to_string()
            });
        };
        let has_terminal = self.terminals.contains_key(id);
        let Some(s) = self.sessions.get(id) else {
            return (!has_terminal).then(|| "attaching…".to_string());
        };
        let why = self.attention.get(id).and_then(|a| a.detail.clone()).filter(|d| !d.trim().is_empty());
        if s.is_live() {
            return (!has_terminal).then(|| "attaching…".to_string());
        }
        if has_terminal && why.is_none() {
            // It ran and ended: its last screen is the useful thing to show.
            return None;
        }
        let state = format!("{:?}", s.state).to_lowercase();
        Some(match why {
            Some(why) => format!("{} is {state}: {why}", s.kind),
            None => format!("{} is {state}.", s.kind),
        })
    }

    /// The size `id` is attached at on this connection, if it is.
    #[must_use]
    pub fn attached_size(&self, id: &str) -> Option<(usize, usize)> {
        self.attached.get(id).copied()
    }

    /// The approval the focused pane's session is waiting on, if any.
    #[must_use]
    pub fn focused_approval(&self) -> Option<(String, Approval)> {
        let id = self.surfaces.focused_session()?.to_string();
        let a = attention::approval(self.attention.get(&id))?;
        Some((id, a))
    }

    fn upsert(&mut self, row: frames::Row) -> bool {
        let id = row.session.id.clone();
        let fresh = !self.sessions.contains_key(&id);
        if fresh {
            self.order.push(id.clone());
        }
        match row.attention {
            Some(a) => self.attention.insert(id.clone(), a),
            None => self.attention.remove(&id),
        };
        self.sessions.insert(id, row.session);
        fresh
    }

    /// One event from the connection.
    pub fn apply(&mut self, ev: Event) {
        match ev {
            Event::Connecting => self.connection = Connection::Connecting,
            Event::Connected(endpoint) => {
                self.connection = Connection::Connected;
                self.endpoint = Some(endpoint);
            }
            Event::Offline(why) => {
                self.connection = Connection::Offline(why);
                // A reconnect must attach again.
                self.attached.clear();
            }
            Event::Frame(Inbound::Hello {
                machine,
                home,
                sessions,
                harnesses,
            }) => {
                self.machine = machine;
                // The daemon may be another machine (WSL, a remote host):
                // titles abbreviate against its home, not this one's.
                if let Some(home) = home {
                    self.home = home;
                }
                if let Some(h) = harnesses {
                    self.set_harnesses(h);
                }
                self.order.clear();
                self.sessions.clear();
                self.attention.clear();
                for row in sessions {
                    self.upsert(row);
                }
                for gone in self.surfaces.tabs.iter().flat_map(|t| t.sessions.values().cloned()).collect::<HashSet<_>>() {
                    if !self.sessions.contains_key(&gone) {
                        self.surfaces.forget(&gone);
                    }
                }
                if self.surfaces.tabs.iter().all(|t| t.sessions.is_empty()) {
                    if let Some(first) = self.sidebar_order().into_iter().find(|id| self.sessions.get(id).is_some_and(Session::is_live)) {
                        self.surfaces.show(&first);
                    }
                }
                self.attached.clear();
            }
            Event::Frame(Inbound::Session(row)) => {
                let id = row.session.id.clone();
                if self.upsert(row) {
                    self.open_pending(&id);
                }
            }
            Event::Frame(Inbound::Removed(id)) => {
                self.order.retain(|x| *x != id);
                self.sessions.remove(&id);
                self.attention.remove(&id);
                self.terminals.remove(&id);
                self.attached.remove(&id);
                self.surfaces.forget(&id);
            }
            Event::Frame(Inbound::Attention { id, attention }) => {
                match attention {
                    Some(a) => self.attention.insert(id, a),
                    None => self.attention.remove(&id),
                };
            }
            Event::Frame(Inbound::Harnesses(h)) => self.set_harnesses(h),
            Event::Frame(Inbound::Output { id, bytes, .. }) => {
                self.terminals.entry(id).or_insert_with(|| TerminalModel::new(80, 24)).feed(&bytes);
            }
            Event::Frame(Inbound::Error(message)) => self.notice = Some(message),
        }
    }

    fn set_harnesses(&mut self, h: Vec<Harness>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.set_harnesses(&h);
        }
        self.harnesses = h;
    }

    /// A session we asked for arrived: show it where the request said.
    fn open_pending(&mut self, id: &str) {
        match self.pending_open.take() {
            Some(OpenIn::Here) if self.surfaces.focused_session().is_none() => self.surfaces.show(id),
            Some(OpenIn::Here | OpenIn::NewTab) => self.surfaces.new_tab(Some(id)),
            None => {}
        }
    }

    // ── layout-driven attach ────────────────────────────────────────────

    /// Lay the panes out in `area` (pixels, the window minus the sidebar and
    /// bars), size each one's grid with `metrics`, and attach/resize to match
    /// ([`Self::sync_attach`]). Returns what to draw.
    pub fn layout(&mut self, area: Rect, metrics: CellMetrics) -> Vec<PaneFrame> {
        self.pane_area = area;
        // Whole pixels, so neighbouring panes share an edge exactly.
        let frames: Vec<PaneFrame> = self
            .surfaces
            .visible(area)
            .into_iter()
            .map(|(pane, r)| {
                let (x, y) = (r.x.round(), r.y.round());
                let rect = Rect {
                    x,
                    y,
                    w: (r.x + r.w).round() - x,
                    h: (r.y + r.h).round() - y,
                };
                PaneFrame {
                    pane,
                    rect,
                    grid: metrics.grid(rect),
                }
            })
            .collect();
        let grids: BTreeMap<PaneId, (usize, usize)> = frames.iter().map(|f| (f.pane, f.grid)).collect();
        self.sync_attach(&grids);
        frames
    }

    /// Attach, resize and detach so every session on screen is attached at
    /// the size its panes allow (`layout::session_sizes`: the smallest pane
    /// showing it). Sessions no longer on screen are detached.
    pub fn sync_attach(&mut self, grids: &BTreeMap<PaneId, (usize, usize)>) {
        let wanted = layout::session_sizes(grids.iter().filter_map(|(pane, size)| {
            let s = self.surfaces.session_of(*pane)?;
            self.sessions.get(s).filter(|x| x.is_live()).map(|_| (s, *size))
        }));
        if self.connection != Connection::Connected {
            return;
        }
        for gone in self.attached.keys().filter(|id| !wanted.contains_key(*id)).cloned().collect::<Vec<_>>() {
            self.attached.remove(&gone);
            self.out.send(frames::detach(&gone));
        }
        for (id, (cols, rows)) in wanted {
            let (c16, r16) = (u16::try_from(cols).unwrap_or(u16::MAX), u16::try_from(rows).unwrap_or(u16::MAX));
            let term = self.terminals.entry(id.clone()).or_insert_with(|| TerminalModel::new(cols, rows));
            match self.attached.get(&id) {
                None => {
                    term.resize(cols, rows);
                    self.out.send(frames::attach(&id, c16, r16));
                    self.attached.insert(id, (cols, rows));
                }
                Some(size) if *size != (cols, rows) => {
                    term.resize(cols, rows);
                    self.out.send(frames::resize(&id, c16, r16));
                    self.attached.insert(id, (cols, rows));
                }
                Some(_) => {}
            }
        }
    }

    // ── keyboard ────────────────────────────────────────────────────────

    /// A keystroke: the dialog, then the sheet, then the keymap, else bytes
    /// to the focused pane's session (when it is attached).
    pub fn key(&mut self, key: keys::Key<'_>) -> Vec<Fetch> {
        if self.dialog.is_some() {
            if matches!(key.key, "escape" | "enter") {
                self.dialog = None;
            }
            return Vec::new();
        }
        if let Some(sheet) = &mut self.sheet {
            let effects = sheet.key(key.key, key.key_char, key.control || key.platform, key.shift);
            return self.run_effects(effects);
        }
        if let Some(action) = self.keymap.action_for(&keys::chord(key)) {
            return self.act(action);
        }
        let Some(id) = self.surfaces.focused_session().filter(|id| self.attached.contains_key(*id)).map(str::to_string) else {
            return Vec::new();
        };
        if let Some(bytes) = keys::encode(key) {
            self.out.send(frames::input(&id, &bytes));
        }
        Vec::new()
    }

    /// Run a keymap action (also what the buttons call).
    pub fn act(&mut self, action: Action) -> Vec<Fetch> {
        let area = self.pane_area;
        match action {
            Action::NewSession => return self.open_sheet(),
            Action::NewShell => self.new_shell_here(),
            Action::Allow => self.approve(Decision::Allow),
            Action::Deny => self.approve(Decision::Deny),
            Action::Kill | Action::KillResume => self.ask_kill(action == Action::KillResume),
            Action::NewTab => {
                let s = self.surfaces.focused_session().map(str::to_string);
                self.surfaces.new_tab(s.as_deref());
            }
            Action::ClosePane => self.close(false),
            Action::CloseTab => self.close(true),
            Action::PreviousTab => self.surfaces.cycle(-1),
            Action::NextTab => self.surfaces.cycle(1),
            Action::SplitRight => self.surfaces.split(Direction::Right),
            Action::SplitDown => self.surfaces.split(Direction::Down),
            Action::SplitLeft => self.surfaces.split(Direction::Left),
            Action::SplitUp => self.surfaces.split(Direction::Up),
            Action::FocusPaneLeft => self.surfaces.focus(Direction::Left, area),
            Action::FocusPaneRight => self.surfaces.focus(Direction::Right, area),
            Action::FocusPaneUp => self.surfaces.focus(Direction::Up, area),
            Action::FocusPaneDown => self.surfaces.focus(Direction::Down, area),
            Action::ZoomPane => self.surfaces.toggle_zoom(),
            Action::EqualizePanes => self.surfaces.equalize(),
            Action::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            other => {
                if let Some(i) = other.focus_session_index() {
                    if let Some(id) = self.sidebar_order().get(i).cloned() {
                        self.show_session(&id);
                    }
                } else {
                    self.notice = Some(format!("{} isn't in this build yet", other.title().trim_end_matches('…')));
                }
            }
        }
        Vec::new()
    }

    /// A fleet row was picked: the focused pane shows it.
    pub fn show_session(&mut self, id: &str) {
        self.surfaces.show(id);
        if let Some(s) = self.sessions.get_mut(id) {
            if s.unread {
                s.unread = false;
                self.out.send(serde_json::json!({ "type": "flow.mark_read", "id": id }).to_string());
            }
        }
    }

    fn focused_worktree(&self) -> Option<String> {
        let s = self.sessions.get(self.surfaces.focused_session()?)?;
        Some(if s.worktree.is_empty() { s.project.clone() } else { s.worktree.clone() }).filter(|w| !w.is_empty())
    }

    fn new_shell_here(&mut self) {
        let n = NewSession {
            kind: "shell".into(),
            worktree: self.focused_worktree(),
            ..NewSession::default()
        };
        self.out.send(frames::new_session(&n));
        self.pending_open = Some(OpenIn::NewTab);
    }

    fn approve(&mut self, decision: Decision) {
        match self.focused_approval() {
            Some((id, a)) => self.out.send(frames::approve(&id, &a.request_id, decision)),
            None => self.notice = Some("nothing to approve in the focused pane".into()),
        }
    }

    fn ask_kill(&mut self, resume: bool) {
        let Some(s) = self.surfaces.focused_session().and_then(|id| self.sessions.get(id)).filter(|s| s.is_live()) else {
            return;
        };
        let state = format!("{:?}", s.state).to_lowercase();
        let verb = if resume { "Kill & Resume" } else { "Kill" };
        self.dialog = Some(Dialog {
            title: format!("{verb} {}?", s.kind),
            message: format!("It is {state}. An agent killed mid-turn loses its in-flight work."),
            buttons: vec![(verb.to_string(), Confirmed::Kill { session: s.id.clone(), resume })],
            offers_dont_ask: false,
        });
    }

    /// `closePane` / `closeTab` with the spec's asking rule.
    fn close(&mut self, whole_tab: bool) {
        let (scope, subject, elsewhere) = if whole_tab {
            let scope = if self.surfaces.tabs.len() > 1 { Scope::Tab } else { Scope::Last };
            let at_risk = self
                .surfaces
                .tab_only_sessions()
                .into_iter()
                .find(|id| self.sessions.get(id).is_some_and(close::has_running_process));
            (scope, at_risk, false)
        } else {
            (
                self.surfaces.close_scope(),
                self.surfaces.focused_session().map(str::to_string),
                self.surfaces.shown_elsewhere(),
            )
        };
        let session = subject.as_ref().and_then(|id| self.sessions.get(id));
        match (close::decide(session, scope, elsewhere, self.confirm_close), subject) {
            (Some(p), Some(id)) => {
                self.dialog = Some(Dialog {
                    title: p.title,
                    message: "The session keeps running in the fleet unless you end it.".into(),
                    buttons: vec![
                        (p.close_title, Confirmed::Close { whole_tab }),
                        (p.kill_title, Confirmed::EndAndClose { session: id, whole_tab }),
                    ],
                    offers_dont_ask: true,
                });
            }
            _ => self.close_now(whole_tab),
        }
    }

    fn close_now(&mut self, whole_tab: bool) {
        if whole_tab {
            self.surfaces.close_tab();
        } else {
            self.surfaces.close_pane();
        }
    }

    /// A dialog button.
    pub fn confirm(&mut self, c: Confirmed) {
        self.dialog = None;
        match c {
            Confirmed::Close { whole_tab } => self.close_now(whole_tab),
            Confirmed::EndAndClose { session, whole_tab } => {
                self.out.send(frames::kill(&session, false));
                self.close_now(whole_tab);
            }
            Confirmed::Kill { session, resume } => self.out.send(frames::kill(&session, resume)),
        }
    }

    /// "Don't ask again": stop confirming closes, and take the dialog's
    /// first (non-destructive) choice.
    pub fn dont_ask_again(&mut self) {
        self.confirm_close = false;
        if let Some(Dialog { buttons, .. }) = self.dialog.take() {
            if let Some((_, c)) = buttons.into_iter().next() {
                self.confirm(c);
            }
        }
    }

    // ── New Session ─────────────────────────────────────────────────────

    /// Open the New Session sheet; returns its first loads.
    pub fn open_sheet(&mut self) -> Vec<Fetch> {
        let seed = self.focused_worktree();
        let (sheet, effects) = Sheet::open(&self.harnesses, &self.home, seed.as_deref());
        self.sheet = Some(sheet);
        self.run_effects(effects)
    }

    /// Carry out the sheet's effects: Start sends `flow.new`, Cancel closes
    /// it, and the two reads come back as [`Fetch`]es for the caller to run
    /// off the UI thread.
    pub fn run_effects(&mut self, effects: Vec<Effect>) -> Vec<Fetch> {
        let mut fetches = Vec::new();
        for e in effects {
            match e {
                Effect::Start(n) => {
                    self.out.send(frames::new_session(&n));
                    self.pending_open = Some(OpenIn::Here);
                    self.sheet = None;
                }
                Effect::Cancel => self.sheet = None,
                effect @ (Effect::Search { .. } | Effect::Infer { .. }) => {
                    if let Some(endpoint) = self.endpoint.clone() {
                        fetches.push(Fetch { endpoint, effect });
                    }
                }
            }
        }
        if self.endpoint.is_none() {
            if let Some(s) = &mut self.sheet {
                s.error = Some("not connected to a flow engine".into());
            }
        }
        fetches
    }

    /// A [`Fetch`] answered. Dropped when the sheet has closed since.
    pub fn loaded(&mut self, l: Loaded) {
        let Some(s) = &mut self.sheet else { return };
        match l {
            Loaded::Repos { generation, result } => s.repos_loaded(generation, result),
            Loaded::Inferred { generation, result } => s.inferred_loaded(generation, result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn core() -> (Core, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (out, rx) = Outbox::channel();
        (Core::new(out, Keymap::defaults(Platform::Other), "/home/me".into()), rx)
    }

    fn sent(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<Value> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect()
    }

    fn hello(sessions: &str) -> Event {
        let text = format!(r#"{{"type":"flow.hello","daemon":{{"machine_label":"m","home":"/home/d"}},"sessions":[{sessions}],"harnesses":[]}}"#);
        Event::Frame(frames::parse(&text).expect("hello"))
    }

    fn session(id: &str) -> Event {
        let text = format!(r#"{{"type":"flow.session","session":{{"id":"{id}","kind":"shell","state":"starting"}}}}"#);
        Event::Frame(frames::parse(&text).expect("session"))
    }

    const METRICS: CellMetrics = CellMetrics { width: 8.0, height: 16.0 };
    /// A pane area whose grid is exactly 80×24 at [`METRICS`].
    const AREA: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 80.0 * 8.0 + 12.0,
        h: 24.0 * 16.0 + 12.0,
    };

    #[test]
    fn new_session_sends_flow_new_opens_the_reply_and_attaches_at_the_pane_size() {
        let (mut c, mut rx) = core();
        c.apply(Event::Connected(Endpoint {
            addr: "127.0.0.1:1".into(),
            token: None,
        }));
        c.apply(hello(""));
        assert_eq!(c.home, "/home/d", "titles use the daemon's home");
        let fetches = c.act(Action::NewSession);
        assert_eq!(fetches.len(), 2, "inference + the repo list");
        let sheet = c.sheet.as_mut().expect("sheet");
        sheet.kind = sheet.rows.iter().position(|r| r.kind == "shell").expect("shell row");
        assert!(c
            .key(keys::Key {
                key: "enter",
                ..keys::Key::default()
            })
            .is_empty());
        assert!(c.sheet.is_none(), "Start closes the sheet");
        let new = sent(&mut rx);
        assert_eq!((new[0]["type"].as_str(), new[0]["kind"].as_str()), (Some("flow.new"), Some("shell")));

        c.apply(session("fs-1"));
        assert_eq!(c.surfaces.focused_session(), Some("fs-1"), "the reply opens in the empty focused pane");
        let panes = c.layout(AREA, METRICS);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].grid, (80, 24));
        let attach = sent(&mut rx);
        assert_eq!(attach[0]["type"], "flow.attach");
        assert_eq!((attach[0]["cols"].as_u64(), attach[0]["rows"].as_u64()), (Some(80), Some(24)));
        assert_eq!(c.attached_size("fs-1"), Some((80, 24)));
        c.layout(AREA, METRICS);
        assert!(sent(&mut rx).is_empty(), "an unchanged layout sends nothing");

        c.key(keys::Key {
            key: "a",
            key_char: Some("a"),
            ..keys::Key::default()
        });
        let input = sent(&mut rx);
        assert_eq!((input[0]["type"].as_str(), input[0]["data_b64"].as_str()), (Some("flow.input"), Some("YQ==")));

        c.apply(Event::Frame(Inbound::Output {
            id: "fs-1".into(),
            seq: 1,
            bytes: b"$ hi".to_vec(),
        }));
        assert_eq!(c.terminals["fs-1"].line_text(0), "$ hi");

        c.apply(Event::Offline("gone".into()));
        assert_eq!(c.attached_size("fs-1"), None, "a reconnect attaches again");
    }

    #[test]
    fn keys_go_nowhere_until_attached_and_fetches_need_an_endpoint() {
        let (mut c, mut rx) = core();
        c.apply(hello(r#"{"id":"fs-1","kind":"shell","state":"idle"}"#));
        assert_eq!(c.surfaces.focused_session(), Some("fs-1"), "hello shows the first live session");
        c.key(keys::Key {
            key: "a",
            key_char: Some("a"),
            ..keys::Key::default()
        });
        assert!(sent(&mut rx).is_empty(), "not connected, not attached: nothing is typed");
        assert!(c.open_sheet().is_empty());
        assert_eq!(c.sheet.as_ref().and_then(|s| s.error.as_deref()), Some("not connected to a flow engine"));
        c.loaded(Loaded::Repos {
            generation: 99,
            result: Err("stale".into()),
        });
        assert_eq!(
            c.sheet.as_ref().and_then(|s| s.error.as_deref()),
            Some("not connected to a flow engine"),
            "stale answers drop"
        );
    }

    #[test]
    fn a_pane_says_why_its_session_ended() {
        let (mut c, _rx) = core();
        let pane = c.surfaces.focused_pane();
        assert!(c.pane_hint(pane).is_some_and(|h| h.starts_with("No sessions yet")));
        c.apply(hello(r#"{"id":"fs-1","kind":"shell","state":"starting"}"#));
        assert_eq!(c.pane_hint(pane).as_deref(), Some("attaching…"));
        let dead = r#"{"type":"flow.session","session":{"id":"fs-1","kind":"shell","state":"dead","attention":{"reason":"launch_failed","detail":"launch failed: tmux not found"}}}"#;
        c.apply(Event::Frame(frames::parse(dead).expect("session")));
        assert_eq!(c.pane_hint(pane).as_deref(), Some("shell is dead: launch failed: tmux not found"));
        c.terminals.insert("fs-1".into(), TerminalModel::new(10, 2));
        assert!(c.pane_hint(pane).is_some(), "a reason beats a stale terminal");
        c.attention.clear();
        assert_eq!(c.pane_hint(pane), None, "an ended session with no reason keeps its last screen");
    }
}
