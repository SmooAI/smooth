//! `tool_hints` — recommended approaches for common operator intents.
//!
//! The agent's freedom to figure out HOW to accomplish a task is the point;
//! Smooth doesn't want to hand it a paint-by-numbers script. But on common
//! intents ("list github repos", "search the web", "format JSON", "find a
//! file") the same approach wins almost every time, and re-deriving it
//! each run wastes LLM iterations.
//!
//! This module wraps a registry of those preferred approaches as a tool
//! the agent can consult BEFORE reaching for bash. The agent reads the
//! task, picks an intent it matches, calls `tool_hints("list github
//! repos")`, and gets back a recommended command + fallback + notes.
//! Then it decides whether to follow the recommendation or do something
//! else.
//!
//! The registry is layered:
//! 1. Built-in defaults (this file's `BUILTIN_HINTS`).
//! 2. User overrides at `~/.smooth/tool_hints/*.toml`.
//! 3. Project-scoped at `<workspace>/.smooth/tool_hints/*.toml`.
//!
//! Project hints win over user hints win over built-ins on intent collision.
//! Schemas are tolerant — extra fields are ignored, missing fields fall
//! back to defaults.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use smooth_operator::tool::{Tool, ToolSchema};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolHint {
    /// Canonical intent name — e.g. `"list github repos"`. Lowercase, words
    /// separated by spaces. The agent searches by substring + keyword
    /// match, so the canonical name should be specific.
    pub intent: String,
    /// Extra keywords / synonyms the matcher checks against the agent's
    /// query. e.g. for "list github repos" useful keywords are
    /// `["github", "repos", "repositories", "gh"]`.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Short label for the preferred tool — what the agent will reach for
    /// first. e.g. `"gh CLI"`, `"jq"`, `"firecrawl"`.
    pub preferred: String,
    /// Concrete command template. The agent should adapt arguments rather
    /// than copy verbatim, but this is a working starting point.
    pub command: String,
    /// Notes on auth, prerequisites, common pitfalls, and how to interpret
    /// output. Empty string is fine when the command is self-explanatory.
    #[serde(default)]
    pub notes: String,
    /// Fallback command for when the preferred tool isn't available
    /// (auth missing, binary not installed, network blocked). Empty
    /// string means "no fallback worth recommending — fail loudly".
    #[serde(default)]
    pub fallback: String,
}

#[derive(Debug, Default, Deserialize)]
struct HintsFile {
    #[serde(default, alias = "hint")]
    hints: Vec<ToolHint>,
}

const BUILTIN_HINTS_TOML: &str = include_str!("tool_hints/builtin.toml");

fn builtin_hints() -> Vec<ToolHint> {
    toml::from_str::<HintsFile>(BUILTIN_HINTS_TOML).map(|f| f.hints).unwrap_or_default()
}

/// Walk a directory and load every `*.toml` file into a flat hint list.
/// Errors on individual files are logged but don't abort the load.
fn load_dir(dir: &std::path::Path) -> Vec<ToolHint> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut hints = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match std::fs::read_to_string(&path).ok().and_then(|raw| toml::from_str::<HintsFile>(&raw).ok()) {
            Some(f) => hints.extend(f.hints),
            None => tracing::warn!(path = %path.display(), "tool_hints: failed to parse"),
        }
    }
    hints
}

/// Compose the layered registry: project (highest) → user → built-in.
/// Later additions overwrite earlier ones on `intent` collision.
fn load_registry(workspace: Option<&std::path::Path>) -> Vec<ToolHint> {
    let mut by_intent: std::collections::HashMap<String, ToolHint> = std::collections::HashMap::new();
    for h in builtin_hints() {
        by_intent.insert(h.intent.clone(), h);
    }
    if let Some(home) = dirs_next::home_dir() {
        for h in load_dir(&home.join(".smooth").join("tool_hints")) {
            by_intent.insert(h.intent.clone(), h);
        }
    }
    if let Some(ws) = workspace {
        for h in load_dir(&ws.join(".smooth").join("tool_hints")) {
            by_intent.insert(h.intent.clone(), h);
        }
    }
    by_intent.into_values().collect()
}

