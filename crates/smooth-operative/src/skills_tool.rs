//! `skill_use` — the skills invocation tool (pearl th-e0f812).
//!
//! Discovery + the `Skill` data model live in `smooth_cast::skills`.
//! This module is the *runtime*: it registers a single `skill_use`
//! tool into the operative's ToolRegistry and injects a compact
//! catalog of available skills into the system prompt so the model
//! knows what it can reach for.
//!
//! A skill is a prompt, not code. Invoking `skill_use("add-show")`
//! returns the skill's markdown body (with a constraints header) as
//! tool output — it lands in the conversation as instructions the
//! agent then follows. There is no separate execution surface; the
//! recipe drives the ordinary bash/file/edit tools.
//!
//! Claude-Code parity: this mirrors how Claude Code's Skill tool
//! surfaces `~/.claude/skills/` recipes. Because discovery already
//! reads `~/.claude/skills/`, a user's existing Claude Code skills
//! work here unchanged.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use smooth_cast::skills::{render_invocation, Skill};
use smooth_operator::tool::{Tool, ToolSchema};

/// The `skill_use` tool. Holds the discovered skill set (name-collision
/// already resolved by `discover`) and returns a skill's body on call.
pub struct SkillUseTool {
    skills: Arc<Vec<Skill>>,
}

#[async_trait]
impl Tool for SkillUseTool {
    fn schema(&self) -> ToolSchema {
        // List the available names in the tool description too, so the
        // model sees them even if it skims past the system-prompt
        // catalog. Cheap — names only, not descriptions or bodies.
        let names: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
        ToolSchema {
            name: "skill_use".to_string(),
            description: format!(
                "Load a SKILL — a reusable recipe that encodes the right way to do a task. \
Returns the skill's full instructions, which you then follow. Available skills: {}.",
                names.join(", ")
            ),
            parameters: json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string", "description": "Skill name to load (exact match, e.g. \"add-show\")." }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let name = arguments.get("name").and_then(|v| v.as_str()).map(str::trim).unwrap_or_default();
        if name.is_empty() {
            anyhow::bail!("skill_use requires a non-empty 'name'");
        }
        match self.skills.iter().find(|s| s.name == name) {
            Some(skill) => Ok(render_invocation(skill)),
            None => {
                let available: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
                anyhow::bail!("no skill named {name:?}. Available: {}", available.join(", "))
            }
        }
    }

    fn is_read_only(&self) -> bool {
        // Loading a recipe reads nothing and changes nothing — the
        // recipe's own steps run through their own (possibly write)
        // tools, each surveilled independently.
        true
    }
}

/// Discover skills for `workspace`, register the `skill_use` tool, and
/// return the shared skill set so the caller can render the system-prompt
/// catalog from the same list. Always registers (discovery includes the
/// built-in `create-skill`, so the set is never empty).
pub fn register_skill_tool(tools: &mut smooth_operator::ToolRegistry, workspace: &std::path::Path) -> Arc<Vec<Skill>> {
    let skills = Arc::new(smooth_cast::skills::discover(workspace));
    tracing::info!(count = skills.len(), "registered skill_use — skills available to the agent");
    tools.register(SkillUseTool { skills: Arc::clone(&skills) });
    skills
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_cast::skills::{SkillScope, SkillSource};
    use std::path::PathBuf;

    fn sample() -> Arc<Vec<Skill>> {
        Arc::new(vec![Skill {
            name: "add-show".to_string(),
            description: "Add a show".to_string(),
            triggers: vec!["add show".to_string()],
            scope: SkillScope::Host,
            allowed_hosts: vec!["smoo-hub".to_string()],
            allowed_tools: vec!["bash".to_string()],
            body: "POST to /api/shows".to_string(),
            source: SkillSource::UserSmooth,
            path: PathBuf::from("/tmp/add-show/SKILL.md"),
        }])
    }

    #[tokio::test]
    async fn happy_path_returns_body() {
        let tool = SkillUseTool { skills: sample() };
        let out = tool.execute(json!({ "name": "add-show" })).await.expect("ok");
        assert!(out.contains("# Skill: add-show"));
        assert!(out.contains("POST to /api/shows"));
        assert!(out.contains("smoo-hub"), "constraints header should surface allowed_hosts");
    }

    #[tokio::test]
    async fn missing_skill_errors_with_available_list() {
        let tool = SkillUseTool { skills: sample() };
        let err = tool.execute(json!({ "name": "nope" })).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no skill named"));
        assert!(msg.contains("add-show"), "error should list what IS available");
    }

    #[tokio::test]
    async fn empty_name_errors() {
        let tool = SkillUseTool { skills: sample() };
        assert!(tool.execute(json!({ "name": "  " })).await.is_err());
        assert!(tool.execute(json!({})).await.is_err());
    }

    #[test]
    fn schema_lists_available_names() {
        let tool = SkillUseTool { skills: sample() };
        let schema = tool.schema();
        assert_eq!(schema.name, "skill_use");
        assert!(schema.description.contains("add-show"), "names should be in the tool description");
    }

    #[test]
    fn skill_use_is_read_only() {
        assert!(SkillUseTool { skills: sample() }.is_read_only());
    }
}
