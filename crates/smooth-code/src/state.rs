//! Centralized application state for the coding TUI.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::autocomplete::{AutocompleteState, PearlSuggestion};
use crate::files::FileTree;
use crate::git::GitState;
use crate::model_picker::ModelPickerState;

/// Overall health of startup checks.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// All checks passed.
    Healthy,
    /// Some checks produced warnings.
    Warnings(usize),
    /// Health checks have not run yet.
    #[default]
    Unknown,
}

/// Status of a tool call invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    /// Queued but not yet started.
    Pending,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Done,
    /// Completed with an error.
    Error,
}

impl fmt::Display for ToolStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Done => write!(f, "done"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// State for a single tool call associated with an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallState {
    /// Unique identifier for this tool call.
    pub id: String,
    /// Name of the tool being invoked.
    pub tool_name: String,
    /// First 80 characters of the serialized arguments. Used for the
    /// compact tool-call header line.
    pub arguments_preview: String,
    /// Full deserialized arguments — preserved so the renderer can
    /// produce a unified diff for `edit_file` / `write_file` /
    /// `apply_patch`. `None` when the upstream event didn't carry
    /// parseable JSON (e.g. legacy callers, tools that emit a raw
    /// string). Skipped on serialize so saved sessions don't bloat
    /// with full file contents on every edit.
    #[serde(skip)]
    pub arguments_full: Option<serde_json::Value>,
    /// Tool output (stdout/result), if available.
    pub output: Option<String>,
    /// Current execution status.
    pub status: ToolStatus,
    /// Whether the output is collapsed in the UI.
    pub collapsed: bool,
    /// When the tool call was initiated.
    pub started_at: DateTime<Utc>,
    /// Execution duration in milliseconds, set when finished.
    pub duration_ms: Option<u64>,
}

impl ToolCallState {
    /// Create a new `ToolCallState` with `Running` status and a truncated arguments preview.
    pub fn new(id: impl Into<String>, tool_name: impl Into<String>, arguments: &serde_json::Value) -> Self {
        let args_str = pretty_args_preview(arguments);
        let arguments_preview = truncate_preview(&args_str);

        Self {
            id: id.into(),
            tool_name: tool_name.into(),
            arguments_preview,
            arguments_full: Some(arguments.clone()),
            output: None,
            status: ToolStatus::Running,
            collapsed: true,
            started_at: Utc::now(),
            duration_ms: None,
        }
    }

    /// Convenience constructor used by the WebSocket dispatch path,
    /// where the runner forwards the arguments as a serialized JSON
    /// string. Falls back to `None` for `arguments_full` when the
    /// string isn't valid JSON — the diff renderer will skip it and
    /// the standard collapsed-output path still works.
    pub fn from_raw(id: impl Into<String>, tool_name: impl Into<String>, arguments_json: &str) -> Self {
        let parsed: Option<serde_json::Value> = serde_json::from_str(arguments_json).ok();
        let preview_src = parsed.as_ref().map_or_else(|| arguments_json.to_string(), pretty_args_preview);
        let arguments_preview = truncate_preview(&preview_src);
        Self {
            id: id.into(),
            tool_name: tool_name.into(),
            arguments_preview,
            arguments_full: parsed,
            output: None,
            status: ToolStatus::Running,
            collapsed: true,
            started_at: Utc::now(),
            duration_ms: None,
        }
    }
}

/// Render an argument JSON Value into a compact, human-readable
/// preview for the tool-call header line. Replaces the raw
/// `serde_json::to_string` form (`{"path":"README.md"}`) with
/// shapes that match how the user would describe the call:
///
///   - Empty object → `""`
///   - Single key → just the value (`README.md` instead of
///     `{"path":"README.md"}`)
///   - Multiple keys → `key=value, key=value` form, with strings
///     quoted to keep paths/patterns visually distinct from numbers
///     and booleans (`pattern="postgres", include="*.ts"`).
///   - Arrays / nested objects → fall back to compact JSON.
fn pretty_args_preview(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) if map.is_empty() => String::new(),
        serde_json::Value::Object(map) if map.len() == 1 => {
            let (_, value) = map.iter().next().expect("len == 1");
            format_arg_value(value)
        }
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, val)| format!("{k}={}", format_arg_value(val)))
            .collect::<Vec<_>>()
            .join(", "),
        _ => v.to_string(),
    }
}

/// Format a single argument value for the preview. Strings get
/// quoted (so `"*.ts"` is distinguishable from a bareword), other
/// scalars render as JSON-compact, nested arrays/objects fall
/// back to compact JSON.
fn format_arg_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => format!("\"{s}\""),
        _ => v.to_string(),
    }
}

