//! TUI app — state, event loop, tab navigation, mouse support.

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::theme;
use crate::views::{beads, chat, dashboard, messages, operators, reviews, system};

/// Available tabs.
const TAB_NAMES: &[&str] = &["Dashboard", "Beads", "Operators", "Chat", "Messages", "Reviews", "System"];

/// Application state.
pub struct App {
    pub active_tab: usize,
    pub leader_url: String,
    pub health: dashboard::HealthData,
    pub chat_state: chat::ChatState,
    pub beads_state: beads::BeadsState,
    pub operators_state: operators::OperatorsState,
    pub messages_state: messages::MessagesState,
    pub reviews_state: reviews::ReviewsState,
    pub should_quit: bool,
    /// Connection error message (empty = connected).
    pub connection_error: String,
    /// Cached tab positions for mouse hit-testing: (start_col, end_col) for each tab.
    tab_regions: Vec<(u16, u16)>,
    /// Channel for sending chat requests to background tasks.
    chat_tx: Option<mpsc::Sender<String>>,
    /// Channel for receiving chat responses from background tasks.
    chat_rx: Option<mpsc::Receiver<String>>,
    /// HTTP client with timeout.
    http: reqwest::Client,
}

impl App {
    pub fn new(leader_url: &str) -> Self {
        let (tx, rx) = mpsc::channel(16);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            active_tab: 0,
            leader_url: leader_url.to_string(),
            health: dashboard::HealthData::default(),
            chat_state: chat::ChatState::default(),
            beads_state: beads::BeadsState::default(),
            operators_state: operators::OperatorsState::default(),
            messages_state: messages::MessagesState::default(),
            reviews_state: reviews::ReviewsState::default(),
            should_quit: false,
            connection_error: String::new(),
            tab_regions: Vec::new(),
            chat_tx: Some(tx),
            chat_rx: Some(rx),
            http,
        }
    }

    /// Fetch health data from leader.
    pub async fn refresh_health(&mut self) {
        let url = format!("{}/api/system/health", self.leader_url);
        match self.http.get(&url).send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(data) = json.get("data") {
                        self.connection_error.clear();
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
            Err(e) => {
                let msg = if e.is_connect() {
                    format!("Cannot connect to leader at {}", self.leader_url)
                } else if e.is_timeout() {
                    format!("Connection timed out to {}", self.leader_url)
                } else {
                    format!("Error: {e}")
                };
                self.connection_error = msg;
                self.health = dashboard::HealthData::default();
            }
        }
    }

    /// Fetch beads from leader.
    pub async fn refresh_beads(&mut self) {
        let url = format!("{}/api/beads", self.leader_url);
        if let Ok(resp) = self.http.get(&url).send().await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
                    self.beads_state.beads = data
                        .iter()
                        .map(|b| beads::Bead {
                            id: b.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                            title: b.get("title").and_then(|v| v.as_str()).unwrap_or("").into(),
                            status: b.get("status").and_then(|v| v.as_str()).unwrap_or("").into(),
                            priority: b.get("priority").and_then(|v| v.as_str()).unwrap_or("").into(),
                        })
                        .collect();
                }
            }
        }
        self.beads_state.loading = false;
    }

    /// Fetch autocomplete results from leader.
    pub async fn refresh_autocomplete(&mut self) {
        let ac = &self.chat_state.autocomplete;
        if !ac.active || ac.query.is_empty() {
            return;
        }
        let query = ac.query.clone();
        let url = format!("{}/api/search?q={}", self.leader_url, urlencoding::encode(&query));
        if let Ok(resp) = reqwest::get(&url).await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
                    self.chat_state.autocomplete.results = data
                        .iter()
                        .map(|r| chat::SearchResult {
                            result_type: r.get("type").and_then(|v| v.as_str()).unwrap_or("").into(),
                            id: r.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                            label: r.get("label").and_then(|v| v.as_str()).unwrap_or("").into(),
                            detail: r.get("detail").and_then(|v| v.as_str()).map(String::from),
                        })
                        .collect();
                    // Clamp selected index
                    let len = self.chat_state.autocomplete.results.len();
                    if self.chat_state.autocomplete.selected >= len {
                        self.chat_state.autocomplete.selected = len.saturating_sub(1);
                    }
                }
            }
        }
    }

    /// Check if input has an active @ query and update autocomplete state.
    fn update_autocomplete_state(&mut self) {
        // Find the last @ in the input that isn't followed by a space
        if let Some(at_pos) = self.chat_state.input.rfind('@') {
            let after_at = &self.chat_state.input[at_pos + 1..];
            // Active if there's no space after @ (still typing the query)
            if !after_at.contains(' ') {
                self.chat_state.autocomplete.active = true;
                self.chat_state.autocomplete.query = after_at.to_string();
                self.chat_state.autocomplete.at_pos = Some(at_pos);
                return;
            }
        }
        self.chat_state.autocomplete.active = false;
        self.chat_state.autocomplete.results.clear();
        self.chat_state.autocomplete.selected = 0;
        self.chat_state.autocomplete.at_pos = None;
    }

    /// Accept the currently selected autocomplete result.
    fn accept_autocomplete(&mut self) {
        let ac = &self.chat_state.autocomplete;
        if let Some(result) = ac.results.get(ac.selected) {
            let replacement = match result.result_type.as_str() {
                "bead" => format!("@{} ", result.id),
                _ => format!("@{} ", result.id),
            };
            if let Some(at_pos) = ac.at_pos {
                self.chat_state.input.truncate(at_pos);
                self.chat_state.input.push_str(&replacement);
            }
        }
        self.chat_state.autocomplete.active = false;
        self.chat_state.autocomplete.results.clear();
        self.chat_state.autocomplete.selected = 0;
        self.chat_state.autocomplete.at_pos = None;
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Handle autocomplete navigation first
        if self.active_tab == 3 && self.chat_state.autocomplete.active && !self.chat_state.autocomplete.results.is_empty() {
            match code {
                KeyCode::Up => {
                    let len = self.chat_state.autocomplete.results.len();
                    self.chat_state.autocomplete.selected = (self.chat_state.autocomplete.selected + len - 1) % len;
                    return;
                }
                KeyCode::Down => {
                    let len = self.chat_state.autocomplete.results.len();
                    self.chat_state.autocomplete.selected = (self.chat_state.autocomplete.selected + 1) % len;
                    return;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.accept_autocomplete();
                    return;
                }
                KeyCode::Esc => {
                    self.chat_state.autocomplete.active = false;
                    self.chat_state.autocomplete.results.clear();
                    return;
                }
                _ => {} // Fall through to normal input handling
            }
        }

        match code {
            KeyCode::Char('q') if self.active_tab != 3 => self.should_quit = true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.should_quit = true,
            KeyCode::Esc if self.active_tab == 3 => {
                // Esc in chat without autocomplete — switch away
            }
            KeyCode::Tab if self.active_tab != 3 || self.chat_state.input.is_empty() => {
                self.active_tab = (self.active_tab + 1) % TAB_NAMES.len();
            }
            KeyCode::BackTab => {
                self.active_tab = (self.active_tab + TAB_NAMES.len() - 1) % TAB_NAMES.len();
            }
            KeyCode::Char(c) if ('1'..='7').contains(&c) && self.active_tab != 3 => {
                let idx = (c as usize) - ('1' as usize);
                if idx < TAB_NAMES.len() {
                    self.active_tab = idx;
                }
            }
            // Chat input
            KeyCode::Char(c) if self.active_tab == 3 => {
                self.chat_state.input.push(c);
                self.update_autocomplete_state();
            }
            KeyCode::Backspace if self.active_tab == 3 => {
                self.chat_state.input.pop();
                self.update_autocomplete_state();
            }
            KeyCode::Enter if self.active_tab == 3 && !self.chat_state.input.is_empty() && !self.chat_state.streaming => {
                let content = std::mem::take(&mut self.chat_state.input);
                self.chat_state.messages.push(chat::ChatMessage { role: "user".into(), content: content.clone() });
                self.chat_state.streaming = true;
                self.chat_state.scroll_offset = 0;
                self.chat_state.autocomplete = chat::AutocompleteState::default();

                // Send to leader API in background
                if let Some(tx) = &self.chat_tx {
                    let url = format!("{}/api/chat", self.leader_url);
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let client = reqwest::Client::new();
                        let result = client
                            .post(&url)
                            .json(&serde_json::json!({"content": content}))
                            .send()
                            .await;
                        let response = match result {
                            Ok(resp) => match resp.json::<serde_json::Value>().await {
                                Ok(json) => json.get("data").and_then(|d| d.as_str()).unwrap_or("No response").to_string(),
                                Err(e) => format!("Error: {e}"),
                            },
                            Err(e) => format!("Error: {e}"),
                        };
                        let _ = tx.send(response).await;
                    });
                }
            }
            // Chat scroll
            KeyCode::PageUp if self.active_tab == 3 => {
                self.chat_state.scroll_offset = self.chat_state.scroll_offset.saturating_add(10);
            }
            KeyCode::PageDown if self.active_tab == 3 => {
                self.chat_state.scroll_offset = self.chat_state.scroll_offset.saturating_sub(10);
            }
            KeyCode::Up if self.active_tab == 3 => {
                self.chat_state.scroll_offset = self.chat_state.scroll_offset.saturating_add(1);
            }
            KeyCode::Down if self.active_tab == 3 => {
                self.chat_state.scroll_offset = self.chat_state.scroll_offset.saturating_sub(1);
            }
            KeyCode::End if self.active_tab == 3 => {
                self.chat_state.scroll_offset = 0;
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, col: u16, row: u16) {
        // Tab bar is rows 0-2 (3-line block with borders). Row 1 has the tab text.
        if row == 1 {
            for (i, &(start, end)) in self.tab_regions.iter().enumerate() {
                if col >= start && col < end {
                    self.active_tab = i;
                    return;
                }
            }
        }
    }

    fn handle_scroll(&mut self, up: bool) {
        if self.active_tab == 3 {
            if up {
                self.chat_state.scroll_offset = self.chat_state.scroll_offset.saturating_add(3);
            } else {
                self.chat_state.scroll_offset = self.chat_state.scroll_offset.saturating_sub(3);
            }
        }
    }
}

