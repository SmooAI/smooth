//! The window's state and what it does: engine events in, keymap actions,
//! attach/resize bookkeeping, and the New Session / close / approval flows.
//! Every rule comes from `smooth-flow-client` or this app's pure modules
//! (`layout`, `sheet`, `field`, `frames`); the `view` modules only draw.

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui_kit::*;
use smooth_flow_client::attention::{self, Approval, Attention};
use smooth_flow_client::close::{self, Scope};
use smooth_flow_client::harness::Harness;
use smooth_flow_client::keymap::{Action, Keymap, Platform};
use smooth_flow_client::pane::{Direction, PaneId, Rect};
use smooth_flow_client::surfaces::Surfaces;
use smooth_flow_client::{fleet, Session};

use crate::discovery::Endpoint;
use crate::frames::{self, Decision, Inbound, NewSession};
use crate::http;
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

pub struct Workspace {
    pub(crate) focus: FocusHandle,
    out: Outbox,
    pub(crate) connection: Connection,
    pub(crate) machine: String,
    endpoint: Option<Endpoint>,
    pub(crate) order: Vec<String>,
    pub(crate) sessions: HashMap<String, Session>,
    pub(crate) attention: HashMap<String, Attention>,
    pub(crate) harnesses: Vec<Harness>,
    pub(crate) surfaces: Surfaces,
    /// Session → the size it is attached at on this connection.
    attached: HashMap<String, (usize, usize)>,
    pub(crate) terminals: HashMap<String, TerminalModel>,
    pub(crate) home: String,
    pub(crate) keymap: Keymap,
    pub(crate) metrics: Option<CellMetrics>,
    pub(crate) sheet: Option<Sheet>,
    pub(crate) dialog: Option<Dialog>,
    pending_open: Option<OpenIn>,
    pub(crate) sidebar_visible: bool,
    /// Ask before closing a view of a running session (spec §5, §11).
    confirm_close: bool,
    /// The pane area as last laid out, for directional focus.
    pub(crate) pane_area: Rect,
    /// The last engine error or "not yet" note, shown in the footer.
    pub(crate) notice: Option<String>,
}

