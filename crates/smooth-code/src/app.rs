//! Main event loop for the Smooth TUI.

use std::fmt::Write as _;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use smooth_operator::AgentEvent;
use tokio::sync::mpsc;

use crate::commands::{parse_input, CommandOutput, CommandRegistry, InputKind};
use crate::diff_render::RenderCache;
use crate::render;
use crate::session::{Session, SessionManager};
use crate::state::{AppState, ChatMessage, ChatRole, HealthStatus, Mode};

/// Log a diagnostic line to `~/.smooth/logs/smooth-code.log` when
/// `SMOOTH_TUI_DEBUG=1` is set. Used to diagnose the
/// "nothing renders in my terminal" class of bug — the user can flip
/// the env var, re-run `th`, and then tail the log to see exactly
/// where `run()` gave up.
///
/// Always a no-op when the env var isn't set, so the hot path is
/// untouched.
fn tui_debug(msg: impl AsRef<str>) {
    if std::env::var("SMOOTH_TUI_DEBUG").ok().as_deref() != Some("1") {
        return;
    }
    let Some(home) = dirs_next::home_dir() else { return };
    let log_dir = home.join(".smooth").join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("smooth-code.log");
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) else {
        return;
    };
    let ts = chrono::Utc::now().to_rfc3339();
    let _ = writeln!(f, "[{ts}] {}", msg.as_ref());
}

/// Run the Smooth TUI.
///
/// This is the main entry point — it sets up the terminal, runs the event loop,
/// and restores the terminal on exit.
///
/// # Errors
/// Returns an error if terminal setup, rendering, or event handling fails.
///
/// # Panics
/// Panics if the internal state mutex is poisoned (indicates a prior panic in a
/// thread holding the lock).
#[allow(clippy::unused_async)] // async required for caller ergonomics and tokio::spawn inside
pub async fn run(working_dir: PathBuf) -> anyhow::Result<()> {
    tui_debug(format!("app::run start, cwd={}", working_dir.display()));

    // TTY pre-flight. If stdin or stdout isn't a TTY, the TUI will enter
    // alt-screen but render to /dev/null — the user sees nothing and the
    // only clue is the terminal returning to the shell a moment later.
    // Print a clear error up front so pipe/redirect mistakes don't look
    // like a UI bug.
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!(
            "smooth-code requires an interactive terminal (stdin + stdout must be a TTY). \
             If you piped or redirected, run `th` with a direct terminal instead, \
             or use `th code --headless \"your message\"` for scripted runs."
        );
    }
    tui_debug(format!(
        "TTY check passed (TERM={}, TERM_PROGRAM={})",
        std::env::var("TERM").unwrap_or_default(),
        std::env::var("TERM_PROGRAM").unwrap_or_default()
    ));

    // Escape hatch for terminals that don't cleanly handle
    // alternate-screen + synchronized-output + mouse-capture together
    // (some tmux configs, some Windows terminals, certain ssh multiplexes).
    // `SMOOTH_TUI_NO_ALT_SCREEN=1` drops the alt-screen switch and the
    // mouse-capture mode so the UI renders inline in the primary buffer.
    // Scrollback gets mixed in with the TUI output but at least the
    // user can *see* the UI.
    let no_alt_screen = matches!(std::env::var("SMOOTH_TUI_NO_ALT_SCREEN").ok().as_deref(), Some("1"));
    tui_debug(format!("no_alt_screen={no_alt_screen}"));

    // Setup terminal
    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode failed ({e}); terminal may not support raw mode"))?;
    tui_debug("enable_raw_mode OK");

    let mut stdout = io::stdout();
    if !no_alt_screen {
        execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture)
            .map_err(|e| anyhow::anyhow!("EnterAlternateScreen failed ({e}); try SMOOTH_TUI_NO_ALT_SCREEN=1"))?;
        tui_debug("EnterAlternateScreen + EnableMouseCapture OK");
    } else {
        tui_debug("skipped alt-screen + mouse capture (SMOOTH_TUI_NO_ALT_SCREEN=1)");
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| {
        // Best-effort restore if Terminal::new fails after alt-screen entered.
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, crossterm::event::DisableMouseCapture);
        anyhow::anyhow!("Terminal::new failed: {e}")
    })?;
    tui_debug(format!("Terminal::new OK, size={:?}", terminal.size().ok()));

    let state = Arc::new(Mutex::new(AppState::new(working_dir)));
    let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Add welcome message
    {
        let mut s = state.lock().expect("state lock poisoned");
        s.add_message(ChatMessage::system("Welcome to Smooth. Type a message and press Enter to chat."));
    }

    // Run startup health checks asynchronously — TUI renders immediately
    {
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            let (health_status, warnings) = run_startup_health_checks().await;
            let mut s = state_clone.lock().expect("state lock poisoned");
            s.health_status = health_status;
            if !warnings.is_empty() {
                let warning_text = format!(
                    "\u{26a0} Health Check:\n{}",
                    warnings.iter().map(|w| format!("  \u{2022} {w}")).collect::<Vec<_>>().join("\n")
                );
                s.add_message(ChatMessage::new(ChatRole::System, warning_text));
            }
        });
    }

    // Initial forced draw before the event loop starts. If the loop later
    // blocks or errors, we've at least rendered the welcome message once
    // so the user sees the UI is alive.
    {
        let s = state.lock().expect("state lock poisoned");
        if let Err(e) = terminal.draw(|f| render::render(f, &s)) {
            tui_debug(format!("initial terminal.draw failed: {e}"));
        } else {
            tui_debug("initial terminal.draw OK");
        }
    }

    tui_debug("entering event_loop");
    let result = event_loop(&mut terminal, &state, &event_tx, event_rx);
    tui_debug(format!("event_loop returned: {result:?}"));

    // Auto-save on quit
    {
        let s = state.lock().expect("state lock poisoned");
        if let Ok(mgr) = SessionManager::new() {
            let session = Session::from_state(&s);
            let _ = mgr.save(&session);
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    if !no_alt_screen {
        execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture)?;
    }
    terminal.show_cursor()?;
    tui_debug("terminal restored, app::run exit");

    result
}

