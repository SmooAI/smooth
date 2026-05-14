//! TUI-driven bench scenarios.
//!
//! Pearl: th-139b02
//!
//! A scenario is a synthetic user session against the `th` TUI: a
//! fixture repo (a real `.git` directory + source files), a list of
//! user inputs, and assertions about the chat the user *would have
//! seen*.
//!
//! This module covers the schema + TOML parser. The pty-driven
//! runner that actually spawns `th` and captures the rendered chat
//! lives in [`runner`](super::runner) (next subtask of th-139b02).
//!
//! ## Layout on disk
//!
//! ```text
//! crates/smooth-bench/scenarios/
//!   ├── repo-overview/
//!   │     ├── scenario.toml      # this file's schema
//!   │     └── fixture/           # checked-in synthetic repo
//!   ├── stack-discovery/
//!   ├── edit-readme/
//!   └── commit-to-main/          # negative test — agent proposes
//!                                #   command, must NOT auto-commit
//! ```
//!
//! ## scenario.toml schema (v1)
//!
//! ```toml
//! [meta]
//! title = "User asks for a repo overview"
//! description = "First-turn factual Q routes to oracle, agent gives terse answer."
//! agent = "auto"   # or "oracle" / "fixer" / etc. to pin
//!
//! [[turns]]
//! input = "what is this project"
//!
//! [[turns.assert]]
//! kind = "intent_role"
//! expected = "oracle"
//!
//! [[turns.assert]]
//! kind = "tool_called"
//! name = "project_inspect"
//!
//! [[turns.assert]]
//! kind = "response_contains_any"
//! strings = ["budgeting", "next.js", "drizzle"]
//!
//! [[turns.assert]]
//! kind = "response_does_not_contain"
//! strings = ["postgres"]
//! ```

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level scenario, one per `scenario.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Scenario {
    pub meta: ScenarioMeta,
    /// Ordered user turns. Each turn drives a single TUI input and
    /// runs its assertions against the captured chat for that turn
    /// only — earlier turns' chat is left alone (the runner keeps
    /// the TUI session open across turns to exercise in-session
    /// memory, since pearl th-422b93 made that a real feature).
    #[serde(default)]
    pub turns: Vec<Turn>,
}

/// Free-form description so failure reports + the LLM judge have
/// human-language context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenarioMeta {
    pub title: String,
    pub description: String,
    /// Lead role to dispatch under. `"auto"` means "let the intent
    /// classifier pick" (the normal user path). Any role from
    /// `Cast::builtin()` (`oracle`, `fixer`, `mapper`, `heckler`,
    /// `runner`, `scout`) pins the role for the whole scenario.
    #[serde(default = "default_agent")]
    pub agent: String,
    /// Per-turn timeout in seconds — past this the runner kills the
    /// turn and records a timeout failure. Default 120s, generous
    /// for sandboxed LLM dispatches but bounded so a wedged run
    /// doesn't hang the whole bench loop.
    #[serde(default = "default_turn_timeout_s")]
    pub turn_timeout_s: u64,
    /// What to do when an Ask fires during the scenario run.
    /// Defaults to `deny` — bench runs are unattended and the safe
    /// default is to refuse anything the agent didn't have a
    /// pre-approved grant for. Override per-scenario when the test
    /// *wants* to exercise the auto-approve path. Pearl th-400773.
    #[serde(default)]
    pub auto_approve: AutoApprove,
}

/// How a bench scenario resolves Asks raised by Boardroom Narc
/// during the run. Mirrors the CLI's `--auto-approve` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoApprove {
    /// Deny every Ask. The safe default for unattended runs.
    #[default]
    Deny,
    /// Approve at scope=once. The narrowest possible auto-approve
    /// — re-asks on the next request.
    Once,
    /// Approve at scope=session. Subsequent identical asks within
    /// the same scenario run skip the prompt.
    Session,
    /// Approve at scope=project. Writes the grant to the project's
    /// wonk-allow.toml — most bench scenarios should NOT pick this
    /// because it pollutes the project's persistent state.
    Project,
    /// Approve at scope=user. Even more invasive than project; left
    /// here for completeness but bench scenarios almost never want
    /// it.
    User,
}

impl AutoApprove {
    /// Parse from the CLI flag form (`once`, `session`, `project`,
    /// `user`, `deny`). Case-insensitive. Returns `None` for unknown
    /// values so the caller can render a clear error.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "deny" => Some(Self::Deny),
            "once" => Some(Self::Once),
            "session" => Some(Self::Session),
            "project" | "pearl_project" | "pearl-project" => Some(Self::Project),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    /// Stable string form for logs and the CLI flag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Once => "once",
            Self::Session => "session",
            Self::Project => "project",
            Self::User => "user",
        }
    }

    /// True if this mode should deny pending Asks rather than
    /// approve them.
    #[must_use]
    pub fn is_deny(self) -> bool {
        matches!(self, Self::Deny)
    }
}

fn default_agent() -> String {
    "auto".to_string()
}

fn default_turn_timeout_s() -> u64 {
    120
}

/// One user input + its assertions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Turn {
    /// What the user types into the input box this turn.
    pub input: String,
    /// Assertions evaluated against the captured chat for this
    /// turn's response. All must pass for the turn to be green.
    #[serde(default, rename = "assert")]
    pub assertions: Vec<Assertion>,
}

/// Single assertion kind. Tagged on `kind` so TOML is
/// `[[turns.assert]]\nkind = "tool_called"\nname = "grep"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assertion {
    /// The TUI's status bar showed a specific role at any point
    /// during the turn. Used to verify the intent classifier
    /// routed the way we expect (e.g. `commit` keywords go to
    /// `oracle`, not `fixer`).
    IntentRole { expected: String },
    /// A tool call with this name appeared in the captured chat.
    /// Order-independent.
    ToolCalled { name: String },
    /// The final assistant response contains *any* of these
    /// strings (case-insensitive substring match).
    ResponseContainsAny { strings: Vec<String> },
    /// The final assistant response contains *all* of these
    /// strings (case-insensitive substring match).
    ResponseContainsAll { strings: Vec<String> },
    /// The final assistant response contains *none* of these
    /// strings (case-insensitive). Used for negative facts —
    /// "the agent must not say 'postgres' when the repo uses
    /// SQLite".
    ResponseDoesNotContain { strings: Vec<String> },
    /// The response includes a fenced code block — used by the
    /// `commit-to-main` scenario to verify the agent proposed a
    /// `git ...` command instead of pretending to run it.
    /// `language` filters to a specific fence label (`bash`,
    /// `sh`, `git`); `None` accepts any fence.
    CommandProposed {
        #[serde(default)]
        language: Option<String>,
        contains_any: Vec<String>,
    },
    /// No tool with this name was called. Catches the
    /// hallucinated-fix loop — `write_file` should not appear in
    /// a `commit-to-main` scenario response.
    ToolNotCalled { name: String },
    /// An Ask was filed against the AccessStore during this turn.
    /// The TUI / bench harness should observe the request for
    /// `ask_for` (typically a hostname or tool name) and resolve
    /// it at `resolve_with` scope (`once`/`session`/`project`/
    /// `user`/`deny`). When `must_fire` is true, the absence of a
    /// matching pending request fails the assertion — proves the
    /// gating layer actually triggered. When false, the assertion
    /// passes if the request was either resolved correctly OR
    /// never fired (e.g. because a persistent grant covered it).
    /// Pearl th-400773.
    Permission {
        /// Resource the Ask should mention — host for network,
        /// tool name for tool, command for cli. Substring match
        /// case-insensitive.
        ask_for: String,
        /// Resolution to apply: `once` / `session` / `project` /
        /// `user` / `deny`. Same vocabulary as the CLI flag.
        resolve_with: String,
        /// When true (default), the assertion fails if no matching
        /// Ask appeared. When false, only the resolution shape is
        /// checked.
        #[serde(default = "default_must_fire")]
        must_fire: bool,
    },
}