impl Workspace {
    pub fn new(out: Outbox, events: futures::channel::mpsc::UnboundedReceiver<Event>, keymap: Keymap, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            let mut events = events;
            while let Some(ev) = events.next().await {
                if this.update(cx, |ws, cx| ws.apply(ev, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            focus: cx.focus_handle(),
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
            home: dirs_next::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
            keymap,
            metrics: None,
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

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub(crate) fn ordered(&self) -> Vec<Session> {
        self.order.iter().filter_map(|id| self.sessions.get(id).cloned()).collect()
    }

    /// The fleet in sidebar order (grouped), for Focus Session 1–9.
    fn sidebar_order(&self) -> Vec<String> {
        fleet::grouped(&self.ordered()).into_iter().flat_map(|g| g.sessions).collect()
    }

    /// The approval the focused pane's session is waiting on, if any.
    pub(crate) fn focused_approval(&self) -> Option<(String, Approval)> {
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

    fn apply(&mut self, ev: Event, cx: &mut Context<Self>) {
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
        cx.notify();
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

    /// Attach, resize and detach so every session on screen is attached at
    /// the size its panes allow (`layout::session_sizes`: the smallest pane
    /// showing it). Sessions no longer on screen are detached.
    pub(crate) fn sync_attach(&mut self, grids: &BTreeMap<PaneId, (usize, usize)>) {
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

    pub(crate) fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let key = keys::Key {
            key: &k.key,
            key_char: k.key_char.as_deref(),
            control: k.modifiers.control,
            alt: k.modifiers.alt,
            shift: k.modifiers.shift,
            platform: k.modifiers.platform,
        };
        if self.dialog.is_some() {
            if matches!(key.key, "escape" | "enter") {
                self.dialog = None;
                cx.notify();
            }
            return;
        }
        if let Some(sheet) = &mut self.sheet {
            let effects = sheet.key(key.key, key.key_char, key.control || key.platform, key.shift);
            self.run_effects(effects, cx);
            cx.notify();
            return;
        }
        if let Some(action) = self.keymap.action_for(&keys::chord(key)) {
            self.act(action, cx);
            return;
        }
        let Some(id) = self.surfaces.focused_session().filter(|id| self.attached.contains_key(*id)).map(str::to_string) else {
            return;
        };
        if let Some(bytes) = keys::encode(key) {
            self.out.send(frames::input(&id, &bytes));
        }
    }

    /// Run a keymap action (also what the buttons call).
    pub(crate) fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        let area = self.pane_area;
        match action {
            Action::NewSession => self.open_sheet(cx),
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
        cx.notify();
    }

    /// A fleet row was picked: the focused pane shows it.
    pub(crate) fn show_session(&mut self, id: &str) {
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
    pub(crate) fn confirm(&mut self, c: Confirmed, cx: &mut Context<Self>) {
        self.dialog = None;
        match c {
            Confirmed::Close { whole_tab } => self.close_now(whole_tab),
            Confirmed::EndAndClose { session, whole_tab } => {
                self.out.send(frames::kill(&session, false));
                self.close_now(whole_tab);
            }
            Confirmed::Kill { session, resume } => self.out.send(frames::kill(&session, resume)),
        }
        cx.notify();
    }

    pub(crate) fn dont_ask_again(&mut self, cx: &mut Context<Self>) {
        self.confirm_close = false;
        if let Some(Dialog { buttons, .. }) = self.dialog.take() {
            if let Some((_, c)) = buttons.into_iter().next() {
                self.confirm(c, cx);
            }
        }
    }

    // ── New Session ─────────────────────────────────────────────────────

    pub(crate) fn open_sheet(&mut self, cx: &mut Context<Self>) {
        let seed = self.focused_worktree();
        let (sheet, effects) = Sheet::open(&self.harnesses, &self.home, seed.as_deref());
        self.sheet = Some(sheet);
        self.run_effects(effects, cx);
    }

    pub(crate) fn run_effects(&mut self, effects: Vec<Effect>, cx: &mut Context<Self>) {
        for e in effects {
            match e {
                Effect::Start(n) => {
                    self.out.send(frames::new_session(&n));
                    self.pending_open = Some(OpenIn::Here);
                    self.sheet = None;
                }
                Effect::Cancel => self.sheet = None,
                Effect::Search { query, generation } => {
                    let Some(ep) = self.endpoint.clone() else { continue };
                    cx.spawn(async move |this, cx| {
                        let r = cx.background_spawn(async move { http::repos(&ep, &query) }).await;
                        this.update(cx, |ws, cx| {
                            if let Some(s) = &mut ws.sheet {
                                s.repos_loaded(generation, r);
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
                Effect::Infer { cwd, generation } => {
                    let Some(ep) = self.endpoint.clone() else { continue };
                    cx.spawn(async move |this, cx| {
                        let r = cx.background_spawn(async move { http::infer(&ep, cwd.as_deref()) }).await;
                        this.update(cx, |ws, cx| {
                            if let Some(s) = &mut ws.sheet {
                                s.inferred_loaded(generation, r);
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
            }
        }
        if self.endpoint.is_none() {
            if let Some(s) = &mut self.sheet {
                s.error = Some("not connected to a flow engine".into());
            }
        }
        cx.notify();
    }
}

/// The user's keymap: `~/.smooth/smoothflow/keybindings.toml` over the
/// platform defaults (spec §9). Problems are printed, never fatal.
pub fn load_keymap() -> Keymap {
    let path = dirs_next::home_dir().unwrap_or_default().join(".smooth/smoothflow/keybindings.toml");
    let map = std::fs::read_to_string(&path).map_or_else(|_| Keymap::defaults(Platform::current()), |t| Keymap::parse(&t, Platform::current()));
    for p in &map.problems {
        eprintln!("smoothflow: {}: {p}", path.display());
    }
    for (chord, actions) in map.conflicts() {
        let names: Vec<&str> = actions.iter().map(|a| a.name()).collect();
        eprintln!("smoothflow: {} is bound to {} — the first wins", chord.wire(), names.join(", "));
    }
    map
}