/// Truncate the preview to ~80 characters. **Char-aware**, not
/// byte-aware: a write_file or edit_file call frequently includes
/// box-drawing glyphs (`│`, `├`, etc.) and non-ASCII text, and
/// slicing by byte index in the middle of a multi-byte UTF-8
/// sequence panics with "byte index N is not a char boundary."
/// That panic poisoned the AppState mutex and cascaded the whole
/// TUI process. Counting and slicing by chars sidesteps both.
fn truncate_preview(s: &str) -> String {
    const MAX_CHARS: usize = 80;
    if s.chars().count() > MAX_CHARS {
        let truncated: String = s.chars().take(MAX_CHARS).collect();
        format!("{truncated}...")
    } else {
        s.to_string()
    }
}

/// Which panel currently has keyboard focus.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    /// The chat / message list.
    Chat,
    /// The text input area.
    #[default]
    Input,
    /// The sidebar file browser.
    Sidebar,
}

/// The current input mode of the TUI.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Normal mode — keyboard shortcuts active, no text input.
    Normal,
    /// Input mode — typing into the message box.
    #[default]
    Input,
}

/// Role of a chat message sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    System,
}

impl fmt::Display for ChatRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "You"),
            Self::Assistant => write!(f, "Smooth"),
            Self::System => write!(f, "System"),
        }
    }
}

/// A single chat message in the conversation.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: String,
    pub role: ChatRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// Tool calls associated with this message (only meaningful for assistant messages).
    pub tool_calls: Vec<ToolCallState>,
    /// Whether this message is currently being streamed from the agent.
    pub streaming: bool,
}

impl ChatMessage {
    /// Create a new chat message with an auto-generated ID and current timestamp.
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role,
            content: content.into(),
            timestamp: Utc::now(),
            tool_calls: Vec::new(),
            streaming: false,
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ChatRole::User, content)
    }

    /// Create an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(ChatRole::Assistant, content)
    }

    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(ChatRole::System, content)
    }
}

