//! Main event loop for the Smooth TUI.

use std::fmt::Write as _;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

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
    run_with_session(working_dir, None, None).await
}

/// Run the TUI, optionally preloading a persisted session.
///
/// When `resume` is `Some`, the app starts with that session's
/// messages, title, id, and model instead of a fresh one — used by
/// `th code --resume`.
///
/// `agent` is the lead role the TUI should dispatch under —
/// `None` means "use the default" (`fixer`). Flowed through to
/// Big Smooth on every `TaskStart` and surfaced on the status bar.
///
/// # Errors
/// Same as [`run`].
#[allow(clippy::unused_async)]
pub async fn run_with_session(working_dir: PathBuf, resume: Option<crate::session::Session>, agent: Option<String>) -> anyhow::Result<()> {
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

    // Inline-viewport mode: the TUI owns only a small region at the
    // bottom of the terminal (input + status + an optional streaming
    // preview). Finalized chat messages flow into the terminal's
    // own scrollback via `Frame::insert_before`, so the user gets
    // native wheel-scroll, drag-select, copy, and search for free.
    // No alt-screen, no mouse capture — both would break those.
    //
    // The legacy `SMOOTH_TUI_NO_ALT_SCREEN` escape hatch is now a
    // no-op (we never enter alt-screen). Kept readable for one
    // release so users with the var in their shell config don't
    // get a surprise error.
    let _ = std::env::var("SMOOTH_TUI_NO_ALT_SCREEN");

    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode failed ({e}); terminal may not support raw mode"))?;
    tui_debug("enable_raw_mode OK");

    // Enable bracketed paste so multi-line pastes arrive as one
    // `Event::Paste(String)` instead of N Char + Enter events. Without
    // this, pasting "line1\nline2" into the input box submits "line1"
    // immediately on the embedded \n (Enter) and then submits each
    // following line as its own message — a flood of TaskStarts that
    // races the renderer and can panic ratatui's inline-viewport
    // buffer at high enough cadence (pearl th-paste-crash). Best-effort:
    // some terminals don't support bracketed paste; the enable call
    // emits ESC sequences they ignore harmlessly.
    let _ = crossterm::execute!(io::stdout(), crossterm::event::EnableBracketedPaste);

    // Pick a viewport height that fits the input/status plus a
    // reasonable streaming-preview region. 14 rows (3 input + 1
    // status + 10 preview) feels right on an 80x24 terminal; if the
    // terminal is shorter we cap at 60% of its height so the
    // viewport never crowds out scrollback entirely.
    let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
    let viewport_h = u16::min(14, term_h.saturating_mul(3) / 5).max(4);
    tui_debug(format!("viewport: Inline({viewport_h}), term_height={term_h}"));

    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);

    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(viewport_h),
        },
    )
    .map_err(|e| {
        let _ = disable_raw_mode();
        anyhow::anyhow!("Terminal::with_options failed: {e}")
    })?;
    tui_debug(format!("Terminal::with_options OK, size={:?}", terminal.size().ok()));

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

    // Agent selection: explicit `--agent` flag from the CLI beats the
    // session's stored agent (user may want to switch roles on resume).
    // Otherwise keep whatever the session already had.
    let mut initial_state = initial_state;
    if let Some(name) = agent {
        initial_state.agent_name = name;
        // Explicit --agent on the CLI is a pin — don't let the intent
        // classifier override the operator's deliberate choice.
        initial_state.agent_pinned = true;
    }

    let state = Arc::new(Mutex::new(initial_state));

    // Spawn the auto-mode SSE subscriber. Long-running tokio task that
    // tails `/api/access/stream` and pushes Pending / Resolved /
    // Expired events into `state.permission_prompts`. Exits when the
    // last Arc<AppState> is dropped. Pearl th-670fb2.
    {
        let state_for_sse = Arc::clone(&state);
        let base = std::env::var("SMOOTH_BIGSMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
        crate::auto_mode::spawn_subscriber(base, state_for_sse);
    }

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

    // Push the gradient SMOOTH wordmark banner into the terminal's
    // scrollback for fresh sessions, before any messages. Lives at
    // the top of the session like a real terminal program's startup
    // banner. Resumed sessions skip it — the user already saw it
    // when they first started that session.
    if resume.is_none() {
        let banner = render::welcome_banner_lines();
        if let Err(e) = crate::inline::insert_before_lines(&mut terminal, banner) {
            tui_debug(format!("welcome banner insert_before failed: {e}"));
        }
    }

    // Add welcome / resume message. For fresh sessions this is just
    // the "type a message" hint; for resumed sessions it announces
    // which session is back.
    if resume.is_none() {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.add_message(ChatMessage::system("Type a message to get started. /help for commands."));
    } else {
        let title_display = resume
            .as_ref()
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| resume.as_ref().map(|s| s.id.clone()).unwrap_or_default());
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.add_message(ChatMessage::system(format!("Resumed session: {title_display}")));
    }

    // Run startup health checks asynchronously — TUI renders immediately
    {
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            let (health_status, warnings) = run_startup_health_checks().await;
            let mut s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
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
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
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
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mgr) = SessionManager::new() {
            let session = Session::from_state(&s);
            let _ = mgr.save(&session);
        }
    }

    // Restore terminal — inline-viewport mode only needs to disable
    // raw mode and ensure the cursor is visible. There's no alt-
    // screen to leave: the viewport sat in the primary buffer the
    // whole time. Also disable bracketed paste so subsequent shell
    // sessions in the same terminal don't inherit the mode.
    let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableBracketedPaste);
    disable_raw_mode()?;
    terminal.show_cursor()?;
    // Move the cursor below the viewport so the user's next shell
    // prompt doesn't land on top of the (now-final) input row.
    println!();
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
            let s = state.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            // Push every finalized message into the terminal's
            // scrollback BEFORE drawing the viewport. This way the
            // viewport only ever paints the in-flight streaming
            // message + input + status — finalized turns become
            // regular terminal output the user can scroll, select,
            // search, and copy with native terminal tooling.
            crate::inline::flush_to_scrollback(&mut s, terminal)?;
            // Advance spinner each frame for animation
            s.advance_spinner();
            terminal.draw(|f| render::render(f, &s))?;
        }

        // Drain all pending agent events without blocking
        while let Ok(agent_event) = event_rx.try_recv() {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            handle_agent_event(&mut s, agent_event);
        }

        // Poll for terminal events with 50ms timeout for responsive streaming UI
        if event::poll(Duration::from_millis(50))? {
            let evt = event::read()?;
            // Handle bracketed-paste events first — they arrive as a
            // single Event::Paste(String) when the terminal supports
            // it. Newlines in the pasted content are normalized to
            // spaces because the input box is single-line; multi-line
            // input would require a vertically-growing input widget,
            // which is out of scope for this fix. The user gets their
            // paste as one message instead of a TaskStart-per-line
            // flood that crashed the renderer (pearl th-paste-crash).
            if let Event::Paste(text) = &evt {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                let sanitized = text.replace(['\r', '\n'], " ");
                for ch in sanitized.chars() {
                    s.input_insert(ch);
                }
                continue;
            }
            // Pearl th-f294fd: clear the screen on terminal resize so
            // the previous frame's streaming-preview rows don't leak
            // upward into scrollback as ghost content. ratatui's
            // inline viewport autoresizes the viewport rect on the
            // next `terminal.draw()`, but on a height-grow the
            // viewport's NEW top is below its OLD top, so whatever
            // was painted at the OLD position (typically a wall of
            // tool-call rows mid-stream) becomes uncleared scrollback
            // sitting between the legitimate committed messages and
            // the new viewport. `Terminal::clear()` in inline mode
            // moves the cursor to the viewport top and wipes
            // everything from there to the end of the screen, which
            // catches the ghost band. Also force a re-draw by
            // continuing so the next loop iteration paints the new
            // viewport before we wait on more events. Width changes
            // re-wrap the live viewport content naturally; older
            // scrollback above keeps its original wrap, which is the
            // terminal's behavior — we don't try to re-flow it.
            if matches!(evt, Event::Resize(_, _)) {
                let _ = terminal.clear();
                continue;
            }
            if let Event::Key(key) = evt {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());

                // Global keybindings. Ctrl+B used to toggle the
                // sidebar, but inline-viewport mode has no panel
                // for one — the file tree / git pane / etc. that
                // used to live there are reachable via slash
                // commands (`/git`, future `/files`). The key is
                // intentionally left unbound rather than re-purposed
                // so muscle memory doesn't fire something
                // unexpected.
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let KeyCode::Char('c') = key.code {
                        s.should_quit = true;
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
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
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
        AgentEvent::PhaseStart {
            phase,
            alias,
            upstream,
            iteration,
        } => {
            state.current_phase = Some(phase.clone());
            state.current_phase_alias = Some(alias.clone());
            state.current_phase_upstream = upstream.clone();
            // Reset phrase so the new phase shows its first word, not
            // whatever index we were on for the prior phase.
            state.phrase_idx = 0;
            // Surface the iteration boundary inline. The 7-phase
            // decomposition is gone (single CODING phase remains;
            // see crates/smooth-operator/src/coding_workflow.rs:15)
            // so the only useful per-iteration signal is "we just
            // started iteration N", optionally with the routing
            // alias when known.
            let model_part = if alias.is_empty() { String::new() } else { format!(" • {alias}") };
            state.add_message(ChatMessage::system(format!("→ iteration {iteration}{model_part}")));
        }
        AgentEvent::CheckpointSaved { iteration, .. } => {
            state.add_message(ChatMessage::system(format!("✓ snapshot taken (iter {iteration})")));
        }
        AgentEvent::ModelResolved { alias, upstream } => {
            // Pearl th-a10c2d: when running through a smooth-* alias,
            // the gateway resolves to a concrete upstream (e.g.
            // `smooth-coding` → `qwen3-coder-flash`). Surface the
            // upstream so the status bar shows `alias → upstream`
            // even outside of phase-driven runs. This both populates
            // current_phase_upstream (so the render path can pick it
            // up) AND drops an inline system note so the user
            // notices the resolution the first time.
            state.current_phase_alias = Some(alias.clone());
            state.current_phase_upstream = Some(upstream.clone());
            state.add_message(ChatMessage::system(format!("model: {alias} → {upstream}")));
        }
        AgentEvent::StreamingComplete => {
            state.finish_streaming();
        }
        AgentEvent::MaxIterationsReached { max, .. } => {
            state.finish_streaming();
            state.add_message(ChatMessage::system(format!("⚠ hit max iterations ({max}) — stopping")));
        }
        AgentEvent::BudgetExceeded { spent_usd, limit_usd } => {
            state.add_message(ChatMessage::system(format!("⚠ budget exceeded — spent ${spent_usd:.2} of ${limit_usd:.2}")));
        }
        AgentEvent::Error { message } => {
            state.finish_streaming();
            state.add_message(ChatMessage::system(format!("Error: {message}")));
        }
        // Remaining events (LlmRequest, LlmResponse, ToolCallStart,
        // ToolCallComplete, Delegation*, PortForwardActive, …) are
        // either informational duplicates of state we already track
        // (tool calls land on the assistant message; LLM round-trips
        // would be too noisy to surface per-call), or routed via a
        // direct state mutation in run_agent_streaming.
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
            // Pearl th-e0f812: skills appear in the / popup so users
            // can discover them visually. Built-in commands stay
            // first (alphabetical), skills appended after.
            let mut commands = command_registry.list_commands();
            let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            for skill in smooth_operator::skills::discover(&workspace) {
                // Skip if a built-in command already has the same
                // name (precedence: built-ins win).
                if commands.iter().any(|(n, _)| n == &skill.name) {
                    continue;
                }
                let source_label = match skill.source {
                    smooth_operator::skills::SkillSource::Project => "project",
                    smooth_operator::skills::SkillSource::UserSmooth => "user-smooth",
                    smooth_operator::skills::SkillSource::ClaudeCode => "claude-code",
                    smooth_operator::skills::SkillSource::OpenCode => "opencode",
                    smooth_operator::skills::SkillSource::Builtin => "builtin",
                };
                commands.push((skill.name.clone(), format!("[skill:{source_label}] {}", skill.description)));
            }
            state.autocomplete.update_command_query(&query, &commands);
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
    // Auto-mode permission prompts take priority over text input
    // when the input is empty. The keystrokes o/s/p/u/d/D resolve the
    // most recently filed open prompt at the chosen scope. Pearl
    // th-670fb2.
    //
    // We require empty input so users can still type "let me think
    // about this" mid-prompt — only naked dispatch keys act. The
    // prompt itself collapses to a status line as soon as the SSE
    // stream confirms (or as soon as the POST succeeds; the SSE
    // confirmation arrives shortly after).
    if state.input.is_empty() {
        if let KeyCode::Char(c) = key.code {
            if let Some((verdict, scope)) = permission_key_to_scope(c) {
                if try_resolve_open_prompt(state, state_arc.clone(), verdict, scope) {
                    return;
                }
            }
        }
    }

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
                            // Pearl th-e0f812: before failing with
                            // "Unknown command", check if the slash
                            // matches a discovered skill name. If so,
                            // treat `/skill-name [args]` as an
                            // invocation: compose the skill body +
                            // user-supplied args and dispatch through
                            // the normal agent path.
                            let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                            let skills = smooth_operator::skills::discover(&workspace);
                            if let Some(skill) = skills.into_iter().find(|s| s.name == name) {
                                let source_label = match skill.source {
                                    smooth_operator::skills::SkillSource::Project => "project",
                                    smooth_operator::skills::SkillSource::UserSmooth => "user-smooth",
                                    smooth_operator::skills::SkillSource::ClaudeCode => "claude-code",
                                    smooth_operator::skills::SkillSource::OpenCode => "opencode",
                                    smooth_operator::skills::SkillSource::Builtin => "builtin",
                                };
                                state.add_message(ChatMessage::system(format!("✦ Invoking skill: {} (from {})", skill.name, source_label)));
                                let user_request = if args.trim().is_empty() {
                                    "Invoke the skill with reasonable defaults; if any input is required and not provided, ask the user.".to_string()
                                } else {
                                    args.to_string()
                                };
                                let composed = format!(
                                    "## Skill: {} (from {})\n\n{}\n\n---\n\n## User request\n\n{}",
                                    skill.name, source_label, skill.body, user_request
                                );
                                state.add_message(ChatMessage::user(format!("/{name} {args}").trim()));
                                state.thinking = true;
                                // Skills with sandbox-incompatible
                                // operations (scp, sips, etc.) typically
                                // mark scope: host. We don't enforce host
                                // here yet — that's a follow-up. For now
                                // the standard fixer path runs with the
                                // skill body + the pre-grant from
                                // server.rs::extract_skill_allowed_hosts.
                                let agent = "fixer".to_string();
                                let tx_skill = event_tx.clone();
                                let state_for_skill = Arc::clone(&state_arc);
                                tokio::spawn(async move {
                                    if let Err(e) = run_agent_streaming(&composed, tx_skill.clone(), Some(agent), Arc::clone(&state_for_skill)).await {
                                        let _ = tx_skill.send(AgentEvent::Error { message: e.to_string() });
                                    }
                                });
                            } else {
                                state.add_message(ChatMessage::system(format!("Unknown command: /{name}. Type /help for available commands.")));
                            }
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
                                let mut s = state_arc.lock().unwrap_or_else(|e| e.into_inner());
                                s.add_message(ChatMessage::system(format!("$ {cmd}\n{result}")));
                            }
                            Err(e) => {
                                let mut s = state_arc.lock().unwrap_or_else(|e| e.into_inner());
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
                                let mut s = state_for_naming.lock().unwrap_or_else(|e| e.into_inner());
                                s.session_title = Some(title);
                            }
                        });
                    }

                    // Spawn agent task with channel-based streaming.
                    // Capture the active agent so the runner applies
                    // the right permission set on this dispatch. When
                    // the user hasn't pinned a role, classify the
                    // message via the `intent_classifier` shadow role
                    // and pick fixer (work) vs oracle (question) so
                    // the agent doesn't write files for a "how do
                    // I..." question. Classification happens inside
                    // the spawned task so the gateway round-trip
                    // doesn't block the event loop.
                    let message = input;
                    let tx = event_tx;
                    let pinned = state.agent_pinned;
                    let pinned_agent = state.agent_name.clone();
                    let state_for_routing = Arc::clone(&state_arc);
                    tokio::spawn(async move {
                        // Pearl th-e0f812: TUI parity with headless —
                        // chief picks a (role, optional skill). When a
                        // skill is picked, its body is prepended to the
                        // user message so the runner sees the recipe.
                        let (agent, message_with_skill) = if pinned {
                            (pinned_agent, message.clone())
                        } else {
                            let (intent, skill_name) = crate::intent::classify_with_skill(&message).await;
                            let role = intent.role().to_string();
                            if let Ok(mut s) = state_for_routing.lock() {
                                s.agent_name = role.clone();
                            }
                            let composed = if let Some(name) = skill_name {
                                let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                                let skills = smooth_operator::skills::discover(&workspace);
                                if let Some(skill) = skills.iter().find(|s| s.name == name) {
                                    let source_label = match skill.source {
                                        smooth_operator::skills::SkillSource::Project => "project",
                                        smooth_operator::skills::SkillSource::UserSmooth => "user-smooth",
                                        smooth_operator::skills::SkillSource::ClaudeCode => "claude-code",
                                        smooth_operator::skills::SkillSource::OpenCode => "opencode",
                                        smooth_operator::skills::SkillSource::Builtin => "builtin",
                                    };
                                    // Pearl th-e0f812 (user observation 2026-05-12):
                                    // surface the chosen skill in the chat so the
                                    // user knows what's happening. Push as a
                                    // system-style activity line BEFORE the
                                    // streaming response starts.
                                    if let Ok(mut s) = state_for_routing.lock() {
                                        s.messages.push(crate::state::ChatMessage::system(format!(
                                            "✦ Using skill: {} (from {})",
                                            skill.name, source_label
                                        )));
                                    }
                                    format!(
                                        "## Skill: {} (from {})\n\n{}\n\n---\n\n## User request\n\n{}",
                                        skill.name, source_label, skill.body, message
                                    )
                                } else {
                                    message.clone()
                                }
                            } else {
                                message.clone()
                            };
                            (role, composed)
                        };
                        if let Err(e) = run_agent_streaming(&message_with_skill, tx.clone(), Some(agent), Arc::clone(&state_for_routing)).await {
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
    use smooth_operator::cast::Cast;
    use smooth_operator::providers::ProviderRegistry;

    let providers_path = dirs_next::home_dir()?.join(".smooth/providers.json");
    let registry = ProviderRegistry::load_from_file(&providers_path).ok()?;
    let cast = Cast::builtin();
    let agent = cast.get("tagger")?;
    let config = registry.llm_config_for(agent.slot).ok()?;
    let llm = smooth_operator::llm::LlmClient::new(config);

    let system = smooth_operator::conversation::Message::system(&agent.prompt);
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
async fn run_agent_streaming(message: &str, tx: mpsc::UnboundedSender<AgentEvent>, agent: Option<String>, state: Arc<Mutex<AppState>>) -> anyhow::Result<()> {
    use std::collections::{HashMap, VecDeque};

    use crate::client::{BigSmoothClient, ServerEvent};
    use crate::state::{ChatRole, ToolCallState, ToolStatus};

    let url = std::env::var("SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
    let mut client = BigSmoothClient::new(&url);
    client
        .connect()
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to Big Smooth at {url}: {e}. Run: th up"))?;

    // Create the streaming assistant message synchronously so tool
    // calls that arrive before the main event loop has a chance to
    // process AgentEvent::Started have somewhere to attach. Without
    // this, fast-arriving ToolCallStart events would lose their
    // tool_call render entirely (the diff for the very first edit
    // wouldn't show up).
    {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.start_streaming();
    }
    let _ = tx.send(AgentEvent::Started { agent_id: "task".into() });

    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());

    // Build a structured prior-conversation array from this TUI
    // session's chat history. Sent over the wire as `prior_messages`
    // on TaskStart and replayed by the runner as native
    // `Message::user` / `Message::assistant` entries before the
    // current turn (pearl th-422b93). This is how Claude Code /
    // OpenCode / the Anthropic API handle history — proper role
    // alternation, prompt-cache friendly, tool-call structure
    // preserved.
    //
    // Constraints:
    //   - Only User and Assistant roles. System messages are TUI-side
    //     status banners that the agent doesn't need.
    //   - Skip the last two (current user message + streaming assistant
    //     placeholder).
    //   - Drop runner-stderr / cast-summary diagnostic lines from
    //     prose so we don't feed noise back into the model.
    let prior_messages: Vec<crate::client::PriorMessage> = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        let upper = s.messages.len().saturating_sub(2);
        let mut out = Vec::with_capacity(upper);
        for msg in s.messages.iter().take(upper) {
            let role = match msg.role {
                crate::state::ChatRole::User => "user",
                crate::state::ChatRole::Assistant => "assistant",
                crate::state::ChatRole::System => continue,
            };
            let cleaned: String = msg
                .content
                .lines()
                .filter(|l| !l.starts_with("[runner] ") && !l.starts_with("[runner stderr]") && !l.starts_with("[cast-summary]"))
                .collect::<Vec<_>>()
                .join("\n");
            let trimmed = cleaned.trim();
            if trimmed.is_empty() {
                continue;
            }
            out.push(crate::client::PriorMessage {
                role: role.to_string(),
                content: trimmed.to_string(),
            });
        }
        out
    };

    let mut events = client.run_task(message, None, None, cwd.as_deref(), agent.as_deref(), prior_messages).await?;

    // Per-tool-name queues of (id, started_at, args). The runner emits
    // a ToolCallStart, then the tool runs, then a ToolCallComplete —
    // possibly interleaved with other tool calls. ServerEvent has no
    // per-call id field so we associate Start with Complete by
    // tool_name + arrival order. Tools execute in parallel within
    // a single agent turn but the runner serializes the events
    // per-name, so the queue stays in lockstep.
    let mut pending: HashMap<String, VecDeque<(String, std::time::Instant, String)>> = HashMap::new();
    let mut next_id: u64 = 0;

    while let Some(event) = events.recv().await {
        let agent_event = match event {
            ServerEvent::TokenDelta { content, .. } => Some(AgentEvent::TokenDelta { content }),
            ServerEvent::ToolCallStart { tool_name, arguments, .. } => {
                next_id += 1;
                let id = format!("tc-{next_id}");
                {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    // Tool calls hang off the most recent assistant
                    // message. If there isn't one yet, drop the start
                    // event — render will pick up the Complete output
                    // anyway.
                    let attached = s
                        .messages
                        .last_mut()
                        .filter(|m| m.role == ChatRole::Assistant)
                        .map(|msg| msg.tool_calls.push(ToolCallState::from_raw(&id, &tool_name, &arguments)))
                        .is_some();
                    if !attached {
                        // No assistant message yet — skip the queue
                        // bookkeeping too so we don't pop a phantom
                        // entry on Complete.
                        continue;
                    }
                }
                pending
                    .entry(tool_name.clone())
                    .or_default()
                    .push_back((id, std::time::Instant::now(), arguments.clone()));
                Some(AgentEvent::ToolCallStart {
                    iteration: 0,
                    tool_name,
                    arguments,
                })
            }
            ServerEvent::ToolCallComplete {
                tool_name,
                result,
                is_error,
                duration_ms,
                ..
            } => {
                if let Some(q) = pending.get_mut(&tool_name) {
                    if let Some((id, _, _)) = q.pop_front() {
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        for msg in &mut s.messages {
                            for tc in &mut msg.tool_calls {
                                if tc.id == id {
                                    tc.output = Some(result.clone());
                                    tc.status = if is_error { ToolStatus::Error } else { ToolStatus::Done };
                                    tc.duration_ms = Some(duration_ms);
                                }
                            }
                        }
                    }
                }
                Some(AgentEvent::ToolCallComplete {
                    iteration: 0,
                    tool_name,
                    is_error,
                    result,
                    duration_ms,
                })
            }
            ServerEvent::TaskComplete { iterations, cost_usd, .. } => {
                let _ = tx.send(AgentEvent::Completed {
                    agent_id: "task".into(),
                    iterations,
                    cost_usd,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    cached_tokens: 0,
                });
                break;
            }
            ServerEvent::TaskError { message, .. } => {
                let _ = tx.send(AgentEvent::Error { message });
                break;
            }
            ServerEvent::NarcAlert {
                severity, category, message, ..
            } => {
                // Narc severity: Block = the call was actually blocked
                // (treat as error), Warn = informational alert (surface
                // inline so the user can see it but don't kill the
                // response), anything else = quiet by default.
                let sev_lower = severity.to_lowercase();
                let label = if category.is_empty() {
                    format!("Narc {severity}")
                } else {
                    format!("Narc {severity} • {category}")
                };
                if sev_lower == "block" {
                    let _ = tx.send(AgentEvent::Error {
                        message: format!("[{label}] {message}"),
                    });
                } else if sev_lower == "warn" {
                    // Push a system breadcrumb directly. Going through
                    // AgentEvent::Error would terminate the run; we
                    // want the response to keep flowing while the
                    // user sees the warning inline.
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.add_message(crate::state::ChatMessage::system(format!("⚠ {label}: {message}")));
                }
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

/// Map a single ASCII key onto the `(verdict, scope)` it resolves a
/// permission prompt to. Returns `None` for any key that isn't a valid
/// prompt-resolution shortcut, so the caller can fall through to the
/// normal text-input handling.
///
/// Layout matches the inline card render: `o`nce / `s`ession /
/// `p`roject / `u`ser are approve-with-scope; lowercase `d` is a
/// once-only deny; uppercase `D` is a permanent (user-scope) deny.
fn permission_key_to_scope(c: char) -> Option<(smooth_narc::ResolutionVerdict, smooth_narc::judge::Scope)> {
    use smooth_narc::judge::Scope;
    use smooth_narc::ResolutionVerdict;
    match c {
        'o' => Some((ResolutionVerdict::Approve, Scope::Once)),
        's' => Some((ResolutionVerdict::Approve, Scope::Session)),
        'p' => Some((ResolutionVerdict::Approve, Scope::PearlProject)),
        'u' => Some((ResolutionVerdict::Approve, Scope::User)),
        'd' => Some((ResolutionVerdict::Deny, Scope::Once)),
        'D' => Some((ResolutionVerdict::Deny, Scope::User)),
        _ => None,
    }
}

/// Resolve the most recently filed *open* permission prompt at the
/// chosen scope. Returns `true` if a prompt was found and the
/// resolution POST was spawned, `false` if there was nothing to do
/// (no open prompts).
///
/// The state mutation lands synchronously (flip status to
/// `Resolving`); the actual HTTP POST is spawned on tokio and updates
/// the prompt to `Failed` if it errors. The SSE stream's matching
/// `Resolved` event will arrive shortly after a successful POST and
/// flip the status to `Approved`/`Denied` with the canonical
/// resolution payload.
fn try_resolve_open_prompt(
    state: &mut AppState,
    state_arc: Arc<Mutex<AppState>>,
    verdict: smooth_narc::ResolutionVerdict,
    scope: smooth_narc::judge::Scope,
) -> bool {
    use crate::auto_mode::PromptStatus;
    let Some(prompt) = state.permission_prompts.iter_mut().rev().find(|p| p.status.is_open()) else {
        return false;
    };
    let id = prompt.request.id.clone();
    prompt.status = PromptStatus::Resolving { verdict, scope };

    // Detach from the AppState mutation — POST runs in the background.
    let base = std::env::var("SMOOTH_BIGSMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        if let Err(reason) = crate::auto_mode::resolve(&base, &client, &id, verdict, scope, None).await {
            // Mark the prompt as failed so the user can see what went
            // wrong and retry. The SSE stream will not deliver a
            // matching Resolved event since the POST never landed.
            if let Ok(mut s) = state_arc.lock() {
                if let Some(p) = s.permission_prompts.iter_mut().find(|p| p.request.id == id) {
                    p.status = PromptStatus::Failed { reason };
                }
            }
        }
    });
    true
}
