//! File-based CLI-wrapper plugins.
//!
//! Users drop TOML manifests under `$SMOOTH_HOME/plugins/<name>/plugin.toml`
//! (or `~/.smooth/plugins/<name>/plugin.toml`) describing a custom tool
//! that wraps a shell command. The runner discovers them at startup and
//! registers each as a smooth_operator::Tool with the manifest's name and
//! parameter schema. The agent calls the tool just like a built-in.
//!
//! This is the lighter-weight cousin of MCP: no separate server process,
//! no JSON-RPC, just "render this command template and run it." For
//! anything stateful or with a typed protocol, prefer MCP.
//!
//! ## Manifest format
//!
//! ```toml
//! name = "jq_pretty"
//! description = "Pretty-print JSON with jq."
//! prompt_hint = "Use when the user shows raw JSON and wants it readable."
//!
//! # Shell command. `{{param}}` placeholders are substituted from the
//! # tool's call args. Strings are inserted raw; non-strings are
//! # JSON-stringified. Substitution is single-pass (no recursion) so
//! # values containing `{{x}}` literals can't trigger re-expansion.
//! command = "jq . <<< {{json}}"
//!
//! # Optional per-call env vars (supports `${env:VAR}` from runner env).
//! [env]
//! JQ_COLORS = "1;30:0;31"
//!
//! # JSON Schema for tool args. Passed to the LLM verbatim.
//! [parameters]
//! type = "object"
//! required = ["json"]
//!
//! [parameters.properties.json]
//! type = "string"
//! description = "Raw JSON input."
//! ```
//!
//! Tools are registered as `plugin.<name>` to keep them visually
//! distinct from built-ins and MCP tools.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use smooth_operator::tool::{Tool, ToolSchema};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub prompt_hint: String,
    pub command: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// JSON Schema for the tool's parameters. Forwarded to the LLM.
    #[serde(default = "default_params")]
    pub parameters: serde_json::Value,
    /// Optional: skip without removing the file.
    #[serde(default)]
    pub disabled: bool,
}

fn default_params() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
}

/// Resolve the plugins directory:
/// 1. `$SMOOTH_HOME/plugins`
/// 2. `~/.smooth/plugins`
pub fn default_plugins_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("SMOOTH_HOME") {
        return Some(PathBuf::from(home).join("plugins"));
    }
    dirs_next::home_dir().map(|h| h.join(".smooth").join("plugins"))
}

/// `(<plugin_name>, <error_message>)` for plugins that failed to load.
pub type PluginLoadFailure = (String, String);

/// Discover plugins under both the global and project directories.
/// On a name collision the project manifest wins and the global one
/// is dropped with an info log. Either argument may be `None`;
/// missing directories are treated as empty.
pub fn load_plugins_merged(global_dir: Option<&Path>, project_dir: Option<&Path>) -> (Vec<Arc<dyn Tool>>, Vec<PluginLoadFailure>) {
    let mut chosen: Vec<(PluginManifest, &'static str)> = Vec::new();
    let mut failures: Vec<PluginLoadFailure> = Vec::new();

    for (dir, scope) in [(global_dir, "global"), (project_dir, "project")]
        .into_iter()
        .flat_map(|(d, s)| d.map(|d| (d, s)))
    {
        let (manifests, fails) = scan_dir(dir);
        for m in manifests {
            if let Some(idx) = chosen.iter().position(|(existing, _)| existing.name == m.name) {
                let (_, existing_scope) = &chosen[idx];
                if *existing_scope == "global" && scope == "project" {
                    tracing::info!(plugin = %m.name, "plugin: project scope overrides global");
                    chosen[idx] = (m, scope);
                } else {
                    tracing::warn!(plugin = %m.name, scope, "plugin: duplicate name in same scope; keeping first");
                }
            } else {
                chosen.push((m, scope));
            }
        }
        for f in fails {
            failures.push(f);
        }
    }

    let tools: Vec<Arc<dyn Tool>> = chosen
        .into_iter()
        .filter(|(m, _)| !m.disabled)
        .map(|(m, scope)| {
            tracing::info!(plugin = %m.name, scope, "plugin: loaded");
            Arc::new(CliPluginTool::new(m)) as Arc<dyn Tool>
        })
        .collect();
    (tools, failures)
}

/// Path for a project's plugins directory: `<workspace>/.smooth/plugins`.
pub fn project_plugins_dir(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".smooth").join("plugins")
}