/// Centralized application state.
#[allow(clippy::struct_excessive_bools)]
pub struct AppState {
    /// Current input mode.
    pub mode: Mode,
    /// Which panel currently has keyboard focus.
    pub focus: FocusPanel,
    /// Working directory for the coding session.
    pub working_dir: PathBuf,
    /// Unique session identifier.
    pub session_id: String,
    /// Short human-readable title for this session (3–6 words, Title
    /// Case) generated via the `smooth-fast` slot from the user's
    /// first message. `None` until the first real user message is
    /// sent, and remains `None` when the LLM auto-name call fails.
    pub session_title: Option<String>,
    /// Chat message history.
    pub messages: Vec<ChatMessage>,
    /// Number of leading messages that have already been flushed into
    /// the terminal's scrollback via `Frame::insert_before`. Inline-
    /// viewport mode pushes finalized messages into the terminal's
    /// own scroll buffer (so native wheel-scroll, drag-select, and
    /// search all work) and only keeps the in-flight streaming
    /// message inside the viewport. The render path skips messages
    /// at indices `< committed_count` so they don't double-paint
    /// inside the viewport while also living above it as scrollback.
    pub committed_count: usize,
    /// Current text in the input box.
    pub input: String,
    /// Cursor position within the input string (byte offset).
    pub input_cursor: usize,
    /// Whether the sidebar panel is visible.
    pub sidebar_visible: bool,
    /// Scroll offset for the chat area (lines from bottom).
    pub scroll_offset: usize,
    /// Whether the user has manually scrolled up.
    pub user_scrolled: bool,
    /// Display name of the current LLM model.
    pub model_name: String,
    /// User-supplied model override from `th code --model <X>`. When
    /// `Some(_)`, every `TaskStart` dispatched to Big Smooth carries
    /// this value verbatim in its `model` field; Big Smooth's routing
    /// then resolves it against the configured providers (a smooth-*
    /// alias OR a concrete model id like `deepseek-v4-flash`). Pearl
    /// `th-20574a` — before this field existed, the CLI's `--model`
    /// flag was silently dropped on the TUI path and every run used
    /// the default smooth-coding alias regardless of what the user
    /// asked for. `None` (the default) preserves the legacy behavior
    /// of letting Big Smooth pick.
    pub model_override: Option<String>,
    /// Active Smooth Mode id (`flash` / `code+` / `max` / …). Pins each turn
    /// to a concrete model — the mode's `model` is sent on every
    /// `send_message`. Set via `/smooth-mode <preset>`; defaults to
    /// [`crate::modes::DEFAULT_MODE_ID`] (`flash`). Web ↔ TUI parity:
    /// mirrors the composer's `/smooth-mode` switcher (th-f512b1).
    pub mode_id: String,
    /// Per-model cost table fetched once (best-effort) from
    /// `GET {operator}/admin/model-costs`. Empty when the endpoint is
    /// unreachable — the status bar degrades to the mode's tier for the
    /// premium warning and omits the cost badge (th-2a6330).
    pub model_costs: crate::modes::ModelCosts,
    /// Active lead role name (`fixer` / `mapper` / `oracle` / `heckler`).
    /// Flows into every `TaskStart` so the runner applies the right
    /// clearance set; rendered on the status bar so the user can see
    /// which role is active without re-running `th --agent`.
    pub agent_name: String,
    /// `true` once the user has explicitly chosen a role via `/agent`,
    /// `/ask`, or the `--agent` CLI flag. While `false`, the dispatch
    /// path runs an intent classifier on each message and may switch
    /// between `fixer` (for work) and `oracle` (for questions). This
    /// stops the chat from sliding into "writes files for read-only
    /// questions" by default while still letting the user pin a role.
    pub agent_pinned: bool,
    /// When `false` (default), the renderer hides the trailing
    /// `[runner stderr] ... [cast-summary]` diagnostic block from
    /// every assistant message. Toggle with `/verbose`. The content
    /// stays in `msg.content` so saved sessions round-trip — only
    /// the rendered output skips it.
    pub verbose: bool,
    /// Running total of tokens used this session.
    pub total_tokens: u32,
    /// Running total of spend in USD this session. Accumulated on
    /// every `ServerEvent::TaskComplete` — the value the server
    /// reports is already authoritative (LiteLLM response-cost
    /// header when the gateway sets it, ModelPricing fallback
    /// otherwise), so the client just sums them.
    pub total_cost_usd: f64,
    /// Current workflow phase from the latest `PhaseStart` event
    /// (ASSESS / PLAN / EXECUTE / VERIFY / REVIEW / FINALIZE). `None`
    /// outside a workflow-managed run. Shown in the status bar next
    /// to the rotating thesaurus phrase.
    pub current_phase: Option<String>,
    /// Routing alias for the current phase, e.g. `smooth-thinking`.
    pub current_phase_alias: Option<String>,
    /// Concrete upstream model for the current phase when known.
    /// `None` until the gateway tells us.
    pub current_phase_upstream: Option<String>,
    /// Index into the thesaurus for the current phase. Advances on
    /// every spinner tick so long phases feel alive.
    pub phrase_idx: usize,
    /// Flag to exit the main loop.
    pub should_quit: bool,
    /// Whether the agent is currently processing a request.
    pub thinking: bool,
    /// Current frame index for the braille spinner animation.
    pub spinner_frame: usize,
    /// File tree for the sidebar browser.
    pub file_tree: Option<FileTree>,
    /// Autocomplete state for @ references.
    pub autocomplete: AutocompleteState,
    /// Pearls exposed to the `@` picker. Pre-fetched once at app
    /// startup from `~/.smooth/dolt/` so the autocomplete refresh
    /// doesn't pay a Dolt query per keystroke. `None` when the
    /// store isn't reachable (no pearls created yet, smooth-dolt
    /// missing, etc.) — picker silently omits pearls in that case.
    pub pearls: Vec<PearlSuggestion>,
    /// Current git repository state (populated by `GitState::refresh`).
    pub git_state: Option<GitState>,
    /// Model picker popup state.
    pub model_picker: ModelPickerState,
    /// Startup health check status.
    pub health_status: HealthStatus,
    /// An in-flight write-tool approval the operator parked the turn on
    /// (`write_confirmation_required`). When `Some`, the TUI shows an approve/deny
    /// prompt; the dispatch loop polls `decision` and resumes the turn via
    /// `OperatorClient::confirm` (th-1ea4f6).
    pub pending_confirmation: Option<PendingConfirmation>,
}

/// A parked write-tool approval awaiting the user's verdict.
#[derive(Debug, Clone)]
pub struct PendingConfirmation {
    /// The parked turn's request id (echoed back in `confirm_tool_action`).
    pub request_id: String,
    /// The tool the agent wants to run (e.g. `bash`).
    pub tool: String,
    /// Human-readable description of the action to approve.
    pub description: String,
    /// `None` until the user decides; `Some(true)` = approve, `Some(false)` = deny.
    pub decision: Option<bool>,
}

