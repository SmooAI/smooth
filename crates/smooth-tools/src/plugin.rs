//! File-based CLI-wrapper plugins — the zero-code tool tier (pearl th-262e5f).
//!
//! A plugin is a TOML manifest describing a shell command:
//! `$SMOOTH_HOME/plugins/<name>/plugin.toml` (global) and
//! `<workspace>/.smooth/plugins/<name>/plugin.toml` (project). On a name
//! collision the project manifest wins — the same merge rule as `mcp.toml`.
//! Each enabled manifest becomes a [`Tool`] named `plugin_<name>`, registered on
//! the daemon's per-turn registry so it sits behind the permission gate and Narc
//! like any built-in.
//!
//! This is the lighter-weight cousin of MCP: no separate server process, no
//! JSON-RPC, just "render this command template and run it". For anything
//! stateful or with a typed protocol, prefer MCP.
//!
//! The rendered command runs through [`crate::sandbox::SandboxedCommand`] —
//! the same kernel OS sandbox `bash` uses — so a plugin cannot write outside the
//! workspace, read credential stores, or bypass the goalie egress allowlist.
//!
//! ## Manifest format
//!
//! ```toml
//! name = "jq_pretty"
//! description = "Pretty-print JSON with jq."
//! prompt_hint = "Use when the user shows raw JSON and wants it readable."
//!
//! # `{{param}}` placeholders are substituted from the tool's call args.
//! # Strings are inserted raw; non-strings are JSON-stringified. Substitution
//! # is single-pass, so a value containing `{{x}}` can't re-expand.
//! command = "jq . <<< {{json}}"
//!
//! # Optional per-call env vars (supports `${env:VAR}` from the daemon's env).
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use smooth_operator::{Tool, ToolSchema};

/// A plugin manifest, as parsed from `plugin.toml`.
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
    /// JSON Schema for the tool's parameters. Forwarded to the LLM verbatim.
    #[serde(default = "default_params")]
    pub parameters: serde_json::Value,
    /// Skip registration without removing the file.
    #[serde(default)]
    pub disabled: bool,
}

fn default_params() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
}

/// Which directory a manifest came from. Project shadows global.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginScope {
    Global,
    Project,
}

/// A manifest plus where it came from — what the catalog surface renders and
/// what [`tools_for`] turns into tools.
#[derive(Debug, Clone, Serialize)]
pub struct LoadedPlugin {
    #[serde(flatten)]
    pub manifest: PluginManifest,
    pub scope: PluginScope,
    /// The tool name the agent sees (`plugin_<name>`).
    pub tool: String,
    pub path: PathBuf,
}

/// `(<plugin_dir_name>, <error_message>)` for manifests that failed to load.
pub type PluginLoadFailure = (String, String);

/// The global plugins directory: `$SMOOTH_HOME/plugins`, else `~/.smooth/plugins`.
#[must_use]
pub fn default_plugins_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("SMOOTH_HOME") {
        return Some(PathBuf::from(home).join("plugins"));
    }
    dirs_next::home_dir().map(|h| h.join(".smooth").join("plugins"))
}

/// A workspace's plugins directory: `<workspace>/.smooth/plugins`.
#[must_use]
pub fn project_plugins_dir(workspace: &Path) -> PathBuf {
    workspace.join(".smooth").join("plugins")
}

/// The tool name for a manifest name.
///
/// Provider tool names must match `^[a-zA-Z0-9_-]{1,64}$` — Anthropic and the
/// OpenAI-compatible gateway both reject anything else for the WHOLE request —
/// so anything exotic in the plugin's name becomes `_`.
#[must_use]
pub fn tool_name(plugin_name: &str) -> String {
    let cleaned: String = plugin_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(57) // 64 - len("plugin_")
        .collect();
    format!("plugin_{cleaned}")
}

/// Discover the merged plugin catalog for `workspace`.
///
/// Global manifests overlaid with the workspace's project manifests (project
/// wins on a name collision). Disabled manifests are included — the catalog
/// shows them, and [`tools_for`] is what filters them out.
#[must_use]
pub fn discover(workspace: &Path) -> (Vec<LoadedPlugin>, Vec<PluginLoadFailure>) {
    discover_in(default_plugins_dir().as_deref(), Some(&project_plugins_dir(workspace)))
}

