//! `th mcp install --harness …` — register `th mcp serve` with a coding harness.
//!
//! Pearl th-2f33b6. All three harnesses we drive get the SAME stdio server
//! (`th mcp serve`), which is the whole point: Claude Code, Codex and OpenCode
//! sessions on one machine then share one agent mailbox and one pearl store,
//! because they are all talking to the same binary.
//!
//! Each harness stores it differently, so there is one writer per format:
//!
//! | Harness | File | Shape |
//! |---|---|---|
//! | claude-code | `~/.claude.json` | `mcpServers.smooth = {type:"stdio", command, args}` |
//! | codex | `~/.codex/config.toml` | `[mcp_servers.smooth]` |
//! | opencode | `~/.config/opencode/opencode.json` | `mcp.smooth = {type:"local", command:[…]}` |
//!
//! Every writer is **idempotent and preserving**: an existing `smooth` entry
//! with the right command is left alone, an entry pointing somewhere else is
//! updated, and everything else in the file survives untouched. The JSON writers
//! keep key order (serde_json's `preserve_order`) and the TOML writer edits the
//! document in place (`toml_edit`) so comments and layout survive — these are
//! user-owned files, and a config editor that reformats them is one users stop
//! trusting.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The server name we register under, in every harness.
pub const SERVER_NAME: &str = "smooth";
/// The command every harness spawns.
const COMMAND: &str = "th";
const ARGS: [&str; 2] = ["mcp", "serve"];

/// A harness `th mcp install` knows how to write to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    ClaudeCode,
    Codex,
    OpenCode,
}

impl Harness {
    /// Every harness, in install order.
    pub const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::OpenCode];

    /// The CLI spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }

    /// Parse a `--harness` value. `all` is handled by the caller.
    ///
    /// # Errors
    /// Returns an error naming the valid values for anything else.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "claudecode" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::OpenCode),
            other => bail!("unknown harness '{other}' (expected claude-code|codex|opencode|all)"),
        }
    }

    /// Config file this harness keeps user-scoped MCP servers in, under `home`.
    #[must_use]
    pub fn config_path(self, home: &Path) -> PathBuf {
        match self {
            Self::ClaudeCode => home.join(".claude.json"),
            Self::Codex => home.join(".codex").join("config.toml"),
            Self::OpenCode => home.join(".config").join("opencode").join("opencode.json"),
        }
    }

    /// The directory whose existence means "this harness is installed here".
    ///
    /// Deliberately the parent *directory*, not the config file: a harness that
    /// has run but never been configured has the directory and no file yet, and
    /// we do want to write for it. Claude Code is the exception — it keeps its
    /// state in `~/.claude.json` directly, so its own `~/.claude` dir is the
    /// marker.
    #[must_use]
    pub fn marker_dir(self, home: &Path) -> PathBuf {
        match self {
            Self::ClaudeCode => home.join(".claude"),
            Self::Codex => home.join(".codex"),
            Self::OpenCode => home.join(".config").join("opencode"),
        }
    }
}

impl std::fmt::Display for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happened to one harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The entry was missing and we wrote it.
    Added,
    /// An entry was already there, pointing at `th mcp serve`. Untouched.
    AlreadyPresent,
    /// An entry was there but pointed elsewhere; we repointed it.
    Updated,
    /// The harness isn't installed here (no config dir) — nothing written.
    NotInstalled,
}

impl Outcome {
    #[must_use]
    pub const fn wrote(&self) -> bool {
        matches!(self, Self::Added | Self::Updated)
    }
}

/// Install into one harness, rooted at `home`. `dry_run` computes the outcome
/// and prints the would-be file content without writing.
///
/// # Errors
/// Returns an error if the existing config can't be read or parsed, or the
/// write fails.
pub fn install_into(harness: Harness, home: &Path, dry_run: bool) -> Result<Outcome> {
    if !harness.marker_dir(home).is_dir() {
        return Ok(Outcome::NotInstalled);
    }
    let path = harness.config_path(home);
    let (outcome, rendered) = match harness {
        Harness::ClaudeCode => render_claude_code(&path)?,
        Harness::Codex => render_codex(&path)?,
        Harness::OpenCode => render_opencode(&path)?,
    };
    if outcome.wrote() && !dry_run {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&path, rendered).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(outcome)
}

/// Read a JSON config, tolerating a missing file (empty object) but never a
/// malformed one — silently replacing a config we failed to parse would eat
/// whatever else the user had in it.
fn load_json(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&raw).with_context(|| format!("parse {} as JSON — fix or move it, then re-run", path.display()))
}

