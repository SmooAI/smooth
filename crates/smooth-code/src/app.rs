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
use crate::session::SessionManager;
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

/// Write the running cumulative `total_cost_usd` to the path in
/// `SMOOTH_BENCH_COST_SIDECAR` (or do nothing when unset). The bench
/// reads this on task completion to avoid scraping the TUI's status
/// bar, which is brittle against render-timing races and format drift.
/// Opt-in via env so regular `th code` sessions don't drop a file.
/// Best-effort: a write failure must not affect the user's session.
/// Pearl th-a08fa3.
fn write_bench_cost_sidecar(total_cost_usd: f64, iterations: u32) {
    let Ok(path) = std::env::var("SMOOTH_BENCH_COST_SIDECAR") else {
        return;
    };
    if path.is_empty() {
        return;
    }
    write_bench_cost_sidecar_to(std::path::Path::new(&path), total_cost_usd, iterations);
}

/// Path-taking core of [`write_bench_cost_sidecar`]. Pulled out so the
/// IO behavior is unit-testable without touching the process-global
/// `SMOOTH_BENCH_COST_SIDECAR` env var (which would require `unsafe`
/// under Rust 2024 and is forbidden crate-wide).
fn write_bench_cost_sidecar_to(path: &std::path::Path, total_cost_usd: f64, iterations: u32) {
    let body = serde_json::json!({
        "cost_usd": total_cost_usd,
        "iterations": iterations,
        "ts_unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    });
    let Ok(serialized) = serde_json::to_string(&body) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Atomic-ish write: rename(tmp → final). A reader catching us
    // mid-write at the final path is the failure mode we want to
    // avoid — bench polls this file and can race the writer.
    let mut tmp_path = path.as_os_str().to_os_string();
    tmp_path.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp_path);
    if std::fs::write(&tmp_path, serialized.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp_path, path);
    }
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
    run_with_session(working_dir, None, None, None).await
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
pub async fn run_with_session(
    working_dir: PathBuf,
    resume: Option<crate::session::Session>,
    agent: Option<String>,
    model: Option<String>,
) -> anyhow::Result<()> {
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
    // No alt-screen, and no mouse capture *by default* — both would
    // break those.
    //
    // Pearl th-958e2e: capture is now toggled on ONLY while the draft is
    // taller than the input box, so the wheel can scroll it, and off the
    // instant it fits again. That keeps native scrollback and drag-select
    // for the ~99% case where the draft is a line or two, at the cost of
    // the wheel scrolling your draft (and drag-select needing Shift /
    // Option) while a long one is open. See `sync_mouse_capture`.
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
    // status + 10 preview) feels right on an 80x24 terminal; shorter
    // terminals cap at 60% of their height so the viewport never
    // crowds out scrollback, and taller ones scale up to 40% so the
    // composer's growth ceiling rises with them (pearl th-d5eb9f).
    // ponytail: sized once at startup — resize re-plumbing through
    // ratatui's inline viewport when someone actually asks.
    let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
    let viewport_h = u16::max(14, term_h.saturating_mul(2) / 5).min(term_h.saturating_mul(3) / 5).max(4);
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
    // Pearl th-20574a: thread the CLI's `--model` flag through to
    // every TaskStart so bench harnesses (and any user who passes
    // --model) actually get the requested model instead of silently
    // falling back to smooth-coding's default alias.
    if let Some(m) = model {
        // Keep the displayed name in lockstep with the value actually put on
        // the wire — the status bar has no other way to know (th-d49538).
        initial_state.model_name.clone_from(&m);
        initial_state.model_override = Some(m);
    }
    initial_state.viewport_h = viewport_h;

    let state = Arc::new(Mutex::new(initial_state));

    // ponytail: narc TUI removed with the old-cast crate; re-home onto the new engine's NarcHook later (th-3119e3)

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
        // Nothing to say on a fresh session: the splash already renders
        // "Type a message to get started. /help for commands." right above
        // this, and adding it again as a `System:` message printed the same
        // sentence twice on every cold start.
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

    // Populate the model picker's local-provider models in the
    // background so a BYO local server (Ollama, LM Studio) shows up in
    // the `/model` picker's "show all" view. Tolerates no/unreachable
    // local providers (returns empty). Pearl th-f4a0fb.
    {
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            let locals = crate::model_picker::fetch_local_provider_models().await;
            if !locals.is_empty() {
                let mut s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                s.model_picker.local_models = locals;
            }
        });
    }

    // Populate the model picker's catalog from the Smoo gateway's live
    // `GET /v1/model/info` so use-cases / tier / cost / benchmarks reflect
    // the gateway's current model set (and removed models drop out)
    // instead of the baked offline catalog. Tolerates no/unreachable
    // gateway (returns empty → offline catalog used). Pearl th-7ee88e.
    {
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            let catalog = crate::model_picker::fetch_gateway_catalog().await;
            if !catalog.is_empty() {
                let mut s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                s.model_picker.gateway_models = catalog;
            }
        });

        // Skill catalog from the daemon — the ONE catalog every face renders
        // (pearl th-a5952d). Best-effort: unreachable daemon leaves the local
        // discover fallback in place.
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            if let Some(skills) = fetch_remote_skills().await {
                let mut s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                s.remote_skills = Some(skills);
            }
        });

        // The model Big Smooth would run the next turn with, so the idle
        // status bar shows a real name instead of "unknown" (pearl
        // th-7630a7). Display-only: `model_name` never goes on the wire
        // (`model_override` does), and anything the user already chose —
        // `--model` at startup or the picker mid-fetch — wins.
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            if let Some(model) = fetch_daemon_mode().await {
                let mut s = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                if s.model_name.trim().is_empty() {
                    s.model_name = model;
                }
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

    // Restore terminal — inline-viewport mode only needs to disable
    // raw mode and ensure the cursor is visible. There's no alt-
    // screen to leave: the viewport sat in the primary buffer the
    // whole time. Also disable bracketed paste so subsequent shell
    // sessions in the same terminal don't inherit the mode.
    let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableBracketedPaste);
    // Unconditionally release mouse capture. It is only ever on while a long
    // draft is open (see `sync_mouse_capture`), but exiting with it left on
    // would strip the user's shell of wheel-scroll and drag-select — and
    // disabling when it was never enabled is harmless.
    let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableMouseCapture);
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
    // Tracks whether mouse capture is currently on; see `sync_mouse_capture`.
    let mut mouse_captured = false;

    loop {
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
            sync_mouse_capture(&s, &mut mouse_captured);
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
                // A dragged file lands as a paste of its (possibly quoted /
                // escaped) path. If it's an image or PDF, stage it as an
                // attachment instead of inserting the path as text
                // (pearl th-d16f7c).
                if let Some(path) = crate::attachments::attachable_path(text) {
                    match crate::attachments::attach_file(&path) {
                        Ok(a) => {
                            s.messages
                                .push(crate::state::ChatMessage::system(format!("📎 Attached {} ({})", a.name, a.mime)));
                            s.attachments.push(a);
                        }
                        Err(e) => s.messages.push(crate::state::ChatMessage::system(format!("Attachment failed: {e}"))),
                    }
                    continue;
                }
                // A LARGE paste becomes a compact `[Pasted #N — X lines]`
                // reference instead of flooding the draft; the full text is
                // expanded back in at send (pearl th-d5eb9f, Claude Code
                // parity). Small pastes insert inline as before — newlines
                // KEPT (th-958e2e), endings normalized (tmux sends bare \r).
                if text.lines().count() > 5 || text.len() > 400 {
                    s.stage_pasted(text);
                } else {
                    s.input_insert_str(text);
                }
                continue;
            }
            // Wheel over the input scrolls the draft. Only ever reaches us
            // while capture is on, i.e. while the draft overflows the box.
            if let Event::Mouse(mouse) = evt {
                let delta = match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => -1,
                    crossterm::event::MouseEventKind::ScrollDown => 1,
                    _ => continue,
                };
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                let (cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
                let cap = crate::composer::max_text_rows(s.viewport_h);
                s.input_scroll_by(delta, cols.saturating_sub(2), cap);
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

                // Global keybindings. Ctrl+C quits; Ctrl+B toggles the
                // conversation sidebar (list saved sessions → resume /
                // new). The sidebar is an overlay popup, not a
                // persistent pane, because inline-viewport mode has no
                // room for one — finalized chat lives in the terminal's
                // own scrollback.
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('c') => s.should_quit = true,
                        KeyCode::Char('b') => toggle_session_sidebar(&mut s, state),
                        // Ctrl+V: attach an image from the OS clipboard
                        // (screenshots, copied images) — Claude Code parity
                        // (pearl th-d16f7c). Bracketed paste still handles
                        // text; this path only fires for pixel data.
                        KeyCode::Char('v') => match crate::attachments::clipboard_image() {
                            Some(a) => {
                                s.messages
                                    .push(crate::state::ChatMessage::system(format!("📎 Attached {} ({})", a.name, a.mime)));
                                s.attachments.push(a);
                            }
                            None => s.messages.push(crate::state::ChatMessage::system(
                                "No image on the clipboard. (Text pastes with your terminal's normal paste key.)".to_string(),
                            )),
                        },
                        // Readline muscle memory (pearl th-d5eb9f): Ctrl+W
                        // kills the word, Ctrl+U the line — and they double
                        // as the reliable spelling of Alt/Cmd+Backspace,
                        // which many terminals never deliver.
                        KeyCode::Char('w') => s.input_backspace_word(),
                        KeyCode::Char('u') => s.input_backspace_line(),
                        _ => {}
                    }
                    // Ctrl-chorded keys are commands, not text — don't let
                    // them fall through to the input handler as characters.
                    if !matches!(key.code, KeyCode::Char('c')) {
                        continue;
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

/// Toggle the conversation sidebar. Opening it saves the current
/// session first (so it shows up in — and is highlighted within — the
/// freshly-listed set) and loads the summaries from the on-disk
/// [`SessionManager`] store. A store error just yields an empty list
/// (the "New conversation" row still works).
/// Whether the draft is taller than the input box can show.
///
/// Pure and `pub(crate)` so the policy — *capture the mouse only when there is
/// something to scroll* — is unit-testable without a terminal.
pub(crate) fn input_overflows(input: &str, term_width: u16, text_cap: u16) -> bool {
    let inner_width = term_width.saturating_sub(2);
    crate::composer::wrap_rows(input, inner_width).len() > usize::from(text_cap)
}

/// Turn mouse capture on only while the draft overflows its box, and off the
/// moment it fits again (pearl th-958e2e).
///
/// Capture is a global terminal mode: while it is on, the emulator forwards
/// wheel and click events to us instead of scrolling its own scrollback and
/// handling drag-select. Since this TUI deliberately keeps finalized chat in
/// the terminal's scrollback, holding capture for the whole session would cost
/// the user native scrolling and copy/paste permanently. Scoping it to "there
/// is a long draft open" keeps the default interaction intact.
///
/// Both calls are best-effort: a terminal that ignores the escape sequence
/// simply never sends mouse events, and the input still scrolls to follow the
/// cursor.
fn sync_mouse_capture(state: &AppState, captured: &mut bool) {
    let (cols, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let want = input_overflows(&state.input, cols, crate::composer::max_text_rows(state.viewport_h));
    if want == *captured {
        return;
    }
    let result = if want {
        crossterm::execute!(io::stdout(), crossterm::event::EnableMouseCapture)
    } else {
        crossterm::execute!(io::stdout(), crossterm::event::DisableMouseCapture)
    };
    if result.is_ok() {
        *captured = want;
    }
}

fn toggle_session_sidebar(state: &mut AppState, state_arc: &Arc<Mutex<AppState>>) {
    if state.session_picker.active {
        state.session_picker.deactivate();
        return;
    }
    // Sessions ARE daemon conversations (pearl th-aaa53a): the sidebar lists
    // the daemon's `list_conversations` — the same rows the web SPA shows —
    // so a chat started in any face is resumable from every face. The legacy
    // on-disk store is only the offline fallback (a down daemon can't chat
    // anyway, but the user can still eyeball old local transcripts).
    let arc = Arc::clone(state_arc);
    tokio::spawn(async move {
        let url = std::env::var("SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
        let rows = match crate::client::list_remote_conversations(&url).await {
            Ok(convs) => convs
                .into_iter()
                .map(|c| crate::session::SessionSummary {
                    id: c.conversation_id,
                    title: Some(c.title),
                    preview: String::new(),
                    message_count: usize::try_from(c.message_count).unwrap_or(usize::MAX),
                    updated_at: chrono::DateTime::parse_from_rfc3339(&c.updated_at).map_or_else(|_| chrono::Utc::now(), |t| t.with_timezone(&chrono::Utc)),
                })
                .collect(),
            Err(_) => SessionManager::new().and_then(|m| m.list()).unwrap_or_default(),
        };
        let mut s = arc.lock().unwrap_or_else(|e| e.into_inner());
        if !s.session_picker.active {
            let current = s.conversation_id.clone().unwrap_or_default();
            s.session_picker.open(rows, &current);
        }
    });
}

/// Handle a key while the conversation sidebar owns the keyboard.
/// Returns `true` when the key was consumed.
fn handle_session_sidebar_key(key: event::KeyEvent, state: &mut AppState, state_arc: &Arc<Mutex<AppState>>) -> bool {
    use crate::session_picker::PickerAction;

    match key.code {
        KeyCode::Up => state.session_picker.select_up(),
        KeyCode::Down => state.session_picker.select_down(),
        KeyCode::Esc => state.session_picker.deactivate(),
        KeyCode::Char('n') => {
            // Shortcut for the "New conversation" entry regardless of
            // cursor position.
            state.start_new_conversation();
            state.add_message(ChatMessage::system("Started a new conversation. Type a message to get going."));
            state.session_picker.deactivate();
        }
        KeyCode::Enter => {
            match state.session_picker.selected_action() {
                PickerAction::New => {
                    state.start_new_conversation();
                    state.add_message(ChatMessage::system("Started a new conversation. Type a message to get going."));
                }
                PickerAction::Resume(id) => {
                    if state.conversation_id.as_deref() == Some(id.as_str()) {
                        // Already viewing it — just close.
                    } else if let Ok(session) = SessionManager::new().and_then(|m| m.load(&id)) {
                        // Legacy on-disk session (offline-fallback rows only).
                        state.resume_from(&session);
                        let label = state.session_title.clone().unwrap_or_else(|| id.clone());
                        state.add_message(ChatMessage::system(format!("Resumed session: {label}")));
                    } else {
                        // Daemon conversation: bind to it, then hydrate the
                        // transcript from stored history (th-aaa53a). The
                        // engine replays context server-side on the next turn.
                        let title = state
                            .session_picker
                            .sessions
                            .iter()
                            .find(|r| r.id == id)
                            .and_then(|r| r.title.clone())
                            .unwrap_or_else(|| id.clone());
                        state.start_new_conversation();
                        state.conversation_id = Some(id.clone());
                        state.session_title = Some(title.clone());
                        state.add_message(ChatMessage::system(format!("Resuming conversation: {title}…")));
                        let arc = Arc::clone(state_arc);
                        tokio::spawn(async move {
                            let url = std::env::var("SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
                            let fetched = crate::client::fetch_conversation_history(&url, &id).await;
                            let mut s = arc.lock().unwrap_or_else(|e| e.into_inner());
                            // Only hydrate if the user is still on this conversation.
                            if s.conversation_id.as_deref() != Some(id.as_str()) {
                                return;
                            }
                            match fetched {
                                Ok(history) => {
                                    for m in history {
                                        match m.role.as_str() {
                                            "user" => s.add_message(ChatMessage::user(&m.content)),
                                            "assistant" => s.add_message(ChatMessage::assistant(&m.content)),
                                            _ => {}
                                        }
                                    }
                                }
                                Err(e) => s.add_message(ChatMessage::system(format!("Could not load history: {e}"))),
                            }
                        });
                    }
                }
            }
            state.session_picker.deactivate();
        }
        _ => {}
    }
    true
}

/// How many milliseconds a finished tool call took.
///
/// The canonical `toolResult` frame carries no timing — the engine measures a
/// duration but the server doesn't forward it — so the honest number is the one
/// the event loop measured itself, from the `Instant` captured when the
/// matching `ToolCallStart` arrived. A server-sent value wins if one ever shows
/// up. Pearl th-d49538: that `Instant` used to be captured and then dropped on
/// the floor, so every tool in the transcript rendered `0.0s` and the agent
/// read its own timings as evidence of hangs that never happened.
fn resolve_duration_ms(from_server: Option<u64>, started: std::time::Instant) -> u64 {
    from_server.unwrap_or_else(|| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX))
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
        AgentEvent::Completed {
            cost_usd,
            iterations,
            prompt_tokens,
            completion_tokens,
            ..
        } => {
            state.total_cost_usd += cost_usd;
            // th-d49538: the status bar used to say `0 tok` forever because
            // nothing ever added to this counter. Saturating: the display is
            // not worth a panic on an absurd count.
            let turn_tokens = u32::try_from(prompt_tokens.saturating_add(completion_tokens)).unwrap_or(u32::MAX);
            state.total_tokens = state.total_tokens.saturating_add(turn_tokens);
            // Pearl th-a08fa3: write a JSON cost sidecar when
            // SMOOTH_BENCH_COST_SIDECAR is set. The bench needs a
            // deterministic cost signal that doesn't depend on the
            // TUI's render timing or status-bar string format. Opt-in
            // via env so non-bench `th code` sessions don't drop a
            // file. Best-effort: a write error must not affect the
            // user's session.
            write_bench_cost_sidecar(state.total_cost_usd, iterations);
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
            // see crates/smooth-cast/src/coding_workflow.rs)
            // so the only useful per-iteration signal is "we just
            // started iteration N", optionally with the routing
            // alias when known.
            let model_part = if alias.is_empty() { String::new() } else { format!(" • {alias}") };
            state.add_message(ChatMessage::system(format!("→ iteration {iteration}{model_part}")));
            // Pearl th-486bd0: start a fresh streaming ChatMessage
            // for this iteration so the next batch of TokenDeltas
            // doesn't concatenate into the prior iteration's bubble
            // (which produced `III'll help` / `LetLet me me` dupes).
            state.start_iteration();
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
        // Pearl th-486bd0: non-workflow agent paths (single-Agent
        // loop without coding_workflow phases) emit LlmRequest at
        // each iteration boundary but no PhaseStart. Treat LlmRequest
        // as a fallback iteration signal: start a fresh streaming
        // bubble so subsequent TokenDeltas don't concatenate with
        // the prior iteration's content. Iteration #1 is a no-op
        // because the prior empty stub from Started gets recycled.
        AgentEvent::LlmRequest { iteration, .. } if iteration > 1 => {
            state.start_iteration();
        }
        // Remaining events (LlmResponse, ToolCallStart, ToolCallComplete,
        // Delegation*, PortForwardActive, …) are either informational
        // duplicates of state we already track (tool calls land on the
        // assistant message; LLM responses are already streamed via
        // TokenDelta), or routed via a direct state mutation in
        // run_agent_streaming.
        _ => {}
    }
}

/// Refresh the autocomplete query from the current input buffer —
/// the text between `trigger_pos + 1` and the cursor — and re-run
/// the filter against the appropriate candidate source.
/// `GET {SMOOTH_URL}/api/skills?cwd=…` → the daemon's skill catalog, or
/// `None` on any failure so callers keep the local-discover fallback.
async fn fetch_remote_skills() -> Option<Vec<smooth_cast::skills::Skill>> {
    let base = std::env::var("SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
    let cwd = std::env::current_dir().ok()?.to_string_lossy().into_owned();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build().ok()?;
    let resp = client
        .get(format!("{}/api/skills", base.trim_end_matches('/')))
        .query(&[("cwd", cwd.as_str())])
        .send()
        .await
        .ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    serde_json::from_value(body.get("skills")?.clone()).ok()
}

/// `GET {SMOOTH_URL}/api/mode` → the model the daemon would run the next turn
/// with, or `None` on any failure (daemon down, no credentials resolved) so
/// the status bar keeps its honest "unknown" (pearl th-7630a7).
async fn fetch_daemon_mode() -> Option<String> {
    let base = std::env::var("SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build().ok()?;
    let resp = client.get(format!("{}/api/mode", base.trim_end_matches('/'))).send().await.ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    let model = body.get("model")?.as_str()?.trim().to_string();
    if model.is_empty() {
        None
    } else {
        Some(model)
    }
}

/// `GET {SMOOTH_URL}/search?q=…&cwd=…` → popup rows, or `None` on any
/// failure (daemon down, timeout, off-contract JSON) so the caller keeps the
/// locally-computed results. The route is ungated by design — no token.
async fn fetch_remote_mentions(query: &str, cwd: &str) -> Option<Vec<crate::autocomplete::AutocompleteResult>> {
    let base = std::env::var("SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
    let client = reqwest::Client::builder().timeout(Duration::from_millis(1500)).build().ok()?;
    let resp = client
        .get(format!("{}/search", base.trim_end_matches('/')))
        .query(&[("q", query), ("cwd", cwd)])
        .send()
        .await
        .ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    let results = body.get("results")?.as_array()?;
    Some(
        results
            .iter()
            .filter_map(|r| {
                let value = r.get("value")?.as_str()?;
                let label = r.get("label")?.as_str()?.to_string();
                let kind = r.get("kind").and_then(serde_json::Value::as_str).unwrap_or("file");
                let detail = r
                    .get("detail")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| kind.to_string(), ToString::to_string);
                Some(crate::autocomplete::AutocompleteResult {
                    label,
                    detail,
                    insert_text: format!("@{value}"),
                })
            })
            .collect(),
    )
}

fn refresh_autocomplete(state: &mut AppState, command_registry: &CommandRegistry, state_arc: &Arc<Mutex<AppState>>) {
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
            // Local results first — instant popup, and the offline fallback.
            let files: Vec<_> = state.file_tree.as_ref().map(|t| t.entries.clone()).unwrap_or_default();
            let pearls = state.pearls.clone();
            state.autocomplete.update_at_query(&query, &files, &pearls, &workspace_root);

            // Then ask Big Smooth's `/search` — the SAME backend the web
            // composer uses — and overlay its ranked files+paths+pearls when
            // the reply lands, IF the user hasn't typed since (generation
            // guard) and the daemon actually answered (pearl th-8e9cf6).
            if !query.trim().is_empty() {
                state.autocomplete.generation += 1;
                let generation = state.autocomplete.generation;
                let q = query.clone();
                let cwd = workspace_root.to_string_lossy().into_owned();
                let arc = Arc::clone(state_arc);
                tokio::spawn(async move {
                    if let Some(results) = fetch_remote_mentions(&q, &cwd).await {
                        let mut s = arc.lock().unwrap_or_else(|e| e.into_inner());
                        s.autocomplete.apply_remote_results(generation, results);
                    }
                });
            }
        }
        crate::autocomplete::CompletionKind::Command => {
            // Pearl th-e0f812: skills appear in the / popup so users
            // can discover them visually. Built-in commands stay
            // first (alphabetical), skills appended after.
            let mut commands = command_registry.list_commands();
            for skill in crate::commands::available_skills(state) {
                // Skip if a built-in command already has the same
                // name (precedence: built-ins win).
                if commands.iter().any(|(n, _)| n == &skill.name) {
                    continue;
                }
                let source_label = skill.source.label();
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

/// Would submitting `input` right now dispatch a SECOND concurrent agent turn?
///
/// Only [`InputKind::Normal`] reaches the agent; slash commands and `!shell`
/// run locally, so they stay usable while a turn is in flight. Pure so the
/// guard is testable without a terminal (pearl th-426791).
///
/// `ponytail:` an unknown `/name` that resolves to a *skill* also dispatches a
/// turn, and isn't blocked here — deciding that needs the skill registry, and
/// invoking a skill mid-turn is a corner of a corner. Move the check inside the
/// skill branch if it ever bites.
fn blocks_second_turn(input: &str, turn_in_flight: bool) -> bool {
    turn_in_flight && matches!(parse_input(input), InputKind::Normal(text) if !text.is_empty())
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
    // ponytail: narc TUI removed with the old-cast crate; re-home onto the new engine's NarcHook later (th-3119e3)

    // Modified Backspace edits by word/line regardless of what popup is up
    // (pearl th-d5eb9f): Alt+Backspace kills the previous word (crossterm
    // reports terminals' ESC-DEL as ALT), Cmd+Backspace the line (SUPER/META
    // only arrives on terminals with an enhanced keyboard protocol — Ctrl+W /
    // Ctrl+U in the global chord block are the always-works spellings).
    if key.code == KeyCode::Backspace && key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META) {
        if key.modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::META) {
            state.input_backspace_line();
        } else {
            state.input_backspace_word();
        }
        state.autocomplete.deactivate();
        return;
    }

    // Conversation sidebar owns the keyboard while it's visible.
    // Up/Down navigates, Enter resumes (or starts a new chat on the
    // "New conversation" row), `n` is a new-chat shortcut, Esc closes.
    if state.session_picker.active {
        handle_session_sidebar_key(key, state, &state_arc);
        return;
    }

    // Model picker owns the keyboard while it's visible. Up/Down
    // navigates, Enter drills in or applies, Esc backs out (Models →
    // Slots → closed).
    if state.model_picker.active {
        match key.code {
            KeyCode::Up => state.model_picker.select_up(),
            KeyCode::Down => state.model_picker.select_down(),
            // Tab toggles the slot's use-case filter in the Models
            // sub-view so users can override the picker (SMOODEV-1793
            // / th-7ee88e). No-op in Slots view.
            KeyCode::Tab => state.model_picker.toggle_show_all(),
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
                refresh_autocomplete(state, command_registry, &state_arc);
                return;
            }
            KeyCode::Backspace => {
                state.input_backspace();
                if state.input_cursor <= state.autocomplete.trigger_pos {
                    state.autocomplete.deactivate();
                } else {
                    refresh_autocomplete(state, command_registry, &state_arc);
                }
                return;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => {
            // th-426791: a second agent turn dispatched while one is still in
            // flight runs CONCURRENTLY — the two responses stream back
            // interleaved and each lands under the other's prompt. Refuse the
            // dispatch *before* `take_input`, so the draft stays in the box and
            // the keystroke costs nothing. Slash commands and `!shell` are
            // handled locally, so they stay live while Big Smooth works.
            if blocks_second_turn(&state.input, state.thinking) {
                return;
            }
            let input = state.take_input();
            if input.trim().is_empty() && state.attachments.is_empty() {
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
                            let skills = crate::commands::available_skills(state);
                            if let Some(skill) = skills.into_iter().find(|s| s.name == name) {
                                let source_label = skill.source.label();
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
                                    if let Err(e) =
                                        run_agent_streaming(&composed, tx_skill.clone(), Some(agent), Arc::clone(&state_for_skill), Vec::new()).await
                                    {
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
                    // Ship the staged attachments with THIS turn and clear
                    // the tray — same one-shot semantics as the web composer.
                    let images: Vec<String> = state.attachments.drain(..).map(|a| a.data_url).collect();
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
                                let skills = {
                                    let s = state_for_routing.lock().unwrap_or_else(|e| e.into_inner());
                                    crate::commands::available_skills(&s)
                                };
                                if let Some(skill) = skills.iter().find(|s| s.name == name) {
                                    let source_label = skill.source.label();
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
                        if let Err(e) = run_agent_streaming(&message_with_skill, tx.clone(), Some(agent), Arc::clone(&state_for_routing), images).await {
                            let _ = tx.send(AgentEvent::Error { message: e.to_string() });
                        }
                    });
                }
            }
        }
        KeyCode::Backspace => {
            // Empty draft: backspace removes the newest staged attachment,
            // so a mis-paste is one keystroke to undo (pearl th-d16f7c).
            if state.input.is_empty() && !state.attachments.is_empty() {
                let removed = state.attachments.pop();
                if let Some(a) = removed {
                    state.messages.push(crate::state::ChatMessage::system(format!("Removed attachment {}", a.name)));
                }
            } else {
                state.input_backspace();
            }
        }
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
                    refresh_autocomplete(state, command_registry, &state_arc);
                }
                '/' => {
                    state.autocomplete.activate_commands(trigger_pos);
                    refresh_autocomplete(state, command_registry, &state_arc);
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
        warnings.push("No LLM providers configured. Run: /model to select one, or th model login <provider>".into());
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
    use smooth_cast::cast::builtin as cast_builtin;
    use smooth_cast::provider_migration::load_providers_with_migration;

    let providers_path = dirs_next::home_dir()?.join(".smooth/providers.json");
    let registry = load_providers_with_migration(&providers_path).ok()?;
    let cast = cast_builtin();
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
async fn run_agent_streaming(
    message: &str,
    tx: mpsc::UnboundedSender<AgentEvent>,
    agent: Option<String>,
    state: Arc<Mutex<AppState>>,
    images: Vec<String>,
) -> anyhow::Result<()> {
    use std::collections::{HashMap, VecDeque};

    use crate::client::{BigSmoothClient, ServerEvent};
    use crate::state::{ChatRole, ToolCallState, ToolStatus};

    let url = std::env::var("SMOOTH_URL").unwrap_or_else(|_| "http://localhost:4400".into());
    let mut client = BigSmoothClient::new(&url);
    // Resume this TUI session's conversation. A client is built per turn, so
    // without this the daemon opens a fresh conversation every message and the
    // agent starts from zero each time — tell it your name, and the next turn
    // it has never heard of you (pearl th-255d2a). `None` on the first turn.
    {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        client.resume_conversation(s.conversation_id.as_deref());
    }
    client
        .connect()
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to Big Smooth at {url}: {e}. Run: th up"))?;
    // Remember whatever the server bound us to, so the next turn resumes it.
    if let Some(cid) = client.conversation_id() {
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.conversation_id = Some(cid);
    }

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

    // History replay happens SERVER-SIDE: the client resumes the daemon
    // conversation (th-255d2a) and the engine replays its stored history by
    // thread_id on every turn. The old client-side `prior_messages` replay
    // (th-422b93, pre-resume) double-fed that history and is gone (th-aaa53a).

    // Pearl th-20574a: read the user's --model override from AppState
    // so it actually reaches Big Smooth's routing layer. Was a literal
    // `None` here; every TaskStart fell back to the smooth-coding alias
    // regardless of CLI flag.
    let model_override = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        s.model_override.clone()
    };
    let mut events = client
        .run_task(message, model_override.as_deref(), None, cwd.as_deref(), agent.as_deref(), Vec::new(), images)
        .await?;

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
            // Pearl th-486bd0: iteration boundary — reset the
            // streaming ChatMessage so deltas from the next agent
            // iteration land in a fresh bubble, not concatenated
            // into the prior iteration's content.
            ServerEvent::LlmIteration { iteration, .. } => {
                if iteration > 1 {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.start_iteration();
                }
                None
            }
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
                let mut resolved = duration_ms;
                if let Some(q) = pending.get_mut(&tool_name) {
                    if let Some((id, started, _)) = q.pop_front() {
                        let measured = resolve_duration_ms(duration_ms, started);
                        resolved = Some(measured);
                        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                        for msg in &mut s.messages {
                            for tc in &mut msg.tool_calls {
                                if tc.id == id {
                                    tc.output = Some(result.clone());
                                    tc.status = if is_error { ToolStatus::Error } else { ToolStatus::Done };
                                    tc.duration_ms = Some(measured);
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
                    duration_ms: resolved.unwrap_or_default(),
                })
            }
            ServerEvent::TaskComplete { iterations, usage, .. } => {
                let usage = usage.unwrap_or_default();
                let _ = tx.send(AgentEvent::Completed {
                    agent_id: "task".into(),
                    iterations,
                    cost_usd: usage.cost_usd,
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
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
            // Only an error attributed to THIS turn ends it. An unattributed
            // one (rejected heartbeat, late error for an abandoned turn)
            // becomes an inline breadcrumb — same treatment as a `warn`
            // severity above — so the turn keeps streaming. th-472012: a
            // rejected ping used to kill turns whose answers the daemon had
            // already finished.
            ServerEvent::Error { message, request_id } => {
                if request_id.is_some() {
                    let _ = tx.send(AgentEvent::Error { message });
                    break;
                }
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.add_message(crate::state::ChatMessage::system(format!("⚠ {message}")));
                None
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

// ponytail: narc TUI removed with the old-cast crate; re-home onto the new engine's NarcHook later (th-3119e3)

#[cfg(test)]
mod second_turn_guard_tests {
    use super::blocks_second_turn;

    #[test]
    fn nothing_is_blocked_while_idle() {
        assert!(!blocks_second_turn("fix the failing test", false));
        assert!(!blocks_second_turn("/help", false));
    }

    /// The whole point: a chat message can't start a turn racing the live one.
    #[test]
    fn a_chat_message_is_blocked_while_a_turn_is_in_flight() {
        assert!(blocks_second_turn("what did you just say?", true));
        assert!(blocks_second_turn("  leading space still counts  ", true));
    }

    /// Local surfaces stay usable — blocking `/clear` or `/quit` mid-turn would
    /// leave the user with no way out at all.
    #[test]
    fn local_commands_stay_live_while_a_turn_is_in_flight() {
        assert!(!blocks_second_turn("/help", true));
        assert!(!blocks_second_turn("/quit", true));
        assert!(!blocks_second_turn("!git status", true));
    }

    /// An empty draft was already a no-op; the guard must not claim it.
    #[test]
    fn an_empty_draft_is_not_reported_as_blocked() {
        assert!(!blocks_second_turn("", true));
        assert!(!blocks_second_turn("   \n ", true));
    }
}

#[cfg(test)]
mod mouse_capture_policy_tests {
    use super::input_overflows;
    use crate::composer::MAX_TEXT_ROWS;

    /// The whole point of the scoped-capture design: in the common case the
    /// terminal keeps its own wheel, drag-select and copy.
    #[test]
    fn a_normal_draft_never_captures_the_mouse() {
        assert!(!input_overflows("", 80, MAX_TEXT_ROWS));
        assert!(!input_overflows("fix the failing test", 80, MAX_TEXT_ROWS));
        assert!(!input_overflows(&"line\n".repeat(usize::from(MAX_TEXT_ROWS) - 1), 80, MAX_TEXT_ROWS));
    }

    #[test]
    fn an_overflowing_draft_captures_the_mouse() {
        assert!(input_overflows(&"line\n".repeat(usize::from(MAX_TEXT_ROWS) + 2), 80, MAX_TEXT_ROWS));
    }

    /// Exactly-full is not overflowing — capture must not flap on the boundary.
    #[test]
    fn a_draft_that_exactly_fills_the_box_does_not_capture() {
        let exact = (0..MAX_TEXT_ROWS).map(|i| format!("row{i}")).collect::<Vec<_>>().join("\n");
        assert!(!input_overflows(&exact, 80, MAX_TEXT_ROWS));
        assert!(input_overflows(&format!("{exact}\nmore"), 80, MAX_TEXT_ROWS));
    }

    /// Soft-wrapping counts: a single long line can overflow on a narrow
    /// terminal while fitting on a wide one.
    #[test]
    fn wrapping_is_what_decides_not_newline_count() {
        let long = "x".repeat(200);
        assert!(!input_overflows(&long, 80, MAX_TEXT_ROWS), "200 cols fits in 6 rows at width 78");
        assert!(input_overflows(&long, 22, MAX_TEXT_ROWS), "the same text needs 10 rows at width 20");
    }

    /// A degenerate width must not panic or divide by zero.
    #[test]
    fn tiny_terminals_are_handled() {
        assert!(!input_overflows("", 0, MAX_TEXT_ROWS));
        assert!(input_overflows(&"x".repeat(50), 1, MAX_TEXT_ROWS));
    }
}

#[cfg(test)]
mod bench_cost_sidecar_tests {
    use super::write_bench_cost_sidecar_to;

    #[test]
    fn writes_json_file_with_cost_and_iterations() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cost.json");
        write_bench_cost_sidecar_to(&path, 0.4242, 9);
        let body = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["cost_usd"].as_f64(), Some(0.4242));
        assert_eq!(v["iterations"].as_u64(), Some(9));
        assert!(v["ts_unix_ms"].is_u64());
    }

    #[test]
    fn write_is_atomic_via_tmp_then_rename() {
        // After a successful write, only the final path exists — the
        // `.tmp` shadow has been renamed away. A bench polling the
        // final path should never see a half-written file.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cost.json");
        write_bench_cost_sidecar_to(&path, 0.5, 1);
        assert!(path.exists());
        let mut tmp_shadow = path.as_os_str().to_os_string();
        tmp_shadow.push(".tmp");
        assert!(!std::path::Path::new(&tmp_shadow).exists());
    }

    #[test]
    fn creates_parent_dirs_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("deep").join("nested").join("cost.json");
        write_bench_cost_sidecar_to(&path, 0.5, 1);
        assert!(path.exists(), "sidecar should be created with parents");
    }

    #[test]
    fn write_failure_does_not_panic() {
        // Point at a path that can't be created (existing file as a
        // parent dir). The function must swallow the error rather than
        // poison the agent loop.
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let bad_path = blocker.join("cost.json"); // parent is a regular file
        write_bench_cost_sidecar_to(&bad_path, 0.1, 1);
        assert!(!bad_path.exists());
    }

    #[test]
    fn overwrites_existing_file() {
        // The bench may call `th code` multiple times in a sweep against
        // the same task. Each Completed event should clobber the prior
        // sidecar — the freshest cost wins.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cost.json");
        write_bench_cost_sidecar_to(&path, 0.10, 1);
        write_bench_cost_sidecar_to(&path, 0.25, 2);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["cost_usd"].as_f64(), Some(0.25));
        assert_eq!(v["iterations"].as_u64(), Some(2));
    }
}

#[cfg(test)]
mod duration_truth_tests {
    use super::resolve_duration_ms;
    use std::time::{Duration, Instant};

    /// **The th-d49538 regression test.**
    ///
    /// The event loop captures an `Instant` when a tool starts and used to
    /// throw it away on completion, taking the wire's hardcoded `0` instead —
    /// so every tool call in the transcript rendered `0.0s`. With no timing
    /// from the server, the measured elapsed time is the only truth available
    /// and must be what surfaces.
    #[test]
    fn falls_back_to_measured_elapsed_when_the_server_sends_no_timing() {
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        let ms = resolve_duration_ms(None, started);
        assert!(ms >= 20, "expected the measured elapsed time, got {ms}");
        assert!(ms < 60_000, "measurement should be sane, got {ms}");
    }

    /// A server-reported duration is authoritative — it measures the tool
    /// itself rather than the round trip.
    #[test]
    fn server_reported_timing_wins() {
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(resolve_duration_ms(Some(7), started), 7);
    }

    /// A tool that genuinely finished within the same millisecond reports 0 —
    /// that's a measurement, not the old hardcoded placeholder.
    #[test]
    fn instant_tool_reports_zero_from_measurement() {
        assert_eq!(resolve_duration_ms(None, Instant::now()), 0);
    }
}