/// The main event loop — draws the UI and handles input events.
///
/// Processes both terminal key events and agent streaming events via the channel.
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &Arc<Mutex<AppState>>,
    event_tx: &mpsc::UnboundedSender<AgentEvent>,
    mut event_rx: mpsc::UnboundedReceiver<AgentEvent>,
) -> anyhow::Result<()> {
    let command_registry = CommandRegistry::new();
    let mut last_save = std::time::Instant::now();
    let auto_save_interval = Duration::from_secs(30);

    loop {
        // Auto-save every 30s if there are messages
        if last_save.elapsed() >= auto_save_interval {
            let s = state.lock().expect("state lock poisoned");
            if !s.messages.is_empty() {
                if let Ok(mgr) = SessionManager::new() {
                    let session = Session::from_state(&s);
                    let _ = mgr.save(&session);
                }
            }
            drop(s);
            last_save = std::time::Instant::now();
        }
        // Draw with synchronized output to eliminate flicker
        {
            let mut s = state.lock().expect("state lock poisoned");
            // Advance spinner each frame for animation
            s.advance_spinner();
            print!("{}", RenderCache::begin_sync());
            terminal.draw(|f| render::render(f, &s))?;
            print!("{}", RenderCache::end_sync());
        }

        // Drain all pending agent events without blocking
        while let Ok(agent_event) = event_rx.try_recv() {
            let mut s = state.lock().expect("state lock poisoned");
            handle_agent_event(&mut s, agent_event);
        }

        // Poll for terminal events with 50ms timeout for responsive streaming UI
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                let mut s = state.lock().expect("state lock poisoned");

                // Global keybindings
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('c') => {
                            s.should_quit = true;
                        }
                        KeyCode::Char('b') => {
                            s.sidebar_visible = !s.sidebar_visible;
                            continue;
                        }
                        _ => {}
                    }
                }

                if s.should_quit {
                    break;
                }

                match s.mode {
                    Mode::Input => handle_input_mode(key, &mut s, Arc::clone(state), event_tx.clone(), &command_registry),
                    Mode::Normal => handle_normal_mode(key, &mut s),
                }
            }
        }

        // Check if we should quit after event handling
        let s = state.lock().expect("state lock poisoned");
        if s.should_quit {
            break;
        }
    }

    Ok(())
}

