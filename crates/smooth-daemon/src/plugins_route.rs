//! `GET /api/plugins` — the installed-plugin catalog every face renders
//! (pearl th-262e5f).
//!
//! Same contract as `/api/skills`: one catalog, served by the daemon, so a
//! client with no disk access (the web SPA) and a client with disk access
//! (`th code`) render the SAME list the agent actually has registered. The
//! merge (global + project, project wins) lives in `smooth_tools::plugin` —
//! this route is a thin serialization of it.
//!
//! Merged into the operator's router via the local flavor's `serve_routes`
//! seam. Ungated like `/search` and `/api/skills` (catalog reads must work on a
//! tokenless connection), honoring the same guarded `?cwd=` override.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use smooth_tools::LoadedPlugin;

/// `?cwd=` — same guarded override contract as `/search`.
#[derive(Debug, Deserialize)]
struct PluginsQuery {
    #[serde(default)]
    cwd: Option<String>,
}

/// A manifest that failed to parse — surfaced so a face can show "this plugin
/// is installed but broken" instead of silently omitting it.
#[derive(Debug, Serialize)]
pub struct PluginError {
    pub name: String,
    pub error: String,
}

/// The `{ "plugins": [...], "errors": [...] }` envelope.
///
/// Each plugin carries its manifest fields plus `scope`, the registered `tool`
/// name, and the manifest `path`. Disabled plugins are listed (with
/// `disabled: true`) — they exist on disk, they just aren't registered.
#[derive(Debug, Serialize)]
pub struct PluginsResponse {
    pub plugins: Vec<LoadedPlugin>,
    pub errors: Vec<PluginError>,
}

/// Build the `/api/plugins` router bound to `workspace`.
pub fn plugins_router(workspace: PathBuf) -> Router {
    Router::new().route("/api/plugins", get(plugins_handler)).with_state(Arc::new(workspace))
}

async fn plugins_handler(State(workspace): State<Arc<PathBuf>>, Query(query): Query<PluginsQuery>) -> Json<PluginsResponse> {
    let effective = query
        .cwd
        .as_deref()
        .and_then(|c| crate::search::allowed_cwd(c, dirs_next::home_dir().as_deref(), &workspace))
        .unwrap_or_else(|| workspace.as_ref().clone());
    // Discovery reads the filesystem — keep it off the async runtime.
    let (plugins, failures) = tokio::task::spawn_blocking(move || smooth_tools::discover_plugins(&effective))
        .await
        .unwrap_or_default();
    Json(PluginsResponse {
        plugins,
        errors: failures.into_iter().map(|(name, error)| PluginError { name, error }).collect(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn write_plugin(root: &std::path::Path, name: &str, body: &str) {
        let dir = root.join(".smooth/plugins").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), body).unwrap();
    }

    #[tokio::test]
    async fn serves_project_plugins_from_the_workspace() {
        let dir = std::env::temp_dir().join(format!("smooth-plugins-route-{}", std::process::id()));
        write_plugin(
            &dir,
            "deploy",
            "name = \"deploy\"\ndescription = \"Deploy this project.\"\ncommand = \"scripts/deploy.sh {{env}}\"\n",
        );
        write_plugin(&dir, "broken", "name = \"broken\"\n");

        let resp = plugins_handler(State(Arc::new(dir.clone())), Query(PluginsQuery { cwd: None })).await;
        let found = resp.0.plugins.iter().find(|p| p.manifest.name == "deploy").expect("project plugin served");
        assert_eq!(found.tool, "plugin_deploy");
        assert_eq!(found.scope, smooth_tools::PluginScope::Project);
        assert!(!found.manifest.disabled);
        assert!(resp.0.errors.iter().any(|e| e.name == "broken"), "broken manifest reported");

        // The wire shape faces render: flattened manifest + scope + tool.
        let json = serde_json::to_value(&resp.0).unwrap();
        let entry = json["plugins"].as_array().unwrap().iter().find(|p| p["name"] == "deploy").unwrap();
        assert_eq!(entry["tool"], "plugin_deploy");
        assert_eq!(entry["scope"], "project");
        assert_eq!(entry["description"], "Deploy this project.");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn rejected_cwd_falls_back_to_the_daemon_workspace() {
        let dir = std::env::temp_dir().join(format!("smooth-plugins-cwd-{}", std::process::id()));
        write_plugin(&dir, "ws-plugin", "name = \"ws-plugin\"\ndescription = \"workspace\"\ncommand = \"true\"\n");

        // `/` is a real directory but outside home and not the workspace — the
        // override must be refused, serving the workspace catalog instead.
        let resp = plugins_handler(State(Arc::new(dir.clone())), Query(PluginsQuery { cwd: Some("/".into()) })).await;
        assert!(resp.0.plugins.iter().any(|p| p.manifest.name == "ws-plugin"), "fell back to workspace catalog");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
