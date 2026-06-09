//! MCP server config — TOML schema shared with `smooth-operative`.
//!
//! The runner is the consumer; this module exists so `th mcp` commands can
//! manage `~/.smooth/mcp.toml` without pulling rmcp into the CLI binary.
//! Keep the schema in lockstep with `crates/smooth-operative/src/mcp.rs`
//! (`McpConfig` / `McpServerConfig`) — they round-trip through the same file.
//!
//! ## Shipped defaults
//!
//! Smooth registers a small set of MCP servers in the user-global config on
//! `th up` (and via `th mcp install`) so every fresh install gets useful
//! tooling out of the box without requiring the user to memorise spawn
//! commands. Defaults are written **only if absent** — once the user has
//! customised an entry (or removed it), Smooth never re-adds it. Project
//! configs (`<repo>/.smooth/mcp.toml`) continue to shadow globals as before.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
}

impl McpConfig {
    /// Default config path: `$SMOOTH_HOME/mcp.toml` if set, else
    /// `~/.smooth/mcp.toml`.
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(home) = std::env::var("SMOOTH_HOME") {
            return Some(PathBuf::from(home).join("mcp.toml"));
        }
        dirs_next::home_dir().map(|h| h.join(".smooth").join("mcp.toml"))
    }

    /// Project-scoped config: `<repo_root>/.smooth/mcp.toml`. Walks up
    /// from `cwd` to find the nearest `.smooth/` or `.git/` directory.
    /// If neither is found, uses `cwd/.smooth/mcp.toml` verbatim.
    pub fn project_path() -> std::io::Result<PathBuf> {
        let cwd = std::env::current_dir()?;
        Ok(find_project_root(&cwd).unwrap_or(cwd).join(".smooth").join("mcp.toml"))
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        toml::from_str(&contents).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&McpServerConfig> {
        self.servers.iter().find(|s| s.name == name)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let len = self.servers.len();
        self.servers.retain(|s| s.name != name);
        self.servers.len() < len
    }
}

/// Walk upwards from `start` until we find a directory containing a
/// `.smooth/` or `.git/` entry. Returns that directory.
pub fn find_project_root(start: &std::path::Path) -> Option<PathBuf> {
    let mut cur = Some(start.to_path_buf());
    while let Some(dir) = cur {
        if dir.join(".smooth").is_dir() || dir.join(".git").exists() {
            return Some(dir);
        }
        cur = dir.parent().map(std::path::Path::to_path_buf);
    }
    None
}

/// Expand `${env:VAR}` references using the current process environment.
/// Unset variables expand to empty strings. Unterminated references are
/// passed through verbatim.
pub fn expand_env(input: &str) -> String {
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

// ---------------------------------------------------------------------------
// Shipped defaults
// ---------------------------------------------------------------------------

/// A shipped-default MCP server.
///
/// `notes` is what we surface to the user when the default's spawn command
/// isn't on PATH — usually the install one-liner. Kept here (not in `th up`)
/// so `th mcp install` can reuse the same text.
#[derive(Debug, Clone)]
pub struct DefaultMcpServer {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// Probe binary used to check whether the spawn host has the prereq
    /// runtime (e.g. `npx`). Independent of `command` because the runtime
    /// inside the VM is what matters for `command` resolution, but the
    /// presence check runs on the host where we can actually call
    /// `which(1)`.
    pub host_probe: &'static str,
    pub install_hint: &'static str,
    pub description: &'static str,
}

impl DefaultMcpServer {
    fn to_server_config(&self) -> McpServerConfig {
        McpServerConfig {
            name: self.name.to_string(),
            command: self.command.to_string(),
            args: self.args.iter().map(|s| (*s).to_string()).collect(),
            env: HashMap::new(),
            disabled: false,
        }
    }
}

/// The full set of MCP servers Smooth ships by default. Adding to this list
/// means every fresh `th up` will register the entry into
/// `~/.smooth/mcp.toml` if it's not already present.
pub fn default_mcp_servers() -> &'static [DefaultMcpServer] {
    // SAFETY-IN-DESIGN: each default must (a) be MIT-or-equivalent licensed,
    // (b) spawn via a widely available runtime (npx, uvx) so the runner can
    // reach it inside the operator microVM, and (c) be value-add enough to
    // be on by default. The bar is high — every default is a tool every
    // operator sees on every run.
    const DEFAULTS: &[DefaultMcpServer] = &[DefaultMcpServer {
        name: "budget-aware-mcp",
        command: "npx",
        // `-y` so the first invocation auto-accepts the install prompt and
        // the MCP handshake doesn't time out on a TTY question.
        args: &["-y", "budget-aware-mcp"],
        host_probe: "npx",
        install_hint: "npm install -g budget-aware-mcp  (or rely on `npx -y` auto-fetch)",
        description: "Budget-aware code-graph queries (graph_walk, search_graph, check_scope, \
                      explain_symbol, find_dead_code, …) — returns structurally-connected code \
                      under an explicit token budget instead of dumping whole files.",
    }];
    DEFAULTS
}

