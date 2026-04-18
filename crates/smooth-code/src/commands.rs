//! Slash command registry for the Smooth TUI.
//!
//! Provides a [`CommandRegistry`] that maps `/command` names to handlers.
//! Built-in commands include `/help`, `/clear`, `/model`, `/save`,
//! `/sessions`, `/quit`, `/status`, and `/compact`.

use std::collections::HashMap;
use std::fmt;

use crate::extensions::ExtensionRegistry;
use crate::git::GitState;
use crate::state::AppState;

/// Output produced by a slash command handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutput {
    /// Display a system message in the chat area.
    Message(String),
    /// Clear the chat history.
    Clear,
    /// Exit the TUI.
    Quit,
    /// No visible output.
    None,
}

impl fmt::Display for CommandOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(msg) => write!(f, "{msg}"),
            Self::Clear => write!(f, "[clear]"),
            Self::Quit => write!(f, "[quit]"),
            Self::None => write!(f, "[none]"),
        }
    }
}

/// A function that handles a slash command invocation.
pub type CommandHandler = Box<dyn Fn(&str, &mut AppState) -> anyhow::Result<CommandOutput> + Send + Sync>;

/// Definition of a single slash command.
pub struct CommandDef {
    /// The command name (without the leading `/`).
    pub name: String,
    /// Human-readable description shown in `/help`.
    pub description: String,
    /// The handler function.
    pub handler: CommandHandler,
}

