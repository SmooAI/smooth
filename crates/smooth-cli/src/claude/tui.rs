//! `th claude tui` — a ratatui control dashboard for supervised sessions.
//!
//! Lists live sessions with their mode, shows a live snippet of the
//! selected session's pane, and lets you flip driving/manual/paused or
//! attach — the "switch between Big Smooth driving and the session
//! itself" surface. The supervisor owns the `TmuxDriver`; this separate
//! process reads the pane straight from tmux via the registry's socket.
//!
//! The decision logic (`key_action`, `clamp_selected`, `tail_lines`, and
//! the `App` navigation) is pure and unit tested; the draw + event loop
//! is the IO shell, verified by running it.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use super::control::{self, Mode};
use super::registry::{self, SessionEntry};

/// What a keypress maps to. Pure so the binding table is unit tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiAction {
    Quit,
    Up,
    Down,
    SetMode(Mode),
    Attach,
    Refresh,
    Ignore,
}

/// Map a key to an action.
#[must_use]
pub fn key_action(code: KeyCode) -> TuiAction {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => TuiAction::Quit,
        KeyCode::Up | KeyCode::Char('k') => TuiAction::Up,
        KeyCode::Down | KeyCode::Char('j') => TuiAction::Down,
        KeyCode::Char('d') => TuiAction::SetMode(Mode::Driving),
        KeyCode::Char('m') => TuiAction::SetMode(Mode::Manual),
        KeyCode::Char('p') => TuiAction::SetMode(Mode::Paused),
        KeyCode::Char('a') | KeyCode::Enter => TuiAction::Attach,
        KeyCode::Char('r') => TuiAction::Refresh,
        _ => TuiAction::Ignore,
    }
}

/// Clamp a selection index into `[0, len)` (or 0 when empty).
#[must_use]
pub fn clamp_selected(len: usize, selected: usize) -> usize {
    if len == 0 {
        0
    } else {
        selected.min(len - 1)
    }
}

/// Keep only the last `n` lines of `text` (the most recent pane output).
#[must_use]
pub fn tail_lines(text: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

struct App {
    sessions: Vec<SessionEntry>,
    selected: usize,
}

impl App {
    fn loaded() -> Self {
        let mut app = Self {
            sessions: Vec::new(),
            selected: 0,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.sessions = registry::read_live_and_prune();
        self.selected = clamp_selected(self.sessions.len(), self.selected);
    }

    fn up(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = if self.selected == 0 { self.sessions.len() - 1 } else { self.selected - 1 };
        }
    }

    fn down(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = (self.selected + 1) % self.sessions.len();
        }
    }

    fn selected_entry(&self) -> Option<&SessionEntry> {
        self.sessions.get(self.selected)
    }
}

/// Run the control dashboard. Sets up the terminal, runs the loop, and
/// always restores the terminal on exit.
///
/// # Errors
/// On terminal draw / event-read failure.
pub fn run_tui() -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal) -> Result<()> {
    let mut app = App::loaded();
    let mut last_refresh = Instant::now();
    loop {
        let selected = app.selected_entry().cloned();
        let pane = selected.as_ref().map(capture_pane).unwrap_or_default();
        let mode = selected.as_ref().map_or(Mode::default(), |e| control::read_mode(&e.id));
        terminal.draw(|f| render(f, &app, &pane, mode)).context("tui draw")?;

        if event::poll(Duration::from_millis(250)).context("tui event poll")? {
            if let Event::Key(key) = event::read().context("tui event read")? {
                if key.kind == KeyEventKind::Press {
                    match key_action(key.code) {
                        TuiAction::Quit => break,
                        TuiAction::Up => app.up(),
                        TuiAction::Down => app.down(),
                        TuiAction::Refresh => app.refresh(),
                        TuiAction::SetMode(m) => {
                            if let Some(e) = &selected {
                                let _ = control::write_mode(&e.id, m);
                            }
                        }
                        TuiAction::Attach => {
                            if let Some(e) = &selected {
                                attach_handoff(terminal, e)?;
                                app.refresh();
                            }
                        }
                        TuiAction::Ignore => {}
                    }
                }
            }
        }

        if last_refresh.elapsed() >= Duration::from_secs(1) {
            app.refresh();
            last_refresh = Instant::now();
        }
    }
    Ok(())
}

