//! `th ci-queue top` — the queue, live, in the terminal (SMOODEV-3371).
//!
//! One screen of the machine: waiters line up on the left, the gate sits
//! between them and the slot lanes, and each running job is a bar of its
//! elapsed time against that label's usual (p50) run here. Under it, every
//! pressure signal is a gauge whose threshold sits at the same column
//! ([`GATE_X`]), so the thresholds line up into one gate line, with ten
//! minutes of sparkline beside it. Colour is heat on the Aurora spectrum
//! (teal → gold → coral); the whole-machine heat is pinned to the gate's own
//! verdict (`holds`), never recomputed.
//!
//! The web Queue tab (`crates/smooth-web/web/src/ci-queue.ts`) makes the same
//! calls; keep the two in step.
//!
//! Works over ssh and in small terminals: 256-colour fallback when the
//! terminal does not advertise truecolor, no colour at all under `NO_COLOR`,
//! and sections drop away (recent, then locks, then sparklines, then the
//! gauges) as the window shrinks, rather than overlapping.
//!
//! The model (signals, heat, p50s, lanes, reasons, bars, sparklines, key map)
//! is pure and unit tested; `render` is exercised against ratatui's
//! `TestBackend` at several sizes; the event loop is the IO shell.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use super::config::Gate;
use super::pressure::{self, Readings};
use super::queue::{Class, HistoryEntry, JobInfo, Queue, Snapshot};

#[derive(Args, Debug)]
pub struct TopArgs {
    /// How often to re-read the queue, in milliseconds.
    #[arg(long, default_value_t = 1000, value_name = "MS")]
    pub interval: u64,

    /// Show a simulated busy machine instead of the real queue.
    #[arg(long, hide = true)]
    pub demo: bool,
}

/// Where every gauge's threshold sits, as a fraction of the gauge's width.
pub const GATE_X: f64 = 0.8;

/// Ten minutes of pressure samples at the default interval.
const SAMPLE_WINDOW: Duration = Duration::from_secs(600);

/// Finished jobs to read for the per-label p50.
const HISTORY: usize = 500;

// ── Model ────────────────────────────────────────────────────────────────────

/// How close a signal is to its threshold (1.0 = at it) → heat 0 (teal) … 5 (coral).
#[must_use]
pub fn heat_of(ratio: f64) -> u8 {
    match ratio {
        r if !r.is_finite() || r < 0.5 => 0,
        r if r < 0.7 => 1,
        r if r < 0.85 => 2,
        r if r < 1.0 => 3,
        r if r < 1.15 => 4,
        _ => 5,
    }
}

