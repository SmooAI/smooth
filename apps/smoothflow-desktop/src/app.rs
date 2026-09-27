//! The window: fleet sidebar + the focused session's GPU-rendered terminal
//! (Client Spec §4). Everything that decides *what* to show lives in
//! `smooth-flow-client`; this module only draws it.

use std::collections::HashMap;

use gpui_kit::*;
use smooth_flow_client::{fleet, title, Session, SessionState};

use crate::frames::{self, Inbound};
use crate::keys;
use crate::net::{Event, Outbox};
use crate::terminal::{theme, TerminalModel};

const SIDEBAR_WIDTH: f32 = 260.0;
const FONT_SIZE: f32 = 13.0;
/// Monospace cell metrics as multiples of the font size. Approximate until the
/// renderer measures the font (tracked for the terminal element).
const CELL_WIDTH: f32 = 0.6;
const LINE_HEIGHT: f32 = 1.3;

fn mono_family() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(windows) {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

fn hsla(rgb24: u32) -> Hsla {
    rgb(rgb24).into()
}

/// Connection state, as the sidebar footer shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Connected,
    Offline(String),
}

pub struct Workspace {
    focus: FocusHandle,
    out: Outbox,
    connection: Connection,
    machine: String,
    order: Vec<String>,
    sessions: HashMap<String, Session>,
    focused: Option<String>,
    attached: Option<String>,
    terminals: HashMap<String, TerminalModel>,
    home: String,
}

impl Workspace {
    pub fn new(out: Outbox, events: futures::channel::mpsc::UnboundedReceiver<Event>, cx: &mut Context<Self>) -> Self {
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
            order: Vec::new(),
            sessions: HashMap::new(),
            focused: None,
            attached: None,
            terminals: HashMap::new(),
            home: dirs_next::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn ordered(&self) -> Vec<Session> {
        self.order.iter().filter_map(|id| self.sessions.get(id).cloned()).collect()
    }

    fn apply(&mut self, ev: Event, cx: &mut Context<Self>) {
        match ev {
            Event::Connecting => self.connection = Connection::Connecting,
            Event::Connected => self.connection = Connection::Connected,
            Event::Offline(why) => {
                self.connection = Connection::Offline(why);
                // A reconnect must attach again.
                self.attached = None;
            }
            Event::Frame(Inbound::Hello { machine, sessions }) => {
                self.machine = machine;
                self.order = sessions.iter().map(|s| s.id.clone()).collect();
                self.sessions = sessions.into_iter().map(|s| (s.id.clone(), s)).collect();
                if self.focused.as_ref().is_none_or(|f| !self.sessions.contains_key(f)) {
                    self.focused = self.order.iter().find(|id| self.sessions.get(*id).is_some_and(Session::is_live)).cloned();
                }
                self.attached = None;
            }
            Event::Frame(Inbound::Session(s)) => {
                if !self.sessions.contains_key(&s.id) {
                    self.order.push(s.id.clone());
                }
                self.sessions.insert(s.id.clone(), s);
            }
            Event::Frame(Inbound::Removed(id)) => {
                self.order.retain(|x| *x != id);
                self.sessions.remove(&id);
                self.terminals.remove(&id);
                if self.focused.as_deref() == Some(&id) {
                    self.focused = None;
                }
            }
            Event::Frame(Inbound::Output { id, bytes, .. }) => {
                self.terminals.entry(id).or_insert_with(|| TerminalModel::new(80, 24)).feed(&bytes);
            }
            Event::Frame(Inbound::Error(_)) => {}
        }
        cx.notify();
    }

    fn focus_session(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(prev) = self.attached.take() {
            if prev != id {
                self.out.send(frames::detach(&prev));
            }
        }
        self.focused = Some(id);
        cx.notify();
    }

    /// Attach the focused session at the grid size the window allows, or
    /// resize it when the window changed.
    fn sync_attach(&mut self, cols: usize, rows: usize) {
        let Some(id) = self.focused.clone() else { return };
        if !self.sessions.get(&id).is_some_and(Session::is_live) {
            return;
        }
        let term = self.terminals.entry(id.clone()).or_insert_with(|| TerminalModel::new(cols, rows));
        let (c16, r16) = (u16::try_from(cols).unwrap_or(u16::MAX), u16::try_from(rows).unwrap_or(u16::MAX));
        if self.attached.as_deref() != Some(id.as_str()) {
            term.resize(cols, rows);
            self.out.send(frames::attach(&id, c16, r16));
            self.attached = Some(id);
        } else if term.size() != (cols, rows) {
            term.resize(cols, rows);
            self.out.send(frames::resize(&id, c16, r16));
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, _: &mut Context<Self>) {
        let Some(id) = self.attached.clone() else { return };
        let k = &ev.keystroke;
        let key = keys::Key {
            key: &k.key,
            key_char: k.key_char.as_deref(),
            control: k.modifiers.control,
            alt: k.modifiers.alt,
            shift: k.modifiers.shift,
            platform: k.modifiers.platform,
        };
        if let Some(bytes) = keys::encode(key) {
            self.out.send(frames::input(&id, &bytes));
        }
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ordered = self.ordered();
        let counts = fleet::counts(&ordered);
        let mut list = div().flex().flex_col().gap_1().p_2().flex_1();
        for group in fleet::grouped(&ordered) {
            list = list.child(div().text_xs().text_color(hsla(0x9399b2)).mt_2().child(group.project.clone()));
            for id in group.sessions {
                let Some(s) = self.sessions.get(&id) else { continue };
                let selected = self.focused.as_deref() == Some(id.as_str());
                let dot = match s.state {
                    SessionState::Working | SessionState::Starting => 0x89b4fa,
                    SessionState::NeedsYou | SessionState::Limited => 0xf9e2af,
                    SessionState::Idle => 0xa6e3a1,
                    SessionState::Done | SessionState::Dead => 0x6c7086,
                };
                let label = title::tab_title(s, &self.home);
                let state = format!("{:?}", s.state).to_lowercase();
                let row_id = id.clone();
                let mut row = div().flex().items_center().gap_2().px_2().py_1().rounded_md();
                if selected {
                    row = row.bg(hsla(0x313244));
                }
                list = list.child(
                    row
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| this.focus_session(row_id.clone(), cx)))
                        .child(div().size_2().rounded_full().bg(hsla(dot)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_sm().text_color(hsla(theme::FOREGROUND)).child(label))
                                .child(div().text_xs().text_color(hsla(0x7f849c)).child(state)),
                        ),
                );
            }
        }
        let footer = match &self.connection {
            Connection::Connected => format!("● connected · {}", self.machine),
            Connection::Connecting => "connecting…".to_string(),
            Connection::Offline(why) => format!("offline — {why}"),
        };
        div()
            .flex()
            .flex_col()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(hsla(0x181825))
            .border_r_1()
            .border_color(hsla(0x313244))
            .child(div().p_2().text_xs().text_color(hsla(0x9399b2)).child(format!("FLEET · {} sessions", ordered.len())))
            .child(list)
            .child(
                div()
                    .p_2()
                    .text_xs()
                    .text_color(hsla(0x7f849c))
                    .child(footer)
                    .child(format!("{} working · {} need you · {} done · {} idle", counts.working, counts.needs_you, counts.done, counts.idle)),
            )
    }