/// Outcome of `ensure_default_mcp_servers` for a single default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultOutcome {
    /// The entry was missing and we wrote it.
    Added,
    /// An entry with this name already exists — left alone (user owns it).
    AlreadyPresent,
    /// The user explicitly disabled this default via `[[disabled_defaults]]`
    /// in the config (future hook — not currently emitted; carried as a
    /// stable wire-shape so the matching arm in `th mcp install` stays
    /// future-proof against a coming opt-out lever).
    #[allow(dead_code)]
    SkippedByUser,
}

/// Insert any shipped defaults that are not yet in the given config file.
/// Returns one outcome per default in the order from [`default_mcp_servers`].
///
/// The function is intentionally idempotent and conservative: if a default
/// name is already in the config — even if its command/args differ from the
/// shipped version — we treat it as user-owned and never touch it. That
/// makes this safe to call on every `th up` boot.
pub fn ensure_default_mcp_servers(path: &std::path::Path) -> anyhow::Result<Vec<(String, DefaultOutcome)>> {
    let mut cfg = McpConfig::load(path).unwrap_or_default();
    let mut changed = false;
    let mut report: Vec<(String, DefaultOutcome)> = Vec::new();
    for d in default_mcp_servers() {
        if cfg.find(d.name).is_some() {
            report.push((d.name.to_string(), DefaultOutcome::AlreadyPresent));
            continue;
        }
        cfg.servers.push(d.to_server_config());
        report.push((d.name.to_string(), DefaultOutcome::Added));
        changed = true;
    }
    if changed {
        cfg.save(path)?;
    }
    Ok(report)
}