impl AppState {
    /// Create a new `AppState` for the given working directory.
    pub fn new(working_dir: PathBuf) -> Self {
        let file_tree = FileTree::from_dir(&working_dir).ok();
        Self {
            mode: Mode::default(),
            focus: FocusPanel::default(),
            working_dir,
            session_id: Uuid::new_v4().to_string(),
            session_title: None,
            messages: Vec::new(),
            committed_count: 0,
            input: String::new(),
            input_cursor: 0,
            sidebar_visible: false,
            scroll_offset: 0,
            user_scrolled: false,
            model_name: "claude-sonnet-4".to_string(),
            model_override: None,
            mode_id: crate::modes::DEFAULT_MODE_ID.to_string(),
            model_costs: crate::modes::ModelCosts::new(),
            agent_name: "fixer".to_string(),
            agent_pinned: false,
            verbose: false,
            total_tokens: 0,
            total_cost_usd: 0.0,
            current_phase: None,
            current_phase_alias: None,
            current_phase_upstream: None,
            phrase_idx: 0,
            should_quit: false,
            thinking: false,
            spinner_frame: 0,
            file_tree,
            autocomplete: AutocompleteState::default(),
            pearls: Vec::new(),
            git_state: None,
            model_picker: ModelPickerState::new(),
            health_status: HealthStatus::default(),
            pending_confirmation: None,
        }
    }

    /// Restore an `AppState` from a persisted session. Keeps the
    /// session's id, title, messages, model, agent, and token count;
    /// resets everything else (cursor, focus, spinner) to fresh defaults.
    pub fn from_resumed_session(working_dir: PathBuf, session: &crate::session::Session) -> Self {
        let mut state = Self::new(working_dir);
        state.session_id = session.id.clone();
        state.session_title = session.title.clone();
        state.messages = session.messages.iter().map(|m| m.to_chat_message()).collect();
        state.model_name = session.model_name.clone();
        state.total_tokens = session.total_tokens;
        if let Some(ref a) = session.agent_name {
            state.agent_name = a.clone();
            // Resuming a session means the user already picked an
            // agent for it — preserve their choice instead of letting
            // the intent classifier flip it on the first new message.
            state.agent_pinned = true;
        }
        state
    }

