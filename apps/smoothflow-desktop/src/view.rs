//! Drawing the window (Client Spec §4): fleet sidebar, tab strip, approval
//! bar, the pane tree of GPU-rendered terminals, and the modal sheet and
//! dialog. Nothing here decides anything; it reads `Workspace` and calls
//! back into it.

use gpui_kit::*;
use smooth_flow_client::keymap::Action;
use smooth_flow_client::pane::{PaneId, Rect};
use smooth_flow_client::{fleet, title, SessionState};

use smoothflow_desktop::layout::{CellMetrics, PANE_PADDING};
use smoothflow_desktop::sheet::Focus;
use smoothflow_desktop::terminal::theme;

use crate::workspace::{Connection, Workspace};

pub const SIDEBAR_WIDTH: f32 = 260.0;
pub const TAB_STRIP_HEIGHT: f32 = 30.0;
pub const APPROVAL_HEIGHT: f32 = 84.0;
pub const FONT_SIZE: f32 = 13.0;

const SURFACE: u32 = 0x181825;
const BORDER: u32 = 0x313244;
const MUTED: u32 = 0x7f849c;
const SUBTLE: u32 = 0x9399b2;
const ACCENT: u32 = 0x89b4fa;
const AMBER: u32 = 0xf9e2af;
const RED: u32 = 0xf38ba8;
const GREEN: u32 = 0xa6e3a1;

pub fn mono_family() -> &'static str {
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

/// The chord for `action`, as a hint on a button.
fn hint(ws: &Workspace, action: Action) -> String {
    ws.chord_hint(action)
}

fn button(label: impl Into<SharedString>, color: u32, enabled: bool) -> Div {
    let b = div().px_3().py_1().rounded_md().border_1().text_sm().child(label.into());
    if enabled {
        b.border_color(hsla(color)).text_color(hsla(color)).cursor_pointer()
    } else {
        b.border_color(hsla(BORDER)).text_color(hsla(MUTED))
    }
}

/// Measure one terminal cell from the font, through GPUI's text system.
fn measure(window: &Window) -> CellMetrics {
    let ts = window.text_system();
    let id = ts.resolve_font(&font(mono_family()));
    let size = px(FONT_SIZE);
    let advance = ts.advance(id, size, 'm').ok().map(|s| f32::from(s.width));
    CellMetrics::measured(FONT_SIZE, advance, f32::from(ts.ascent(id, size)), f32::from(ts.descent(id, size)))
}