/// Score a hint against a query. Higher = better match. Used to rank
/// recommendations when multiple hints overlap.
fn score(hint: &ToolHint, query: &str) -> u32 {
    let q = query.to_lowercase();
    let mut s: u32 = 0;
    if hint.intent.to_lowercase() == q {
        s += 100;
    } else if hint.intent.to_lowercase().contains(&q) || q.contains(&hint.intent.to_lowercase()) {
        s += 50;
    }
    for k in &hint.keywords {
        if q.contains(&k.to_lowercase()) {
            s += 10;
        }
    }
    s
}

pub struct ToolHintsTool {
    pub registry: Arc<Vec<ToolHint>>,
}

impl ToolHintsTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        let registry = Arc::new(load_registry(workspace.as_deref()));
        Self { registry }
    }
}

#[async_trait]
impl Tool for ToolHintsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "tool_hints".to_string(),
            description: "Look up the recommended approach for a common operator intent (e.g. \"list github repos\", \"format json\", \"search the web\", \"find a file\"). Returns the preferred command + fallback + notes. Consult this BEFORE reaching for bash on a new intent — the team has already settled which tool wins for the common cases. If no hint matches, falls back to your own judgment.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["intent"],
                "properties": {
                    "intent": {
                        "type": "string",
                        "description": "Free-text description of what you're trying to do. Examples: `list github repos`, `format json output`, `search the web for X`, `find files matching pattern`."
                    },
                    "limit": {
                        "type": "number",
                        "default": 3,
                        "description": "Max number of recommendations to return."
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let intent = arguments["intent"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'intent'"))?;
        let limit = arguments.get("limit").and_then(|v| v.as_u64()).unwrap_or(3).clamp(1, 10) as usize;

        let mut scored: Vec<(u32, &ToolHint)> = self.registry.iter().map(|h| (score(h, intent), h)).filter(|(s, _)| *s > 0).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.truncate(limit);

        if scored.is_empty() {
            return Ok(format!(
                "No registered hint for intent `{intent}`. Use your own judgment — bash, the existing tool registry, or ask_smooth if you're unsure."
            ));
        }

        let mut out = String::new();
        for (_, h) in scored {
            out.push_str(&format!("**{}** — preferred: `{}`\n", h.intent, h.preferred));
            out.push_str(&format!("  command: `{}`\n", h.command));
            if !h.notes.is_empty() {
                out.push_str(&format!("  notes: {}\n", h.notes));
            }
            if !h.fallback.is_empty() {
                out.push_str(&format!("  fallback: `{}`\n", h.fallback));
            }
            out.push('\n');
        }
        Ok(out.trim_end().to_string())
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_hints_parses() {
        let hints = builtin_hints();
        assert!(!hints.is_empty(), "built-in hints should not be empty");
        assert!(hints.iter().any(|h| h.intent.contains("github")), "expected a github intent in built-ins");
    }

    #[test]
    fn score_prefers_exact_intent_match() {
        let hints = builtin_hints();
        let h = hints.iter().find(|h| h.intent == "list github repos").expect("github hint");
        let s_exact = score(h, "list github repos");
        let s_fuzzy = score(h, "list repos");
        assert!(s_exact > s_fuzzy, "exact > fuzzy");
    }

    #[test]
    fn score_returns_zero_when_unrelated() {
        let hints = builtin_hints();
        let h = hints.iter().find(|h| h.intent == "list github repos").expect("github hint");
        assert_eq!(score(h, "what's the weather"), 0);
    }

    #[tokio::test]
    async fn execute_returns_github_hint_for_repo_query() {
        let tool = ToolHintsTool::new(None);
        let out = tool.execute(json!({"intent": "list my github repos"})).await.expect("execute");
        assert!(out.contains("gh"), "expected gh recommendation, got: {out}");
        assert!(out.contains("preferred"));
    }

    #[tokio::test]
    async fn execute_returns_no_match_when_unknown_intent() {
        let tool = ToolHintsTool::new(None);
        let out = tool.execute(json!({"intent": "asdfqwerasdf"})).await.expect("execute");
        assert!(out.to_lowercase().contains("no registered hint"));
    }
}