    fn terminal_view(&mut self, cols: usize, rows: usize) -> Div {
        self.sync_attach(cols, rows);
        let family = mono_family();
        let Some(term) = self.focused.as_ref().and_then(|id| self.terminals.get(id)) else {
            let hint = if self.sessions.is_empty() { "No sessions yet. Start one with `th flow new` (New Session is coming here next)." } else { "Pick a session in the sidebar." };
            return div().flex().flex_1().items_center().justify_center().text_color(hsla(0x7f849c)).child(hint);
        };
        let screen = term.screen();
        let mut body = div().flex().flex_col().p_2().font_family(family).text_size(px(FONT_SIZE)).line_height(px(FONT_SIZE * LINE_HEIGHT));
        for runs in &screen.rows {
            let mut text = String::new();
            let mut text_runs = Vec::with_capacity(runs.len() + 1);
            for run in runs {
                let mut color = hsla(run.fg);
                let mut background = run.bg.map(hsla);
                text.push_str(&run.text);
                let f = Font {
                    weight: if run.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
                    style: if run.italic { FontStyle::Italic } else { FontStyle::Normal },
                    ..font(family)
                };
                if run.text.trim().is_empty() && background.is_none() {
                    color = hsla(theme::FOREGROUND);
                    background = None;
                }
                text_runs.push(TextRun {
                    len: run.text.len(),
                    font: f,
                    color,
                    background_color: background,
                    underline: run.underline.then(|| UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(color),
                        wavy: false,
                    }),
                    strikethrough: None,
                });
            }
            if text.is_empty() {
                text.push(' ');
                text_runs.push(TextRun {
                    len: 1,
                    font: font(family),
                    color: hsla(theme::FOREGROUND),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            body = body.child(div().whitespace_nowrap().child(StyledText::new(text).with_runs(text_runs)));
        }
        body
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let term_w = (f32::from(viewport.width) - SIDEBAR_WIDTH - 16.0).max(0.0);
        let term_h = (f32::from(viewport.height) - 16.0).max(0.0);
        let cell_w = FONT_SIZE * CELL_WIDTH;
        let cell_h = FONT_SIZE * LINE_HEIGHT;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped, positive, small")]
        let (cols, rows) = (((term_w / cell_w).floor() as usize).max(20), ((term_h / cell_h).floor() as usize).max(5));
        let sidebar = self.sidebar(cx);
        let terminal = self.terminal_view(cols, rows);
        div()
            .flex()
            .size_full()
            .bg(hsla(theme::BACKGROUND))
            .text_color(hsla(theme::FOREGROUND))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(sidebar)
            .child(terminal.flex_1().h_full().overflow_hidden())
    }
}