impl Workspace {
    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let ordered = self.ordered();
        let counts = fleet::counts(&ordered);
        let focused = self.surfaces.focused_session().map(str::to_string);
        let mut list = div().flex().flex_col().gap_1().p_2().flex_1().overflow_hidden();
        for group in fleet::grouped(&ordered) {
            list = list.child(div().text_xs().text_color(hsla(SUBTLE)).mt_2().child(group.project.clone()));
            for id in group.sessions {
                let Some(s) = self.sessions.get(&id) else { continue };
                let selected = focused.as_deref() == Some(id.as_str());
                let dot = match s.state {
                    SessionState::Working | SessionState::Starting => ACCENT,
                    SessionState::NeedsYou | SessionState::Limited => AMBER,
                    SessionState::Idle => GREEN,
                    SessionState::Done | SessionState::Dead => 0x6c7086,
                };
                let needs_you = matches!(s.state, SessionState::NeedsYou);
                let mut state = format!("{:?}", s.state).to_lowercase().replace("needsyou", "needs you");
                if smooth_flow_client::attention::approval(self.attention.get(&id)).is_some() {
                    state.push_str(" · approve?");
                }
                let row_id = id.clone();
                let mut row = div().flex().items_center().gap_2().px_2().py_1().rounded_md().cursor_pointer();
                if selected {
                    row = row.bg(hsla(BORDER));
                } else if needs_you {
                    row = row.bg(hsla(0x3a3326));
                }
                let mut label = div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_sm().text_color(hsla(theme::FOREGROUND)).child(title::tab_title(s, &self.home)));
                if s.unread {
                    label = label.child(div().size_1p5().rounded_full().bg(hsla(ACCENT)));
                }
                list = list.child(
                    row.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.show_session(&row_id);
                            cx.notify();
                        }),
                    )
                    .child(div().size_2().rounded_full().bg(hsla(dot)))
                    .child(div().flex().flex_col().child(label).child(div().text_xs().text_color(hsla(MUTED)).child(state))),
                );
            }
        }
        let footer = match &self.connection {
            Connection::Connected => format!("● connected · {}", self.machine),
            Connection::Connecting => "connecting…".to_string(),
            Connection::Offline(why) => format!("offline — {why}"),
        };
        let new_hint = hint(self, Action::NewSession);
        div()
            .flex()
            .flex_col()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(hsla(SURFACE))
            .border_r_1()
            .border_color(hsla(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_2()
                    .child(div().text_xs().text_color(hsla(SUBTLE)).child(format!("FLEET · {} sessions", ordered.len())))
                    .child(button(format!("+ New  {new_hint}"), ACCENT, true).text_xs().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| this.act(Action::NewSession, cx)),
                    )),
            )
            .child(list)
            .child(
                div()
                    .p_2()
                    .text_xs()
                    .text_color(hsla(MUTED))
                    .child(footer)
                    .child(format!(
                        "{} working · {} need you · {} done · {} idle",
                        counts.working, counts.needs_you, counts.done, counts.idle
                    ))
                    .children(self.notice.clone().map(|n| div().text_color(hsla(AMBER)).child(n))),
            )
    }

    fn tab_strip(&self, cx: &mut Context<Self>) -> Div {
        let mut strip = div().flex().h(px(TAB_STRIP_HEIGHT)).bg(hsla(SURFACE)).border_b_1().border_color(hsla(BORDER));
        for (i, tab) in self.surfaces.tabs.iter().enumerate() {
            let label = tab
                .sessions
                .get(&tab.focused)
                .and_then(|id| self.sessions.get(id))
                .map_or_else(|| "empty".to_string(), |s| title::tab_title(s, &self.home));
            let active = i == self.surfaces.active;
            let mut t = div()
                .flex()
                .items_center()
                .px_3()
                .h_full()
                .text_sm()
                .border_r_1()
                .border_color(hsla(BORDER))
                .cursor_pointer()
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.surfaces.select(i);
                        cx.notify();
                    }),
                );
            t = if active {
                t.bg(hsla(theme::BACKGROUND))
                    .text_color(hsla(theme::FOREGROUND))
                    .border_t_2()
                    .border_color(hsla(ACCENT))
            } else {
                t.text_color(hsla(MUTED))
            };
            strip = strip.child(t);
        }
        strip
    }

    fn approval_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (id, a) = self.focused_approval()?;
        let who = self.sessions.get(&id).map(|s| title::tab_title(s, &self.home)).unwrap_or_default();
        let (allow, deny) = (hint(self, Action::Allow), hint(self, Action::Deny));
        Some(
            div()
                .flex()
                .items_center()
                .gap_3()
                .h(px(APPROVAL_HEIGHT))
                .px_3()
                .bg(hsla(0x2e2a22))
                .border_b_1()
                .border_color(hsla(AMBER))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .overflow_hidden()
                        .child(div().text_xs().text_color(hsla(AMBER)).child(format!("{} · {who}", a.heading)))
                        .child(div().font_family(mono_family()).text_sm().text_color(hsla(theme::FOREGROUND)).child(a.text)),
                )
                .child(
                    button(format!("Allow  {allow}"), GREEN, true)
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, cx| this.act(Action::Allow, cx))),
                )
                .child(
                    button(format!("Deny  {deny}"), RED, true)
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, cx| this.act(Action::Deny, cx))),
                ),
        )
    }

    /// One pane: its session's terminal (or an empty-pane hint), absolutely
    /// placed at `frame` inside the pane area.
    fn pane(&self, pane: PaneId, frame: Rect, focused: bool, window_focused: bool, m: CellMetrics, cx: &mut Context<Self>) -> Div {
        #[allow(clippy::cast_possible_truncation, reason = "pixel coordinates")]
        let (x, y, w, h) = (frame.x as f32, frame.y as f32, frame.w as f32, frame.h as f32);
        let multi = self.surfaces.visible(self.pane_area).len() > 1;
        let mut el = div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
            .overflow_hidden()
            .bg(hsla(theme::BACKGROUND))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.surfaces.focus_pane(pane);
                    cx.notify();
                }),
            );
        if multi {
            el = el.border_1().border_color(hsla(if focused { ACCENT } else { BORDER }));
        }
        let hint_text = self.pane_hint(pane);
        let term = self.surfaces.session_of(pane).and_then(|id| self.terminals.get(id));
        let (Some(term), None) = (term, &hint_text) else {
            let hint_text = hint_text.unwrap_or_else(|| "attaching…".to_string());
            return el.flex().items_center().justify_center().text_color(hsla(MUTED)).child(hint_text);
        };
        let block = focused && window_focused;
        let screen = term.screen(block);
        let family = mono_family();
        let mut body = div()
            .absolute()
            .left(px(PANE_PADDING))
            .top(px(PANE_PADDING))
            .flex()
            .flex_col()
            .font_family(family)
            .text_size(px(FONT_SIZE))
            .line_height(px(m.height));
        for runs in &screen.rows {
            let mut text = String::new();
            let mut text_runs = Vec::with_capacity(runs.len() + 1);
            for run in runs {
                text.push_str(&run.text);
                let color = hsla(run.fg);
                text_runs.push(TextRun {
                    len: run.text.len(),
                    font: Font {
                        weight: if run.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
                        style: if run.italic { FontStyle::Italic } else { FontStyle::Normal },
                        ..font(family)
                    },
                    color,
                    background_color: run.bg.map(hsla),
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
            body = body.child(div().h(px(m.height)).whitespace_nowrap().child(StyledText::new(text).with_runs(text_runs)));
        }
        el = el.child(body);
        if !block {
            if let Some((row, col)) = screen.cursor {
                #[allow(clippy::cast_precision_loss, reason = "small grid indices")]
                let (cx0, cy0) = (PANE_PADDING + col as f32 * m.width, PANE_PADDING + row as f32 * m.height);
                el = el.child(
                    div()
                        .absolute()
                        .left(px(cx0))
                        .top(px(cy0))
                        .w(px(m.width))
                        .h(px(m.height))
                        .border_1()
                        .border_color(hsla(theme::CURSOR)),
                );
            }
        }
        el
    }

    fn sheet_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let s = self.sheet.as_ref()?;
        let focus_ring = |f: Focus| if s.focus == f { ACCENT } else { BORDER };
        let mut kinds = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .rounded_md()
            .border_1()
            .border_color(hsla(focus_ring(Focus::Kind)));
        for (i, row) in s.rows.iter().enumerate() {
            let selected = i == s.kind;
            let color = if !row.enabled {
                MUTED
            } else if row.needs_setup {
                AMBER
            } else {
                theme::FOREGROUND
            };
            let mut r = div()
                .px_2()
                .py_0p5()
                .rounded_sm()
                .text_sm()
                .text_color(hsla(color))
                .cursor_pointer()
                .child(row.label.clone());
            if selected {
                r = r.bg(hsla(BORDER));
            }
            kinds = kinds.child(r.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    if let Some(s) = &mut this.sheet {
                        s.kind = i;
                        s.focus = Focus::Kind;
                    }
                    cx.notify();
                }),
            ));
        }
        let mut detail = div().flex().flex_col();
        if let Some(r) = s.selected() {
            if let Some(reason) = r.reason.clone().filter(|_| !r.enabled || r.needs_setup) {
                detail = detail.child(div().text_xs().text_color(hsla(if r.enabled { AMBER } else { MUTED })).child(reason));
            }
            if let Some(fix) = r.fix.clone() {
                detail = detail.child(div().text_xs().font_family(mono_family()).text_color(hsla(SUBTLE)).child(format!("fix: {fix}")));
            }
        }
        let field = |f: &smoothflow_desktop::field::Field, focused: bool, placeholder: String| {
            let (before, after) = f.split();
            let mut d = div()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(hsla(if focused { ACCENT } else { BORDER }))
                .font_family(mono_family())
                .text_sm();
            if f.text().is_empty() && !focused {
                d = d.text_color(hsla(MUTED)).child(placeholder);
            } else if focused {
                d = d
                    .flex()
                    .child(before.to_string())
                    .child(div().w(px(1.5)).h(px(FONT_SIZE + 2.0)).bg(hsla(theme::CURSOR)))
                    .child(after.to_string());
                if f.text().is_empty() {
                    d = d.child(div().text_color(hsla(MUTED)).child(placeholder));
                }
            } else {
                d = d.child(f.text().to_string());
            }
            d
        };
        let inferred_dir = s
            .inferred
            .as_ref()
            .map(|i| smooth_flow_client::directory::abbreviate(&i.worktree, &self.home))
            .filter(|w| !w.is_empty())
            .unwrap_or_else(|| "search repos, or type ~/… or /…".into());
        let dir_field = field(&s.directory, s.focus == Focus::Directory, inferred_dir).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                if let Some(s) = &mut this.sheet {
                    s.focus = Focus::Directory;
                }
                cx.notify();
            }),
        );
        let mut matches = div().flex().flex_col();
        if s.focus == Focus::Directory {
            for (i, r) in s.matches.iter().take(8).enumerate() {
                let path = r.path.clone();
                let mut m = div()
                    .px_2()
                    .text_xs()
                    .font_family(mono_family())
                    .cursor_pointer()
                    .text_color(hsla(theme::FOREGROUND))
                    .child(format!(
                        "{}{}",
                        smooth_flow_client::directory::abbreviate(&r.path, &self.home),
                        r.branch.as_deref().map(|b| format!("  ({b})")).unwrap_or_default()
                    ));
                if i == s.highlighted {
                    m = m.bg(hsla(BORDER));
                }
                matches = matches.child(m.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        let fx = this.sheet.as_mut().map(|s| vec![s.pick(&path)]).unwrap_or_default();
                        this.run_effects(fx, cx);
                    }),
                ));
            }
            if s.scanning {
                matches = matches.child(div().px_2().text_xs().text_color(hsla(MUTED)).child("indexing repos…"));
            }
        }
        let context = s.context_lines().map_or_else(
            || div().text_xs().text_color(hsla(MUTED)).child("reading the context…"),
            |(t, facts, note)| {
                div()
                    .flex()
                    .flex_col()
                    .p_2()
                    .rounded_md()
                    .bg(hsla(SURFACE))
                    .child(div().text_sm().child(t))
                    .child(div().text_xs().font_family(mono_family()).text_color(hsla(MUTED)).child(facts))
                    .children(note.map(|n| div().text_xs().text_color(hsla(MUTED)).child(n)))
            },
        );
        let prompt = field(&s.prompt, s.focus == Focus::Prompt, "Prompt (optional)".into()).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                if let Some(s) = &mut this.sheet {
                    s.focus = Focus::Prompt;
                }
                cx.notify();
            }),
        );
        let can_start = s.can_start();
        let card = div()
            .flex()
            .flex_col()
            .gap_2()
            .w(px(560.0))
            .p_4()
            .rounded_lg()
            .bg(hsla(theme::BACKGROUND))
            .border_1()
            .border_color(hsla(BORDER))
            .text_color(hsla(theme::FOREGROUND))
            .child(div().text_lg().child("New session"))
            .child(div().text_xs().text_color(hsla(SUBTLE)).child("Kind"))
            .child(kinds)
            .child(detail)
            .child(div().text_xs().text_color(hsla(SUBTLE)).child("Directory"))
            .child(dir_field)
            .child(matches)
            .child(context)
            .child(prompt)
            .children(s.error.clone().map(|e| div().text_xs().text_color(hsla(RED)).child(e)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(hsla(MUTED))
                            .child("Tab moves · ↑↓ choose · Enter starts · Esc cancels"),
                    )
                    .child(button("Cancel", SUBTLE, true).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.sheet = None;
                            cx.notify();
                        }),
                    ))
                    .child(button("Start", ACCENT, can_start).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            let fx = this
                                .sheet
                                .as_ref()
                                .and_then(|s| s.new_session())
                                .map(smoothflow_desktop::sheet::Effect::Start)
                                .into_iter()
                                .collect();
                            this.run_effects(fx, cx);
                        }),
                    )),
            );
        Some(scrim().child(card))
    }

    fn dialog_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let d = self.dialog.as_ref()?;
        let mut buttons = div().flex().justify_end().gap_2();
        if d.offers_dont_ask {
            buttons = buttons.child(
                div()
                    .text_xs()
                    .text_color(hsla(MUTED))
                    .cursor_pointer()
                    .child("Don't ask again")
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, cx| this.dont_ask_again(cx))),
            );
        }
        buttons = buttons.child(button("Cancel (Enter)", ACCENT, true).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.dialog = None;
                cx.notify();
            }),
        ));
        for (i, (label, c)) in d.buttons.iter().enumerate() {
            let c = c.clone();
            let destructive = i + 1 == d.buttons.len();
            buttons = buttons.child(button(label.clone(), if destructive { RED } else { SUBTLE }, true).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| this.confirm(c.clone(), cx)),
            ));
        }
        Some(
            scrim().child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w(px(440.0))
                    .p_4()
                    .rounded_lg()
                    .bg(hsla(theme::BACKGROUND))
                    .border_1()
                    .border_color(hsla(BORDER))
                    .text_color(hsla(theme::FOREGROUND))
                    .child(div().text_lg().child(d.title.clone()))
                    .child(div().text_sm().text_color(hsla(SUBTLE)).child(d.message.clone()))
                    .child(buttons),
            ),
        )
    }
}

