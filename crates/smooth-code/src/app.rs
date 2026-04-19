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
    run_with_session(working_dir, None).await
}

/// Run the TUI, optionally preloading a persisted session.
///
/// When `resume` is `Some`, the app starts with that session's
/// messages, title, id, and model instead of a fresh one — used by
/// `th code --resume`.
///
/// # Errors
/// Same as [`run`].
#[allow(clippy::unused_async)]
pub async fn run_with_session(working_dir: PathBuf, resume: Option<crate::session::Session>) -> anyhow::Result<()> {
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

    let initial_state = match resume {
        Some(ref session) => {
            tui_debug(format!(
                "resuming session id={} title={:?} messages={}",
                session.id,
                session.title,
                session.messages.len()
            ));
            AppState::from_resumed_session(working_dir, session)
        }
        None => AppState::new(working_dir),
    };

    let state = Arc::new(Mutex::new(initial_state));

    // Load pearls for the `@` picker in a background thread so the
    // TUI can paint immediately. Pearls only matter when the user
    // types `@`; a slight delay before they show up is fine.
    // Best-effort — a missing or empty pearl store just means no
    // pearls show up in the popup, and the workspace-file path keeps
    // working.
    {
        let state_bg = Arc::clone(&state);
        std::thread::spawn(move || {
            let pearls = load_pearls_for_autocomplete();
            if let Ok(mut s) = state_bg.lock() {
                let n = pearls.len();
                s.pearls = pearls;
                tui_debug(format!("pearls loaded for @ picker: {n}"));
            }
        });
    }
    let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Add welcome message only for fresh sessions — a resumed one
    // already has a real message history.
    if resume.is_none() {
        let mut s = state.lock().expect("state lock poisoned");
        s.add_message(ChatMessage::system("Welcome to Smooth. Type a message and press Enter to chat."));
    } else {
        let title_display = resume
            .as_ref()
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| resume.as_ref().map(|s| s.id.clone()).unwrap_or_default());
        let mut s = state.lock().expect("state lock poisoned");
        s.add_message(ChatMessage::system(format!("Resumed session: {title_display}")));
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
        // Draw. We do NOT wrap this in CSI 2026 synchronized output —
        // on terminals that half-support it (or where `print!`
        // doesn't flush between the begin/end), frames get stuck in
        // the terminal's buffer until process exit, which shows up as
        // "typing goes into the input but nothing renders until
        // ^C". ratatui's double-buffered backend already produces
        // flicker-free output via crossterm's diff rendering.
        {
            let mut s = state.lock().expect("state lock poisoned");
            // Advance spinner each frame for animation
            s.advance_spinner();
            terminal.draw(|f| render::render(f, &s))?;
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
        AgentEvent::Completed { cost_usd, .. } => {
            state.total_cost_usd += cost_usd;
            // Workflow has wrapped up — clear the phase indicator so
            // the status bar doesn't keep showing "FINALIZE" while
            // the agent is idle.
            state.current_phase = None;
            state.current_phase_alias = None;
            state.current_phase_upstream = None;
            state.finish_streaming();
        }
        AgentEvent::PhaseStart { phase, alias, upstream, .. } => {
            state.current_phase = Some(phase);
            state.current_phase_alias = Some(alias);
            state.current_phase_upstream = upstream;
            // Reset phrase so the new phase shows its first word, not
            // whatever index we were on for the prior phase.
            state.phrase_idx = 0;
        }
        AgentEvent::StreamingComplete | AgentEvent::MaxIterationsReached { .. } => {
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

/// Refresh the autocomplete query from the current input buffer —
/// the text between `trigger_pos + 1` and the cursor — and re-run
/// the filter against the appropriate candidate source.
fn refresh_autocomplete(state: &mut AppState, command_registry: &CommandRegistry) {
    if !state.autocomplete.active {
        return;
    }
    let start = state.autocomplete.trigger_pos.saturating_add(1);
    let end = state.input_cursor.max(start);
    if end > state.input.len() || start > state.input.len() {
        state.autocomplete.deactivate();
        return;
    }
    let query = state.input[start..end].to_string();
    let workspace_root = state.working_dir.clone();
    match state.autocomplete.kind {
        crate::autocomplete::CompletionKind::File => {
            let files: Vec<_> = state.file_tree.as_ref().map(|t| t.entries.clone()).unwrap_or_default();
            let pearls = state.pearls.clone();
            state.autocomplete.update_at_query(&query, &files, &pearls, &workspace_root);
        }
        crate::autocomplete::CompletionKind::Command => {
            state.autocomplete.update_command_query(&query, &command_registry.list_commands());
        }
    }
    // Empty results → silently close the popup. Matters for the
    // "slash can be typed mid-message" behaviour: typing "/" pops
    // the command picker for discoverability; once the user types
    // something the registry can't match (e.g. "/tmp/foo"), the
    // popup vanishes without stealing their keystrokes.
    if state.autocomplete.results.is_empty() {
        state.autocomplete.deactivate();
    }
}

/// Accept the currently selected autocomplete result: replace
/// `input[trigger_pos..cursor]` with the suggestion's insert text
/// and close the popup.
fn accept_autocomplete(state: &mut AppState) {
    let Some(result) = state.autocomplete.selected_result().cloned() else {
        state.autocomplete.deactivate();
        return;
    };
    let start = state.autocomplete.trigger_pos;
    let end = state.input_cursor.min(state.input.len());
    if start > end {
        state.autocomplete.deactivate();
        return;
    }
    state.input.replace_range(start..end, &result.insert_text);
    state.input_cursor = start + result.insert_text.len();
    state.autocomplete.deactivate();
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
    // Model picker owns the keyboard while it's visible. Up/Down
    // navigates, Enter drills in or applies, Esc backs out (Models →
    // Slots → closed).
    if state.model_picker.active {
        match key.code {
            KeyCode::Up => state.model_picker.select_up(),
            KeyCode::Down => state.model_picker.select_down(),
            KeyCode::Enter => match state.model_picker.view {
                crate::model_picker::PickerView::Slots => state.model_picker.open_models_for_selected(),
                crate::model_picker::PickerView::Models { .. } => {
                    // apply_selected_model returns to Slots on success;
                    // on failure it leaves the error stashed and keeps
                    // the user in Models view so they can retry.
                    let _ = state.model_picker.apply_selected_model();
                    // When the user applied the Default slot, keep the
                    // displayed model name in the status bar consistent.
                    if let Some(def_entry) = state
                        .model_picker
                        .slots
                        .iter()
                        .find(|e| matches!(e.slot, crate::model_picker::PickerSlot::Default))
                    {
                        state.model_name = def_entry.current_model.clone();
                    }
                }
            },
            KeyCode::Esc => match state.model_picker.view {
                crate::model_picker::PickerView::Slots => state.model_picker.deactivate(),
                crate::model_picker::PickerView::Models { .. } => state.model_picker.back_to_slots(),
            },
            _ => {}
        }
        return;
    }

    // Autocomplete-first key handling. When the popup is active it
    // owns the up/down arrows, Tab, and Enter so the user can pick a
    // suggestion without triggering the usual line semantics.
    if state.autocomplete.active {
        match key.code {
            KeyCode::Up => {
                state.autocomplete.select_up();
                return;
            }
            KeyCode::Down => {
                state.autocomplete.select_down();
                return;
            }
            KeyCode::Tab | KeyCode::Enter => {
                accept_autocomplete(state);
                return;
            }
            KeyCode::Esc => {
                state.autocomplete.deactivate();
                return;
            }
            KeyCode::Char(c) if c.is_whitespace() => {
                // Space/tab ends the active query cleanly; fall
                // through so the whitespace still gets inserted.
                state.autocomplete.deactivate();
                state.input_insert(c);
                return;
            }
            KeyCode::Char(c) => {
                state.input_insert(c);
                refresh_autocomplete(state, command_registry);
                return;
            }
            KeyCode::Backspace => {
                state.input_backspace();
                if state.input_cursor <= state.autocomplete.trigger_pos {
                    state.autocomplete.deactivate();
                } else {
                    refresh_autocomplete(state, command_registry);
                }
                return;
            }
            _ => {}
        }
    }

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
                    // If this is the user's first message and the session
                    // doesn't have a title yet, kick off an async auto-name
                    // via the smooth-fast slot. Detached task — the chat
                    // response isn't gated on it; title lands whenever the
                    // completion comes back and we save-on-next-tick.
                    let is_first_user_message = state.session_title.is_none() && !state.messages.iter().any(|m| matches!(m.role, ChatRole::User));

                    state.add_message(ChatMessage::user(&input));
                    state.thinking = true;

                    if is_first_user_message {
                        let naming_prompt = input.clone();
                        let state_for_naming = Arc::clone(&state_arc);
                        tokio::spawn(async move {
                            if let Some(title) = auto_name_session(&naming_prompt).await {
                                let mut s = state_for_naming.lock().expect("state lock poisoned");
                                s.session_title = Some(title);
                            }
                        });
                    }

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
        KeyCode::Char(c) => {
            // Trigger autocomplete on `@` or `/` anywhere in the
            // input. Pre-cursor trigger (Claude-Code-style) so users
            // can reference commands mid-message for discoverability
            // ("use /help to see options"). refresh_autocomplete
            // silently closes the popup if the subsequent text
            // doesn't match any candidate, so a literal `/path/to`
            // or a stray slash doesn't hijack the keystrokes.
            let trigger_pos = state.input_cursor;
            state.input_insert(c);
            match c {
                '@' => {
                    state.autocomplete.activate_files(trigger_pos);
                    refresh_autocomplete(state, command_registry);
                }
                '/' => {
                    state.autocomplete.activate_commands(trigger_pos);
                    refresh_autocomplete(state, command_registry);
                }
                _ => {}
            }
        }
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

/// Best-effort load of open + in-progress pearls for the `@`
/// picker. Tries `<cwd>/.smooth/dolt/` first (project-scoped) and
/// falls back to `~/.smooth/dolt/` (global). Returns an empty vec
/// on any failure — the picker treats "no pearls" as "just show
/// files and paths."
fn load_pearls_for_autocomplete() -> Vec<crate::autocomplete::PearlSuggestion> {
    use smooth_pearls::{PearlQuery, PearlStore};

    let candidates = [
        std::env::current_dir().ok().map(|d| d.join(".smooth/dolt")),
        dirs_next::home_dir().map(|h| h.join(".smooth/dolt")),
    ];
    for dir in candidates.into_iter().flatten() {
        if !dir.exists() {
            continue;
        }
        let Ok(store) = PearlStore::open(&dir) else { continue };
        let Ok(pearls) = store.list(&PearlQuery::new()) else { continue };
        return pearls
            .into_iter()
            .filter(|p| !matches!(p.status, smooth_pearls::PearlStatus::Closed))
            .take(100)
            .map(|p| crate::autocomplete::PearlSuggestion { id: p.id, title: p.title })
            .collect();
    }
    Vec::new()
}

/// Generate a 3–6 word Title Case summary of the user's first
/// message via the `smooth-fast` routing slot (Haiku-class). Returns
/// `None` when the slot isn't configured or the LLM call fails.
///
/// Mirrors the session-titling pattern in `smooth-bigsmooth`
/// (`server.rs::auto_name_session`) so the same prompt + trimming
/// rules produce consistent titles across the web chat and the
/// `th` TUI.
async fn auto_name_session(user_prompt: &str) -> Option<String> {
    use smooth_operator::providers::{Activity, ProviderRegistry};

    let providers_path = dirs_next::home_dir()?.join(".smooth/providers.json");
    let registry = ProviderRegistry::load_from_file(&providers_path).ok()?;
    let config = registry.llm_config_for(Activity::Fast).ok()?;
    let llm = smooth_operator::llm::LlmClient::new(config);

    let system = smooth_operator::conversation::Message::system(
        "You name chat sessions. Return ONLY a 3-to-6 word Title Case \
         summary of the user's first message. No quotes, no trailing \
         punctuation, no preamble. Example: \"help me refactor my auth \
         middleware to use JWT\" → Refactor Auth Middleware To JWT.",
    );
    let user = smooth_operator::conversation::Message::user(user_prompt);
    let resp = llm.chat(&[&system, &user], &[]).await.ok()?;

    let cleaned = resp
        .content
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c == '\n')
        .chars()
        .take(60)
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
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
            ServerEvent::TaskComplete { iterations, cost_usd, .. } => {
                let _ = tx.send(AgentEvent::Completed {
                    agent_id: "task".into(),
                    iterations,
                    cost_usd,
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