fn default_must_fire() -> bool {
    true
}

/// Read and parse a scenario from `<dir>/scenario.toml`. The
/// returned scenario's paths are relative to `dir` — the runner
/// resolves them when copying the fixture into a scratch dir.
pub fn load_scenario(dir: &Path) -> Result<Scenario> {
    let path = dir.join("scenario.toml");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("reading scenario {}", path.display()))?;
    parse_scenario(&raw).with_context(|| format!("parsing scenario {}", path.display()))
}

/// Parse a scenario from a raw TOML string. Public for tests +
/// for callers that want to validate without hitting the disk.
pub fn parse_scenario(raw: &str) -> Result<Scenario> {
    let scenario: Scenario = toml::from_str(raw).map_err(|e| anyhow!("invalid scenario.toml: {e}"))?;
    validate_scenario(&scenario)?;
    Ok(scenario)
}

fn validate_scenario(s: &Scenario) -> Result<()> {
    if s.meta.title.trim().is_empty() {
        return Err(anyhow!("scenario.meta.title must be non-empty"));
    }
    if s.turns.is_empty() {
        return Err(anyhow!("scenario must have at least one turn"));
    }
    for (i, turn) in s.turns.iter().enumerate() {
        if turn.input.trim().is_empty() {
            return Err(anyhow!("turn {}: input must be non-empty", i + 1));
        }
    }
    Ok(())
}