/// Where a ratio lands on a gauge whose threshold sits at [`GATE_X`].
#[must_use]
pub fn gauge_fill(ratio: Option<f64>) -> f64 {
    ratio.filter(|r| r.is_finite()).map_or(0.0, |r| (r * GATE_X).clamp(0.0, 1.0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKey {
    Memory,
    Pressure,
    Swap,
    Cpu,
    Load,
    Disk,
}

/// One gate signal against its threshold, "worse" upward.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub key: SignalKey,
    pub name: &'static str,
    pub value: String,
    pub limit: String,
    /// reading ÷ threshold; 1.0 = at it. `None` = unknown.
    pub ratio: Option<f64>,
    /// The gate has this signal turned off.
    pub off: bool,
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Each gate signal, in the gate's order. The macOS pressure level only when
/// the platform reports one.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "GB and ratio display")]
pub fn signals(r: &Readings, g: &Gate) -> Vec<Signal> {
    let mut out = Vec::with_capacity(6);
    let avail = r.mem_available_pct();
    let mem_limit = 100.0 - g.min_available_memory_pct;
    out.push(Signal {
        key: SignalKey::Memory,
        name: "Memory",
        value: avail.map_or_else(|| "unknown".into(), |a| format!("{:.0}% used", 100.0 - a)),
        limit: format!("holds above {mem_limit:.0}% used"),
        ratio: avail.filter(|_| mem_limit > 0.0).map(|a| (100.0 - a) / mem_limit),
        off: g.min_available_memory_pct <= 0.0,
    });
    if let Some(level) = r.memory_pressure_level {
        let max = g.max_memory_pressure_level;
        out.push(Signal {
            key: SignalKey::Pressure,
            name: "Mem pressure",
            value: pressure::pressure_name(level).into(),
            limit: format!("holds above {}", pressure::pressure_name(max)),
            ratio: Some(if level <= max {
                0.35 * f64::from(level) / f64::from(max.max(1))
            } else if level >= 4 {
                1.3
            } else {
                1.05
            }),
            off: max == 0,
        });
    }
    let swap = r.swap_used_pct();
    out.push(Signal {
        key: SignalKey::Swap,
        name: "Swap",
        value: swap.map_or_else(|| "unknown".into(), |s| format!("{s:.0}% used")),
        limit: format!("holds above {:.0}% while memory is tight", g.max_swap_used_pct),
        ratio: swap.filter(|_| g.max_swap_used_pct > 0.0).map(|s| s / g.max_swap_used_pct),
        off: g.max_swap_used_pct <= 0.0,
    });
    out.push(Signal {
        key: SignalKey::Cpu,
        name: "CPU",
        value: r.cpu_busy_pct.map_or_else(|| "unknown".into(), |c| format!("{c:.0}% busy")),
        limit: format!("holds above {:.0}%", g.max_cpu_busy_pct),
        ratio: r.cpu_busy_pct.filter(|_| g.max_cpu_busy_pct > 0.0).map(|c| c / g.max_cpu_busy_pct),
        off: g.max_cpu_busy_pct <= 0.0,
    });
    let per = r.load_per_core();
    out.push(Signal {
        key: SignalKey::Load,
        name: "Load",
        value: match (r.load1, per) {
            (Some(l), Some(p)) => format!("{l:.0} · {p:.1}/core"),
            _ => "unknown".into(),
        },
        limit: format!("backstop above {}/core", g.max_load_per_core),
        ratio: per.filter(|_| g.max_load_per_core > 0.0).map(|p| p / g.max_load_per_core),
        off: g.max_load_per_core <= 0.0,
    });
    let free_gb = r.disks.iter().filter_map(|d| d.free_bytes).min().map(|b| b as f64 / GIB);
    out.push(Signal {
        key: SignalKey::Disk,
        name: "Disk",
        value: free_gb.map_or_else(|| "unknown".into(), |gb| format!("{gb:.0} GB free")),
        limit: format!("holds below {} GB", g.min_free_disk_gb),
        ratio: free_gb.filter(|_| g.min_free_disk_gb > 0.0).map(|gb| g.min_free_disk_gb / gb.max(0.1)),
        off: g.min_free_disk_gb <= 0.0,
    });
    out
}

/// Memory tight enough for swap to count — the same test as `pressure::holds`.
fn memory_tight(r: &Readings, g: &Gate) -> bool {
    r.memory_pressure_level.is_some_and(|l| l > 1)
        || (g.min_available_memory_pct > 0.0 && r.mem_available_pct().is_some_and(|a| a < 2.0 * g.min_available_memory_pct))
}

/// The machine's heat: the hottest live signal, pinned to the gate's verdict —
/// no hotter than gold (3) while nothing is held, no cooler than orange (4)
/// while something is.
#[must_use]
pub fn machine_heat(s: &Snapshot) -> u8 {
    let g = &s.config.gate;
    let tight = memory_tight(&s.readings, g);
    let worst = signals(&s.readings, g)
        .iter()
        .filter(|x| !x.off && (x.key != SignalKey::Swap || tight))
        .filter_map(|x| x.ratio)
        .fold(0.0_f64, f64::max);
    let h = heat_of(worst);
    if s.holds.is_empty() {
        h.min(3)
    } else {
        h.max(4)
    }
}

/// Median run time per label, over jobs that actually ran.
#[must_use]
pub fn label_p50s(history: &[HistoryEntry]) -> HashMap<String, u64> {
    let mut runs: HashMap<&str, Vec<u64>> = HashMap::new();
    for h in history {
        if h.run_ms == 0 || h.outcome == "wait-timeout" || h.outcome == "spawn-failed" {
            continue;
        }
        runs.entry(h.label.as_str()).or_default().push(h.run_ms);
    }
    runs.into_iter()
        .map(|(label, mut v)| {
            v.sort_unstable();
            let mid = v.len() / 2;
            let p50 = if v.len() % 2 == 1 { v[mid] } else { u64::midpoint(v[mid - 1], v[mid]) };
            (label.to_string(), p50)
        })
        .collect()
}

/// A running job's elapsed time, and that ÷ its label's p50 (`None` when the
/// label has never finished here).
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "a progress fraction")]
pub fn progress(j: &JobInfo, now_ms: u64, p50s: &HashMap<String, u64>) -> (u64, Option<f64>) {
    let elapsed = now_ms.saturating_sub(j.admitted_at_ms.unwrap_or(j.queued_at_ms));
    let frac = p50s.get(&j.label).filter(|p| **p > 0).map(|p| elapsed as f64 / *p as f64);
    (elapsed, frac)
}

/// One entry per slot of `class`, `None` where the slot is free; a job in a
/// slot past the configured count (the config shrank under it) still shows.
/// The queue numbers slots from 1 (`heavy-1.slot`), so slot n is entry n-1.
#[must_use]
pub fn lanes(s: &Snapshot, class: Class) -> Vec<Option<&JobInfo>> {
    let n = class.slots(&s.config);
    let mut out: Vec<Option<&JobInfo>> = vec![None; n];
    let mut extra = Vec::new();
    for j in s.running.iter().filter(|j| j.class == class) {
        match j.slot {
            Some(i) if (1..=n).contains(&i) && out[i - 1].is_none() => out[i - 1] = Some(j),
            _ => extra.push(Some(j)),
        }
    }
    out.extend(extra);
    out
}

/// Why a waiter is waiting: the queue's own words (`waiting_on`, which the
/// waiter publishes each time its blocker changes), else the best the rest of
/// the snapshot says — a waiter that has not polled yet has no reason written.
#[must_use]
pub fn wait_reason(j: &JobInfo, s: &Snapshot) -> String {
    if let Some(reason) = &j.waiting_on {
        return reason.clone();
    }
    let busy = s.running.iter().filter(|r| r.class == j.class).count();
    let slots = j.class.slots(&s.config);
    if let Some(lock) = j.locks.iter().find(|l| s.running.iter().any(|r| r.locks.contains(l))) {
        let holder = s.running.iter().find(|r| r.locks.contains(lock));
        return holder.map_or_else(
            || format!("{} held", short_lock(lock)),
            |h| format!("{} held by #{} {}", short_lock(lock), h.ticket, h.label),
        );
    }
    if busy >= slots {
        return format!("{busy}/{slots} {} busy", j.class.name());
    }
    if j.class == Class::Heavy && busy > 0 {
        if let Some(h) = s.holds.first() {
            return h.clone();
        }
    }
    let ahead = s.waiting.iter().filter(|w| w.class == j.class && w.ticket < j.ticket).count();
    if ahead > 0 {
        format!("{ahead} ahead in line")
    } else {
        "next in line".into()
    }
}

/// `cargo:/Users/x/.cargo/shared-target` → `cargo:~/.cargo/shared-target`.
#[must_use]
pub fn short_lock(lock: &str) -> String {
    for root in ["/Users/", "/home/"] {
        if let Some(i) = lock.find(root) {
            let rest = &lock[i + root.len()..];
            let after_user = rest.find('/').map_or("", |k| &rest[k..]);
            return format!("{}~{after_user}", &lock[..i]);
        }
    }
    lock.to_string()
}

/// The last path segment — usually the worktree.
#[must_use]
pub fn worktree_of(cwd: &std::path::Path) -> String {
    cwd.file_name().map_or_else(|| cwd.display().to_string(), |n| n.to_string_lossy().into_owned())
}

#[must_use]
pub fn dur_ms(ms: u64) -> String {
    let s = (ms + 500) / 1000;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Which character a bar cell is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Fill,
    Empty,
    Marker,
}

/// A `width`-cell bar filled to `frac`, with an optional marker cell at a
/// fraction of the width (the gate line).
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "cell counts from a clamped fraction"
)]
pub fn bar(width: usize, frac: f64, marker: Option<f64>) -> Vec<Cell> {
    let filled = (frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    let mark = marker.map(|m| ((m.clamp(0.0, 1.0) * width as f64).round() as usize).min(width.saturating_sub(1)));
    (0..width)
        .map(|i| {
            if Some(i) == mark {
                Cell::Marker
            } else if i < filled {
                Cell::Fill
            } else {
                Cell::Empty
            }
        })
        .collect()
}

const SPARKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The last `width` buckets of a ratio series as block characters, scaled so
/// the threshold sits at [`GATE_X`] of the height. Unknown is a space.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "bucket math on small counts"
)]
pub fn spark(values: &[Option<f64>], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let per = values.len().div_ceil(width).max(1);
    let chunks: Vec<&[Option<f64>]> = values.chunks(per).collect();
    let mut out: String = " ".repeat(width.saturating_sub(chunks.len()));
    for c in chunks.iter().rev().take(width).rev() {
        let worst = c.iter().filter_map(|v| *v).fold(None::<f64>, |m, v| Some(m.map_or(v, |m| m.max(v))));
        out.push(worst.map_or(' ', |v| SPARKS[((gauge_fill(Some(v)) * 7.0).round() as usize).min(7)]));
    }
    out
}

/// What a keypress does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Up,
    Down,
    Details,
    Pause,
    Ignore,
}

#[must_use]
pub fn key_action(code: KeyCode, mods: KeyModifiers) -> Action {
    match code {
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => Action::Quit,
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Enter | KeyCode::Esc => Action::Details,
        KeyCode::Char('p' | ' ') => Action::Pause,
        _ => Action::Ignore,
    }
}

// ── Palette ──────────────────────────────────────────────────────────────────

/// The Aurora spectrum plus the neutrals, resolved for this terminal.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub heat: [Color; 6],
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub ok: Color,
    /// The selected row's background; `None` = reverse video (no colour).
    pub select: Option<Color>,
}

impl Palette {
    /// Truecolor when the terminal says so (`COLORTERM`), 256-colour
    /// otherwise (plain ssh, tmux without RGB), nothing under `NO_COLOR`.
    #[must_use]
    pub fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::mono();
        }
        let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
        if truecolor {
            Self::rgb()
        } else {
            Self::indexed()
        }
    }

    #[must_use]
    pub const fn rgb() -> Self {
        Self {
            heat: [
                Color::Rgb(0x00, 0xa6, 0xa6),
                Color::Rgb(0x12, 0xb3, 0x9a),
                Color::Rgb(0x7c, 0xb3, 0x52),
                Color::Rgb(0xf4, 0x9f, 0x0a),
                Color::Rgb(0xff, 0x8a, 0x3d),
                Color::Rgb(0xff, 0x6b, 0x6c),
            ],
            text: Color::Rgb(0xf4, 0xf7, 0xfb),
            muted: Color::Rgb(0x93, 0xa2, 0xbd),
            faint: Color::Rgb(0x5f, 0x6f, 0x8c),
            ok: Color::Rgb(0x34, 0xd3, 0x99),
            select: Some(Color::Rgb(0x1e, 0x29, 0x42)),
        }
    }

    #[must_use]
    pub const fn indexed() -> Self {
        Self {
            heat: [
                Color::Indexed(37),
                Color::Indexed(36),
                Color::Indexed(107),
                Color::Indexed(214),
                Color::Indexed(209),
                Color::Indexed(203),
            ],
            text: Color::Indexed(255),
            muted: Color::Indexed(247),
            faint: Color::Indexed(242),
            ok: Color::Indexed(78),
            select: Some(Color::Indexed(236)),
        }
    }

    #[must_use]
    pub const fn mono() -> Self {
        Self {
            heat: [Color::Reset; 6],
            text: Color::Reset,
            muted: Color::Reset,
            faint: Color::Reset,
            ok: Color::Reset,
            select: None,
        }
    }

    fn heat(&self, h: u8) -> Color {
        self.heat[usize::from(h.min(5))]
    }

    /// How a selected row is marked.
    fn selected(&self) -> Style {
        self.select
            .map_or_else(|| Style::new().add_modifier(Modifier::REVERSED), |bg| Style::new().bg(bg))
    }
}

