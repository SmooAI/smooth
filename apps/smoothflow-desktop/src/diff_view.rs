//! Drawing the Diff tab (Client Spec §14): the base picker and toolbar, the
//! file tree, the virtualized diff body, and the comment being written.
//! Everything it shows and does is `smoothflow_desktop::diff::Viewer`; this
//! only draws its rows and calls back into it.

use std::ops::Range;

use gpui_kit::*;
use smooth_flow_client::diff::{Base, LineKind};

use smoothflow_desktop::diff::{palette, styled, Line, Row, BASES};
use smoothflow_desktop::terminal::theme;

use crate::view::{button, hsla, mono_family, ACCENT, AMBER, BORDER, FONT_SIZE, MUTED, RED, SUBTLE, SURFACE};
use crate::workspace::Workspace;

/// Every row of the diff body is this tall (the list is uniform).
pub const ROW_HEIGHT: f32 = 20.0;
const TOOLBAR_HEIGHT: f32 = 34.0;
const TREE_WIDTH: f32 = 260.0;
/// Line-number gutter: two 5-digit columns.
const GUTTER_CHARS: usize = 12;
/// The `+`/`-` column after the gutter in unified mode.
const SIGN_WIDTH: f32 = 16.0;
/// Room after a line's last character when scrolled all the way right.
const END_PAD_CHARS: f32 = 2.0;
/// Ctrl/⌘+Enter adds a comment.
const SUBMIT_CHORD: &str = if cfg!(target_os = "macos") { "⌘↩" } else { "Ctrl+Enter" };

fn tint(rgb24: u32, a: f32) -> Hsla {
    Hsla { a, ..hsla(rgb24) }
}

fn line_bg(kind: LineKind) -> Option<Hsla> {
    match kind {
        LineKind::Add => Some(tint(palette::GREEN, 0.10)),
        LineKind::Del => Some(tint(palette::RED, 0.10)),
        LineKind::Ctx => None,
    }
}