/// [`discover`] against explicit directories (either may be `None`; missing
/// directories are empty).
#[must_use]
pub fn discover_in(global_dir: Option<&Path>, project_dir: Option<&Path>) -> (Vec<LoadedPlugin>, Vec<PluginLoadFailure>) {
    let mut chosen: Vec<LoadedPlugin> = Vec::new();
    let mut failures: Vec<PluginLoadFailure> = Vec::new();

    for (dir, scope) in [(global_dir, PluginScope::Global), (project_dir, PluginScope::Project)]
        .into_iter()
        .filter_map(|(d, s)| d.map(|d| (d, s)))
    {
        let (loaded, fails) = scan_dir(dir, scope);
        for plugin in loaded {
            if let Some(idx) = chosen.iter().position(|existing| existing.manifest.name == plugin.manifest.name) {
                if chosen[idx].scope == PluginScope::Global && scope == PluginScope::Project {
                    tracing::info!(plugin = %plugin.manifest.name, "plugin: project scope overrides global");
                    chosen[idx] = plugin;
                } else {
                    tracing::warn!(plugin = %plugin.manifest.name, ?scope, "plugin: duplicate name in same scope; keeping first");
                }
            } else {
                chosen.push(plugin);
            }
        }
        failures.extend(fails);
    }

    chosen.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    (chosen, failures)
}

/// The enabled plugins of `workspace` as registrable tools, each running its
/// command sandboxed to `workspace` with egress through `proxy`.
#[must_use]
pub fn tools_for(workspace: &Path, proxy: Option<&str>) -> Vec<Arc<dyn Tool>> {
    let (plugins, failures) = discover(workspace);
    for (name, err) in &failures {
        tracing::warn!(plugin = %name, error = %err, "plugin: manifest failed to load");
    }
    build_tools(plugins, workspace, proxy)
}

/// Turn discovered manifests into tools, dropping the disabled ones.
fn build_tools(plugins: Vec<LoadedPlugin>, workspace: &Path, proxy: Option<&str>) -> Vec<Arc<dyn Tool>> {
    plugins
        .into_iter()
        .filter(|p| !p.manifest.disabled)
        .map(|p| {
            tracing::debug!(plugin = %p.manifest.name, tool = %p.tool, scope = ?p.scope, "plugin: registered");
            Arc::new(CliPluginTool {
                tool_name: p.tool,
                manifest: p.manifest,
                workspace: workspace.to_path_buf(),
                proxy: proxy.map(str::to_owned),
            }) as Arc<dyn Tool>
        })
        .collect()
}

fn scan_dir(dir: &Path, scope: PluginScope) -> (Vec<LoadedPlugin>, Vec<PluginLoadFailure>) {
    let mut loaded: Vec<LoadedPlugin> = Vec::new();
    let mut failures: Vec<PluginLoadFailure> = Vec::new();

    if !dir.is_dir() {
        return (loaded, failures);
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            failures.push((dir.display().to_string(), format!("read_dir: {e}")));
            return (loaded, failures);
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
            Ok(manifest) => loaded.push(LoadedPlugin {
                tool: tool_name(&manifest.name),
                manifest,
                scope,
                path: manifest_path,
            }),
            Err(e) => failures.push((display_name, e.to_string())),
        }
    }

    (loaded, failures)
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
struct CliPluginTool {
    tool_name: String,
    manifest: PluginManifest,
    workspace: PathBuf,
    proxy: Option<String>,
}

/// Substitute `{{key}}` placeholders in `template` with values from `args`.
/// String values are inserted raw (no quoting); non-strings are
/// JSON-stringified; missing keys expand to empty. Single-pass, so a value
/// containing `{{x}}` can't trigger recursion.
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