// ── App ──────────────────────────────────────────────────────────────────────

/// One pressure sample: when, and each signal's ratio.
type SampleRow = (u64, Vec<(SignalKey, Option<f64>)>);

pub struct App {
    pub snap: Snapshot,
    /// Ten minutes of samples, oldest first.
    samples: VecDeque<SampleRow>,
    p50s: HashMap<String, u64>,
    selected: usize,
    detail: bool,
    paused: bool,
    read_error: Option<String>,
}

impl App {
    #[must_use]
    pub fn new(snap: Snapshot) -> Self {
        let mut app = Self {
            p50s: HashMap::new(),
            snap,
            samples: VecDeque::new(),
            selected: 0,
            detail: false,
            paused: false,
            read_error: None,
        };
        app.absorb();
        app
    }

    /// Take a new snapshot, keeping the selection on the same ticket.
    pub fn update(&mut self, snap: Snapshot) {
        let ticket = self.selected_job().map(|j| j.ticket);
        self.snap = snap;
        self.read_error = None;
        self.absorb();
        let tickets: Vec<u64> = self.jobs().iter().map(|j| j.ticket).collect();
        self.selected = ticket
            .and_then(|t| tickets.iter().position(|x| *x == t))
            .unwrap_or_else(|| self.selected.min(tickets.len().saturating_sub(1)));
        if tickets.is_empty() {
            self.detail = false;
        }
    }

    fn absorb(&mut self) {
        self.p50s = label_p50s(&self.snap.history);
        let t = self.snap.now_ms;
        let ratios = signals(&self.snap.readings, &self.snap.config.gate)
            .into_iter()
            .map(|s| (s.key, s.ratio))
            .collect();
        self.samples.push_back((t, ratios));
        let window = u64::try_from(SAMPLE_WINDOW.as_millis()).unwrap_or(u64::MAX);
        while self.samples.front().is_some_and(|(s, _)| s.saturating_add(window) < t) {
            self.samples.pop_front();
        }
    }

    /// Selectable jobs, top to bottom as drawn: heavy lanes, light lanes, the line.
    fn jobs(&self) -> Vec<&JobInfo> {
        let mut v: Vec<&JobInfo> = lanes(&self.snap, Class::Heavy).into_iter().flatten().collect();
        v.extend(lanes(&self.snap, Class::Light).into_iter().flatten());
        v.extend(self.snap.waiting.iter());
        v
    }

    fn selected_job(&self) -> Option<&JobInfo> {
        self.jobs().get(self.selected).copied()
    }

    fn series(&self, key: SignalKey) -> Vec<Option<f64>> {
        self.samples
            .iter()
            .map(|(_, r)| r.iter().find(|(k, _)| *k == key).and_then(|(_, v)| *v))
            .collect()
    }

    /// Apply a key. Returns false to quit.
    pub fn on(&mut self, a: Action) -> bool {
        let n = self.jobs().len();
        match a {
            Action::Quit => return false,
            Action::Up if n > 0 => self.selected = if self.selected == 0 { n - 1 } else { self.selected - 1 },
            Action::Down if n > 0 => self.selected = (self.selected + 1) % n,
            Action::Details => self.detail = !self.detail && n > 0,
            Action::Pause => self.paused = !self.paused,
            _ => {}
        }
        true
    }
}

// ── Render ───────────────────────────────────────────────────────────────────

fn bar_spans<'a>(cells: &[Cell], fill: Color, empty: Color, marker: Color) -> Vec<Span<'a>> {
    cells
        .iter()
        .map(|c| match c {
            Cell::Fill => Span::styled("━", Style::new().fg(fill)),
            Cell::Empty => Span::styled("─", Style::new().fg(empty)),
            Cell::Marker => Span::styled("┃", Style::new().fg(marker)),
        })
        .collect()
}

fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        format!("{s:<width$}")
    } else if width == 0 {
        String::new()
    } else {
        format!("{}…", s.chars().take(width - 1).collect::<String>())
    }
}