/// `~/.claude.json` → top-level `mcpServers.smooth`.
fn render_claude_code(path: &Path) -> Result<(Outcome, String)> {
    let mut doc = load_json(path)?;
    let entry = serde_json::json!({
        "type": "stdio",
        "command": COMMAND,
        "args": ARGS,
    });
    let root = doc.as_object_mut().ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", path.display()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("`mcpServers` in {} is not an object", path.display()))?;

    let outcome = classify(servers.get(SERVER_NAME), &entry);
    if outcome.wrote() {
        merge_entry(servers, entry);
    }
    Ok((outcome, format!("{}\n", serde_json::to_string_pretty(&doc)?)))
}

/// Write our keys into the `smooth` entry, leaving any key the user added
/// (`env`, `timeout`, …) in place. Replacing the whole object would silently
/// revert their customisation every time this ran.
fn merge_entry(servers: &mut serde_json::Map<String, serde_json::Value>, entry: serde_json::Value) {
    let Some(ours) = entry.as_object() else { return };
    match servers.get_mut(SERVER_NAME).and_then(serde_json::Value::as_object_mut) {
        Some(existing) => {
            for (k, v) in ours {
                existing.insert(k.clone(), v.clone());
            }
        }
        None => {
            servers.insert(SERVER_NAME.to_string(), entry);
        }
    }
}

/// `~/.config/opencode/opencode.json` → `mcp.smooth` (a `local` server).
fn render_opencode(path: &Path) -> Result<(Outcome, String)> {
    let mut doc = load_json(path)?;
    // OpenCode takes the command as ONE argv array, not command + args.
    let entry = serde_json::json!({
        "type": "local",
        "command": [COMMAND, ARGS[0], ARGS[1]],
        "enabled": true,
    });
    let root = doc.as_object_mut().ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", path.display()))?;
    root.entry("$schema").or_insert_with(|| serde_json::json!("https://opencode.ai/config.json"));
    let servers = root
        .entry("mcp")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("`mcp` in {} is not an object", path.display()))?;

    let outcome = classify(servers.get(SERVER_NAME), &entry);
    if outcome.wrote() {
        merge_entry(servers, entry);
    }
    Ok((outcome, format!("{}\n", serde_json::to_string_pretty(&doc)?)))
}

/// Is the existing entry already ours?
///
/// Compares only the fields we own, so a user who added `env` or `timeout`
/// alongside our command keeps them instead of having them silently reverted.
fn classify(existing: Option<&serde_json::Value>, desired: &serde_json::Value) -> Outcome {
    let Some(cur) = existing else { return Outcome::Added };
    let same = desired.as_object().is_some_and(|d| d.iter().all(|(k, v)| cur.get(k) == Some(v)));
    if same {
        Outcome::AlreadyPresent
    } else {
        Outcome::Updated
    }
}

/// `~/.codex/config.toml` → `[mcp_servers.smooth]`, edited in place so the
/// user's comments, key order and formatting survive.
fn render_codex(path: &Path) -> Result<(Outcome, String)> {
    let raw = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("parse {} as TOML — fix or move it, then re-run", path.display()))?;

    let existing = doc.get("mcp_servers").and_then(|t| t.get(SERVER_NAME));
    let matches = existing.is_some_and(|e| {
        e.get("command").and_then(|c| c.as_str()) == Some(COMMAND)
            && e.get("args")
                .and_then(|a| a.as_array())
                .is_some_and(|a| a.iter().filter_map(toml_edit::Value::as_str).eq(ARGS))
    });
    if matches {
        return Ok((Outcome::AlreadyPresent, raw));
    }
    let outcome = if existing.is_some() { Outcome::Updated } else { Outcome::Added };

    let servers = doc["mcp_servers"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    // `[mcp_servers]` itself carries no keys — render it implicitly so the file
    // shows `[mcp_servers.smooth]` and nothing else.
    if let Some(t) = servers.as_table_mut() {
        t.set_implicit(true);
    }
    let mut args = toml_edit::Array::new();
    for a in ARGS {
        args.push(a);
    }
    let entry = &mut doc["mcp_servers"][SERVER_NAME];
    // Indexing alone would materialise an inline `smooth = { … }`; Codex reads
    // either, but a standard `[mcp_servers.smooth]` header is what its own docs
    // and every other tool write, so hand-editing later isn't a surprise.
    if !entry.is_table() {
        *entry = toml_edit::Item::Table(toml_edit::Table::new());
    }
    entry["command"] = toml_edit::value(COMMAND);
    entry["args"] = toml_edit::value(args);
    Ok((outcome, doc.to_string()))
}