/// Suspend the TUI, hand the terminal to `tmux attach`, then restore it.
fn attach_handoff(terminal: &mut DefaultTerminal, entry: &SessionEntry) -> Result<()> {
    ratatui::restore();
    let status = Command::new("tmux").args(["-L", &entry.socket, "attach", "-t", &entry.session]).status();
    *terminal = ratatui::init();
    let _ = terminal.clear();
    status.context("running tmux attach")?;
    Ok(())
}

fn capture_pane(entry: &SessionEntry) -> String {
    Command::new("tmux")
        .args(["-L", &entry.socket, "capture-pane", "-t", &entry.session, "-p"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn render(f: &mut Frame, app: &App, pane: &str, mode: Mode) {
    let [body, footer] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    let [left, right] = Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).areas(body);

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|e| {
            let m = control::read_mode(&e.id);
            ListItem::new(format!("{}  {:7}  {}", e.id, m.as_str(), e.label.as_deref().unwrap_or("-")))
        })
        .collect();
    let mut state = ListState::default();
    if !app.sessions.is_empty() {
        state.select(Some(app.selected));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" sessions "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, left, &mut state);

    let title = app
        .selected_entry()
        .map_or_else(|| " (no supervised sessions — th claude run ) ".to_string(), |e| format!(" {} [{mode}] ", e.id));
    // Show the most recent lines that fit the preview pane.
    let visible_rows = usize::from(right.height.saturating_sub(2));
    let shown = tail_lines(pane, visible_rows);
    let preview = Paragraph::new(shown)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(preview, right);

    let foot = Paragraph::new("↑/↓ select · d driving · m manual · p paused · a/enter attach · r refresh · q quit");
    f.render_widget(foot, footer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(id: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_string(),
            session: format!("claude-{id}"),
            socket: format!("sock-{id}"),
            cwd: "/tmp".to_string(),
            label: None,
            pid: 1,
            started_at: Utc::now(),
        }
    }

    fn app_with(n: usize) -> App {
        App {
            sessions: (0..n).map(|i| entry(&format!("s{i}"))).collect(),
            selected: 0,
        }
    }

    #[test]
    fn key_bindings() {
        assert_eq!(key_action(KeyCode::Char('q')), TuiAction::Quit);
        assert_eq!(key_action(KeyCode::Esc), TuiAction::Quit);
        assert_eq!(key_action(KeyCode::Char('k')), TuiAction::Up);
        assert_eq!(key_action(KeyCode::Up), TuiAction::Up);
        assert_eq!(key_action(KeyCode::Char('j')), TuiAction::Down);
        assert_eq!(key_action(KeyCode::Char('d')), TuiAction::SetMode(Mode::Driving));
        assert_eq!(key_action(KeyCode::Char('m')), TuiAction::SetMode(Mode::Manual));
        assert_eq!(key_action(KeyCode::Char('p')), TuiAction::SetMode(Mode::Paused));
        assert_eq!(key_action(KeyCode::Enter), TuiAction::Attach);
        assert_eq!(key_action(KeyCode::Char('a')), TuiAction::Attach);
        assert_eq!(key_action(KeyCode::Char('r')), TuiAction::Refresh);
        assert_eq!(key_action(KeyCode::Char('z')), TuiAction::Ignore);
    }

    #[test]
    fn clamp_handles_empty_and_overflow() {
        assert_eq!(clamp_selected(0, 5), 0);
        assert_eq!(clamp_selected(3, 5), 2);
        assert_eq!(clamp_selected(3, 1), 1);
    }

    #[test]
    fn tail_keeps_recent_lines() {
        let text = "a\nb\nc\nd\ne";
        assert_eq!(tail_lines(text, 2), "d\ne");
        assert_eq!(tail_lines(text, 10), text);
        assert_eq!(tail_lines(text, 0), "");
    }

    #[test]
    fn navigation_wraps() {
        let mut app = app_with(3);
        assert_eq!(app.selected, 0);
        app.up(); // wrap to last
        assert_eq!(app.selected, 2);
        app.down(); // wrap to first
        assert_eq!(app.selected, 0);
        app.down();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn navigation_noop_when_empty() {
        let mut app = app_with(0);
        app.up();
        app.down();
        assert_eq!(app.selected, 0);
        assert!(app.selected_entry().is_none());
    }

    #[test]
    fn selected_entry_tracks_index() {
        let mut app = app_with(2);
        assert_eq!(app.selected_entry().unwrap().id, "s0");
        app.down();
        assert_eq!(app.selected_entry().unwrap().id, "s1");
    }
}