/// Draw the whole screen.
pub fn render(f: &mut Frame, app: &App, p: &Palette) {
    let area = f.area();
    if area.width < 30 || area.height < 8 {
        f.render_widget(
            Paragraph::new("th ci-queue top needs at least 30×8. Make the window bigger.").style(Style::new().fg(p.muted)),
            area,
        );
        return;
    }
    let s = &app.snap;
    let heat = p.heat(machine_heat(s));
    let sigs = signals(&s.readings, &s.config.gate);
    let tall = area.height >= 32;
    let show_gauges = area.height >= 20;
    let gauges_h = if show_gauges { u16::try_from(sigs.len()).unwrap_or(5) + 2 } else { 1 };
    // On a tall screen the body takes only what the line and the lanes need,
    // and the spare rows go to recent history instead of blank lanes.
    let needed = (s.waiting.len() * 2 + 1).max(s.config.slots.heavy + s.config.slots.light + 5).max(5);
    let (body_c, bottom_c) = if tall {
        (Constraint::Max(u16::try_from(needed).unwrap_or(u16::MAX)), Constraint::Min(8))
    } else {
        (Constraint::Min(5), Constraint::Length(0))
    };
    let [header, body, gauges, bottom, footer] =
        Layout::vertical([Constraint::Length(2), body_c, Constraint::Length(gauges_h), bottom_c, Constraint::Length(1)]).areas(area);

    render_header(f, header, app, p, heat);
    render_body(f, body, app, p, heat);
    if show_gauges {
        render_gauges(f, gauges, app, &sigs, p, heat);
    } else {
        render_gauge_line(f, gauges, &sigs, p);
    }
    if tall {
        let [locks, recent] = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(bottom);
        render_locks(f, locks, s, p);
        render_recent(f, recent, s, p);
    }
    render_footer(f, footer, app, p);
    if app.detail {
        if let Some(j) = app.selected_job() {
            render_detail(f, area, app, j, p, heat);
        }
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App, p: &Palette, heat: Color) {
    let s = &app.snap;
    let busy = |c: Class| s.running.iter().filter(|j| j.class == c).count();
    let verdict = match s.holds.first() {
        Some(h) => format!("HOLDING heavy jobs: {h}"),
        None if s.running.is_empty() && s.waiting.is_empty() => "Idle. The machine has room.".to_string(),
        None if busy(Class::Heavy) >= s.config.slots.heavy => "Heavy lanes full. Jobs wait their turn.".to_string(),
        None => "Admitting jobs as slots free up.".to_string(),
    };
    let counts = format!(
        "heavy {}/{} · light {}/{} · {} waiting",
        busy(Class::Heavy),
        s.config.slots.heavy,
        busy(Class::Light),
        s.config.slots.light,
        s.waiting.len()
    );
    let [left, right] = Layout::horizontal([Constraint::Min(10), Constraint::Length(u16::try_from(counts.len() + 1).unwrap_or(40))]).areas(area);
    let title = Line::from(vec![
        Span::styled("● ", Style::new().fg(heat)),
        Span::styled("ci-queue  ", Style::new().fg(p.muted)),
        Span::styled(
            verdict,
            Style::new().fg(if s.holds.is_empty() { p.text } else { heat }).add_modifier(Modifier::BOLD),
        ),
    ]);
    let sub = app.read_error.as_ref().map_or_else(
        || Line::styled(format!("  {}", s.dir.display()), Style::new().fg(p.faint)),
        |e| Line::styled(format!("  showing the last reading: {e}"), Style::new().fg(p.heat(3))),
    );
    f.render_widget(Paragraph::new(vec![title, sub]), left);
    f.render_widget(Paragraph::new(Line::styled(counts, Style::new().fg(p.muted))).right_aligned(), right);
}

fn render_body(f: &mut Frame, area: Rect, app: &App, p: &Palette, heat: Color) {
    let s = &app.snap;
    let wide = area.width >= 90;
    let [line, gate, lanes_area] =
        Layout::horizontal([Constraint::Percentage(if wide { 36 } else { 42 }), Constraint::Length(3), Constraint::Min(10)]).areas(area);

    // The line.
    let selected = app.selected_job().map(|j| j.ticket);
    let items: Vec<ListItem> = s
        .waiting
        .iter()
        .map(|j| {
            let reason = wait_reason(j, s);
            let tone = if reason.contains("held") {
                p.heat(3)
            } else if reason.contains("busy") || reason.contains("line") {
                p.muted
            } else {
                p.heat(5)
            };
            let w = usize::from(line.width.saturating_sub(4));
            let age = dur_ms(s.now_ms.saturating_sub(j.queued_at_ms));
            let label_w = w.saturating_sub(age.len() + 8);
            let lock = if j.locks.is_empty() { "" } else { "◈ " };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(format!("#{:<5} ", j.ticket), Style::new().fg(p.faint)),
                    Span::styled(fit(&j.label, label_w), Style::new().fg(p.text)),
                    Span::styled(format!(" {age}"), Style::new().fg(p.muted)),
                ]),
                Line::styled(
                    format!("       {}", fit(&format!("{lock}{reason}"), w.saturating_sub(7))),
                    Style::new().fg(tone),
                ),
            ])
        })
        .collect();
    let line_block = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(p.faint))
        .title(Span::styled(format!(" In line ({}) ", s.waiting.len()), Style::new().fg(p.muted)));
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("  Nobody waiting.", Style::new().fg(p.faint))).block(line_block),
            line,
        );
    } else {
        let mut state = ListState::default();
        state.select(selected.and_then(|t| s.waiting.iter().position(|j| j.ticket == t)));
        let list = List::new(items).block(line_block).highlight_style(p.selected());
        f.render_stateful_widget(list, line, &mut state);
    }

    // The gate: a solid rail while admitting, dashed while the pressure gate holds.
    let held = !s.holds.is_empty();
    let rail: Vec<Line> = (0..gate.height)
        .map(|i| {
            let ch = if held && i % 2 == 1 { " " } else { "┃" };
            Line::styled(format!(" {ch}"), Style::new().fg(heat))
        })
        .collect();
    f.render_widget(Paragraph::new(rail), gate);

    // The lanes.
    let heavy = lanes(s, Class::Heavy);
    let light = lanes(s, Class::Light);
    let heavy_h = u16::try_from(heavy.len()).unwrap_or(2) + 1;
    let [heavy_area, light_area] = Layout::vertical([Constraint::Length(heavy_h + 1), Constraint::Min(2)]).areas(lanes_area);
    render_lane(f, heavy_area, app, Class::Heavy, &heavy, p, selected);
    render_lane(f, light_area, app, Class::Light, &light, p, selected);
}