/// Map an `AgentEvent` to the appropriate state mutation.
fn handle_agent_event(state: &mut AppState, event: AgentEvent) {
    match event {
        AgentEvent::Started { .. } => {
            state.start_streaming();
        }
        AgentEvent::TokenDelta { content } => {
            state.append_stream_content(&content);
        }
        AgentEvent::Completed { .. } | AgentEvent::StreamingComplete | AgentEvent::MaxIterationsReached { .. } => {
            state.finish_streaming();
        }
        AgentEvent::Error { message } => {
            state.finish_streaming();
            state.add_message(ChatMessage::system(format!("Error: {message}")));
        }
        // Other events (LlmRequest, LlmResponse, ToolCallStart, ToolCallComplete, CheckpointSaved)
        // are informational — no state change needed for now.
        _ => {}
    }
}

/// Handle key events in input mode.
#[allow(clippy::needless_pass_by_value)] // Arc is cloned into async tasks
fn handle_input_mode(
    key: event::KeyEvent,
    state: &mut AppState,
    state_arc: Arc<Mutex<AppState>>,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    command_registry: &CommandRegistry,
) {
    match key.code {
        KeyCode::Enter => {
            let input = state.take_input();
            if input.trim().is_empty() {
                return;
            }

            match parse_input(&input) {
                InputKind::SlashCommand { name, args } => {
                    match command_registry.execute(name, args, state) {
                        Some(Ok(CommandOutput::Message(msg))) => {
                            state.add_message(ChatMessage::system(msg));
                        }
                        Some(Ok(CommandOutput::Clear | CommandOutput::Quit | CommandOutput::None)) => {
                            // Clear: already handled by handler
                            // Quit: should_quit already set by handler
                            // None: no visible output
                        }
                        Some(Err(e)) => {
                            state.add_message(ChatMessage::system(format!("Command error: {e}")));
                        }
                        None => {
                            state.add_message(ChatMessage::system(format!("Unknown command: /{name}. Type /help for available commands.")));
                        }
                    }
                }
                InputKind::BangCommand(cmd) => {
                    let cmd = cmd.to_string();
                    let state_arc = Arc::clone(&state_arc);
                    tokio::spawn(async move {
                        let output = tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await;
                        match output {
                            Ok(out) => {
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                let stderr = String::from_utf8_lossy(&out.stderr);
                                let mut result = stdout.to_string();
                                if !stderr.is_empty() {
                                    if !result.is_empty() {
                                        result.push('\n');
                                    }
                                    let _ = write!(result, "stderr: {stderr}");
                                }
                                if result.is_empty() {
                                    result = "(no output)".to_string();
                                }
                                let mut s = state_arc.lock().expect("state lock poisoned");
                                s.add_message(ChatMessage::system(format!("$ {cmd}\n{result}")));
                            }
                            Err(e) => {
                                let mut s = state_arc.lock().expect("state lock poisoned");
                                s.add_message(ChatMessage::system(format!("Shell error: {e}")));
                            }
                        }
                    });
                }
                InputKind::Normal(_) => {
                    state.add_message(ChatMessage::user(&input));
                    state.thinking = true;

                    // Spawn agent task with channel-based streaming
                    let message = input;
                    let tx = event_tx;
                    tokio::spawn(async move {
                        if let Err(e) = run_agent_streaming(&message, tx.clone()).await {
                            let _ = tx.send(AgentEvent::Error { message: e.to_string() });
                        }
                    });
                }
            }
        }
        KeyCode::Backspace => state.input_backspace(),
        KeyCode::Left => state.input_move_left(),
        KeyCode::Right => state.input_move_right(),
        KeyCode::Esc => {
            state.mode = Mode::Normal;
        }
        KeyCode::Char(c) => state.input_insert(c),
        _ => {}
    }
}