/// Discover every `scenarios/<name>/scenario.toml` under the bench
/// crate's checkout. Returns `(name, scenario_dir, parsed)` triples
/// in stable lexical order so test runs are deterministic.
pub fn discover_scenarios(scenarios_root: &Path) -> Result<Vec<(String, PathBuf, Scenario)>> {
    if !scenarios_root.is_dir() {
        return Err(anyhow!("scenarios root not a directory: {}", scenarios_root.display()));
    }
    let mut entries: Vec<(String, PathBuf, Scenario)> = Vec::new();
    let mut dirs: Vec<_> = std::fs::read_dir(scenarios_root)
        .with_context(|| format!("reading {}", scenarios_root.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    dirs.sort_by_key(std::fs::DirEntry::file_name);
    for entry in dirs {
        let dir = entry.path();
        let name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("scenario dir name not utf-8: {}", dir.display()))?
            .to_string();
        let scenario_path = dir.join("scenario.toml");
        if !scenario_path.is_file() {
            // Skip directories that aren't scenarios (e.g. a
            // shared `_lib/` helpers dir). Not an error.
            continue;
        }
        let scenario = load_scenario(&dir).with_context(|| format!("loading scenario {name}"))?;
        entries.push((name, dir, scenario));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_toml() -> &'static str {
        r#"
[meta]
title = "Repo overview"
description = "User asks what the project does."

[[turns]]
input = "what is this project"

[[turns.assert]]
kind = "tool_called"
name = "project_inspect"

[[turns.assert]]
kind = "response_contains_any"
strings = ["budgeting", "drizzle"]
"#
    }

    #[test]
    fn parse_minimal_scenario() {
        let s = parse_scenario(minimal_toml()).expect("parse");
        assert_eq!(s.meta.title, "Repo overview");
        assert_eq!(s.meta.agent, "auto"); // default
        assert_eq!(s.meta.turn_timeout_s, 120);
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].input, "what is this project");
        assert_eq!(s.turns[0].assertions.len(), 2);
        match &s.turns[0].assertions[0] {
            Assertion::ToolCalled { name } => assert_eq!(name, "project_inspect"),
            other => panic!("unexpected variant {other:?}"),
        }
        match &s.turns[0].assertions[1] {
            Assertion::ResponseContainsAny { strings } => {
                assert_eq!(strings.len(), 2);
                assert!(strings.contains(&"budgeting".to_string()));
            }
            other => panic!("unexpected variant {other:?}"),
        }
    }

    #[test]
    fn empty_title_rejected() {
        let raw = r#"
[meta]
title = ""
description = "x"

[[turns]]
input = "hi"
"#;
        let err = parse_scenario(raw).expect_err("must reject empty title");
        assert!(err.to_string().contains("title"));
    }

    #[test]
    fn empty_input_rejected() {
        let raw = r#"
[meta]
title = "x"
description = "y"

[[turns]]
input = ""
"#;
        let err = parse_scenario(raw).expect_err("must reject empty turn input");
        assert!(err.to_string().contains("input"));
    }

    #[test]
    fn no_turns_rejected() {
        let raw = r#"
[meta]
title = "x"
description = "y"
"#;
        let err = parse_scenario(raw).expect_err("must reject zero turns");
        assert!(err.to_string().contains("at least one"));
    }

    #[test]
    fn assertion_variants_roundtrip() {
        // Pin the wire shape so a future serde_with_change doesn't
        // silently rename a kind and break authored scenario files.
        let raw = r#"
[meta]
title = "all kinds"
description = "y"

[[turns]]
input = "x"

[[turns.assert]]
kind = "intent_role"
expected = "oracle"

[[turns.assert]]
kind = "tool_called"
name = "grep"

[[turns.assert]]
kind = "tool_not_called"
name = "write_file"

[[turns.assert]]
kind = "response_contains_any"
strings = ["a"]

[[turns.assert]]
kind = "response_contains_all"
strings = ["a", "b"]

[[turns.assert]]
kind = "response_does_not_contain"
strings = ["postgres"]

[[turns.assert]]
kind = "command_proposed"
language = "bash"
contains_any = ["git commit", "git add"]

[[turns.assert]]
kind = "permission"
ask_for = "api.openai.com"
resolve_with = "session"
"#;
        let s = parse_scenario(raw).expect("parse");
        let kinds: Vec<&Assertion> = s.turns[0].assertions.iter().collect();
        assert_eq!(kinds.len(), 8);
        // Spot-check each variant landed in the right shape.
        assert!(matches!(kinds[0], Assertion::IntentRole { .. }));
        assert!(matches!(kinds[1], Assertion::ToolCalled { .. }));
        assert!(matches!(kinds[2], Assertion::ToolNotCalled { .. }));
        assert!(matches!(kinds[3], Assertion::ResponseContainsAny { .. }));
        assert!(matches!(kinds[4], Assertion::ResponseContainsAll { .. }));
        assert!(matches!(kinds[5], Assertion::ResponseDoesNotContain { .. }));
        assert!(matches!(kinds[6], Assertion::CommandProposed { .. }));
        match kinds[7] {
            Assertion::Permission {
                ask_for,
                resolve_with,
                must_fire,
            } => {
                assert_eq!(ask_for, "api.openai.com");
                assert_eq!(resolve_with, "session");
                assert!(must_fire, "must_fire defaults to true");
            }
            other => panic!("expected Permission, got {other:?}"),
        }
    }

    #[test]
    fn permission_assertion_must_fire_can_be_overridden() {
        let raw = r#"
[meta]
title = "T"
description = "D"

[[turns]]
input = "x"

[[turns.assert]]
kind = "permission"
ask_for = "api.example.com"
resolve_with = "deny"
must_fire = false
"#;
        let s = parse_scenario(raw).expect("parse");
        match &s.turns[0].assertions[0] {
            Assertion::Permission { must_fire, .. } => assert!(!*must_fire),
            other => panic!("unexpected variant {other:?}"),
        }
    }

    #[test]
    fn auto_approve_defaults_to_deny() {
        let s = parse_scenario(minimal_toml()).expect("parse");
        assert_eq!(s.meta.auto_approve, AutoApprove::Deny);
        assert!(s.meta.auto_approve.is_deny());
    }

    #[test]
    fn auto_approve_can_be_set_in_meta() {
        let raw = r#"
[meta]
title = "T"
description = "D"
auto_approve = "session"

[[turns]]
input = "x"
"#;
        let s = parse_scenario(raw).expect("parse");
        assert_eq!(s.meta.auto_approve, AutoApprove::Session);
        assert!(!s.meta.auto_approve.is_deny());
    }

    #[test]
    fn auto_approve_parses_canonical_forms() {
        assert_eq!(AutoApprove::parse("deny"), Some(AutoApprove::Deny));
        assert_eq!(AutoApprove::parse("once"), Some(AutoApprove::Once));
        assert_eq!(AutoApprove::parse("session"), Some(AutoApprove::Session));
        assert_eq!(AutoApprove::parse("project"), Some(AutoApprove::Project));
        assert_eq!(AutoApprove::parse("user"), Some(AutoApprove::User));
        // Aliases.
        assert_eq!(AutoApprove::parse("pearl_project"), Some(AutoApprove::Project));
        assert_eq!(AutoApprove::parse("Pearl-Project"), Some(AutoApprove::Project));
        // Case-insensitive.
        assert_eq!(AutoApprove::parse("SESSION"), Some(AutoApprove::Session));
        // Unknown returns None so the caller can render a clear error.
        assert_eq!(AutoApprove::parse("forever"), None);
        assert_eq!(AutoApprove::parse(""), None);
    }

    #[test]
    fn auto_approve_round_trips_through_as_str_and_parse() {
        for mode in [
            AutoApprove::Deny,
            AutoApprove::Once,
            AutoApprove::Session,
            AutoApprove::Project,
            AutoApprove::User,
        ] {
            let s = mode.as_str();
            assert_eq!(AutoApprove::parse(s), Some(mode), "round-trip {s}");
        }
    }

    #[test]
    fn auto_approve_serde_uses_snake_case() {
        // The TOML schema treats `pearl_project` as the canonical
        // wire form for Project — match smooth_narc::judge::Scope
        // exactly.
        assert_eq!(serde_json::to_string(&AutoApprove::Project).unwrap(), "\"project\"");
        let m: AutoApprove = serde_json::from_str("\"session\"").unwrap();
        assert_eq!(m, AutoApprove::Session);
    }

    #[test]
    fn discover_scenarios_returns_sorted_list() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        for name in ["zebra", "alpha", "middle"] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("scenario.toml"),
                format!(
                    r#"
[meta]
title = "{name}"
description = "x"

[[turns]]
input = "hi"
"#
                ),
            )
            .unwrap();
        }
        // A non-scenario directory (no scenario.toml) must be
        // silently skipped, not error out.
        std::fs::create_dir_all(root.join("_helpers")).unwrap();

        let found = discover_scenarios(root).expect("discover");
        let names: Vec<&str> = found.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn unknown_assertion_kind_rejected() {
        let raw = r#"
[meta]
title = "x"
description = "y"

[[turns]]
input = "hi"

[[turns.assert]]
kind = "do_a_barrel_roll"
"#;
        assert!(parse_scenario(raw).is_err());
    }
}
