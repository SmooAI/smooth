//! Main event loop for the Smooth Coding TUI.

use std::fmt::Write as _;
use std::io;
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
use crate::state::{AppState, ChatMessage, Mode};

/// Run the Smooth Coding TUI.
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
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let state = Arc::new(Mutex::new(AppState::new(working_dir)));
    let (event_tx, event_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Add welcome message
    {
        let mut s = state.lock().expect("state lock poisoned");
        s.add_message(ChatMessage::system("Welcome to Smooth Coding. Type a message and press Enter to chat."));
    }

    let result = event_loop(&mut terminal, &state, &event_tx, event_rx);

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
    execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture)?;
    terminal.show_cursor()?;

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

/// Run a query through the agent framework with channel-based streaming.
///
/// Sends `AgentEvent`s through the channel as the agent processes.
/// The caller's event loop picks them up via `try_recv()`.
async fn run_agent_streaming(message: &str, tx: mpsc::UnboundedSender<AgentEvent>) -> anyhow::Result<()> {
    use smooth_operator::{Agent, AgentConfig, LlmConfig, ToolRegistry};

    // Try to get API key from environment
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .unwrap_or_default();

    if api_key.is_empty() {
        return Err(anyhow::anyhow!(
            "No API key found. Set ANTHROPIC_API_KEY or OPENAI_API_KEY environment variable."
        ));
    }

    let llm_config = LlmConfig::opencode_zen(api_key).with_temperature(0.3);

    let system_prompt = "You are Smooth Coding, an AI coding assistant. Help the user with their coding questions. Be concise and helpful.";

    let config = AgentConfig::new("smooth-coding", system_prompt, llm_config).with_max_iterations(1);

    let tools = ToolRegistry::new();
    let agent = Agent::new(config, tools);

    let _conversation = agent.run_with_channel(message, tx).await?;

    Ok(())
}