/// Same `${env:VAR}` substitution scheme the MCP config uses.
fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("${env:") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 6..];
        if let Some(end) = after.find('}') {
            out.push_str(&std::env::var(&after[..end]).unwrap_or_default());
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
        // The LLM sees description + prompt_hint: what it does, then when to
        // reach for it.
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

    fn is_concurrent_safe(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let map = match arguments {
            serde_json::Value::Object(m) => m,
            serde_json::Value::Null => serde_json::Map::new(),
            other => {
                let mut m = serde_json::Map::new();
                m.insert("input".into(), other);
                m
            }
        };
        let rendered = render_command(&self.manifest.command, &map);

        // Same deny gates + kernel sandbox as the `bash` tool: a plugin is a
        // user-installed shell alias, not an escape hatch around them.
        if crate::guard::is_circuit_breaker(&rendered) {
            return Ok(format!("BLOCKED: plugin `{}` rendered a circuit-breaker command: {rendered}", self.tool_name));
        }
        if crate::permission::bash_denied(&rendered) {
            return Ok(format!("BLOCKED: a permission policy (deny) rule refused this command: {rendered}"));
        }

        let mut policy = crate::sandbox::SandboxPolicy::for_workspace(self.workspace.clone());
        if let Some(addr) = &self.proxy {
            policy = policy.with_proxy(addr.clone());
        }
        let mut cmd = crate::sandbox::SandboxedCommand::shell(&policy, &rendered).into_command();
        cmd.current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &self.manifest.env {
            cmd.env(k, expand_env(v));
        }

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
        // Many CLIs write progress to stderr even on success — keep it.
        if stderr.trim().is_empty() {
            Ok(stdout)
        } else {
            Ok(format!("{stdout}\n[stderr]\n{}", stderr.trim()))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn write_plugin(root: &Path, name: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), body).unwrap();
    }

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
        assert_eq!(render_command("hi {{name}}!", &serde_json::Map::new()), "hi !");
    }

    #[test]
    fn render_unterminated_passes_through() {
        assert_eq!(render_command("hi {{name", &serde_json::Map::new()), "hi {{name");
    }

    #[test]
    fn tool_names_are_provider_safe() {
        assert_eq!(tool_name("jq"), "plugin_jq");
        assert_eq!(tool_name("deploy.staging"), "plugin_deploy_staging");
        assert!(tool_name(&"x".repeat(100)).len() <= 64);
    }

    #[test]
    fn discover_reports_invalid_manifests_and_keeps_disabled_in_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(dir.path(), "good", "name = \"good\"\ndescription = \"ok\"\ncommand = \"echo hi\"\n");
        write_plugin(dir.path(), "bad", "name = \"bad\"\n"); // no command
        write_plugin(dir.path(), "off", "name = \"off\"\ncommand = \"echo hi\"\ndisabled = true\n");

        let (plugins, failures) = discover_in(Some(dir.path()), None);
        assert_eq!(plugins.len(), 2, "good + off (disabled still catalogued)");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "bad");

        // `tools_for` reads the real global dir, so drive the same build step
        // the daemon's provider uses against the fixture directory.
        let tools = build_tools(plugins, &std::env::temp_dir(), None);
        assert_eq!(tools.len(), 1, "only the enabled, valid plugin registers");
        assert_eq!(tools[0].schema().name, "plugin_good");
    }

    #[test]
    fn project_scope_overrides_global_on_collision() {
        let global = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_plugin(global.path(), "jq", "name = \"jq\"\ndescription = \"global jq\"\ncommand = \"jq-global\"\n");
        write_plugin(project.path(), "jq", "name = \"jq\"\ndescription = \"project jq\"\ncommand = \"jq-project\"\n");

        let (plugins, failures) = discover_in(Some(global.path()), Some(project.path()));
        assert!(failures.is_empty());
        assert_eq!(plugins.len(), 1, "the collision collapses to one entry");
        assert_eq!(plugins[0].scope, PluginScope::Project);
        assert_eq!(plugins[0].manifest.command, "jq-project");
        assert_eq!(plugins[0].tool, "plugin_jq");
    }

    #[test]
    fn schema_combines_description_and_prompt_hint() {
        let tool = CliPluginTool {
            tool_name: "plugin_x".into(),
            manifest: PluginManifest {
                name: "x".into(),
                description: "does x".into(),
                prompt_hint: "use for x".into(),
                command: "true".into(),
                env: HashMap::new(),
                parameters: default_params(),
                disabled: false,
            },
            workspace: std::env::temp_dir(),
            proxy: None,
        };
        let schema = tool.schema();
        assert_eq!(schema.name, "plugin_x");
        assert!(schema.description.contains("does x") && schema.description.contains("use for x"));
    }

    #[tokio::test]
    async fn executes_the_rendered_command_in_the_sandbox() {
        let workspace = tempfile::tempdir().unwrap();
        let tool = CliPluginTool {
            tool_name: "plugin_echo".into(),
            manifest: PluginManifest {
                name: "echo".into(),
                description: "echo".into(),
                prompt_hint: String::new(),
                command: "echo {{msg}}".into(),
                env: HashMap::new(),
                parameters: default_params(),
                disabled: false,
            },
            workspace: workspace.path().to_path_buf(),
            proxy: None,
        };
        let out = tool.execute(serde_json::json!({"msg": "hello"})).await.unwrap();
        assert!(out.contains("hello"), "{out}");
    }

    #[tokio::test]
    async fn nonzero_exit_is_an_error() {
        let workspace = tempfile::tempdir().unwrap();
        let tool = CliPluginTool {
            tool_name: "plugin_fail".into(),
            manifest: PluginManifest {
                name: "fail".into(),
                description: String::new(),
                prompt_hint: String::new(),
                command: "exit 3".into(),
                env: HashMap::new(),
                parameters: default_params(),
                disabled: false,
            },
            workspace: workspace.path().to_path_buf(),
            proxy: None,
        };
        assert!(tool.execute(serde_json::Value::Null).await.is_err());
    }

    #[tokio::test]
    async fn circuit_breaker_commands_are_refused_before_spawn() {
        let workspace = tempfile::tempdir().unwrap();
        let tool = CliPluginTool {
            tool_name: "plugin_nuke".into(),
            manifest: PluginManifest {
                name: "nuke".into(),
                description: String::new(),
                prompt_hint: String::new(),
                command: "rm -rf /".into(),
                env: HashMap::new(),
                parameters: default_params(),
                disabled: false,
            },
            workspace: workspace.path().to_path_buf(),
            proxy: None,
        };
        let out = tool.execute(serde_json::Value::Null).await.unwrap();
        assert!(out.starts_with("BLOCKED:"), "{out}");
    }
}