/// Handle key events in normal mode.
fn handle_normal_mode(key: event::KeyEvent, state: &mut AppState) {
    match key.code {
        KeyCode::Char('i') => {
            state.mode = Mode::Input;
        }
        KeyCode::Char('q') => {
            state.should_quit = true;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if state.scroll_offset > 0 {
                state.scroll_offset -= 1;
            }
            if state.scroll_offset == 0 {
                state.user_scrolled = false;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.scroll_offset += 1;
            state.user_scrolled = true;
        }
        _ => {}
    }
}

/// Run startup health checks and return the status plus any warning messages.
///
/// Checks:
/// 1. Big Smooth API reachability (`http://localhost:4400/health`)
/// 2. LLM providers config (`~/.smooth/providers.json`)
/// 3. Database existence (`~/.smooth/smooth.db`)
async fn run_startup_health_checks() -> (HealthStatus, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();

    // 1. Check Big Smooth API
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().ok();

    if let Some(client) = &client {
        match client.get("http://localhost:4400/health").send().await {
            Ok(r) if r.status().is_success() => {}
            _ => warnings.push("Big Smooth API not running. Starting...".into()),
        }
    }

    // 2. Check providers
    let providers_path = dirs_next::home_dir().map(|h| h.join(".smooth/providers.json"));
    if providers_path.as_ref().is_none_or(|p| !p.exists()) {
        warnings.push("No LLM providers configured. Run: /model to select one, or th auth login <provider>".into());
    }

    // 3. Check database
    let db_path = dirs_next::home_dir().map(|h| h.join(".smooth/smooth.db"));
    if db_path.as_ref().is_none_or(|p| !p.exists()) {
        warnings.push("Database not found. Will be created on first use.".into());
    }

    let status = if warnings.is_empty() {
        HealthStatus::Healthy
    } else {
        HealthStatus::Warnings(warnings.len())
    };

    (status, warnings)
}

/// Send a task to Big Smooth via WebSocket and bridge its `ServerEvent`s
/// to the `AgentEvent` channel the TUI already consumes. All actual tool
/// execution happens inside a hardware-isolated sandbox — smooth-code is
/// just a rendering client.
async fn run_agent_streaming(message: &str, tx: mpsc::UnboundedSender<AgentEvent>) -> anyhow::Result<()> {
    use crate::client::{BigSmoothClient, ServerEvent};

    let url = std::env::var("SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
    let mut client = BigSmoothClient::new(&url);
    client
        .connect()
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to Big Smooth at {url}: {e}. Run: th up"))?;

    let _ = tx.send(AgentEvent::Started { agent_id: "task".into() });

    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());
    let mut events = client.run_task(message, None, None, cwd.as_deref()).await?;

    while let Some(event) = events.recv().await {
        let agent_event = match event {
            ServerEvent::TokenDelta { content, .. } => Some(AgentEvent::TokenDelta { content }),
            ServerEvent::ToolCallStart { tool_name, .. } => Some(AgentEvent::ToolCallStart { iteration: 0, tool_name }),
            ServerEvent::ToolCallComplete { tool_name, is_error, .. } => Some(AgentEvent::ToolCallComplete {
                iteration: 0,
                tool_name,
                is_error,
            }),
            ServerEvent::TaskComplete { iterations, .. } => {
                let _ = tx.send(AgentEvent::Completed {
                    agent_id: "task".into(),
                    iterations,
                });
                break;
            }
            ServerEvent::TaskError { message, .. } => {
                let _ = tx.send(AgentEvent::Error { message });
                break;
            }
            ServerEvent::NarcAlert { severity, message, .. } => {
                let _ = tx.send(AgentEvent::Error {
                    message: format!("[Narc {severity}] {message}"),
                });
                None
            }
            ServerEvent::Error { message } => {
                let _ = tx.send(AgentEvent::Error { message });
                break;
            }
            _ => None,
        };
        if let Some(e) = agent_event {
            if tx.send(e).is_err() {
                break;
            }
        }
    }

    Ok(())
}