fn scrim() -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(Hsla { a: 0.6, ..hsla(0x11111b) })
        // Modal: clicks must not reach the panes and fleet rows beneath.
        .occlude()
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = *self.metrics.get_or_insert_with(|| measure(window));
        let viewport = window.viewport_size();
        let sidebar_w = if self.sidebar_visible { SIDEBAR_WIDTH } else { 0.0 };
        let strip = self.surfaces.tabs.len() > 1;
        let approval = self.approval_bar(cx);
        let top = if strip { TAB_STRIP_HEIGHT } else { 0.0 } + if approval.is_some() { APPROVAL_HEIGHT } else { 0.0 };
        let pane_area = Rect {
            x: 0.0,
            y: 0.0,
            w: f64::from((f32::from(viewport.width) - sidebar_w).max(0.0)),
            h: f64::from((f32::from(viewport.height) - top).max(0.0)),
        };
        // Lays the panes out and attaches/resizes their sessions to match.
        let visible = self.layout(pane_area, m);

        let focused_pane = self.surfaces.focused_pane();
        let window_focused = self.focus.is_focused(window);
        let mut area = div().relative().flex_1().overflow_hidden();
        for f in &visible {
            area = area.child(self.pane(f.pane, f.rect, f.pane == focused_pane, window_focused, m, cx));
        }
        let mut center = div().flex().flex_col().flex_1().h_full().overflow_hidden();
        if strip {
            center = center.child(self.tab_strip(cx));
        }
        if let Some(a) = approval {
            center = center.child(a);
        }
        center = center.child(area);
        let mut root = div()
            .relative()
            .flex()
            .size_full()
            .bg(hsla(theme::BACKGROUND))
            .text_color(hsla(theme::FOREGROUND))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key));
        if self.sidebar_visible {
            root = root.child(self.sidebar(cx));
        }
        root = root.child(center);
        if let Some(s) = self.sheet_view(cx) {
            root = root.child(s);
        }
        if let Some(d) = self.dialog_view(cx) {
            root = root.child(d);
        }
        root
    }
}