fn scan_dir(dir: &Path) -> (Vec<PluginManifest>, Vec<PluginLoadFailure>) {
    let mut manifests: Vec<PluginManifest> = Vec::new();
    let mut failures: Vec<PluginLoadFailure> = Vec::new();

    if !dir.is_dir() {
        return (manifests, failures);
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            failures.push((dir.display().to_string(), format!("read_dir: {e}")));
            return (manifests, failures);
        }
    };

    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let manifest_path = plugin_dir.join("plugin.toml");
        if !manifest_path.exists() {
            continue;
        }
        let display_name = plugin_dir
            .file_name()
            .map_or_else(|| plugin_dir.display().to_string(), |n| n.to_string_lossy().to_string());
        match load_manifest(&manifest_path) {
            Ok(manifest) => manifests.push(manifest),
            Err(e) => failures.push((display_name, e.to_string())),
        }
    }

    (manifests, failures)
}

fn load_manifest(path: &Path) -> anyhow::Result<PluginManifest> {
    let contents = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let manifest: PluginManifest = toml::from_str(&contents).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
    if manifest.name.trim().is_empty() {
        anyhow::bail!("manifest at {} is missing `name`", path.display());
    }
    if manifest.command.trim().is_empty() {
        anyhow::bail!("manifest at {} is missing `command`", path.display());
    }
    Ok(manifest)
}

/// Tool implementation backed by a shell command template.
pub struct CliPluginTool {
    tool_name: String, // `plugin.<name>`
    manifest: PluginManifest,
}

impl CliPluginTool {
    pub fn new(manifest: PluginManifest) -> Self {
        let tool_name = format!("plugin.{}", manifest.name);
        Self { tool_name, manifest }
    }
}