/// Check whether the shipped-default runtime (`npx`, `uvx`, …) is on PATH.
/// Used by the `th up` startup banner to log a clear install hint when the
/// default would otherwise fail at spawn time. Returns `true` if reachable.
pub fn host_probe_on_path(probe: &str) -> bool {
    // No `which` crate dep on the CLI — fall back to walking $PATH. Keeps
    // the binary slim and behaves identically across macOS/Linux/Windows
    // (we additionally check `.exe` on Windows so `npx.cmd` resolves).
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(probe);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let mut c = candidate.clone();
                c.set_extension(ext);
                if c.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");

        let mut cfg = McpConfig::default();
        cfg.servers.push(McpServerConfig {
            name: "playwright".into(),
            command: "npx".into(),
            args: vec!["@playwright/mcp@latest".into()],
            env: HashMap::from([("BROWSER".into(), "chromium".into())]),
            disabled: false,
        });
        cfg.save(&path).unwrap();

        let loaded = McpConfig::load(&path).unwrap();
        assert_eq!(loaded.servers.len(), 1);
        assert_eq!(loaded.find("playwright").unwrap().command, "npx");

        let mut loaded = loaded;
        assert!(loaded.remove("playwright"));
        assert!(loaded.servers.is_empty());
        assert!(!loaded.remove("playwright"));
    }

    #[test]
    fn default_set_includes_budget_aware_mcp() {
        let defaults = default_mcp_servers();
        assert!(
            defaults.iter().any(|d| d.name == "budget-aware-mcp"),
            "budget-aware-mcp must ship as a default — it's the operator's \
             primary context-budget tool"
        );
        let bam = defaults.iter().find(|d| d.name == "budget-aware-mcp").unwrap();
        assert_eq!(bam.command, "npx");
        assert!(bam.args.contains(&"-y"));
        assert!(bam.args.contains(&"budget-aware-mcp"));
        // Description should mention the headline tools so docs/listing surface them.
        assert!(bam.description.contains("graph_walk"));
        assert!(bam.description.contains("check_scope"));
    }

    #[test]
    fn ensure_defaults_writes_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        assert!(!path.exists(), "precondition: config file does not exist");

        let report = ensure_default_mcp_servers(&path).expect("ensure_defaults succeeds on missing config");
        assert!(path.exists(), "config file written");
        let cfg = McpConfig::load(&path).unwrap();
        assert!(cfg.find("budget-aware-mcp").is_some(), "budget-aware-mcp registered");

        for (_, outcome) in &report {
            assert_eq!(*outcome, DefaultOutcome::Added);
        }
    }

    #[test]
    fn ensure_defaults_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        ensure_default_mcp_servers(&path).unwrap();
        let mtime_first = std::fs::metadata(&path).unwrap().modified().unwrap();
        // Re-run — must not bump mtime, must not duplicate entries.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let report = ensure_default_mcp_servers(&path).unwrap();
        for (_, outcome) in &report {
            assert_eq!(*outcome, DefaultOutcome::AlreadyPresent);
        }
        let cfg = McpConfig::load(&path).unwrap();
        let count = cfg.servers.iter().filter(|s| s.name == "budget-aware-mcp").count();
        assert_eq!(count, 1, "no duplicates");
        let mtime_second = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_first, mtime_second, "no rewrite when nothing changes");
    }

    #[test]
    fn ensure_defaults_respects_user_customization() {
        // If the user has already added their own `budget-aware-mcp` with a
        // different command, we MUST leave it alone — this is the "your
        // config wins" guarantee.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        let mut cfg = McpConfig::default();
        cfg.servers.push(McpServerConfig {
            name: "budget-aware-mcp".into(),
            command: "/usr/local/bin/budget-aware-mcp".into(),
            args: vec!["--from-source".into()],
            env: HashMap::new(),
            disabled: false,
        });
        cfg.save(&path).unwrap();

        let report = ensure_default_mcp_servers(&path).unwrap();
        for (name, outcome) in &report {
            if name == "budget-aware-mcp" {
                assert_eq!(*outcome, DefaultOutcome::AlreadyPresent);
            }
        }
        let after = McpConfig::load(&path).unwrap();
        let entry = after.find("budget-aware-mcp").unwrap();
        assert_eq!(entry.command, "/usr/local/bin/budget-aware-mcp", "user override preserved");
        assert_eq!(entry.args, vec!["--from-source"]);
    }

    #[test]
    fn host_probe_returns_false_for_obviously_missing_binary() {
        // 32 random chars — vanishingly unlikely to exist on any $PATH.
        let probe = "smooth_test_definitely_not_a_real_binary_xyz123abc";
        assert!(!host_probe_on_path(probe));
    }

    #[test]
    fn expand_env_substitutes_and_handles_missing() {
        std::env::set_var("SMOOTH_CLI_TEST_VAR", "value");
        assert_eq!(expand_env("${env:SMOOTH_CLI_TEST_VAR}"), "value");
        std::env::remove_var("SMOOTH_CLI_TEST_VAR");

        std::env::remove_var("SMOOTH_CLI_TEST_MISSING");
        assert_eq!(expand_env("a-${env:SMOOTH_CLI_TEST_MISSING}-b"), "a--b");
        assert_eq!(expand_env("prefix-${env:UNCLOSED"), "prefix-${env:UNCLOSED");
    }
}