    /// The active [`Mode`](crate::modes::Mode), resolved from [`Self::mode_id`].
    /// Falls back to the default mode (`flash`) when the stored id is unknown.
    pub fn active_mode(&self) -> &'static crate::modes::Mode {
        crate::modes::mode_by_id(&self.mode_id)
    }

    /// The model id to send on the next turn: an explicit `--model` CLI
    /// override wins; otherwise the active mode's model. Keeps the legacy
    /// `--model` flag working while letting `/smooth-mode` drive routing.
    pub fn turn_model(&self) -> String {
        self.model_override.clone().unwrap_or_else(|| self.active_mode().model.to_string())
    }

    /// Add a message to the conversation history.
    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        // Auto-scroll to bottom when not manually scrolled
        if !self.user_scrolled {
            self.scroll_offset = 0;
        }
    }

    /// Insert a character at the current cursor position.
    ///
    /// Newlines and carriage returns are normalized to spaces — the
    /// input box is single-line, and bracketed paste is best-effort
    /// (pearl th-paste-crash). If bracketed paste isn't supported by
    /// the terminal, multi-line pastes arrive as a stream of Char
    /// events including embedded \n / \r; without this clamp those
    /// would render as literal newlines in the Paragraph and trigger
    /// the inline-viewport buffer overflow panic seen in
    /// `index outside of buffer: ...`.
    pub fn input_insert(&mut self, ch: char) {
        let ch = if matches!(ch, '\n' | '\r') { ' ' } else { ch };
        self.input.insert(self.input_cursor, ch);
        self.input_cursor += ch.len_utf8();
    }

    /// Delete the character before the cursor (backspace).
    pub fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            // Find the previous char boundary
            let prev = self.input[..self.input_cursor].char_indices().next_back().map_or(0, |(i, _)| i);
            self.input.remove(prev);
            self.input_cursor = prev;
        }
    }

    /// Move the input cursor one character to the left.
    pub fn input_move_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor = self.input[..self.input_cursor].char_indices().next_back().map_or(0, |(i, _)| i);
        }
    }

    /// Move the input cursor one character to the right.
    pub fn input_move_right(&mut self) {
        if self.input_cursor < self.input.len() {
            self.input_cursor = self.input[self.input_cursor..]
                .char_indices()
                .nth(1)
                .map_or(self.input.len(), |(i, _)| self.input_cursor + i);
        }
    }

    /// Take the current input, clearing it and resetting the cursor.
    pub fn take_input(&mut self) -> String {
        self.input_cursor = 0;
        std::mem::take(&mut self.input)
    }

    /// Clear the input buffer.
    pub fn input_clear(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
    }

    /// Add a tool call to the last assistant message.
    ///
    /// If the last message is not an assistant message, this is a no-op.
    pub fn add_tool_call(&mut self, id: &str, tool_name: &str, arguments: &serde_json::Value) {
        if let Some(msg) = self.messages.last_mut() {
            if msg.role == ChatRole::Assistant {
                msg.tool_calls.push(ToolCallState::new(id, tool_name, arguments));
            }
        }
    }

    /// Update an existing tool call by ID — set output, error status, and duration.
    ///
    /// Searches all messages for the matching tool call.
    pub fn update_tool_call(&mut self, id: &str, output: &str, is_error: bool, duration_ms: u64) {
        for msg in &mut self.messages {
            for tc in &mut msg.tool_calls {
                if tc.id == id {
                    tc.output = Some(output.to_string());
                    tc.status = if is_error { ToolStatus::Error } else { ToolStatus::Done };
                    tc.duration_ms = Some(duration_ms);
                    return;
                }
            }
        }
    }

    /// Toggle the collapsed state of a tool call by ID.
    pub fn toggle_tool_collapse(&mut self, id: &str) {
        for msg in &mut self.messages {
            for tc in &mut msg.tool_calls {
                if tc.id == id {
                    tc.collapsed = !tc.collapsed;
                    return;
                }
            }
        }
    }

    /// Braille spinner frames used for streaming animation.
    const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    /// Advance the spinner to the next frame, cycling through all 10 braille frames.
    /// Also advances the phase thesaurus index — spinner ticks every ~100 ms
    /// so the phrase rotates every ~3 seconds (every 30 ticks). The TUI reads
    /// `phrase_idx / 30 % phrases_len` at render time.
    pub fn advance_spinner(&mut self) {
        self.spinner_frame = (self.spinner_frame + 1) % Self::SPINNER_FRAMES.len();
        self.phrase_idx = self.phrase_idx.wrapping_add(1);
    }

    /// Get the current spinner character.
    pub fn spinner_char(&self) -> &str {
        Self::SPINNER_FRAMES[self.spinner_frame % Self::SPINNER_FRAMES.len()]
    }

    /// Start streaming: create an empty assistant message with
    /// `streaming = true`. Idempotent — a no-op when the last message
    /// is already a streaming assistant message, so callers can call
    /// it both eagerly (synchronously, before tool calls land) and
    /// lazily (from the agent-event handler) without producing a
    /// duplicate empty message.
    pub fn start_streaming(&mut self) {
        let already_streaming = self.messages.last().is_some_and(|m| m.role == ChatRole::Assistant && m.streaming);
        if !already_streaming {
            let mut msg = ChatMessage::assistant("");
            msg.streaming = true;
            self.add_message(msg);
        }
        self.thinking = true;
    }

    /// Append content to the last streaming assistant message.
    ///
    /// Raw content goes in verbatim — ANSI escape sequences from the
    /// runner are preserved so the renderer can turn them into actual
    /// colored Spans instead of either stripping them or showing
    /// `[2m...[0m` as plain text.
    ///
    /// No-op if the last message is not a streaming assistant message.
    pub fn append_stream_content(&mut self, content: &str) {
        if let Some(msg) = self.messages.last_mut() {
            if msg.role == ChatRole::Assistant && msg.streaming {
                msg.content.push_str(content);
                // Auto-scroll to bottom when not manually scrolled
                if !self.user_scrolled {
                    self.scroll_offset = 0;
                }
            }
        }
    }

    /// Finish streaming: set `streaming = false` on the last message and clear thinking.
    pub fn finish_streaming(&mut self) {
        if let Some(msg) = self.messages.last_mut() {
            if msg.role == ChatRole::Assistant && msg.streaming {
                msg.streaming = false;
            }
        }
        self.thinking = false;
    }

    /// Start a fresh streaming ChatMessage for a new agent iteration
    /// within the same user turn. Pearl th-486bd0: coding-workflow
    /// runs emit multiple LlmRequest/PhaseStart events per user
    /// message (ASSESS → THINK → EXECUTE → VERIFY phases, or even
    /// just a multi-iteration agent loop). `start_streaming` is
    /// idempotent and would have appended each iteration's stream
    /// content into the SAME ChatMessage — producing massive
    /// concatenated walls of text with duplications across iteration
    /// boundaries (the `III'll help` / `LetLet me me` patterns
    /// observed in the bench). This finishes the current streaming
    /// message (only when it has content; an empty stub gets dropped
    /// so we don't litter the chat with empty bubbles) and starts a
    /// fresh one.
    pub fn start_iteration(&mut self) {
        // If the last message is a streaming assistant message AND
        // has no content, drop it — happens when LlmRequest fires
        // before any TokenDelta on the prior iteration (e.g. the
        // iteration was tool-call only). Otherwise finalize it.
        if let Some(last) = self.messages.last() {
            if last.role == ChatRole::Assistant && last.streaming && last.content.is_empty() {
                self.messages.pop();
            } else {
                self.finish_streaming();
            }
        }
        let mut msg = ChatMessage::assistant("");
        msg.streaming = true;
        self.add_message(msg);
        self.thinking = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a clean temp directory for tests instead of using `/tmp` which
    /// may contain files that cause non-deterministic sort panics on CI.
    fn test_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    #[test]
    fn test_health_status_variants() {
        assert_eq!(HealthStatus::default(), HealthStatus::Unknown);
        assert_eq!(HealthStatus::Healthy, HealthStatus::Healthy);
        assert_eq!(HealthStatus::Warnings(3), HealthStatus::Warnings(3));
        assert_ne!(HealthStatus::Healthy, HealthStatus::Unknown);
        assert_ne!(HealthStatus::Warnings(1), HealthStatus::Warnings(2));
    }

    #[test]
    fn test_health_warnings_generate_system_message() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        let warnings = ["API not running".to_string(), "No providers".to_string()];

        let warning_text = format!(
            "\u{26a0} Health Check:\n{}",
            warnings.iter().map(|w| format!("  \u{2022} {w}")).collect::<Vec<_>>().join("\n")
        );
        state.add_message(ChatMessage::new(ChatRole::System, warning_text));
        state.health_status = HealthStatus::Warnings(warnings.len());

        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].role, ChatRole::System);
        assert!(state.messages[0].content.contains("API not running"));
        assert!(state.messages[0].content.contains("No providers"));
        assert_eq!(state.health_status, HealthStatus::Warnings(2));
    }

    #[test]
    fn test_health_no_warnings_no_extra_message() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        // Simulate healthy status — no messages added
        state.health_status = HealthStatus::Healthy;

        assert!(state.messages.is_empty());
        assert_eq!(state.health_status, HealthStatus::Healthy);
    }

    #[test]
    fn test_state_creation_defaults() {
        let (_dir, path) = test_dir();
        let state = AppState::new(path.clone());
        assert_eq!(state.mode, Mode::Input);
        assert_eq!(state.working_dir, path);
        assert!(state.messages.is_empty());
        assert!(state.input.is_empty());
        assert_eq!(state.input_cursor, 0);
        assert!(!state.sidebar_visible);
        assert_eq!(state.scroll_offset, 0);
        assert!(!state.user_scrolled);
        assert!(!state.should_quit);
        assert!(!state.thinking);
        assert_eq!(state.total_tokens, 0);
    }

    #[test]
    fn test_chat_message_constructors() {
        let user = ChatMessage::user("hello");
        assert_eq!(user.role, ChatRole::User);
        assert_eq!(user.content, "hello");

        let assistant = ChatMessage::assistant("hi there");
        assert_eq!(assistant.role, ChatRole::Assistant);
        assert_eq!(assistant.content, "hi there");

        let system = ChatMessage::system("init");
        assert_eq!(system.role, ChatRole::System);
        assert_eq!(system.content, "init");
    }

    #[test]
    fn test_add_message() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        assert!(state.messages.is_empty());

        state.add_message(ChatMessage::user("test"));
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "test");
    }

    #[test]
    fn test_input_insert_and_cursor() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.input_insert('h');
        state.input_insert('i');
        assert_eq!(state.input, "hi");
        assert_eq!(state.input_cursor, 2);
    }

    #[test]
    fn test_input_backspace() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.input_insert('a');
        state.input_insert('b');
        state.input_backspace();
        assert_eq!(state.input, "a");
        assert_eq!(state.input_cursor, 1);

        // Backspace at position 0 does nothing
        state.input_backspace();
        assert!(state.input.is_empty());
        state.input_backspace(); // no panic
    }

    #[test]
    fn test_input_move_left_right() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.input_insert('a');
        state.input_insert('b');
        state.input_insert('c');
        assert_eq!(state.input_cursor, 3);

        state.input_move_left();
        assert_eq!(state.input_cursor, 2);

        state.input_move_left();
        assert_eq!(state.input_cursor, 1);

        state.input_move_right();
        assert_eq!(state.input_cursor, 2);

        // Move left past beginning does nothing
        state.input_cursor = 0;
        state.input_move_left();
        assert_eq!(state.input_cursor, 0);

        // Move right past end does nothing
        state.input_cursor = 3;
        state.input_move_right();
        assert_eq!(state.input_cursor, 3);
    }

    #[test]
    fn test_take_input() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.input_insert('h');
        state.input_insert('i');
        let taken = state.take_input();
        assert_eq!(taken, "hi");
        assert!(state.input.is_empty());
        assert_eq!(state.input_cursor, 0);
    }

    #[test]
    fn test_input_clear() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.input_insert('x');
        state.input_clear();
        assert!(state.input.is_empty());
        assert_eq!(state.input_cursor, 0);
    }

    #[test]
    fn test_chat_role_display() {
        assert_eq!(format!("{}", ChatRole::User), "You");
        assert_eq!(format!("{}", ChatRole::Assistant), "Smooth");
        assert_eq!(format!("{}", ChatRole::System), "System");
    }

    #[test]
    fn test_mode_default() {
        assert_eq!(Mode::default(), Mode::Input);
    }

    #[test]
    fn test_tool_call_state_creation() {
        let args = serde_json::json!({"file": "src/main.rs"});
        let tc = ToolCallState::new("tc-1", "edit_file", &args);
        assert_eq!(tc.id, "tc-1");
        assert_eq!(tc.tool_name, "edit_file");
        assert_eq!(tc.status, ToolStatus::Running);
        assert!(tc.collapsed);
        assert!(tc.output.is_none());
        assert!(tc.duration_ms.is_none());
    }

    #[test]
    fn test_tool_status_display() {
        assert_eq!(format!("{}", ToolStatus::Pending), "pending");
        assert_eq!(format!("{}", ToolStatus::Running), "running");
        assert_eq!(format!("{}", ToolStatus::Done), "done");
        assert_eq!(format!("{}", ToolStatus::Error), "error");
    }

    #[test]
    fn test_add_tool_call_to_last_assistant_message() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.add_message(ChatMessage::assistant("thinking..."));
        let args = serde_json::json!({"cmd": "cargo test"});
        state.add_tool_call("tc-1", "bash", &args);

        assert_eq!(state.messages[0].tool_calls.len(), 1);
        assert_eq!(state.messages[0].tool_calls[0].tool_name, "bash");

        // Adding to a user message is a no-op
        state.add_message(ChatMessage::user("hello"));
        state.add_tool_call("tc-2", "read_file", &serde_json::json!({}));
        assert!(state.messages[1].tool_calls.is_empty());
    }

    #[test]
    fn test_update_tool_call_done_and_error() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.add_message(ChatMessage::assistant("working"));
        state.add_tool_call("tc-1", "bash", &serde_json::json!({}));
        state.add_tool_call("tc-2", "read_file", &serde_json::json!({}));

        // Update tc-1 as done
        state.update_tool_call("tc-1", "ok", false, 2300);
        let tc1 = &state.messages[0].tool_calls[0];
        assert_eq!(tc1.status, ToolStatus::Done);
        assert_eq!(tc1.output.as_deref(), Some("ok"));
        assert_eq!(tc1.duration_ms, Some(2300));

        // Update tc-2 as error
        state.update_tool_call("tc-2", "File not found", true, 50);
        let tc2 = &state.messages[0].tool_calls[1];
        assert_eq!(tc2.status, ToolStatus::Error);
        assert_eq!(tc2.output.as_deref(), Some("File not found"));
    }

    #[test]
    fn test_toggle_tool_collapse() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.add_message(ChatMessage::assistant("hi"));
        state.add_tool_call("tc-1", "bash", &serde_json::json!({}));

        assert!(state.messages[0].tool_calls[0].collapsed);
        state.toggle_tool_collapse("tc-1");
        assert!(!state.messages[0].tool_calls[0].collapsed);
        state.toggle_tool_collapse("tc-1");
        assert!(state.messages[0].tool_calls[0].collapsed);
    }

    #[test]
    fn test_arguments_preview_truncation() {
        let long_value = "x".repeat(200);
        let args = serde_json::json!({"data": long_value});
        let tc = ToolCallState::new("tc-1", "write_file", &args);
        // The preview should truncate at 80 chars + "...". The exact
        // shape comes from `pretty_args_preview` (single-key object
        // → just the value, surrounded by quotes), but truncation
        // logic is the only thing this test asserts.
        assert_eq!(tc.arguments_preview.len(), 83);
        assert!(tc.arguments_preview.ends_with("..."));
    }

    #[test]
    fn test_arguments_preview_truncation_handles_multibyte_chars() {
        // Regression for the panic that crashed the TUI on a write_file
        // call whose new_string contained box-drawing glyphs (`│` is
        // 3 bytes). Slicing by byte index 80 landed inside the multi-
        // byte sequence and panicked. Char-aware truncation makes this
        // safe regardless of where char boundaries fall.
        let body = "│".repeat(100); // 100 chars, 300 bytes
        let args = serde_json::json!({"new_string": body});
        let tc = ToolCallState::new("tc-x", "write_file", &args);
        // Must not panic and must end with "..."
        assert!(tc.arguments_preview.ends_with("..."));
        // Must contain only complete `│` characters in the truncated
        // section — never a partial byte sequence.
        let body_part = tc.arguments_preview.trim_end_matches("...").trim_start_matches('"');
        for ch in body_part.chars() {
            assert!(ch == '│' || ch == '"', "unexpected char {ch:?} in truncated preview");
        }
    }

    #[test]
    fn test_arguments_preview_pretty_shapes() {
        // Empty object → empty preview (no-arg tool).
        let tc = ToolCallState::new("tc-1", "project_inspect", &serde_json::json!({}));
        assert_eq!(tc.arguments_preview, "");

        // Single-key object → unwrapped to the quoted value.
        let tc = ToolCallState::new("tc-2", "read_file", &serde_json::json!({"path": "README.md"}));
        assert_eq!(tc.arguments_preview, "\"README.md\"");

        // Multi-key object → key=value, key=value with strings quoted.
        let tc = ToolCallState::new("tc-3", "grep", &serde_json::json!({"pattern": "postgres", "include": "*.ts"}));
        // Order isn't strictly stable across serde_json versions, but
        // both keys should appear with their string values quoted.
        assert!(tc.arguments_preview.contains(r#"pattern="postgres""#), "preview={}", tc.arguments_preview);
        assert!(tc.arguments_preview.contains(r#"include="*.ts""#), "preview={}", tc.arguments_preview);
    }

    #[test]
    fn test_streaming_field_defaults_to_false() {
        let msg = ChatMessage::user("hello");
        assert!(!msg.streaming);
        let msg = ChatMessage::assistant("hi");
        assert!(!msg.streaming);
        let msg = ChatMessage::system("init");
        assert!(!msg.streaming);
    }

    #[test]
    fn test_start_streaming_creates_streaming_message() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.start_streaming();

        assert_eq!(state.messages.len(), 1);
        let msg = &state.messages[0];
        assert_eq!(msg.role, ChatRole::Assistant);
        assert!(msg.content.is_empty());
        assert!(msg.streaming);
        assert!(state.thinking);
    }

    #[test]
    fn test_append_stream_content() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.start_streaming();

        state.append_stream_content("Hello");
        assert_eq!(state.messages[0].content, "Hello");

        state.append_stream_content(", world!");
        assert_eq!(state.messages[0].content, "Hello, world!");

        // Append to non-streaming message is a no-op
        state.messages[0].streaming = false;
        state.append_stream_content(" extra");
        assert_eq!(state.messages[0].content, "Hello, world!");
    }

    #[test]
    fn test_finish_streaming() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        state.start_streaming();
        state.append_stream_content("done");
        state.finish_streaming();

        assert!(!state.messages[0].streaming);
        assert!(!state.thinking);
        assert_eq!(state.messages[0].content, "done");
    }

    #[test]
    fn test_advance_spinner_cycles() {
        let (_dir, path) = test_dir();
        let mut state = AppState::new(path);
        assert_eq!(state.spinner_frame, 0);
        assert_eq!(state.spinner_char(), "⠋");

        state.advance_spinner();
        assert_eq!(state.spinner_frame, 1);
        assert_eq!(state.spinner_char(), "⠙");

        // Cycle through all 10 frames back to 0
        for _ in 0..9 {
            state.advance_spinner();
        }
        assert_eq!(state.spinner_frame, 0);
        assert_eq!(state.spinner_char(), "⠋");
    }
}