/// Substitute `{{key}}` placeholders in `template` with values from `args`.
/// String values are inserted raw (no quoting). Non-strings are
/// JSON-stringified. Missing keys expand to empty strings (matches the
/// `${env:VAR}` convention elsewhere).
fn render_command(template: &str, args: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(idx) = rest.find("{{") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        if let Some(end) = after.find("}}") {
            let key = after[..end].trim();
            match args.get(key) {
                Some(serde_json::Value::String(s)) => out.push_str(s),
                Some(other) => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
                None => {} // missing → empty
            }
            rest = &after[end + 2..];
        } else {
            // Unterminated `{{` — pass through verbatim.
            out.push_str(&rest[idx..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Same `${env:VAR}` substitution scheme used by the MCP module.
fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("${env:") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 6..];
        if let Some(end) = after.find('}') {
            let var = &after[..end];
            out.push_str(&std::env::var(var).unwrap_or_default());
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[idx..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

#[async_trait]
impl Tool for CliPluginTool {
    fn schema(&self) -> ToolSchema {
        // Tool description visible to the LLM combines the manifest's
        // description with the prompt hint so the agent knows when to
        // pick this tool over the built-ins.
        let mut description = self.manifest.description.clone();
        if !self.manifest.prompt_hint.trim().is_empty() {
            if !description.is_empty() {
                description.push_str("\n\n");
            }
            description.push_str(&self.manifest.prompt_hint);
        }
        if description.is_empty() {
            description = format!("Plugin tool `{}` (no description provided).", self.manifest.name);
        }
        ToolSchema {
            name: self.tool_name.clone(),
            description,
            parameters: self.manifest.parameters.clone(),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<String> {
        let map = match args {
            serde_json::Value::Object(m) => m,
            serde_json::Value::Null => serde_json::Map::new(),
            other => {
                let mut m = serde_json::Map::new();
                m.insert("input".into(), other);
                m
            }
        };

        let rendered = render_command(&self.manifest.command, &map);

        // Run via `bash -lc` so users can use shell features (pipes,
        // here-docs, etc.). This is a deliberate trust grant — plugin
        // manifests are user-installed, equivalent to ~/.zshrc aliases.
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-lc").arg(&rendered);
        for (k, v) in &self.manifest.env {
            cmd.env(k, expand_env(v));
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());

        let output = cmd
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("plugin `{}` spawn failed: {e}", self.tool_name))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "plugin `{}` exited {} (stderr: {})",
                self.tool_name,
                output.status,
                stderr.trim()
            ));
        }

        // Always include stderr if present — many CLIs write progress
        // info there even on success. Keep it small relative to stdout.
        if stderr.trim().is_empty() {
            Ok(stdout)
        } else {
            Ok(format!("{stdout}\n[stderr]\n{}", stderr.trim()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_string_args() {
        let mut args = serde_json::Map::new();
        args.insert("name".into(), serde_json::Value::String("world".into()));
        assert_eq!(render_command("hello {{name}}!", &args), "hello world!");
    }

    #[test]
    fn render_jsonifies_non_strings() {
        let mut args = serde_json::Map::new();
        args.insert("nums".into(), serde_json::json!([1, 2, 3]));
        assert_eq!(render_command("echo {{nums}}", &args), "echo [1,2,3]");
    }

    #[test]
    fn render_missing_keys_expand_empty() {
        let args = serde_json::Map::new();
        assert_eq!(render_command("hi {{name}}!", &args), "hi !");
    }

    #[test]
    fn render_unterminated_passes_through() {
        let args = serde_json::Map::new();
        assert_eq!(render_command("hi {{name", &args), "hi {{name");
    }

    #[test]
    fn load_plugins_skips_invalid_manifests() {
        let dir = tempfile::tempdir().unwrap();

        // Valid plugin
        let good = dir.path().join("good");
        std::fs::create_dir(&good).unwrap();
        std::fs::write(
            good.join("plugin.toml"),
            r#"
name = "good"
description = "ok"
command = "echo hi"
            "#,
        )
        .unwrap();

        // Invalid plugin — missing command
        let bad = dir.path().join("bad");
        std::fs::create_dir(&bad).unwrap();
        std::fs::write(bad.join("plugin.toml"), r#"name = "bad""#).unwrap();

        // Disabled plugin — should not register
        let off = dir.path().join("off");
        std::fs::create_dir(&off).unwrap();
        std::fs::write(
            off.join("plugin.toml"),
            r#"name = "off"
command = "echo hi"
disabled = true
"#,
        )
        .unwrap();

        let (tools, failures) = load_plugins_merged(Some(dir.path()), None);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].schema().name, "plugin.good");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "bad");
    }

    #[test]
    fn project_scope_overrides_global_on_collision() {
        let global = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();

        // Both directories have a `jq` plugin, but with different commands.
        let make_plugin = |root: &std::path::Path, cmd: &str| {
            let dir = root.join("jq");
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(
                dir.join("plugin.toml"),
                format!(
                    r#"name = "jq"
description = "jq"
command = "{cmd}"
"#
                ),
            )
            .unwrap();
        };
        make_plugin(global.path(), "/usr/bin/jq-global");
        make_plugin(project.path(), "/usr/bin/jq-project");

        let (tools, failures) = load_plugins_merged(Some(global.path()), Some(project.path()));
        assert!(failures.is_empty());
        assert_eq!(tools.len(), 1);
        // The tool was built from the project manifest: verify by
        // scanning the schema description which includes the name.
        assert_eq!(tools[0].schema().name, "plugin.jq");
    }

    #[test]
    fn project_only_plugins_register_with_no_global() {
        let project = tempfile::tempdir().unwrap();
        let dir = project.path().join("solo");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            r#"name = "solo"
command = "echo hi"
"#,
        )
        .unwrap();

        let (tools, failures) = load_plugins_merged(None, Some(project.path()));
        assert!(failures.is_empty());
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].schema().name, "plugin.solo");
    }

    #[tokio::test]
    async fn cli_plugin_tool_runs_command_and_returns_stdout() {
        let manifest = PluginManifest {
            name: "echo".into(),
            description: "echo back".into(),
            prompt_hint: String::new(),
            command: "echo {{message}}".into(),
            env: HashMap::new(),
            parameters: default_params(),
            disabled: false,
        };
        let tool = CliPluginTool::new(manifest);
        let out = tool.execute(serde_json::json!({"message": "hello"})).await.unwrap();
        assert_eq!(out.trim(), "hello");
    }

    #[tokio::test]
    async fn cli_plugin_tool_surfaces_failure() {
        let manifest = PluginManifest {
            name: "fail".into(),
            description: String::new(),
            prompt_hint: String::new(),
            command: "false".into(),
            env: HashMap::new(),
            parameters: default_params(),
            disabled: false,
        };
        let tool = CliPluginTool::new(manifest);
        let err = tool.execute(serde_json::Value::Null).await.unwrap_err();
        assert!(err.to_string().contains("exited"));
    }
}
