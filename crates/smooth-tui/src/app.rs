//! TUI app — state, event loop, tab navigation, mouse support.

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Terminal;

use crate::theme;
use crate::views::{chat, dashboard};

/// Available tabs.
const TAB_NAMES: &[&str] = &["Dashboard", "Beads", "Operators", "Chat", "Messages", "Reviews", "System"];

/// Application state.
pub struct App {
    pub active_tab: usize,
    pub leader_url: String,
    pub health: dashboard::HealthData,
    pub chat_state: chat::ChatState,
    pub should_quit: bool,
}

impl App {
    pub fn new(leader_url: &str) -> Self {
        Self {
            active_tab: 0,
            leader_url: leader_url.to_string(),
            health: dashboard::HealthData::default(),
            chat_state: chat::ChatState::default(),
            should_quit: false,
        }
    }

    /// Fetch health data from leader.
    pub async fn refresh_health(&mut self) {
        let url = format!("{}/api/system/health", self.leader_url);
        if let Ok(resp) = reqwest::get(&url).await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(data) = json.get("data") {
                    self.health.leader_status = data.pointer("/leader/status").and_then(|v| v.as_str()).unwrap_or("unknown").into();
                    self.health.leader_uptime = data.pointer("/leader/uptime").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    self.health.db_status = data.pointer("/database/status").and_then(|v| v.as_str()).unwrap_or("unknown").into();
                    self.health.db_path = data.pointer("/database/path").and_then(|v| v.as_str()).unwrap_or("").into();
                    self.health.sandbox_status = data.pointer("/sandbox/status").and_then(|v| v.as_str()).unwrap_or("unknown").into();
                    self.health.sandbox_active = data.pointer("/sandbox/active_sandboxes").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    self.health.sandbox_max = data.pointer("/sandbox/max_concurrency").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
                    self.health.tailscale_status = data.pointer("/tailscale/status").and_then(|v| v.as_str()).unwrap_or("disconnected").into();
                    self.health.tailscale_hostname = data.pointer("/tailscale/hostname").and_then(|v| v.as_str()).map(String::from);
                    self.health.beads_status = data.pointer("/beads/status").and_then(|v| v.as_str()).unwrap_or("unknown").into();
                    self.health.beads_open = data.pointer("/beads/open_issues").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                }
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Char('q') if self.active_tab != 3 => self.should_quit = true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.should_quit = true,
            KeyCode::Tab => {
                self.active_tab = (self.active_tab + 1) % TAB_NAMES.len();
            }
            KeyCode::BackTab => {
                self.active_tab = (self.active_tab + TAB_NAMES.len() - 1) % TAB_NAMES.len();
            }
            KeyCode::Char(c) if ('1'..='7').contains(&c) => {
                let idx = (c as usize) - ('1' as usize);
                if idx < TAB_NAMES.len() {
                    self.active_tab = idx;
                }
            }
            // Chat input
            KeyCode::Char(c) if self.active_tab == 3 => {
                self.chat_state.input.push(c);
            }
            KeyCode::Backspace if self.active_tab == 3 => {
                self.chat_state.input.pop();
            }
            KeyCode::Enter if self.active_tab == 3 && !self.chat_state.input.is_empty() => {
                let content = std::mem::take(&mut self.chat_state.input);
                self.chat_state.messages.push(chat::ChatMessage { role: "user".into(), content });
                // TODO: send to leader API and stream response
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, col: u16, row: u16) {
        // Tab bar is at row 2, tabs start after the header
        if row == 2 {
            let mut x: u16 = 1;
            for (i, name) in TAB_NAMES.iter().enumerate() {
                let width = name.len() as u16 + 3; // " Name "
                if col >= x && col < x + width {
                    self.active_tab = i;
                    return;
                }
                x += width;
            }
        }
    }
}

fn ui(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    // Header with tabs
    let titles: Vec<Line> = TAB_NAMES
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let style = if i == app.active_tab { theme::active_tab() } else { theme::inactive_tab() };
            Line::from(Span::styled(format!(" {t} "), style))
        })
        .collect();

    let tabs = Tabs::new(titles)
        .select(app.active_tab)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::SMOO_GREEN))
                .title(Span::styled(" SMOO.AI / SMOOTH ", theme::title())),
        )
        .highlight_style(theme::active_tab());
    f.render_widget(tabs, chunks[0]);

    // Content
    let content_area = chunks[1];
    match app.active_tab {
        0 => dashboard::render(f, content_area, &app.health),
        3 => chat::render(f, content_area, &app.chat_state),
        _ => {
            let placeholder = Paragraph::new(Line::from(Span::styled(format!("{} — coming soon", TAB_NAMES[app.active_tab]), theme::muted())));
            f.render_widget(placeholder, content_area);
        }
    }

    // Status bar
    let status = Paragraph::new(Line::from(vec![
        Span::styled(" Tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":switch  "),
        Span::styled("1-7", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":jump  "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":quit  "),
        Span::styled("click", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":select tab  "),
        Span::styled(&app.leader_url, theme::muted()),
    ]));
    f.render_widget(status, chunks[2]);
}

/// Run the TUI event loop.
pub async fn run(leader_url: &str) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(leader_url);

    // Initial health fetch
    app.refresh_health().await;

    let mut last_refresh = std::time::Instant::now();

    loop {
        terminal.draw(|f| ui(f, &app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    app.handle_key(key.code, key.modifiers);
                }
                Event::Mouse(mouse) => {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        app.handle_mouse(mouse.column, mouse.row);
                    }
                }
                _ => {}
            }
        }

        // Periodic health refresh (every 5s)
        if last_refresh.elapsed() > Duration::from_secs(5) {
            app.refresh_health().await;
            last_refresh = std::time::Instant::now();
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    Ok(())
}