/// The home directory `th mcp install` writes under. `$SMOOTH_HARNESS_HOME`
/// overrides it (tests, and anyone with a non-standard layout).
///
/// # Errors
/// Returns an error if no home directory can be resolved.
pub fn harness_home() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("SMOOTH_HARNESS_HOME") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    dirs_next::home_dir().context("no home directory — set $SMOOTH_HARNESS_HOME")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A home with all three harnesses "installed" (their marker dirs exist).
    fn home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        for h in Harness::ALL {
            std::fs::create_dir_all(h.marker_dir(tmp.path())).unwrap();
        }
        tmp
    }

    fn read(h: Harness, home: &Path) -> String {
        std::fs::read_to_string(h.config_path(home)).unwrap()
    }

    fn json_at(h: Harness, home: &Path) -> serde_json::Value {
        serde_json::from_str(&read(h, home)).unwrap()
    }

    #[test]
    fn harness_parses_its_spellings_and_rejects_junk() {
        assert_eq!(Harness::parse("claude-code").unwrap(), Harness::ClaudeCode);
        assert_eq!(Harness::parse(" CODEX ").unwrap(), Harness::Codex);
        assert_eq!(Harness::parse("opencode").unwrap(), Harness::OpenCode);
        assert!(Harness::parse("cursor").is_err());
        assert!(Harness::parse("all").is_err(), "`all` is the caller's job, not a Harness");
    }

    #[test]
    fn claude_code_adds_the_server_and_keeps_everything_else() {
        let tmp = home();
        std::fs::write(
            Harness::ClaudeCode.config_path(tmp.path()),
            r#"{"numStartups":42,"projects":{"/a":{"trust":true}},"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();

        assert_eq!(install_into(Harness::ClaudeCode, tmp.path(), false).unwrap(), Outcome::Added);
        let doc = json_at(Harness::ClaudeCode, tmp.path());
        assert_eq!(doc["mcpServers"]["smooth"]["command"], "th");
        assert_eq!(doc["mcpServers"]["smooth"]["args"], serde_json::json!(["mcp", "serve"]));
        assert_eq!(doc["mcpServers"]["smooth"]["type"], "stdio");
        // Unrelated keys — including a sibling MCP server — are untouched.
        assert_eq!(doc["numStartups"], 42);
        assert_eq!(doc["projects"]["/a"]["trust"], true);
        assert_eq!(doc["mcpServers"]["other"]["command"], "x");
    }

    #[test]
    fn claude_code_is_idempotent() {
        let tmp = home();
        assert_eq!(install_into(Harness::ClaudeCode, tmp.path(), false).unwrap(), Outcome::Added);
        let first = read(Harness::ClaudeCode, tmp.path());
        assert_eq!(install_into(Harness::ClaudeCode, tmp.path(), false).unwrap(), Outcome::AlreadyPresent);
        assert_eq!(read(Harness::ClaudeCode, tmp.path()), first, "a second run must not rewrite the file");
    }

    #[test]
    fn claude_code_repoints_a_stale_entry_but_keeps_extra_keys() {
        let tmp = home();
        std::fs::write(
            Harness::ClaudeCode.config_path(tmp.path()),
            r#"{"mcpServers":{"smooth":{"type":"stdio","command":"/old/path/th","args":["mcp","serve"],"env":{"KEEP":"1"}}}}"#,
        )
        .unwrap();
        assert_eq!(install_into(Harness::ClaudeCode, tmp.path(), false).unwrap(), Outcome::Updated);
        let doc = json_at(Harness::ClaudeCode, tmp.path());
        assert_eq!(doc["mcpServers"]["smooth"]["command"], "th");
        // We own command/args/type; the user owns anything else they added.
        assert_eq!(doc["mcpServers"]["smooth"]["env"]["KEEP"], "1");
    }

    #[test]
    fn opencode_writes_an_argv_array_and_keeps_the_rest() {
        let tmp = home();
        std::fs::write(
            Harness::OpenCode.config_path(tmp.path()),
            r#"{"$schema":"https://opencode.ai/config.json","provider":{"smooai":{"name":"Smoo AI"}}}"#,
        )
        .unwrap();

        assert_eq!(install_into(Harness::OpenCode, tmp.path(), false).unwrap(), Outcome::Added);
        let doc = json_at(Harness::OpenCode, tmp.path());
        // OpenCode's McpLocalConfig takes one argv array, not command + args.
        assert_eq!(doc["mcp"]["smooth"]["type"], "local");
        assert_eq!(doc["mcp"]["smooth"]["command"], serde_json::json!(["th", "mcp", "serve"]));
        assert_eq!(doc["mcp"]["smooth"]["enabled"], true);
        assert_eq!(doc["provider"]["smooai"]["name"], "Smoo AI");
        assert_eq!(doc["$schema"], "https://opencode.ai/config.json");

        assert_eq!(install_into(Harness::OpenCode, tmp.path(), false).unwrap(), Outcome::AlreadyPresent);
    }

    #[test]
    fn opencode_seeds_the_schema_on_a_fresh_file() {
        let tmp = home();
        install_into(Harness::OpenCode, tmp.path(), false).unwrap();
        assert_eq!(json_at(Harness::OpenCode, tmp.path())["$schema"], "https://opencode.ai/config.json");
    }

    #[test]
    fn codex_preserves_comments_and_unknown_tables() {
        let tmp = home();
        std::fs::write(
            Harness::Codex.config_path(tmp.path()),
            "# my codex config\nmodel = \"gpt-5.5\"\n\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();

        assert_eq!(install_into(Harness::Codex, tmp.path(), false).unwrap(), Outcome::Added);
        let out = read(Harness::Codex, tmp.path());
        assert!(out.contains("# my codex config"), "comments must survive:\n{out}");
        assert!(out.contains("model = \"gpt-5.5\""), "{out}");
        assert!(out.contains("trust_level = \"trusted\""), "{out}");
        assert!(out.contains("[mcp_servers.smooth]"), "{out}");

        let doc: toml::Value = toml::from_str(&out).unwrap();
        let entry = &doc["mcp_servers"]["smooth"];
        assert_eq!(entry["command"].as_str(), Some("th"));
        assert_eq!(entry["args"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn codex_is_idempotent_and_repoints_a_stale_entry() {
        let tmp = home();
        assert_eq!(install_into(Harness::Codex, tmp.path(), false).unwrap(), Outcome::Added);
        let first = read(Harness::Codex, tmp.path());
        assert_eq!(install_into(Harness::Codex, tmp.path(), false).unwrap(), Outcome::AlreadyPresent);
        assert_eq!(read(Harness::Codex, tmp.path()), first);

        std::fs::write(
            Harness::Codex.config_path(tmp.path()),
            "[mcp_servers.smooth]\ncommand = \"/old/th\"\nargs = [\"mcp\", \"serve\"]\n",
        )
        .unwrap();
        assert_eq!(install_into(Harness::Codex, tmp.path(), false).unwrap(), Outcome::Updated);
        assert!(read(Harness::Codex, tmp.path()).contains("command = \"th\""));
    }

    #[test]
    fn a_harness_that_is_not_installed_is_skipped_not_created() {
        let tmp = TempDir::new().unwrap(); // no marker dirs at all
        for h in Harness::ALL {
            assert_eq!(install_into(h, tmp.path(), false).unwrap(), Outcome::NotInstalled);
            assert!(!h.config_path(tmp.path()).exists(), "{h} must not be conjured into existence");
        }
    }

    #[test]
    fn dry_run_reports_the_outcome_without_touching_disk() {
        let tmp = home();
        for h in Harness::ALL {
            assert_eq!(install_into(h, tmp.path(), true).unwrap(), Outcome::Added);
            assert!(!h.config_path(tmp.path()).exists(), "{h}: dry run wrote a file");
        }
    }

    #[test]
    fn a_malformed_config_errors_instead_of_being_replaced() {
        let tmp = home();
        std::fs::write(Harness::ClaudeCode.config_path(tmp.path()), "{not json").unwrap();
        std::fs::write(Harness::Codex.config_path(tmp.path()), "[[[").unwrap();
        assert!(install_into(Harness::ClaudeCode, tmp.path(), false).is_err());
        assert!(install_into(Harness::Codex, tmp.path(), false).is_err());
        // The originals are still there — we refuse rather than clobber.
        assert_eq!(read(Harness::ClaudeCode, tmp.path()), "{not json");
    }

    #[test]
    fn an_empty_config_file_is_treated_as_empty_not_malformed() {
        let tmp = home();
        std::fs::write(Harness::ClaudeCode.config_path(tmp.path()), "  \n").unwrap();
        assert_eq!(install_into(Harness::ClaudeCode, tmp.path(), false).unwrap(), Outcome::Added);
        assert_eq!(json_at(Harness::ClaudeCode, tmp.path())["mcpServers"]["smooth"]["command"], "th");
    }
}