/// Build gradient "Smoo" title spans — orange to green.
fn gradient_title() -> Vec<Span<'static>> {
    // S -> m -> o -> o with color interpolation from orange (#f49f0a) to green (#00a6a6)
    let colors = [
        Color::Rgb(244, 159, 10),  // S — orange
        Color::Rgb(183, 137, 40),  // m
        Color::Rgb(122, 150, 70),  // o
        Color::Rgb(61, 168, 100),  // o
        Color::Rgb(0, 166, 166),   // . — green
        Color::Rgb(0, 166, 166),   // A — green
        Color::Rgb(0, 166, 166),   // I — green
    ];
    let text = ['S', 'm', 'o', 'o', '.', 'A', 'I'];

    let mut spans = vec![Span::raw(" ")];
    for (ch, color) in text.iter().zip(colors.iter()) {
        spans.push(Span::styled(ch.to_string(), Style::default().fg(*color).add_modifier(Modifier::BOLD)));
    }
    spans.push(Span::styled(" / ", theme::muted()));
    spans.push(Span::styled("SMOOTH ", Style::default().fg(theme::SMOO_GREEN).add_modifier(Modifier::BOLD)));
    spans
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
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

    // Calculate tab regions for mouse hit-testing.
    // ratatui Tabs widget renders: "│ Tab1 │ Tab2 │ ..." inside the border.
    // The border takes 1 col on each side, and each tab is separated by " │ ".
    let mut regions = Vec::new();
    let mut x: u16 = 2; // start after left border + initial padding
    for name in TAB_NAMES {
        let tab_width = name.len() as u16 + 2; // " Name "
        regions.push((x, x + tab_width));
        x += tab_width + 3; // " │ " separator
    }
    app.tab_regions = regions;

    let tabs = Tabs::new(titles)
        .select(app.active_tab)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::SMOO_GREEN))
                .title(Line::from(gradient_title())),
        )
        .highlight_style(theme::active_tab())
        .divider("│");
    f.render_widget(tabs, chunks[0]);

    // Connection error banner
    let content_area = if !app.connection_error.is_empty() {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(chunks[1]);

        let error_msg = Paragraph::new(Line::from(vec![
            Span::styled(" ○ ", Style::default().fg(Color::Red)),
            Span::styled(&app.connection_error, Style::default().fg(Color::Red)),
            Span::styled("  — retrying every 5s", theme::muted()),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(Span::styled(" Disconnected ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))),
        );
        f.render_widget(error_msg, split[0]);
        split[1]
    } else {
        chunks[1]
    };

    // Content
    match app.active_tab {
        0 => dashboard::render(f, content_area, &app.health),
        1 => beads::render(f, content_area, &app.beads_state),
        2 => operators::render(f, content_area, &app.operators_state),
        3 => chat::render(f, content_area, &app.chat_state),
        4 => messages::render(f, content_area, &app.messages_state),
        5 => reviews::render(f, content_area, &app.reviews_state),
        6 => system::render(f, content_area, &app.health, &app.leader_url),
        _ => {}
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

    // Initial data fetch
    app.refresh_health().await;
    app.refresh_beads().await;

    let mut last_refresh = std::time::Instant::now();
    let mut last_ac_query = String::new();

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    app.handle_key(key.code, key.modifiers);
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        app.handle_mouse(mouse.column, mouse.row);
                    }
                    MouseEventKind::ScrollUp => app.handle_scroll(true),
                    MouseEventKind::ScrollDown => app.handle_scroll(false),
                    _ => {}
                },
                _ => {}
            }
        }

        // Fetch autocomplete results when query changes
        if app.chat_state.autocomplete.active && app.chat_state.autocomplete.query != last_ac_query {
            last_ac_query.clone_from(&app.chat_state.autocomplete.query);
            app.refresh_autocomplete().await;
        } else if !app.chat_state.autocomplete.active {
            last_ac_query.clear();
        }

        // Check for chat responses
        if let Some(rx) = &mut app.chat_rx {
            if let Ok(response) = rx.try_recv() {
                app.chat_state.streaming = false;
                app.chat_state.scroll_offset = 0; // auto-scroll to bottom
                app.chat_state.messages.push(chat::ChatMessage {
                    role: "assistant".into(),
                    content: response,
                });
            }
        }

        // Periodic refresh (every 5s)
        if last_refresh.elapsed() > Duration::from_secs(5) {
            app.refresh_health().await;
            app.refresh_beads().await;
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