fn render_lane(frame: &mut Frame, area: Rect, app: &App, class: Class, jobs: &[Option<&JobInfo>], p: &Palette, selected: Option<u64>) {
    let s = &app.snap;
    let busy = jobs.iter().flatten().count();
    let blurb = if class == Class::Heavy {
        "typecheck, clippy, tests"
    } else {
        "formatters, linters"
    };
    let block = Block::new().borders(Borders::TOP).border_style(Style::new().fg(p.faint)).title(Line::from(vec![
        Span::styled(format!(" {} {busy}/{} ", class.name(), jobs.len()), Style::new().fg(p.muted)),
        Span::styled(format!("{blurb} "), Style::new().fg(p.faint)),
    ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    let label_w = (width * 2 / 5).clamp(8, 34);
    let time_w = 16;
    let bar_w = width.saturating_sub(label_w + time_w + 4);
    let mut lines = Vec::new();
    for (i, j) in jobs.iter().enumerate() {
        if lines.len() >= usize::from(inner.height) {
            break;
        }
        let Some(j) = j else {
            let held = class == Class::Heavy && !s.holds.is_empty() && busy > 0;
            lines.push(Line::styled(
                format!(
                    "  {:<label_w$} {}",
                    format!("slot {}", i + 1),
                    if held { "free, but the gate is holding" } else { "free" }
                ),
                Style::new().fg(p.faint),
            ));
            continue;
        };
        let (elapsed, frac) = progress(j, s.now_ms, &app.p50s);
        let over = frac.is_some_and(|f| f > 1.0);
        let fill = if over { p.heat(3) } else { p.heat(0) };
        let mut spans = vec![Span::styled(if j.locks.is_empty() { "  " } else { "◈ " }, Style::new().fg(p.muted))];
        let mut label = Style::new().fg(p.text);
        if selected == Some(j.ticket) {
            label = label.patch(p.selected());
        }
        spans.push(Span::styled(fit(&j.label, label_w), label));
        spans.push(Span::raw(" "));
        match frac {
            Some(fr) => spans.extend(bar_spans(&bar(bar_w, fr, None), fill, p.faint, fill)),
            None => spans.push(Span::styled(fit(&"┄".repeat(bar_w), bar_w), Style::new().fg(p.faint))),
        }
        let usual = app.p50s.get(&j.label).map_or_else(|| "new".to_string(), |p50| dur_ms(*p50));
        spans.push(Span::styled(
            format!(" {:>7}", dur_ms(elapsed)),
            Style::new().fg(if over { p.heat(3) } else { p.text }),
        ));
        spans.push(Span::styled(format!(" / {usual:<6}"), Style::new().fg(p.faint)));
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_gauges(f: &mut Frame, area: Rect, app: &App, sigs: &[Signal], p: &Palette, heat: Color) {
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(p.faint))
        .title(Span::styled(" Pressure  ┃ = where the gate holds new heavy jobs ", Style::new().fg(p.muted)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = usize::from(inner.width);
    let spark_w = if w >= 100 {
        30
    } else if w >= 80 {
        16
    } else {
        0
    };
    let name_w = 13;
    let value_w = 16;
    let limit_w = if w >= 110 { 34 } else { 0 };
    let bar_w = w.saturating_sub(name_w + value_w + spark_w + limit_w + 4).max(6);
    let lines: Vec<Line> = sigs
        .iter()
        .map(|s| {
            let color = if s.off || s.ratio.is_none() {
                p.faint
            } else {
                p.heat(heat_of(s.ratio.unwrap_or(0.0)))
            };
            let mut spans = vec![
                Span::styled(fit(s.name, name_w), Style::new().fg(p.text)),
                Span::styled(
                    fit(&if s.off { format!("{} (off)", s.value) } else { s.value.clone() }, value_w),
                    Style::new().fg(color),
                ),
                Span::raw(" "),
            ];
            spans.extend(bar_spans(&bar(bar_w, gauge_fill(s.ratio), Some(GATE_X)), color, p.faint, heat));
            if spark_w > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(spark(&app.series(s.key), spark_w), Style::new().fg(color)));
            }
            if limit_w > 0 {
                spans.push(Span::styled(format!(" {}", fit(&s.limit, limit_w)), Style::new().fg(p.faint)));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// The small-terminal fallback: every signal on one line.
fn render_gauge_line(f: &mut Frame, area: Rect, sigs: &[Signal], p: &Palette) {
    let mut spans = Vec::new();
    for s in sigs {
        let color = if s.off || s.ratio.is_none() {
            p.faint
        } else {
            p.heat(heat_of(s.ratio.unwrap_or(0.0)))
        };
        spans.push(Span::styled(format!("{} {}  ", s.name.to_lowercase(), s.value), Style::new().fg(color)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_locks(f: &mut Frame, area: Rect, s: &Snapshot, p: &Palette) {
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(p.faint))
        .title(Span::styled(" Locks ", Style::new().fg(p.muted)));
    let mut lines = Vec::new();
    for j in &s.running {
        for lock in &j.locks {
            let waiters: Vec<String> = s.waiting.iter().filter(|w| w.locks.contains(lock)).map(|w| format!("#{}", w.ticket)).collect();
            lines.push(Line::styled(short_lock(lock), Style::new().fg(p.muted)));
            lines.push(Line::styled(format!("  held by #{} {}", j.ticket, j.label), Style::new().fg(p.text)));
            if !waiters.is_empty() {
                lines.push(Line::styled(
                    format!("  {} waiting: {}", waiters.len(), waiters.join(" ")),
                    Style::new().fg(p.heat(3)),
                ));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled("No shared resource is held.", Style::new().fg(p.faint)));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_recent(f: &mut Frame, area: Rect, s: &Snapshot, p: &Palette) {
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(p.faint))
        .title(Span::styled(" Recent ", Style::new().fg(p.muted)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = usize::from(inner.width);
    let lines: Vec<Line> = s
        .history
        .iter()
        .rev()
        .take(usize::from(inner.height))
        .map(|h| {
            let ok = h.outcome == "exit" && h.exit == 0;
            let (what, color) = match h.outcome.as_str() {
                "exit" if h.exit == 0 => ("passed".to_string(), p.ok),
                "exit" => (format!("exit {}", h.exit), p.heat(5)),
                "wait-timeout" => ("gave up".to_string(), p.heat(3)),
                other => (other.to_string(), p.heat(5)),
            };
            let times = format!("waited {:>6}  ran {:>6}", dur_ms(h.wait_ms), dur_ms(h.run_ms));
            let label_w = w.saturating_sub(times.len() + 12);
            Line::from(vec![
                Span::styled(format!("{} {:<8}", if ok { "✓" } else { "✗" }, what), Style::new().fg(color)),
                Span::styled(fit(&format!("{} · {}", h.label, worktree_of(&h.cwd)), label_w), Style::new().fg(p.text)),
                Span::styled(format!(" {times}"), Style::new().fg(p.muted)),
            ])
        })
        .collect();
    if lines.is_empty() {
        f.render_widget(Paragraph::new(Line::styled("No finished jobs yet.", Style::new().fg(p.faint))), inner);
    } else {
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn render_footer(f: &mut Frame, area: Rect, app: &App, p: &Palette) {
    let key = |k: &'static str| Span::styled(k, Style::new().fg(p.text).add_modifier(Modifier::BOLD));
    let dim = |t: &'static str| Span::styled(t, Style::new().fg(p.faint));
    let mut spans = vec![
        key("q"),
        dim(" quit  "),
        key("↑↓"),
        dim(" select  "),
        key("enter"),
        dim(" details  "),
        key("p"),
        dim(if app.paused { " resume" } else { " pause" }),
    ];
    if app.paused {
        spans.push(Span::styled("   PAUSED", Style::new().fg(p.heat(3)).add_modifier(Modifier::BOLD)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_detail(f: &mut Frame, area: Rect, app: &App, j: &JobInfo, p: &Palette, heat: Color) {
    let s = &app.snap;
    let [row] = Layout::vertical([Constraint::Length(10)]).flex(Flex::Center).areas(area);
    let [pop] = Layout::horizontal([Constraint::Max(76)]).flex(Flex::Center).areas(row);
    f.render_widget(Clear, pop);
    let running = j.admitted_at_ms.is_some();
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<11}"), Style::new().fg(p.faint)),
            Span::styled(v, Style::new().fg(p.text)),
        ])
    };
    let mut lines = vec![
        kv(
            "Ticket",
            format!(
                "#{} · {}{}",
                j.ticket,
                j.class.name(),
                if running {
                    j.slot.map(|n| format!(" slot {n}")).unwrap_or_default()
                } else {
                    String::new()
                }
            ),
        ),
        kv("Worktree", j.cwd.display().to_string()),
        kv(
            "Process",
            format!("th pid {}{}", j.pid, j.child_pid.map(|c| format!(" · job pid {c}")).unwrap_or_default()),
        ),
    ];
    if running {
        let (elapsed, _) = progress(j, s.now_ms, &app.p50s);
        let usual = app
            .p50s
            .get(&j.label)
            .map_or_else(|| "unknown — first run here".to_string(), |p50| dur_ms(*p50));
        lines.push(kv("Running", format!("{} (usually {usual})", dur_ms(elapsed))));
    } else {
        lines.push(kv("Waited", dur_ms(s.now_ms.saturating_sub(j.queued_at_ms))));
        lines.push(kv("Waiting on", wait_reason(j, s)));
    }
    if !j.locks.is_empty() {
        lines.push(kv("Locks", j.locks.iter().map(|l| short_lock(l)).collect::<Vec<_>>().join(", ")));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(heat))
        .title(Span::styled(format!(" {} ", j.label), Style::new().fg(p.text).add_modifier(Modifier::BOLD)))
        .title_bottom(Span::styled(" enter to close ", Style::new().fg(p.faint)));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), pop);
}

// ── IO shell ─────────────────────────────────────────────────────────────────

/// Run the TUI until `q`. Returns the process exit code.
///
/// # Errors
/// When the first snapshot cannot be read, or the terminal fails.
pub fn run(q: &Queue, a: &TopArgs) -> Result<i32> {
    use std::io::IsTerminal as _;
    if !cfg!(unix) {
        println!("th ci-queue is Unix-only (it relies on flock and process groups). On this OS `run` executes jobs directly and nothing is queued.");
        return Ok(0);
    }
    if !std::io::stdout().is_terminal() {
        eprintln!("th ci-queue top: stdout is not a terminal. Use `th ci-queue status --json` for scripts.");
        return Ok(2);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut demo = a.demo.then(demo::Demo::new);
    let mut app = match demo.as_mut() {
        Some(d) => {
            // Ten minutes behind it, so the sparklines open full.
            let mut app = App::new(d.step(1000));
            for _ in 0..600 {
                app.update(d.step(1000));
            }
            app
        }
        None => App::new(q.snapshot(HISTORY, &cwd).context("reading the queue")?),
    };
    let mut read = || -> Result<Snapshot> { demo.as_mut().map_or_else(|| q.snapshot(HISTORY, &cwd), |d| Ok(d.step(1000))) };
    let palette = Palette::detect();
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &palette, Duration::from_millis(a.interval.max(100)), &mut read);
    ratatui::restore();
    result.map(|()| 0)
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App, p: &Palette, every: Duration, read: &mut dyn FnMut() -> Result<Snapshot>) -> Result<()> {
    let mut last = Instant::now();
    loop {
        terminal.draw(|f| render(f, app, p)).context("drawing")?;
        let wait = every.saturating_sub(last.elapsed());
        if event::poll(wait).context("reading keys")? {
            if let Event::Key(k) = event::read().context("reading keys")? {
                if k.kind == KeyEventKind::Press && !app.on(key_action(k.code, k.modifiers)) {
                    return Ok(());
                }
            }
        }
        if last.elapsed() >= every {
            last = Instant::now();
            if !app.paused {
                match read() {
                    Ok(s) => app.update(s),
                    Err(e) => app.read_error = Some(format!("{e:#}")),
                }
            }
        }
    }
}

/// A seeded, deterministic busy machine for `--demo`: heavy jobs pile up,
/// cargo jobs fight over the shared target, and load follows the lanes.
pub mod demo {
    use std::path::PathBuf;

    use super::super::budget::View;
    use super::super::config::Config;
    use super::super::pressure::{self, Disk, Readings};
    use super::super::queue::{Class, HistoryEntry, JobInfo, Snapshot, SNAPSHOT_SCHEMA};

    const GIB: u64 = 1024 * 1024 * 1024;
    const CARGO: &str = "cargo:/Users/dev/.cargo/shared-target";
    const JOBS: [(&str, Class, bool, u64, &str); 9] = [
        ("cargo clippy --workspace", Class::Heavy, true, 95_000, "smooth-SMOODEV-3342-models"),
        ("pnpm turbo typecheck", Class::Heavy, false, 70_000, "smooai-SMOODEV-3323-hitl"),
        ("cargo test -p smooth-cli", Class::Heavy, true, 140_000, "smooth-SMOODEV-3355-ci-queue"),
        ("tsgo --noEmit", Class::Heavy, false, 38_000, "smooai-SMOODEV-3207-ask-smooth"),
        ("vitest run packages/backend", Class::Heavy, false, 55_000, "smooai-SMOODEV-3352-o11y"),
        ("oxfmt --check", Class::Light, false, 16_000, "smooai-SMOODEV-3323-hitl"),
        ("cargo fmt --check", Class::Light, false, 11_000, "smooth-th-1efb59-flow-mcp"),
        ("oxlint .", Class::Light, false, 21_000, "smooai-SMOODEV-3207-ask-smooth"),
        ("lint-staged", Class::Light, false, 26_000, "smooai-SMOODEV-3352-o11y"),
    ];

    pub struct Demo {
        seed: u64,
        ticket: u64,
        now: u64,
        running: Vec<(JobInfo, u64)>,
        waiting: Vec<JobInfo>,
        history: Vec<HistoryEntry>,
        load: f64,
        swap_gb: f64,
        avail_pct: f64,
    }

    impl Default for Demo {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Demo {
        #[must_use]
        pub fn new() -> Self {
            let mut d = Self {
                seed: 7,
                ticket: 400,
                now: 1_790_550_000_000,
                running: Vec::new(),
                waiting: Vec::new(),
                history: Vec::new(),
                load: 38.0,
                swap_gb: 17.0,
                avail_pct: 14.0,
            };
            for i in 0..40_u64 {
                let (label, class, _, ms, wt) = JOBS[usize::try_from(i).unwrap_or(0) % JOBS.len()];
                let run = ms * (70 + d.rand(60)) / 100;
                let fail = d.rand(100) < 12;
                let ticket = d.next_ticket();
                let wait_ms = d.rand(90_000);
                d.history.push(HistoryEntry {
                    label: label.into(),
                    class: Some(class),
                    cwd: PathBuf::from("/Users/dev").join(wt),
                    ticket,
                    queued_at_ms: d.now - (40 - i) * 60_000,
                    wait_ms,
                    run_ms: run,
                    outcome: "exit".into(),
                    exit: i32::from(fail),
                    ..HistoryEntry::default()
                });
            }
            for _ in 0..9 {
                d.arrive();
            }
            d.admit();
            d
        }

        fn next_ticket(&mut self) -> u64 {
            self.ticket += 1;
            self.ticket
        }

        /// xorshift64*, reduced to `0..n`.
        fn rand(&mut self, n: u64) -> u64 {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            self.seed.wrapping_mul(0x2545_f491_4f6c_dd1d) % n.max(1)
        }

        fn arrive(&mut self) {
            let (label, class, cargo, _, wt) = JOBS[usize::try_from(self.rand(JOBS.len() as u64)).unwrap_or(0)];
            let t = self.next_ticket();
            self.waiting.push(JobInfo {
                class,
                ticket: t,
                label: label.into(),
                pid: u32::try_from(40_000 + t).unwrap_or(0),
                cwd: PathBuf::from("/Users/dev").join(wt),
                queued_at_ms: self.now,
                locks: if cargo { vec![CARGO.into()] } else { Vec::new() },
                ..JobInfo::default()
            });
        }

        /// The real gate's verdict on the simulated readings.
        fn holds(&self) -> Vec<String> {
            pressure::holds(&self.readings(), &Config::default().gate)
        }

        fn admit(&mut self) {
            let holds = self.holds();
            for class in [Class::Heavy, Class::Light] {
                let slots = if class == Class::Heavy { 2 } else { 6 };
                let mut line: Vec<u64> = self.waiting.iter().filter(|w| w.class == class).map(|w| w.ticket).collect();
                line.sort_unstable();
                for t in line {
                    let Some(w) = self.waiting.iter().find(|w| w.ticket == t).cloned() else {
                        continue;
                    };
                    let busy: Vec<usize> = self.running.iter().filter(|(r, _)| r.class == class).filter_map(|(r, _)| r.slot).collect();
                    let locked = w.locks.iter().any(|l| self.running.iter().any(|(r, _)| r.locks.contains(l)));
                    if locked || busy.len() >= slots || (class == Class::Heavy && !holds.is_empty() && !busy.is_empty()) {
                        continue;
                    }
                    let slot = (1..=slots).find(|i| !busy.contains(i)).unwrap_or(1);
                    let (_, _, _, ms, _) = JOBS.iter().find(|j| j.0 == w.label).copied().unwrap_or(JOBS[0]);
                    let due = self.now + ms * (60 + self.rand(90)) / 100;
                    self.waiting.retain(|x| x.ticket != t);
                    self.running.push((
                        JobInfo {
                            admitted_at_ms: Some(self.now),
                            slot: Some(slot),
                            child_pid: Some(w.pid + 1),
                            ..w
                        },
                        due,
                    ));
                }
            }
        }

        /// Advance `ms` and return the snapshot.
        #[allow(clippy::cast_precision_loss, clippy::suboptimal_flops, reason = "a simulation")]
        pub fn step(&mut self, ms: u64) -> Snapshot {
            self.now += ms;
            let now = self.now;
            let (done, still): (Vec<_>, Vec<_>) = std::mem::take(&mut self.running).into_iter().partition(|(_, due)| *due <= now);
            self.running = still;
            for (j, _) in done {
                let fail = self.rand(100) < 15;
                let admitted = j.admitted_at_ms.unwrap_or(now);
                self.history.push(HistoryEntry {
                    label: j.label,
                    class: Some(j.class),
                    cwd: j.cwd,
                    ticket: j.ticket,
                    queued_at_ms: j.queued_at_ms,
                    finished_at_ms: now,
                    wait_ms: admitted - j.queued_at_ms,
                    run_ms: now - admitted,
                    outcome: "exit".into(),
                    exit: i32::from(fail),
                    ..HistoryEntry::default()
                });
            }
            if self.history.len() > 200 {
                self.history.drain(..self.history.len() - 200);
            }
            if self.rand(1000) < 300 * ms / 1000 && self.waiting.len() < 10 {
                self.arrive();
            }
            let heavy = self.running.iter().filter(|(j, _)| j.class == Class::Heavy).count() as f64;
            let light = self.running.len() as f64 - heavy;
            let target = light.mul_add(2.5, heavy.mul_add(15.0, 16.0)) + self.rand(8) as f64;
            self.load += (target - self.load) * 0.08;
            self.avail_pct = (self.avail_pct + (18.0 - heavy * 5.0 - self.avail_pct) * 0.05).clamp(3.0, 40.0);
            self.swap_gb = (self.swap_gb + if heavy >= 2.0 { 0.03 } else { -0.06 }).clamp(12.0, 22.6);
            self.admit();
            self.snapshot()
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss, reason = "a simulation")]
        fn readings(&self) -> Readings {
            let total = 24 * GIB;
            Readings {
                mem_total_bytes: Some(total),
                mem_available_bytes: Some((total as f64 * self.avail_pct / 100.0) as u64),
                memory_pressure_level: Some(if self.avail_pct < 8.0 { 2 } else { 1 }),
                swap_total_bytes: Some((23.5 * GIB as f64) as u64),
                swap_used_bytes: Some((self.swap_gb * GIB as f64) as u64),
                load1: Some(self.load),
                cpu_busy_pct: Some((self.load / 12.0 * 60.0).min(100.0)),
                cores: 12,
                disks: vec![Disk {
                    path: PathBuf::from("/Users/dev/.cargo/shared-target"),
                    free_bytes: Some(142 * GIB),
                }],
            }
        }

        #[must_use]
        pub fn snapshot(&self) -> Snapshot {
            let mut waiting = self.waiting.clone();
            waiting.sort_by_key(|w| w.ticket);
            let mut snap = Snapshot {
                schema: SNAPSHOT_SCHEMA,
                budget: View::default(),
                dir: PathBuf::from("/Users/dev/.smooth/ci-queue"),
                config: Config::default(),
                now_ms: self.now,
                running: self.running.iter().map(|(j, _)| j.clone()).collect(),
                waiting,
                readings: self.readings(),
                holds: self.holds(),
                history: self.history.clone(),
            };
            // Publish each waiter's blocker the way a real waiter does.
            let reasons: Vec<String> = snap.waiting.iter().map(|w| super::wait_reason(w, &snap)).collect();
            for (w, r) in snap.waiting.iter_mut().zip(reasons) {
                w.waiting_on = Some(r);
            }
            snap
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::super::config::Config;
    use super::super::pressure::Disk;
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const GB: u64 = 1024 * 1024 * 1024;

    fn readings() -> Readings {
        Readings {
            mem_total_bytes: Some(24 * GB),
            mem_available_bytes: Some(12 * GB),
            memory_pressure_level: Some(1),
            swap_total_bytes: Some(20 * GB),
            swap_used_bytes: Some(2 * GB),
            load1: Some(12.0),
            cpu_busy_pct: Some(45.0),
            cores: 12,
            disks: vec![Disk {
                path: "/".into(),
                free_bytes: Some(200 * GB),
            }],
        }
    }

    fn snap() -> Snapshot {
        Snapshot {
            schema: super::super::queue::SNAPSHOT_SCHEMA,
            budget: super::super::budget::View::default(),
            dir: "/q".into(),
            config: Config::default(),
            now_ms: 100_000,
            running: Vec::new(),
            waiting: Vec::new(),
            readings: readings(),
            holds: Vec::new(),
            history: Vec::new(),
        }
    }

    fn job(ticket: u64, class: Class) -> JobInfo {
        JobInfo {
            class,
            ticket,
            label: format!("job {ticket}"),
            pid: 1,
            cwd: "/w/repo".into(),
            ..JobInfo::default()
        }
    }

    fn running(ticket: u64, class: Class, slot: usize) -> JobInfo {
        JobInfo {
            admitted_at_ms: Some(40_000),
            slot: Some(slot),
            ..job(ticket, class)
        }
    }

    fn hist(label: &str, run_ms: u64, outcome: &str) -> HistoryEntry {
        HistoryEntry {
            label: label.into(),
            class: Some(Class::Heavy),
            cwd: "/w".into(),
            run_ms,
            outcome: outcome.into(),
            ..HistoryEntry::default()
        }
    }

    #[test]
    fn heat_walks_the_spectrum_toward_and_past_the_threshold() {
        assert_eq!([0.0, 0.6, 0.8, 0.95, 1.05, 2.0, f64::NAN].map(heat_of), [0, 1, 2, 3, 4, 5, 0]);
    }

    #[test]
    fn every_gauge_puts_its_threshold_at_the_same_column() {
        assert!((gauge_fill(Some(1.0)) - GATE_X).abs() < 1e-9);
        assert!(gauge_fill(Some(1.1)) > GATE_X);
        assert!((gauge_fill(Some(10.0)) - 1.0).abs() < 1e-9);
        assert!(gauge_fill(None).abs() < 1e-9);
    }

    #[test]
    fn signals_read_each_threshold_worse_upward() {
        let s = signals(&readings(), &Config::default().gate);
        let get = |k| s.iter().find(|x| x.key == k).unwrap();
        assert_eq!(get(SignalKey::Memory).value, "50% used");
        assert!((get(SignalKey::Memory).ratio.unwrap() - 50.0 / 95.0).abs() < 1e-9);
        assert_eq!(get(SignalKey::Swap).value, "10% used");
        assert_eq!(get(SignalKey::Load).value, "12 · 1.0/core");
        assert!((get(SignalKey::Load).ratio.unwrap() - 1.0 / 12.0).abs() < 1e-9);
        assert_eq!(get(SignalKey::Cpu).value, "45% busy");
        assert!((get(SignalKey::Cpu).ratio.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(get(SignalKey::Disk).value, "200 GB free");
        assert!((get(SignalKey::Disk).ratio.unwrap() - 0.1).abs() < 1e-9);
        assert_eq!(get(SignalKey::Pressure).value, "normal");
    }

    #[test]
    fn unknown_is_unknown_and_a_zero_threshold_is_off() {
        let r = Readings {
            cores: 12,
            ..Readings::default()
        };
        let mut g = Config::default().gate;
        g.max_load_per_core = 0.0;
        let s = signals(&r, &g);
        assert!(s.iter().all(|x| x.ratio.is_none() && x.value == "unknown"));
        assert!(s.iter().find(|x| x.key == SignalKey::Load).unwrap().off);
        assert!(!s.iter().any(|x| x.key == SignalKey::Pressure), "no pressure row off macOS");
    }

    #[test]
    fn machine_heat_follows_the_gate_verdict() {
        assert_eq!(machine_heat(&snap()), 1);
        let mut busy = snap();
        busy.readings.load1 = Some(216.0);
        assert_eq!(machine_heat(&busy), 3, "hot readings with no hold stay at gold");
        busy.holds = vec!["load".into()];
        assert_eq!(machine_heat(&busy), 5);
        let mut cool_hold = snap();
        cool_hold.holds = vec!["disk".into()];
        assert_eq!(machine_heat(&cool_hold), 4, "a hold is never cooler than orange");
    }

    #[test]
    fn full_swap_alone_does_not_heat_the_machine() {
        let mut s = snap();
        s.readings.swap_used_bytes = Some(19 * GB + GB / 2);
        assert_eq!(machine_heat(&s), 1);
        s.readings.memory_pressure_level = Some(2);
        assert_eq!(machine_heat(&s), 3);
    }

    #[test]
    fn p50s_ignore_jobs_that_never_ran() {
        let p = label_p50s(&[
            hist("clippy", 10_000, "exit"),
            hist("clippy", 30_000, "exit"),
            hist("clippy", 20_000, "exit"),
            hist("tsc", 4_000, "exit"),
            hist("tsc", 6_000, "signal"),
            hist("tsc", 0, "wait-timeout"),
            hist("never", 5_000, "spawn-failed"),
        ]);
        assert_eq!(p.get("clippy"), Some(&20_000));
        assert_eq!(p.get("tsc"), Some(&5_000));
        assert!(!p.contains_key("never"));
    }

    #[test]
    fn progress_is_elapsed_over_p50_or_indeterminate() {
        let p50 = HashMap::from([("job 1".to_string(), 60_000)]);
        let j = running(1, Class::Heavy, 0);
        let (e, f) = progress(&j, 70_000, &p50);
        assert_eq!(e, 30_000);
        assert!((f.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(progress(&running(2, Class::Heavy, 0), 70_000, &p50).1, None);
    }

    #[test]
    fn lanes_keep_free_slots_and_overflow() {
        let mut s = snap();
        // The queue numbers slots from 1: heavy-2 is the second lane.
        s.running = vec![running(1, Class::Heavy, 2), running(2, Class::Light, 1), running(3, Class::Heavy, 9)];
        let heavy = lanes(&s, Class::Heavy);
        assert_eq!(heavy.len(), 3);
        assert!(heavy[0].is_none());
        assert_eq!(heavy[1].unwrap().ticket, 1);
        assert_eq!(heavy[2].unwrap().ticket, 3);
        assert_eq!(lanes(&s, Class::Light).len(), 6);
    }

    #[test]
    fn wait_reasons_name_the_lock_the_slots_or_the_gate() {
        let lock = "cargo:/Users/me/.cargo/shared-target".to_string();
        let mut s = snap();
        let mut holder = running(1, Class::Heavy, 1);
        holder.locks = vec![lock.clone()];
        let mut waiter = job(3, Class::Heavy);
        waiter.locks = vec![lock];
        s.running = vec![holder];
        s.waiting = vec![waiter, job(4, Class::Heavy)];
        assert_eq!(wait_reason(&s.waiting[0], &s), "cargo:~/.cargo/shared-target held by #1 job 1");
        assert_eq!(wait_reason(&s.waiting[1], &s), "1 ahead in line");
        s.holds = vec!["swap 93%".into()];
        assert_eq!(wait_reason(&s.waiting[1], &s), "swap 93%");
        s.running.push(running(2, Class::Heavy, 2));
        assert_eq!(wait_reason(&s.waiting[1], &s), "2/2 heavy busy");
        s.waiting[1].waiting_on = Some("swap 96% > 90%".into());
        assert_eq!(wait_reason(&s.waiting[1], &s), "swap 96% > 90%", "the queue's own words win");
    }

    #[test]
    fn short_lock_hides_the_home_dir() {
        assert_eq!(short_lock("cargo:/Users/me/.cargo/t"), "cargo:~/.cargo/t");
        assert_eq!(short_lock("cargo:/home/me/t"), "cargo:~/t");
        assert_eq!(short_lock("docker"), "docker");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(dur_ms(5_000), "5s");
        assert_eq!(dur_ms(125_000), "2m05s");
        assert_eq!(dur_ms((3 * 3600 + 7 * 60) * 1000), "3h07m");
    }

    #[test]
    fn bars_fill_and_mark() {
        let b = bar(10, 0.5, Some(0.8));
        assert_eq!(b.iter().filter(|c| **c == Cell::Fill).count(), 5);
        assert_eq!(b[8], Cell::Marker);
        assert_eq!(bar(10, 2.0, None).iter().filter(|c| **c == Cell::Fill).count(), 10);
        assert_eq!(bar(4, 1.0, Some(1.0))[3], Cell::Marker, "a marker at the end stays inside");
        assert!(bar(0, 0.5, Some(0.8)).is_empty());
    }

    #[test]
    fn sparklines_bucket_to_width_and_keep_unknown_blank() {
        assert_eq!(spark(&[Some(0.0), Some(1.25)], 4), "  ▁█");
        assert_eq!(spark(&[None, Some(0.0)], 2), " ▁");
        let long: Vec<Option<f64>> = (0..100).map(|i| Some(f64::from(i) / 100.0)).collect();
        assert_eq!(spark(&long, 10).chars().count(), 10);
        assert_eq!(spark(&long, 0), "");
    }

    #[test]
    fn keys_map_to_actions() {
        let none = KeyModifiers::NONE;
        assert_eq!(key_action(KeyCode::Char('q'), none), Action::Quit);
        assert_eq!(key_action(KeyCode::Char('c'), KeyModifiers::CONTROL), Action::Quit);
        assert_eq!(key_action(KeyCode::Up, none), Action::Up);
        assert_eq!(key_action(KeyCode::Char('j'), none), Action::Down);
        assert_eq!(key_action(KeyCode::Enter, none), Action::Details);
        assert_eq!(key_action(KeyCode::Char('p'), none), Action::Pause);
        assert_eq!(key_action(KeyCode::Char('x'), none), Action::Ignore);
    }

    #[test]
    fn selection_follows_its_ticket_across_updates() {
        let mut s = snap();
        s.running = vec![running(1, Class::Heavy, 1)];
        s.waiting = vec![job(5, Class::Heavy), job(6, Class::Heavy)];
        let mut app = App::new(Snapshot {
            running: s.running.clone(),
            waiting: s.waiting.clone(),
            ..snap()
        });
        app.on(Action::Down);
        app.on(Action::Down);
        assert_eq!(app.selected_job().unwrap().ticket, 6);
        // #5 is admitted: #6 is still selected, now one row up.
        s.running.push(running(5, Class::Heavy, 2));
        s.waiting.remove(0);
        app.update(s);
        assert_eq!(app.selected_job().unwrap().ticket, 6);
        app.on(Action::Details);
        assert!(app.detail);
        assert!(!app.on(Action::Quit));
    }

    #[test]
    fn details_do_not_open_on_an_empty_queue() {
        let mut app = App::new(snap());
        app.on(Action::Details);
        assert!(!app.detail);
        app.on(Action::Up);
        app.on(Action::Pause);
        assert!(app.paused);
    }

    fn draw(app: &App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| render(f, app, &Palette::rgb())).unwrap();
        let buf = t.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn busy_app() -> App {
        let mut d = demo::Demo::new();
        let mut app = App::new(d.step(1000));
        for _ in 0..300 {
            app.update(d.step(1000));
        }
        app
    }

    #[test]
    fn a_big_terminal_shows_every_section() {
        let app = busy_app();
        let screen = draw(&app, 140, 44);
        for want in [
            "ci-queue", "In line", "heavy", "light", "Pressure", "Memory", "Load", "Disk", "Locks", "Recent", "q quit",
        ] {
            assert!(screen.contains(want), "missing {want:?}:\n{screen}");
        }
        assert!(screen.contains('┃'), "the gate line");
    }

    #[test]
    fn a_small_terminal_drops_sections_instead_of_overlapping() {
        let app = busy_app();
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("Pressure"));
        assert!(!screen.contains("Recent"), "recent drops first:\n{screen}");
        let tiny = draw(&app, 60, 14);
        assert!(!tiny.contains("Pressure"));
        assert!(tiny.contains("load "), "one-line pressure summary:\n{tiny}");
        let too_small = draw(&app, 20, 5);
        assert!(too_small.contains("th ci-queue"));
    }

    #[test]
    fn an_idle_queue_renders_its_empty_states() {
        let screen = draw(&App::new(snap()), 120, 40);
        for want in ["Idle.", "Nobody waiting.", "free", "No shared resource is held.", "No finished jobs yet."] {
            assert!(screen.contains(want), "missing {want:?}:\n{screen}");
        }
    }

    #[test]
    fn the_detail_popup_describes_the_selected_job() {
        let mut app = busy_app();
        app.on(Action::Details);
        let screen = draw(&app, 120, 40);
        assert!(screen.contains("Ticket"), "{screen}");
        assert!(screen.contains("enter to close"));
    }

    #[test]
    fn the_demo_is_deterministic_and_respects_slots_and_locks() {
        let mut a = demo::Demo::new();
        let mut b = demo::Demo::new();
        for _ in 0..400 {
            let (sa, sb) = (a.step(1000), b.step(1000));
            assert_eq!(serde_json::to_string(&sa).unwrap(), serde_json::to_string(&sb).unwrap());
            assert!(sa.running.iter().filter(|j| j.class == Class::Heavy).count() <= 2);
            assert!(sa.running.iter().filter(|j| j.class == Class::Light).count() <= 6);
            assert!(
                sa.running.iter().all(|j| j.slot.is_some_and(|n| n >= 1)),
                "slots are numbered from 1, like the queue"
            );
            let held: Vec<&String> = sa.running.iter().flat_map(|j| &j.locks).collect();
            let unique: std::collections::HashSet<_> = held.iter().collect();
            assert_eq!(unique.len(), held.len(), "a lock held twice");
            assert!(sa.waiting.iter().all(|w| w.waiting_on.is_some()), "every demo waiter publishes its blocker");
        }
    }

    #[test]
    fn palettes_resolve_for_every_terminal() {
        assert_eq!(Palette::rgb().heat(9), Palette::rgb().heat[5]);
        assert_eq!(Palette::indexed().heat[0], Color::Indexed(37));
        assert_eq!(Palette::mono().text, Color::Reset);
    }
}