/// Registry of all available slash commands.
pub struct CommandRegistry {
    commands: HashMap<String, CommandDef>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRegistry {
    /// Create a new registry pre-populated with the built-in commands.
    pub fn new() -> Self {
        let mut reg = Self { commands: HashMap::new() };
        reg.register_builtins();
        reg
    }

    /// Register a command in the registry.
    pub fn register(&mut self, name: impl Into<String>, description: impl Into<String>, handler: CommandHandler) {
        let name = name.into();
        self.commands.insert(
            name.clone(),
            CommandDef {
                name,
                description: description.into(),
                handler,
            },
        );
    }

    /// Execute a command by name, passing the argument string and mutable state.
    ///
    /// Returns `None` if the command is not found.
    pub fn execute(&self, name: &str, args: &str, state: &mut AppState) -> Option<anyhow::Result<CommandOutput>> {
        self.commands.get(name).map(|def| (def.handler)(args, state))
    }

    /// Return a sorted list of `(name, description)` pairs for all registered commands.
    pub fn list_commands(&self) -> Vec<(String, String)> {
        let mut cmds: Vec<_> = self.commands.values().map(|d| (d.name.clone(), d.description.clone())).collect();
        cmds.sort_by(|a, b| a.0.cmp(&b.0));
        cmds
    }

    /// Register all built-in commands.
    fn register_builtins(&mut self) {
        // /help
        self.register("help", "List all available commands", Box::new(cmd_help));

        // /clear
        self.register("clear", "Clear chat history", Box::new(cmd_clear));

        // /model
        self.register("model", "Show or switch model: /model [name]", Box::new(cmd_model));

        // /save
        self.register("save", "Force save the current session", Box::new(cmd_save));

        // /sessions
        self.register("sessions", "List saved sessions", Box::new(cmd_sessions));

        // /quit
        self.register("quit", "Exit the TUI", Box::new(cmd_quit));

        // /status
        self.register("status", "Show system status (tokens, cost, model)", Box::new(cmd_status));

        // /compact
        self.register("compact", "Trigger context compaction", Box::new(cmd_compact));

        // /git
        self.register(
            "git",
            "Git operations: /git status | diff [file] | stage <file> | unstage <file> | commit <message>",
            Box::new(cmd_git),
        );

        // /tree
        self.register("tree", "Show session tree structure as ASCII art", Box::new(cmd_tree));

        // /fork
        self.register("fork", "Fork from current point (creates new branch)", Box::new(cmd_fork));

        // /goto
        self.register("goto", "Navigate to a specific point in history: /goto <id>", Box::new(cmd_goto));

        // /skill
        self.register("skill", "Invoke a skill: /skill (list) or /skill:<name> [args]", Box::new(cmd_skill));

        // /rename
        self.register("rename", "Rename the current session: /rename <title>", Box::new(cmd_rename));
    }

    /// Execute a slash command, handling `/skill:name` syntax by splitting the colon-separated
    /// skill name from the command prefix.
    ///
    /// Returns `None` if the command is not found.
    pub fn execute_input(&self, name: &str, args: &str, state: &mut AppState) -> Option<anyhow::Result<CommandOutput>> {
        // Check for `/skill:name` pattern
        if let Some(skill_name) = name.strip_prefix("skill:") {
            // Reconstruct args as "skill_name rest_of_args"
            let combined = if args.is_empty() {
                skill_name.to_string()
            } else {
                format!("{skill_name} {args}")
            };
            return self.execute("skill", &combined, state);
        }
        self.execute(name, args, state)
    }
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_help(_args: &str, _state: &mut AppState) -> anyhow::Result<CommandOutput> {
    // Build the help text from a temporary registry to avoid borrowing issues.
    let reg = CommandRegistry::new();
    let cmds = reg.list_commands();
    let mut lines = vec!["Available commands:".to_string()];
    for (name, desc) in &cmds {
        lines.push(format!("  /{name} — {desc}"));
    }
    Ok(CommandOutput::Message(lines.join("\n")))
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_clear(_args: &str, state: &mut AppState) -> anyhow::Result<CommandOutput> {
    state.messages.clear();
    state.scroll_offset = 0;
    state.user_scrolled = false;
    Ok(CommandOutput::Clear)
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_model(args: &str, state: &mut AppState) -> anyhow::Result<CommandOutput> {
    let args = args.trim();
    if args.is_empty() {
        // No args — open the model picker popup
        state.model_picker.activate();
        Ok(CommandOutput::None)
    } else {
        let old = state.model_name.clone();
        state.model_name = args.to_string();
        Ok(CommandOutput::Message(format!(
            "Model switched: {old} -> {} (current: {})",
            state.model_name, state.model_name
        )))
    }
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_save(_args: &str, _state: &mut AppState) -> anyhow::Result<CommandOutput> {
    // Actual save logic is handled by the caller after seeing this output.
    Ok(CommandOutput::Message("Session saved.".to_string()))
}

fn cmd_sessions(_args: &str, _state: &mut AppState) -> anyhow::Result<CommandOutput> {
    use crate::session::SessionManager;

    let mgr = SessionManager::new()?;
    let summaries = mgr.list()?;
    if summaries.is_empty() {
        return Ok(CommandOutput::Message("No saved sessions.".to_string()));
    }
    let mut lines = vec!["Saved sessions:".to_string()];
    for s in &summaries {
        lines.push(format!(
            "  {} — {} ({} msgs, {})",
            s.id,
            s.preview,
            s.message_count,
            s.updated_at.format("%Y-%m-%d %H:%M")
        ));
    }
    Ok(CommandOutput::Message(lines.join("\n")))
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_quit(_args: &str, state: &mut AppState) -> anyhow::Result<CommandOutput> {
    state.should_quit = true;
    Ok(CommandOutput::Quit)
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_status(_args: &str, state: &mut AppState) -> anyhow::Result<CommandOutput> {
    let status = format!(
        "Model: {}\nTokens used: {}\nMessages: {}\nSession: {}",
        state.model_name,
        state.total_tokens,
        state.messages.len(),
        state.session_id,
    );
    Ok(CommandOutput::Message(status))
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_compact(_args: &str, _state: &mut AppState) -> anyhow::Result<CommandOutput> {
    // Placeholder — real compaction would summarise older messages.
    Ok(CommandOutput::Message("Context compaction triggered (not yet implemented).".to_string()))
}

fn cmd_git(args: &str, state: &mut AppState) -> anyhow::Result<CommandOutput> {
    let args = args.trim();
    let (sub, rest) = args.split_once(' ').unwrap_or((args, ""));
    let rest = rest.trim();

    match sub {
        "status" => {
            let git_state = GitState::refresh(&state.working_dir)?;
            state.git_state = Some(git_state.clone());

            if !git_state.is_repo {
                return Ok(CommandOutput::Message("Not a git repository.".to_string()));
            }

            let mut lines = vec![format!("Branch: {}", git_state.branch)];
            if git_state.files.is_empty() {
                lines.push("Working tree clean.".to_string());
            } else {
                lines.push(format!("{} changed file(s):", git_state.files.len()));
                for f in &git_state.files {
                    lines.push(format!("  {:>10}  {}", f.status, f.path));
                }
            }
            Ok(CommandOutput::Message(lines.join("\n")))
        }
        "diff" => {
            let file = if rest.is_empty() { "." } else { rest };
            let diff = GitState::diff(&state.working_dir, file)?;
            if diff.is_empty() {
                Ok(CommandOutput::Message("No diff output.".to_string()))
            } else {
                Ok(CommandOutput::Message(diff))
            }
        }
        "stage" => {
            if rest.is_empty() {
                return Ok(CommandOutput::Message("Usage: /git stage <file>".to_string()));
            }
            GitState::stage(&state.working_dir, rest)?;
            Ok(CommandOutput::Message(format!("Staged: {rest}")))
        }
        "unstage" => {
            if rest.is_empty() {
                return Ok(CommandOutput::Message("Usage: /git unstage <file>".to_string()));
            }
            GitState::unstage(&state.working_dir, rest)?;
            Ok(CommandOutput::Message(format!("Unstaged: {rest}")))
        }
        "commit" => {
            if rest.is_empty() {
                return Ok(CommandOutput::Message("Usage: /git commit <message>".to_string()));
            }
            GitState::commit(&state.working_dir, rest)?;
            Ok(CommandOutput::Message(format!("Committed: {rest}")))
        }
        "" => Ok(CommandOutput::Message(
            "Usage: /git status | diff [file] | stage <file> | unstage <file> | commit <message>".to_string(),
        )),
        other => Ok(CommandOutput::Message(format!("Unknown git subcommand: {other}"))),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_tree(_args: &str, state: &mut AppState) -> anyhow::Result<CommandOutput> {
    use crate::session::BranchableSession;

    let mut bs = BranchableSession::new();
    for msg in &state.messages {
        bs.append(msg);
    }
    let tree = bs.render_tree();
    if tree.is_empty() {
        Ok(CommandOutput::Message("(empty session — no messages yet)".to_string()))
    } else {
        Ok(CommandOutput::Message(tree))
    }
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_fork(_args: &str, state: &mut AppState) -> anyhow::Result<CommandOutput> {
    if state.messages.is_empty() {
        return Ok(CommandOutput::Message("Cannot fork: no messages in session.".to_string()));
    }
    let last_id = state.messages.last().map(|m| m.id.clone()).unwrap_or_default();
    let short_id = if last_id.len() > 8 { &last_id[..8] } else { &last_id };
    Ok(CommandOutput::Message(format!(
        "Forked from entry {short_id}. New messages will create a branch.\n(Branch navigation requires BranchableSession — use /goto <id> to navigate.)"
    )))
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_goto(args: &str, _state: &mut AppState) -> anyhow::Result<CommandOutput> {
    let target = args.trim();
    if target.is_empty() {
        return Ok(CommandOutput::Message("Usage: /goto <entry-id>".to_string()));
    }
    Ok(CommandOutput::Message(format!(
        "Navigate to entry {target}.\n(Full branch navigation requires BranchableSession integration.)"
    )))
}

fn cmd_rename(args: &str, state: &mut AppState) -> anyhow::Result<CommandOutput> {
    use crate::session::{Session, SessionManager};

    let new_title = args.trim();
    if new_title.is_empty() {
        let current = state.session_title.as_deref().unwrap_or("(untitled)");
        return Ok(CommandOutput::Message(format!("Usage: /rename <title>\nCurrent title: {current}")));
    }

    let old = state.session_title.clone().unwrap_or_else(|| "(untitled)".to_string());
    state.session_title = Some(new_title.to_string());

    // Persist immediately so the rename survives a quit before the next auto-save.
    let mgr = SessionManager::new()?;
    let mut session = Session::from_state(state);
    if let Ok(existing) = mgr.load(&state.session_id) {
        session.created_at = existing.created_at;
    }
    mgr.save(&session)?;

    Ok(CommandOutput::Message(format!("Session renamed: {old} -> {new_title}")))
}

#[allow(clippy::unnecessary_wraps)]
fn cmd_skill(args: &str, _state: &mut AppState) -> anyhow::Result<CommandOutput> {
    let args = args.trim();

    // Build a registry and load the default skills directory
    let mut registry = ExtensionRegistry::new();
    let skills_dir = registry.skills_dir().to_path_buf();
    if skills_dir.is_dir() {
        let _ = registry.load_skills_dir(&skills_dir);
    }

    if args.is_empty() {
        // /skill with no args — list available skills
        let skills = registry.list_skills();
        if skills.is_empty() {
            return Ok(CommandOutput::Message("No skills available. Add .md files to ~/.smooth/skills/".to_string()));
        }
        let mut lines = vec!["Available skills:".to_string()];
        for s in &skills {
            lines.push(format!("  /skill:{} — {}", s.name, s.description));
        }
        Ok(CommandOutput::Message(lines.join("\n")))
    } else {
        // /skill:name [args] — the name is the first word of args
        let (skill_name, _rest) = args.split_once(' ').unwrap_or((args, ""));

        registry.find_skill(skill_name).map_or_else(
            || {
                Ok(CommandOutput::Message(format!(
                    "Unknown skill: {skill_name}. Use /skill to list available skills."
                )))
            },
            |skill| {
                let vars = HashMap::new();
                let rendered = skill.render(&vars);
                Ok(CommandOutput::Message(rendered))
            },
        )
    }
}

/// Parse a raw input string into its kind: slash command, bang shell, or plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind<'a> {
    /// A slash command with name and arguments.
    SlashCommand { name: &'a str, args: &'a str },
    /// A bang shell command.
    BangCommand(&'a str),
    /// A normal chat message.
    Normal(&'a str),
}

/// Classify raw input text.
pub fn parse_input(input: &str) -> InputKind<'_> {
    let trimmed = input.trim();
    trimmed.strip_prefix('/').map_or_else(
        || trimmed.strip_prefix('!').map_or_else(|| InputKind::Normal(trimmed), InputKind::BangCommand),
        |rest| {
            let (name, args) = rest.split_once(' ').unwrap_or((rest, ""));
            InputKind::SlashCommand { name, args }
        },
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_register_and_execute() {
        let mut reg = CommandRegistry { commands: HashMap::new() };
        reg.register(
            "ping",
            "Reply with pong",
            Box::new(|_args, _state| Ok(CommandOutput::Message("pong".to_string()))),
        );

        let mut state = AppState::new(PathBuf::from("/tmp"));
        let result = reg.execute("ping", "", &mut state);
        assert!(result.is_some());
        let output = result.expect("command should exist").expect("handler should succeed");
        assert_eq!(output, CommandOutput::Message("pong".to_string()));
    }

    #[test]
    fn test_help_lists_all_commands() {
        let reg = CommandRegistry::new();
        let cmds = reg.list_commands();
        let names: Vec<_> = cmds.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"help"));
        assert!(names.contains(&"clear"));
        assert!(names.contains(&"model"));
        assert!(names.contains(&"save"));
        assert!(names.contains(&"sessions"));
        assert!(names.contains(&"quit"));
        assert!(names.contains(&"status"));
        assert!(names.contains(&"compact"));
        assert!(names.contains(&"rename"));
    }

    #[test]
    fn test_rename_without_args_reports_current_title() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        state.session_title = Some("old-title".to_string());
        let reg = CommandRegistry::new();
        let output = reg.execute("rename", "", &mut state).expect("rename exists").expect("handler ok");
        match output {
            CommandOutput::Message(msg) => {
                assert!(msg.contains("Usage:"));
                assert!(msg.contains("old-title"));
            }
            other => panic!("Expected Message, got {other:?}"),
        }
        // Title unchanged
        assert_eq!(state.session_title.as_deref(), Some("old-title"));
    }

    #[test]
    fn test_rename_persists_via_with_dir() {
        use crate::session::{Session, SessionManager};

        // Use a tempdir-backed SessionManager directly to prove the round-trip:
        // set title on state, Session::from_state captures it, save + load returns it.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let mgr = SessionManager::with_dir(tmp.path().to_path_buf()).expect("manager");

        let mut state = AppState::new(PathBuf::from("/tmp"));
        state.session_id = "sess-persist".to_string();
        state.session_title = Some("Renamed".to_string());

        let session = Session::from_state(&state);
        mgr.save(&session).expect("save");

        let loaded = mgr.load("sess-persist").expect("load");
        assert_eq!(loaded.title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn test_clear_empties_messages() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        state.add_message(crate::state::ChatMessage::user("hello"));
        state.add_message(crate::state::ChatMessage::assistant("hi"));
        assert_eq!(state.messages.len(), 2);

        let reg = CommandRegistry::new();
        let output = reg.execute("clear", "", &mut state).expect("clear exists").expect("handler ok");
        assert_eq!(output, CommandOutput::Clear);
        assert!(state.messages.is_empty());
    }

    #[test]
    fn test_command_output_message_adds_system_message() {
        let mut state = AppState::new(PathBuf::from("/tmp"));
        let reg = CommandRegistry::new();
        let output = reg.execute("status", "", &mut state).expect("status exists").expect("handler ok");
        match output {
            CommandOutput::Message(msg) => {
                assert!(msg.contains("Model:"));
                assert!(msg.contains("Tokens used:"));
            }
            other => panic!("Expected Message, got {other:?}"),
        }
    }

    #[test]
    fn test_command_parsing_slash_vs_bang_vs_normal() {
        // Slash command
        assert_eq!(parse_input("/help"), InputKind::SlashCommand { name: "help", args: "" });
        assert_eq!(parse_input("/model gpt-4o"), InputKind::SlashCommand { name: "model", args: "gpt-4o" });

        // Bang command
        assert_eq!(parse_input("!ls -la"), InputKind::BangCommand("ls -la"));

        // Normal text
        assert_eq!(parse_input("hello world"), InputKind::Normal("hello world"));

        // Edge cases
        assert_eq!(parse_input("  /quit  "), InputKind::SlashCommand { name: "quit", args: "" });
        assert_eq!(parse_input("  !pwd  "), InputKind::BangCommand("pwd"));
    }
}