/// A diff line as `StyledText`: the engine's syntax colors, word spans as a
/// stronger add/del background.
fn line_text(l: &Line, legend: &[String]) -> StyledText {
    let (text, runs) = styled(l, legend);
    let word_bg = tint(if l.kind == LineKind::Add { palette::GREEN } else { palette::RED }, 0.32);
    let family = mono_family();
    let mut text_runs: Vec<TextRun> = runs
        .iter()
        .map(|r| TextRun {
            len: r.len,
            font: font(family),
            color: hsla(r.fg),
            background_color: r.word.then_some(word_bg),
            underline: None,
            strikethrough: None,
        })
        .collect();
    let text = if text.is_empty() {
        text_runs.push(TextRun {
            len: 1,
            font: font(family),
            color: hsla(palette::TEXT),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        " ".to_string()
    } else {
        text
    };
    StyledText::new(text).with_runs(text_runs)
}

fn number(n: Option<u32>) -> String {
    n.map_or_else(|| " ".repeat(5), |n| format!("{n:>5}"))
}

impl Workspace {
    /// One monospace column, in pixels.
    fn diff_char_width(&self) -> f32 {
        self.metrics.map_or(8.0, |m| m.width).max(6.0)
    }

    fn gutter_width(&self) -> f32 {
        #[allow(clippy::cast_precision_loss, reason = "a fixed small character count")]
        let chars = GUTTER_CHARS as f32;
        chars * self.diff_char_width()
    }

    /// Tell the viewer how many columns of code fit, from the list's width
    /// last frame (one pane's, side by side), so it can clamp the offset.
    fn measure_diff_columns(&mut self) {
        let Some(width) = self.diff_scroll.0.borrow().last_item_size.map(|s| f32::from(s.item.width)) else {
            return;
        };
        let text = if self.diff.split {
            (width - 1.0) / 2.0 - self.gutter_width()
        } else {
            width - self.gutter_width() - SIGN_WIDTH
        };
        let cw = self.diff_char_width();
        self.diff.set_view_columns(text / cw - END_PAD_CHARS);
    }

    /// The whole Diff tab for the focused session.
    pub(crate) fn diff_panel(&mut self, cx: &mut Context<Self>) -> Div {
        self.measure_diff_columns();
        if let Some((row, top)) = self.diff.take_scroll() {
            self.diff_scroll
                .scroll_to_item(row, if top { ScrollStrategy::Top } else { ScrollStrategy::Nearest });
        }
        let mut panel = div().flex().flex_col().flex_1().size_full().overflow_hidden().child(self.diff_toolbar(cx));
        let count = self.diff.rows.len();
        let body = if count == 0 {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(hsla(MUTED))
                .child(if self.diff.loading() { "Loading…" } else { "" })
                .into_any_element()
        } else {
            let mut list = uniform_list(
                "diff-rows",
                count,
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    // Rows coming on screen page in files the frame left out.
                    this.diff.visible(range.clone());
                    range.map(|ix| this.diff_row(ix, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.diff_scroll)
            .flex_1()
            .h_full()
            .font_family(mono_family())
            .text_size(px(FONT_SIZE));
            // Marking x scrollable keeps a sideways swipe from scrolling the
            // list vertically; the list never scrolls x itself (its rows fit),
            // the code inside them does, by the viewer's shared offset.
            list.interactivity().base_style.overflow.x = Some(Overflow::Scroll);
            div()
                .flex()
                .flex_col()
                .flex_1()
                .h_full()
                .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, window, cx| {
                    let d = e.delta.pixel_delta(window.line_height());
                    let (dx, dy) = (f32::from(d.x), f32::from(d.y));
                    let cw = this.diff_char_width();
                    if dx.abs() > dy.abs() && this.diff.scroll_by(-dx / cw) {
                        cx.notify();
                    }
                }))
                .child(list)
                .into_any_element()
        };
        let mut middle = div().flex().flex_1().overflow_hidden();
        if !self.diff.tree.is_empty() {
            middle = middle.child(self.diff_tree(cx));
        }
        middle = middle.child(div().flex().flex_col().flex_1().h_full().overflow_hidden().child(body));
        panel = panel.child(middle);
        if let Some(d) = &self.diff.draft {
            let window = d.window();
            let total = d.field.lines().count();
            let (caret_line, caret_col) = d.field.caret_line_col();
            let more = if window.len() < total {
                format!(" · lines {}–{} of {total}", window.start + 1, window.end)
            } else {
                String::new()
            };
            let mut editor = div()
                .flex()
                .flex_col()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(hsla(ACCENT))
                .font_family(mono_family())
                .text_sm();
            for (i, line) in d.field.lines().enumerate().skip(window.start).take(window.len()) {
                let mut row = div().flex().h(px(FONT_SIZE + 6.0)).items_center().whitespace_nowrap().overflow_hidden();
                if i == caret_line {
                    let at = line.char_indices().nth(caret_col).map_or(line.len(), |(b, _)| b);
                    let (before, after) = line.split_at(at);
                    row = row
                        .child(before.to_string())
                        .child(div().flex_none().w(px(1.5)).h(px(FONT_SIZE + 2.0)).bg(hsla(theme::CURSOR)))
                        .child(after.to_string());
                } else {
                    row = row.child(if line.is_empty() { " ".to_string() } else { line.to_string() });
                }
                editor = editor.child(row);
            }
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .bg(hsla(SURFACE))
                    .border_t_1()
                    .border_color(hsla(ACCENT))
                    .child(div().text_xs().text_color(hsla(SUBTLE)).child(format!(
                        "Comment on {} — {SUBMIT_CHORD} adds it · Enter for a new line · Esc cancels · comments go to the agent together, as one review{more}",
                        d.target.place()
                    )))
                    .child(editor),
            );
        }
        panel
    }

    fn diff_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let mut bar = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(TOOLBAR_HEIGHT))
            .px_2()
            .bg(hsla(SURFACE))
            .border_b_1()
            .border_color(hsla(BORDER));
        let mut picker = div().flex().rounded_md().border_1().border_color(hsla(BORDER)).overflow_hidden();
        for b in BASES {
            let on = self.diff.base == b;
            let mut seg = div().px_2().py_0p5().text_xs().cursor_pointer().child(self.diff.base_label(b)).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.diff.set_base(b);
                    cx.notify();
                }),
            );
            seg = if on {
                seg.bg(hsla(BORDER)).text_color(hsla(theme::FOREGROUND))
            } else {
                seg.text_color(hsla(MUTED))
            };
            picker = picker.child(seg);
        }
        bar = bar.child(picker);
        let split = self.diff.split;
        bar = bar
            .child(button(if split { "Split  u" } else { "Unified  u" }, SUBTLE, true).text_xs().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.diff.toggle_split();
                    cx.notify();
                }),
            ))
            .child(button("Refresh", SUBTLE, true).text_xs().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.diff.request();
                    cx.notify();
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(hsla(SUBTLE))
                    .child(self.diff.summary()),
            );
        if let Some(status) = self.diff.status.clone() {
            bar = bar.child(
                div()
                    .max_w(px(520.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(hsla(if self.diff.error { RED } else { AMBER }))
                    .child(status),
            );
        }
        let n = self.diff.comments().len();
        if n > 0 {
            let ok = self.diff.can_review();
            bar = bar.child(
                button(
                    if ok {
                        format!("Send review to agent ({n})")
                    } else {
                        "A review goes to an agent, not a shell".into()
                    },
                    ACCENT,
                    ok,
                )
                .text_xs()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.diff.submit_review();
                        cx.notify();
                    }),
                ),
            );
        }
        bar
    }

    fn diff_tree(&self, cx: &mut Context<Self>) -> Div {
        let current = self.diff.cursor_file();
        let count = self.diff.tree.len();
        div()
            .w(px(TREE_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .bg(hsla(SURFACE))
            .border_r_1()
            .border_color(hsla(BORDER))
            .child(
                uniform_list(
                    "diff-tree",
                    count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range
                            .filter_map(|ix| {
                                let row = this.diff.tree.get(ix)?.clone();
                                #[allow(clippy::cast_precision_loss, reason = "tree depth is small")]
                                let indent = 8.0 + row.depth as f32 * 12.0;
                                let mut el = div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .h(px(ROW_HEIGHT))
                                    .pl(px(indent))
                                    .pr_2()
                                    .text_xs()
                                    .whitespace_nowrap()
                                    .overflow_hidden();
                                let Some(fi) = row.file else {
                                    return Some(el.text_color(hsla(SUBTLE)).child(format!("{}/", row.name)));
                                };
                                let f = this.diff.diff.as_ref()?.files.get(fi)?;
                                if current == Some(fi) {
                                    el = el.bg(hsla(BORDER));
                                }
                                let viewed = this.diff.file_viewed(fi);
                                Some(
                                    el.cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                                this.diff.jump_to_file(fi);
                                                cx.notify();
                                            }),
                                        )
                                        .child(div().w(px(10.0)).text_color(hsla(palette::badge(&f.status))).child(f.badge()))
                                        .child(
                                            div()
                                                .flex_1()
                                                .overflow_hidden()
                                                .text_color(hsla(if viewed { MUTED } else { theme::FOREGROUND }))
                                                .child(row.name.clone()),
                                        )
                                        .child(div().text_color(hsla(palette::GREEN)).child(format!("+{}", f.added)))
                                        .child(div().text_color(hsla(palette::RED)).child(format!("−{}", f.deleted)))
                                        .child(div().w(px(10.0)).text_color(hsla(palette::GREEN)).child(if viewed { "✓" } else { "" })),
                                )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .h_full(),
            )
    }

    /// One row of the diff body.
    fn diff_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.diff.rows.get(ix).cloned() else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let Some(d) = self.diff.diff.as_ref() else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let cursor = self.diff.cursor == Some(ix);
        let mut el = div()
            .id(("diff-row", ix))
            .flex()
            .items_center()
            .h(px(ROW_HEIGHT))
            .w_full()
            .whitespace_nowrap()
            .overflow_hidden();
        let el = match row {
            Row::Banner(text) => el.px_2().text_xs().text_color(hsla(AMBER)).child(text),
            Row::FileHeader { file } => {
                let f = &d.files[file];
                let collapsed = self.diff.is_collapsed(file);
                let viewed = self.diff.file_viewed(file);
                let name = f.old_path.as_ref().map_or_else(|| f.path.clone(), |o| format!("{o} → {}", f.path));
                el = el.gap_2().px_2().bg(hsla(SURFACE)).border_t_1().border_color(hsla(BORDER)).text_xs();
                el.child(
                    div()
                        .cursor_pointer()
                        .text_color(hsla(SUBTLE))
                        .child(if collapsed { "▸" } else { "▾" })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                this.diff.toggle_collapse(file);
                                cx.notify();
                            }),
                        ),
                )
                .child(div().text_color(hsla(palette::badge(&f.status))).child(f.badge()))
                .child(div().text_color(hsla(theme::FOREGROUND)).child(name))
                .child(div().text_color(hsla(palette::GREEN)).child(format!("+{}", f.added)))
                .child(div().text_color(hsla(palette::RED)).child(format!("−{}", f.deleted)))
                .children(f.language.clone().map(|l| div().text_color(hsla(MUTED)).child(l)))
                .child(div().flex_1())
                .child(
                    div()
                        .cursor_pointer()
                        .text_color(hsla(if viewed { palette::GREEN } else { MUTED }))
                        .child(if viewed { "✓ Viewed  v" } else { "Viewed  v" })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                this.diff.toggle_viewed(file);
                                cx.notify();
                            }),
                        ),
                )
            }
            Row::Notice { file, text, .. } => {
                let collapsed = self.diff.is_collapsed(file);
                el = el.gap_2().pl(px(24.0)).text_xs().text_color(hsla(MUTED)).child(text);
                if collapsed {
                    el = el.child(div().cursor_pointer().text_color(hsla(ACCENT)).child("Show").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.diff.toggle_collapse(file);
                            cx.notify();
                        }),
                    ));
                }
                el
            }
            Row::HunkHeader { file, hunk } => {
                let h = &d.files[file].hunks[hunk];
                let can_stage = self.diff.base == Base::Uncommitted;
                el = el
                    .gap_2()
                    .px_2()
                    .bg(tint(palette::BLUE, 0.06))
                    .text_xs()
                    .child(div().text_color(hsla(palette::BLUE)).child(h.header()));
                if h.staged {
                    el = el.child(div().text_color(hsla(palette::GREEN)).child("staged"));
                }
                el = el.child(div().flex_1());
                if can_stage {
                    el = el.child(
                        div()
                            .cursor_pointer()
                            .text_color(hsla(SUBTLE))
                            .child(if h.staged { "Unstage  s" } else { "Stage  s" })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.diff.stage(file, hunk);
                                    cx.notify();
                                }),
                            ),
                    );
                }
                el.child(div().cursor_pointer().text_color(hsla(RED)).child("Revert  r").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        if let Some(ask) = this.diff.ask_revert(file, hunk) {
                            this.ask_revert(ask);
                        }
                        cx.notify();
                    }),
                ))
            }
            Row::Line { file, hunk, line } => {
                let l = &d.files[file].hunks[hunk].lines[line];
                if let Some(bg) = line_bg(l.kind) {
                    el = el.bg(bg);
                }
                let sign = match l.kind {
                    LineKind::Add => "+",
                    LineKind::Del => "-",
                    LineKind::Ctx => " ",
                };
                el.child(self.gutter(ix, format!("{} {}", number(l.old), number(l.new)), cx))
                    .child(div().flex_none().w(px(SIGN_WIDTH)).pl_1().text_color(hsla(MUTED)).child(sign))
                    .child(self.text_cell(ix, l, &d.legend, cx))
            }
            Row::Pair { file, hunk, left, right } => {
                let h = &d.files[file].hunks[hunk];
                let half = |side: Option<usize>, old: bool, cx: &mut Context<Self>| {
                    let mut cell = div().flex().items_center().w(relative(0.5)).h_full().overflow_hidden();
                    let Some(l) = side.map(|i| &h.lines[i]) else {
                        return cell.bg(tint(palette::OVERLAY0, 0.06));
                    };
                    if let Some(bg) = line_bg(l.kind) {
                        cell = cell.bg(bg);
                    }
                    cell.child(self.gutter(ix, number(if old { l.old } else { l.new }), cx))
                        .child(self.text_cell(ix, l, &d.legend, cx))
                };
                el.child(half(left, true, cx))
                    .child(div().w(px(1.0)).h_full().bg(hsla(BORDER)))
                    .child(half(right, false, cx))
            }
            Row::Comment { index, .. } => {
                let Some(c) = self.diff.comments().get(index) else {
                    return el.into_any_element();
                };
                el.gap_2()
                    .pl(px(24.0))
                    .bg(tint(palette::YELLOW, 0.08))
                    .text_xs()
                    .child(div().text_color(hsla(AMBER)).child(format!("» {}", c.place())))
                    // One row per comment: its lines run together, each break marked.
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_color(hsla(theme::FOREGROUND))
                            .child(c.text.replace('\n', " ⏎ ")),
                    )
                    .child(div().pr_2().cursor_pointer().text_color(hsla(MUTED)).child("remove").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.diff.remove_comment(index);
                            cx.notify();
                        }),
                    ))
            }
        };
        let el = if cursor {
            el.border_l_2().border_color(hsla(ACCENT)).bg(tint(palette::BLUE, 0.16))
        } else if self.diff.selected(ix) {
            el.bg(tint(palette::BLUE, 0.10))
        } else {
            el
        };
        el.into_any_element()
    }

    /// The line-number gutter: a click comments on the line (shift extends).
    /// It stays put while the code beside it scrolls sideways.
    fn gutter(&self, ix: usize, numbers: String, cx: &mut Context<Self>) -> Div {
        let w = self.gutter_width();
        let numbers = if numbers.len() < 6 { format!("{numbers:>5}") } else { numbers };
        div()
            .w(px(w))
            .flex_none()
            .cursor_pointer()
            .text_color(hsla(MUTED))
            .child(numbers)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    this.diff.click(ix, true, e.modifiers.shift);
                    cx.notify();
                }),
            )
    }

    /// The code of a line, shifted left by the viewer's horizontal offset
    /// (shared by every row, and by both panes side by side) and clipped.
    fn text_cell(&self, ix: usize, l: &Line, legend: &[String], cx: &mut Context<Self>) -> Div {
        let shift = self.diff.scroll_x() * self.diff_char_width();
        div()
            .flex()
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(div().flex_none().ml(px(-shift)).child(line_text(l, legend)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    this.diff.click(ix, false, e.modifiers.shift);
                    cx.notify();
                }),
            )
    }
}
