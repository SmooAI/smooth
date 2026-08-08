//! `GET /api/skills` — the skill catalog every face renders (pearl th-a5952d).
//!
//! One catalog, served by the daemon: `smooth_cast::skills::discover` (the
//! same discovery the persona's "Available skills" section uses) serialized
//! for clients. The web SPA has no disk access — this route is how it gets a
//! `/`-skill menu at all; `th code` prefers it too (falling back to its own
//! local `discover` only when the daemon is unreachable).
//!
//! Merged into the operator's router via the local flavor's `serve_routes`
//! seam, like `/search`. Ungated for the same reason `/search` is (mention
//! and catalog reads must work on a tokenless connection), and it honors the
//! same guarded `?cwd=` override — a client working in a different directory
//! gets that workspace's project skills, but only for the daemon workspace or
//! paths under the user's home (see `search::allowed_cwd`).

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use smooth_cast::skills::Skill;

/// `?cwd=` — same guarded override contract as `/search`.
#[derive(Debug, Deserialize)]
struct SkillsQuery {
    #[serde(default)]
    cwd: Option<String>,
}

/// The `{ "skills": [...] }` envelope. Each entry is the full normalized
/// [`Skill`] (name, description, triggers, scope, body, source, path) — the
/// body rides along because clients compose it into the turn today; when the
/// engine grows a `skill` field on `send_message`, clients can stop reading it.
#[derive(Debug, Serialize)]
pub struct SkillsResponse {
    pub skills: Vec<Skill>,
}

/// Build the `/api/skills` router bound to `workspace`.
pub fn skills_router(workspace: PathBuf) -> Router {
    Router::new().route("/api/skills", get(skills_handler)).with_state(Arc::new(workspace))
}

async fn skills_handler(State(workspace): State<Arc<PathBuf>>, Query(query): Query<SkillsQuery>) -> Json<SkillsResponse> {
    let effective = query
        .cwd
        .as_deref()
        .and_then(|c| crate::search::allowed_cwd(c, dirs_next::home_dir().as_deref(), &workspace))
        .unwrap_or_else(|| workspace.as_ref().clone());
    // Discovery walks the filesystem — keep it off the async runtime.
    let skills = tokio::task::spawn_blocking(move || smooth_cast::skills::discover(&effective))
        .await
        .unwrap_or_default();
    Json(SkillsResponse { skills })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    /// A minimal Claude-Code-dialect skill fixture.
    const SAMPLE: &str = "---\nname: sample-skill\ndescription: does the sample thing\n---\n\nDo the thing.\n";

    #[tokio::test]
    async fn serves_project_skills_from_the_workspace() {
        let dir = std::env::temp_dir().join(format!("smooth-skills-route-{}", std::process::id()));
        let skill_dir = dir.join(".smooth/skills/sample-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), SAMPLE).unwrap();

        let resp = skills_handler(State(Arc::new(dir.clone())), Query(SkillsQuery { cwd: None })).await;
        let found = resp.0.skills.iter().find(|s| s.name == "sample-skill").expect("project skill served");
        assert_eq!(found.description, "does the sample thing");
        assert!(found.body.contains("Do the thing."));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn rejected_cwd_falls_back_to_the_daemon_workspace() {
        let dir = std::env::temp_dir().join(format!("smooth-skills-cwd-{}", std::process::id()));
        let skill_dir = dir.join(".smooth/skills/ws-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: ws-skill\ndescription: workspace skill\n---\nbody\n").unwrap();

        // `/` is a real directory but outside home and not the workspace —
        // the override must be refused, serving the workspace catalog instead.
        let resp = skills_handler(State(Arc::new(dir.clone())), Query(SkillsQuery { cwd: Some("/".into()) })).await;
        assert!(resp.0.skills.iter().any(|s| s.name == "ws-skill"), "fell back to workspace catalog");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn skill_serializes_with_name_description_and_body() {
        let skills = smooth_cast::skills::parse_skill_string(SAMPLE, std::path::Path::new("/x/SKILL.md"), smooth_cast::skills::SkillSource::Builtin);
        let skill = skills.unwrap().unwrap();
        let json = serde_json::to_value(&skill).unwrap();
        assert_eq!(json["name"], "sample-skill");
        assert_eq!(json["description"], "does the sample thing");
        assert!(json["body"].as_str().unwrap().contains("Do the thing."));
    }
}
