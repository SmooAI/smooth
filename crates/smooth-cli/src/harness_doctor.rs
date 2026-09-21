//! `th harness doctor [name]` (pearl th-3cabf6) — does each harness actually
//! work on THIS machine?
//!
//! `th harness list` says whether a binary resolves. Doctor says whether a
//! SmoothFlow session on it will behave: the binary that really runs (and
//! whether `which` hands you a cmux shim instead), its version, whether it
//! resolves under the SmoothFlow app's launchd environment and not just your
//! shell's, whether its state signal is wired AND live (Codex runs zero hooks
//! until its "Hooks need review" dialog is trusted), and whether it is signed
//! in — as far as that can be read without a prompt.
//!
//! **Read-only.** Doctor never installs, never trusts a hook dialog, never
//! logs in, never writes a file. It runs `<binary> --version` (and, on macOS,
//! a `security find-generic-password` attribute lookup that reveals no
//! secret), reads config files, and opens a TCP connection to the daemon's
//! advertised address. Every degraded row carries the ONE command that fixes
//! it; running that is the user's call.
//!
//! Per-harness knowledge (hook wiring, auth files, install commands) lives in
//! [`known`]; a harness without an entry still gets the generic checks.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anstream::println;
use anyhow::{bail, Result};
use owo_colors::OwoColorize;
use serde::Serialize;
use serde_json::Value;
use smooth_flow::harness::{Manifest, Registry, StateSource};

/// The PATH the SmoothFlow app's daemon gets when launchd starts it (the
/// LaunchAgent's `EnvironmentVariables`; a Finder-launched app gets even less
/// and prepends only `~/.cargo/bin`). `binary.prefer_paths` still apply.
pub const APP_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The per-harness verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Works,
    Degraded,
    NotInstalled,
}

impl Verdict {
    const fn label(self) -> &'static str {
        match self {
            Self::Works => "works",
            Self::Degraded => "degraded",
            Self::NotInstalled => "not installed",
        }
    }

    const fn glyph(self) -> &'static str {
        match self {
            Self::Works => "●",
            Self::Degraded => "◐",
            Self::NotInstalled => "○",
        }
    }
}

/// How one check came out. `Fail` degrades the harness; `Warn` is worth
/// knowing but a session still works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// `binary` | `cmux_shim` | `app_env` | `version` | `hooks` | `signal` | `auth`.
    pub id: &'static str,
    pub level: Level,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

fn check(id: &'static str, level: Level, detail: impl Into<String>, fix: Option<String>) -> Check {
    Check {
        id,
        level,
        detail: detail.into(),
        fix,
    }
}

/// One harness, diagnosed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnosis {
    pub name: String,
    pub display_name: String,
    /// `builtin` | `user` | `project` | `package`.
    pub origin: String,
    /// `hooks` | `scrape` | `native`.
    pub state_source: String,
    pub verdict: Verdict,
    /// Why it is not `works` — the first failing check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The one command that fixes `reason`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// What SmoothFlow launches from your shell's environment.
    pub binary: Option<String>,
    /// What it launches from the app's environment (differs only when PATH does).
    pub app_binary: Option<String>,
    /// The cmux CLI shim `which <name>` returns instead, if any.
    pub cmux_shim: Option<String>,
    pub version: Option<String>,
    pub checks: Vec<Check>,
}

/// What doctor reads about the machine — injected so tests run on a scratch HOME.
#[derive(Debug, Clone)]
pub struct Machine {
    pub home: PathBuf,
    pub shell_path: OsString,
    /// `None` skips the app-environment check (no SmoothFlow app off macOS).
    pub app_path: Option<OsString>,
    /// Env vars the auth checks consult (API keys, `CODEX_HOME`, `XDG_DATA_HOME`).
    pub env: BTreeMap<String, String>,
    /// Consult the macOS keychain (attributes only) for Claude Code's login.
    pub keychain: bool,
}

const ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_HOME",
    "XDG_DATA_HOME",
    "GEMINI_API_KEY",
    "OPENROUTER_API_KEY",
];

impl Machine {
    /// This machine, as the running `th` sees it.
    #[must_use]
    pub fn current(home: PathBuf) -> Self {
        let env = ENV_KEYS
            .iter()
            .filter_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).map(|v| ((*k).to_string(), v)))
            .collect();
        Self {
            home,
            shell_path: std::env::var_os("PATH").unwrap_or_default(),
            app_path: cfg!(target_os = "macos").then(|| OsString::from(APP_PATH)),
            env,
            keychain: cfg!(target_os = "macos"),
        }
    }

    fn has_env(&self, key: &str) -> bool {
        self.env.contains_key(key)
    }
}

// ── per-harness knowledge ────────────────────────────────────────────────────

/// What doctor knows about a harness beyond its manifest.
pub struct Known {
    /// The install command for a missing binary.
    pub install: &'static str,
    /// Hook wiring check (for `state.source = "hooks"`).
    pub hooks: Option<fn(&Machine) -> Check>,
    /// Non-interactive sign-in check.
    pub auth: Option<fn(&Machine) -> Check>,
}

/// The knowledge table, by manifest name. Add a row when a harness has a
/// hook-install / trust / auth state doctor can read without a prompt.
#[must_use]
pub fn known(name: &str) -> Option<Known> {
    Some(match name {
        "claude" => Known {
            install: "curl -fsSL https://claude.ai/install.sh | bash",
            hooks: Some(claude_hooks),
            auth: Some(claude_auth),
        },
        "codex" => Known {
            install: "npm i -g @openai/codex",
            hooks: Some(codex_hooks),
            auth: Some(codex_auth),
        },
        "opencode" => Known {
            install: "curl -fsSL https://opencode.ai/install | bash",
            hooks: Some(opencode_hooks),
            auth: Some(opencode_auth),
        },
        // th-5a2314: the scraped harnesses (th-e77603). No hooks to wire;
        // what stops a session is a missing provider, so that is the check.
        "aider" => Known {
            install: "uv tool install aider-chat",
            hooks: None,
            auth: Some(aider_auth),
        },
        "goose" => Known {
            install: "curl -fsSL https://github.com/block/goose/releases/download/stable/download_cli.sh | CONFIGURE=false bash",
            hooks: None,
            auth: Some(goose_auth),
        },
        "crush" => Known {
            install: "brew install charmbracelet/tap/crush",
            hooks: None,
            auth: Some(crush_auth),
        },
        "cline" => Known {
            install: "npm i -g cline",
            hooks: None,
            auth: Some(cline_auth),
        },
        "th-code" => Known {
            install: "brew install SmooAI/tools/th",
            hooks: None,
            auth: Some(th_code_auth),
        },
        _ => return None,
    })
}

fn newest_version_dir(dir: &Path) -> Option<(String, PathBuf)> {
    let mut versions: Vec<(Vec<u64>, String)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .map(|v| (v.split('.').filter_map(|p| p.parse::<u64>().ok()).collect(), v))
        .collect();
    versions.sort();
    versions.pop().map(|(_, v)| (v.clone(), dir.join(v)))
}

/// A plugin version as comparable numbers (`0.41.4` → `[0, 41, 4]`).
fn version_key(v: &str) -> Vec<u64> {
    v.split('.').filter_map(|p| p.parse().ok()).collect()
}

/// A project-scoped smooth-agent install older than the user-scoped one.
///
/// A Claude Code session started in that project loads the project's copy.
/// `claude plugin update` without `--scope project` never touches it, so
/// the user-scope update reports success while that checkout keeps a plugin
/// with no flow hook (or a stale one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalePin {
    pub project: PathBuf,
    pub version: String,
    pub user_version: String,
}

impl StalePin {
    /// The one command that fixes it.
    #[must_use]
    pub fn fix(&self) -> String {
        format!("cd '{}' && claude plugin update smooth-agent@smooth --scope project", self.project.display())
    }
}

/// Every project-scoped smooth-agent pin in `~/.claude/plugins/installed_plugins.json`
/// that is older than the user-scoped install. Pins for directories that no
/// longer exist are skipped: no session can start there, and pruning them is
/// the user's call.
#[must_use]
pub fn stale_project_pins(home: &Path) -> Vec<StalePin> {
    let Some(entries) = std::fs::read_to_string(home.join(".claude/plugins/installed_plugins.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.pointer("/plugins/smooth-agent@smooth").and_then(Value::as_array).cloned())
    else {
        return Vec::new();
    };
    let field = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
    let Some(user_version) = entries
        .iter()
        .find(|e| field(e, "scope").as_deref() == Some("user"))
        .and_then(|e| field(e, "version"))
    else {
        return Vec::new();
    };
    let mut stale: Vec<StalePin> = entries
        .iter()
        .filter(|e| field(e, "scope").as_deref() == Some("project"))
        .filter_map(|e| Some((PathBuf::from(field(e, "projectPath")?), field(e, "version")?)))
        .filter(|(project, version)| project.is_dir() && version_key(version) < version_key(&user_version))
        .map(|(project, version)| StalePin {
            project,
            version,
            user_version: user_version.clone(),
        })
        .collect();
    stale.sort_by(|a, b| a.project.cmp(&b.project));
    stale
}

fn mentions_flow_hook(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.contains("flow-hook.sh"))
}

/// Whether an installed `flow-hook.sh` presents the per-launch hook token
/// (th-91d032). One that predates it is refused by the engine for every
/// session SmoothFlow launched, so those sessions silently scrape.
fn flow_hook_is_authenticated(script: &Path) -> bool {
    std::fs::read_to_string(script).is_ok_and(|t| t.contains(smooth_flow::hook_auth::TOKEN_FILE_ENV))
}

/// The `flow-hook.sh` path a `hooks.json` command runs (`/p/flow-hook.sh Stop codex`).
fn flow_hook_script(command: &str) -> Option<PathBuf> {
    command.split_whitespace().find(|w| w.ends_with("flow-hook.sh")).map(PathBuf::from)
}

fn claude_hooks(m: &Machine) -> Check {
    // `claude plugin update` restarts nothing: a running session keeps the
    // hooks it started with.
    let fix = Some("th harness enable claude-code   # then restart Claude Code sessions".to_string());
    let enabled = std::fs::read_to_string(m.home.join(".claude/settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.pointer("/enabledPlugins/smooth-agent@smooth").and_then(Value::as_bool))
        .unwrap_or(false);
    let Some((version, dir)) = newest_version_dir(&m.home.join(".claude/plugins/cache/smooth/smooth-agent")) else {
        return check(
            "hooks",
            Level::Fail,
            "the smooth-agent plugin is not installed — no SmoothFlow hooks, state falls back to pane scraping",
            fix,
        );
    };
    if !enabled {
        return check(
            "hooks",
            Level::Fail,
            format!("smooth-agent {version} is installed but not enabled in ~/.claude/settings.json"),
            fix,
        );
    }
    if !mentions_flow_hook(&dir.join("hooks/hooks.json")) {
        return check(
            "hooks",
            Level::Fail,
            format!("smooth-agent {version} predates SmoothFlow's flow hook — sessions fall back to pane scraping"),
            fix,
        );
    }
    if !flow_hook_is_authenticated(&dir.join("hooks/flow-hook.sh")) {
        return check(
            "hooks",
            Level::Fail,
            format!(
                "smooth-agent {version}'s flow-hook.sh predates hook tokens (th-91d032) — SmoothFlow refuses its hooks, sessions fall back to pane scraping"
            ),
            fix,
        );
    }
    // The user-scope install is fine; a project-scoped pin can still shadow
    // it for every session started in that checkout.
    let stale = stale_project_pins(&m.home);
    if let Some(first) = stale.first() {
        let list = stale
            .iter()
            .map(|p| format!("{} ({})", p.project.display(), p.version))
            .collect::<Vec<_>>()
            .join(", ");
        return check(
            "hooks",
            Level::Fail,
            format!(
                "smooth-agent {} is installed for the user, but {} project-scoped pin{} older: {list}. Sessions started there load the old copy",
                first.user_version,
                stale.len(),
                if stale.len() == 1 { " is" } else { "s are" }
            ),
            Some(first.fix()),
        );
    }
    check("hooks", Level::Ok, format!("smooth-agent {version} posts every event to /api/flow/hooks"), None)
}

fn codex_home(m: &Machine) -> PathBuf {
    m.env.get("CODEX_HOME").map_or_else(|| m.home.join(".codex"), PathBuf::from)
}

/// `PreToolUse` → `pre_tool_use` (the event spelling in Codex's trust keys).
fn snake(event: &str) -> String {
    let mut out = String::new();
    for (i, c) in event.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Codex trust keys (`<hooks.json path>:<event>:<group>:<hook>`) of every
/// flow-hook command in `hooks.json`.
fn codex_flow_hook_keys(hooks_json: &Path) -> Vec<String> {
    let Some(v) = std::fs::read_to_string(hooks_json).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return Vec::new();
    };
    let table = v.get("hooks").unwrap_or(&v);
    let Some(events) = table.as_object() else { return Vec::new() };
    let mut keys = Vec::new();
    for (event, groups) in events {
        for (gi, group) in groups.as_array().into_iter().flatten().enumerate() {
            for (hi, hook) in group.get("hooks").and_then(Value::as_array).into_iter().flatten().enumerate() {
                if hook.get("command").and_then(Value::as_str).is_some_and(|c| c.contains("flow-hook.sh")) {
                    keys.push(format!("{}:{}:{gi}:{hi}", hooks_json.display(), snake(event)));
                }
            }
        }
    }
    keys
}

/// Every distinct `flow-hook.sh` path `hooks.json` runs.
fn codex_flow_hook_scripts(hooks_json: &Path) -> Vec<PathBuf> {
    let Some(v) = std::fs::read_to_string(hooks_json).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return Vec::new();
    };
    let table = v.get("hooks").unwrap_or(&v);
    let mut out: Vec<PathBuf> = table
        .as_object()
        .into_iter()
        .flat_map(|events| events.values())
        .flat_map(|groups| groups.as_array().into_iter().flatten())
        .flat_map(|group| group.get("hooks").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|hook| hook.get("command").and_then(Value::as_str).and_then(flow_hook_script))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn codex_hooks(m: &Machine) -> Check {
    let dir = codex_home(m);
    let hooks_json = dir.join("hooks.json");
    let keys = codex_flow_hook_keys(&hooks_json);
    if keys.is_empty() {
        return check(
            "hooks",
            Level::Fail,
            format!("no SmoothFlow flow hook in {} — state falls back to pane scraping", hooks_json.display()),
            Some("th harness enable codex".into()),
        );
    }
    let stale: Vec<PathBuf> = codex_flow_hook_scripts(&hooks_json)
        .into_iter()
        .filter(|p| p.is_file() && !flow_hook_is_authenticated(p))
        .collect();
    if let Some(script) = stale.first() {
        return check(
            "hooks",
            Level::Fail,
            format!(
                "{} predates hook tokens (th-91d032) — SmoothFlow refuses its hooks, sessions fall back to pane scraping",
                script.display()
            ),
            Some("th harness enable codex   # then accept Codex's \"Hooks need review\" dialog again".into()),
        );
    }
    let trusted: Vec<String> = std::fs::read_to_string(dir.join("config.toml"))
        .ok()
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("hooks")
                .and_then(|h| h.get("state"))
                .and_then(toml_edit::Item::as_table_like)
                .map(|t| t.iter().filter(|(_, v)| v.get("trusted_hash").is_some()).map(|(k, _)| k.to_string()).collect())
        })
        .unwrap_or_default();
    let untrusted = keys.iter().filter(|k| !trusted.contains(k)).count();
    if untrusted > 0 {
        return check(
            "hooks",
            Level::Fail,
            format!(
                "{untrusted} of {} SmoothFlow hooks are not trusted — Codex runs zero hooks until its \"Hooks need review\" dialog is accepted",
                keys.len()
            ),
            Some("codex   # in a trusted project: choose \"Trust all\" on the Hooks need review dialog".into()),
        );
    }
    check(
        "hooks",
        Level::Ok,
        format!(
            "{} flow hooks in {}, all trusted (a later edit needs re-review)",
            keys.len(),
            hooks_json.display()
        ),
        None,
    )
}

fn opencode_hooks(m: &Machine) -> Check {
    let link = m.home.join(".config/opencode/plugins/smooth-agent.js");
    let fix = Some("th harness enable opencode".to_string());
    let Ok(target) = std::fs::canonicalize(&link) else {
        return check(
            "hooks",
            Level::Fail,
            format!("the smooth-agent lifecycle plugin is not linked at {}", link.display()),
            fix,
        );
    };
    let text = std::fs::read_to_string(&target).unwrap_or_default();
    if !text.contains("/api/flow/hooks") {
        return check(
            "hooks",
            Level::Fail,
            format!("{} predates SmoothFlow — it posts no flow events", target.display()),
            fix,
        );
    }
    if !text.contains(smooth_flow::hook_auth::TOKEN_FILE_ENV) {
        return check(
            "hooks",
            Level::Fail,
            format!(
                "{} predates hook tokens (th-91d032) — SmoothFlow refuses its hooks, sessions fall back to pane scraping",
                target.display()
            ),
            fix,
        );
    }
    // OpenCode ≥1.18 fires only the generic `event` plugin hook.
    if !text.contains("event") {
        return check(
            "hooks",
            Level::Fail,
            "the lifecycle plugin only uses named session.* hooks, which OpenCode ≥1.18 never fires",
            fix,
        );
    }
    check("hooks", Level::Ok, format!("lifecycle plugin linked ({})", target.display()), None)
}

fn claude_auth(m: &Machine) -> Check {
    if m.has_env("ANTHROPIC_API_KEY") || m.has_env("CLAUDE_CODE_OAUTH_TOKEN") {
        return check("auth", Level::Ok, "an Anthropic key / OAuth token is set in the environment", None);
    }
    if m.home.join(".claude/.credentials.json").is_file() {
        return check("auth", Level::Ok, "signed in (~/.claude/.credentials.json)", None);
    }
    if m.keychain {
        // Attributes only: no -w/-g, so nothing secret is read and no prompt shows.
        let found = Command::new("security")
            .args(["find-generic-password", "-s", "Claude Code-credentials"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if found {
            return check("auth", Level::Ok, "signed in (login keychain)", None);
        }
    }
    check(
        "auth",
        Level::Fail,
        "not signed in — a session would stop at Claude Code's login screen",
        Some("claude   # then /login".into()),
    )
}

fn codex_auth(m: &Machine) -> Check {
    if m.has_env("OPENAI_API_KEY") {
        return check("auth", Level::Ok, "OPENAI_API_KEY is set", None);
    }
    let file = codex_home(m).join("auth.json");
    if file.is_file() {
        return check("auth", Level::Ok, format!("signed in ({})", file.display()), None);
    }
    check(
        "auth",
        Level::Fail,
        "not signed in — a session would stop at Codex's sign-in",
        Some("codex login".into()),
    )
}

fn opencode_auth(m: &Machine) -> Check {
    let data = m.env.get("XDG_DATA_HOME").map_or_else(|| m.home.join(".local/share"), PathBuf::from);
    let file = data.join("opencode/auth.json");
    let has_provider = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v.as_object().is_some_and(|o| !o.is_empty()));
    if has_provider {
        return check("auth", Level::Ok, format!("provider credentials in {}", file.display()), None);
    }
    if ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY", "OPENROUTER_API_KEY"]
        .iter()
        .any(|k| m.has_env(k))
    {
        return check("auth", Level::Ok, "a provider API key is set in the environment", None);
    }
    check(
        "auth",
        Level::Warn,
        "no provider credentials — only OpenCode's free models will answer",
        Some("opencode auth login".into()),
    )
}

/// A provider API key doctor can see in the environment, if any.
fn provider_env_key(m: &Machine) -> Option<&'static str> {
    ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY", "OPENROUTER_API_KEY"]
        .into_iter()
        .find(|k| m.has_env(k))
}

fn aider_auth(m: &Machine) -> Check {
    if let Some(k) = provider_env_key(m) {
        return check("auth", Level::Ok, format!("{k} is set"), None);
    }
    let conf = m.home.join(".aider.conf.yml");
    let has_key = std::fs::read_to_string(&conf).is_ok_and(|t| {
        t.lines()
            .filter_map(|l| l.split_once(':'))
            .any(|(k, v)| !k.trim_start().starts_with('#') && k.trim().ends_with("api-key") && !v.trim().is_empty())
    });
    if has_key {
        return check("auth", Level::Ok, format!("an API key in {}", conf.display()), None);
    }
    // Warn, not fail: the app's daemon may carry a key this shell does not.
    check(
        "auth",
        Level::Warn,
        "no provider API key in this environment or ~/.aider.conf.yml — aider would stop to ask for one",
        Some("echo 'anthropic-api-key: <key>' >> ~/.aider.conf.yml".into()),
    )
}

fn goose_auth(m: &Machine) -> Check {
    let conf = m.home.join(".config/goose/config.yaml");
    let provider = std::fs::read_to_string(&conf)
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.trim().strip_prefix("GOOSE_PROVIDER:").map(|v| v.trim().trim_matches(['"', '\'']).to_string()))
        })
        .filter(|v| !v.is_empty());
    provider.map_or_else(
        || {
            check(
                "auth",
                Level::Fail,
                format!("no GOOSE_PROVIDER in {} — a session would stop at goose's provider setup", conf.display()),
                Some("goose configure".into()),
            )
        },
        |p| check("auth", Level::Ok, format!("GOOSE_PROVIDER = {p} ({})", conf.display()), None),
    )
}

/// A JSON file whose `key` is a non-empty object.
fn json_object_nonempty(path: &Path, key: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get(key).and_then(Value::as_object).map(|o| !o.is_empty()))
        .unwrap_or(false)
}

fn crush_auth(m: &Machine) -> Check {
    let data = m.env.get("XDG_DATA_HOME").map_or_else(|| m.home.join(".local/share"), PathBuf::from);
    for conf in [m.home.join(".config/crush/crush.json"), data.join("crush/crush.json")] {
        if json_object_nonempty(&conf, "providers") {
            return check("auth", Level::Ok, format!("providers configured in {}", conf.display()), None);
        }
    }
    if let Some(k) = provider_env_key(m) {
        return check("auth", Level::Ok, format!("{k} is set"), None);
    }
    check(
        "auth",
        Level::Fail,
        "no provider in ~/.config/crush/crush.json or ~/.local/share/crush/crush.json — a session would open on crush's provider picker",
        Some("crush   # pick a provider once; SmoothFlow sessions reuse it".into()),
    )
}

fn cline_auth(m: &Machine) -> Check {
    let file = m.home.join(".cline/data/settings/providers.json");
    let configured = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| match &v {
            Value::Object(o) => !o.is_empty(),
            Value::Array(a) => !a.is_empty(),
            _ => false,
        });
    if configured {
        return check("auth", Level::Ok, format!("providers configured in {}", file.display()), None);
    }
    check(
        "auth",
        Level::Fail,
        format!("no providers in {} — a session would open on cline's sign-in / provider screen", file.display()),
        Some("cline   # sign in or choose a provider once; SmoothFlow sessions reuse it".into()),
    )
}

fn th_code_auth(m: &Machine) -> Check {
    let file = m.home.join(".smooth/providers.json");
    if file.is_file() {
        return check("auth", Level::Ok, "an LLM provider is configured (~/.smooth/providers.json)", None);
    }
    check(
        "auth",
        Level::Fail,
        "no LLM provider configured — th code has no model to talk to",
        Some("th model login".into()),
    )
}

// ── generic checks ───────────────────────────────────────────────────────────

/// The first `names` hit on `path` with NO directory skipped — what `which` says.
fn which_unfiltered(manifest: &Manifest, path: &std::ffi::OsStr) -> Option<PathBuf> {
    manifest
        .binary
        .names
        .iter()
        .find_map(|n| std::env::split_paths(path).map(|d| d.join(n)).find(|p| is_executable(p)))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.is_file() && std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

fn is_shim(manifest: &Manifest, p: &Path) -> bool {
    p.parent().is_some_and(|d| {
        d.components()
            .any(|c| manifest.binary.skip_path_patterns.iter().any(|s| c.as_os_str() == s.as_str()))
    })
}

fn install_fix(manifest: &Manifest) -> String {
    let name = &manifest.binary.names[0];
    known(&manifest.name).map_or_else(
        || {
            manifest
                .binary
                .prefer_paths
                .first()
                .map_or_else(|| format!("install `{name}` on PATH"), |p| format!("install `{name}` on PATH or at ~/{p}"))
        },
        |k| k.install.to_string(),
    )
}

/// A `#!` script's interpreter, when it is found by PATH lookup (`#!/usr/bin/env node`)
/// or given absolutely (`#!/opt/x/bin/python`).
fn interpreter(p: &Path) -> Option<(String, bool)> {
    let mut head = [0u8; 256];
    let n = std::fs::File::open(p).and_then(|mut f| f.read(&mut head)).ok()?;
    let text = String::from_utf8_lossy(&head[..n]);
    let line = text.lines().next()?.strip_prefix("#!")?.trim();
    let mut parts = line.split_whitespace();
    let prog = parts.next()?;
    if prog.ends_with("/env") {
        let next = parts.find(|a| !a.starts_with('-'))?;
        Some((next.to_string(), true))
    } else {
        Some((prog.to_string(), false))
    }
}

fn app_env_check(manifest: &Manifest, m: &Machine, shell_bin: &Path, app_path: &std::ffi::OsStr) -> (Check, Option<PathBuf>) {
    let Some(app_bin) = manifest.resolve_binary_in(&m.home, app_path) else {
        let fix = manifest.binary.prefer_paths.first().map_or_else(
            || format!("ln -s '{}' /usr/local/bin/{}", shell_bin.display(), manifest.binary.names[0]),
            |rel| {
                let target = m.home.join(rel);
                let dir = target.parent().map_or_else(String::new, |d| format!("mkdir -p '{}' && ", d.display()));
                format!("{dir}ln -s '{}' '{}'", shell_bin.display(), target.display())
            },
        );
        return (
            check(
                "app_env",
                Level::Fail,
                format!(
                    "found from your shell ({}) but not from the SmoothFlow app's environment (launchd PATH {})",
                    shell_bin.display(),
                    app_path.to_string_lossy()
                ),
                Some(fix),
            ),
            None,
        );
    };
    if let Some((interp, via_path)) = interpreter(&app_bin) {
        let found = if via_path {
            std::env::split_paths(app_path).map(|d| d.join(&interp)).any(|p| is_executable(&p))
        } else {
            is_executable(Path::new(&interp))
        };
        if !found {
            let fix = if via_path {
                format!("ln -s \"$(command -v {interp})\" /usr/local/bin/{interp}")
            } else {
                format!("reinstall {} (its interpreter {interp} is gone)", manifest.binary.names[0])
            };
            return (
                check(
                    "app_env",
                    Level::Fail,
                    format!(
                        "{} is a `{interp}` script and `{interp}` is not on the app's PATH — it would exit 127 in a SmoothFlow pane",
                        app_bin.display()
                    ),
                    Some(fix),
                ),
                Some(app_bin),
            );
        }
    }
    let detail = if app_bin == shell_bin {
        "resolves the same under the app's launchd environment".to_string()
    } else {
        format!("the app runs {} (your shell: {})", app_bin.display(), shell_bin.display())
    };
    (check("app_env", Level::Ok, detail, None), Some(app_bin))
}

/// `<bin> --version`, first non-empty line; `Err` when it cannot run at all.
fn version_of(bin: &Path) -> Result<std::result::Result<String, String>> {
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut out = String::new();
            if let Some(mut s) = child.stdout.take() {
                let _ = s.read_to_string(&mut out);
            }
            if let Some(mut s) = child.stderr.take() {
                let _ = s.read_to_string(&mut out);
            }
            let first = out
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .chars()
                .take(80)
                .collect::<String>();
            return Ok(if status.success() {
                Ok(first)
            } else {
                Err(format!("`--version` exited {}: {first}", status.code().unwrap_or(-1)))
            });
        }
        if start.elapsed() > VERSION_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Err(format!("`--version` did not answer within {}s", VERSION_TIMEOUT.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Is the daemon hooks post to (`~/.smooth/daemon.addr`) accepting connections?
fn daemon_check(m: &Machine) -> Check {
    let addr_file = m.home.join(".smooth/daemon.addr");
    let fix = Some("th up".to_string());
    let Some(addr) = std::fs::read_to_string(&addr_file)
        .ok()
        .map(|s| s.trim().trim_start_matches("http://").trim_end_matches('/').to_string())
    else {
        return check(
            "signal",
            Level::Warn,
            "no daemon has advertised an address — reports are dropped until SmoothFlow or `th up` runs one",
            fix,
        );
    };
    let reachable = addr
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|sa| TcpStream::connect_timeout(&sa, Duration::from_millis(400)).is_ok());
    if reachable {
        check("signal", Level::Ok, format!("daemon listening at {addr}"), None)
    } else {
        check(
            "signal",
            Level::Warn,
            format!("daemon.addr says {addr} but nothing answers there — reports are dropped until it runs"),
            fix,
        )
    }
}

/// The manifest's `state.source` spelling (`StateSource::as_str` is the
/// session's, where scrape reads `inferred`).
const fn manifest_source(s: StateSource) -> &'static str {
    match s {
        StateSource::Hooks => "hooks",
        StateSource::Scrape => "scrape",
        StateSource::Native => "native",
    }
}

/// Why nothing resolved: an Info when the CLI is simply absent, a Fail when
/// only a cmux shim is there.
fn missing_binary_check(manifest: &Manifest, shim: Option<&PathBuf>) -> Check {
    let (level, detail) = shim.map_or_else(
        || {
            let prefer = if manifest.binary.prefer_paths.is_empty() {
                String::new()
            } else {
                format!(" at ~/{}", manifest.binary.prefer_paths.join(", ~/"))
            };
            (Level::Info, format!("`{}` not found{prefer} or on PATH", manifest.binary.names.join("` / `")))
        },
        |s| {
            (
                Level::Fail,
                format!(
                    "only cmux's CLI shim is installed ({}) — it injects cmux's own session id and hooks, so SmoothFlow will not run it",
                    s.display()
                ),
            )
        },
    );
    check("binary", level, detail, Some(install_fix(manifest)))
}

/// The checks for an installed binary; returns `(app_binary, version)`.
fn installed_checks(manifest: &Manifest, m: &Machine, bin: &Path, checks: &mut Vec<Check>) -> (Option<PathBuf>, Option<String>) {
    let app_binary = m.app_path.as_ref().and_then(|app_path| {
        let (c, app) = app_env_check(manifest, m, bin, app_path);
        checks.push(c);
        app
    });
    let run = app_binary.clone().unwrap_or_else(|| bin.to_path_buf());
    let version = match version_of(&run) {
        Ok(Ok(v)) => {
            checks.push(check("version", Level::Ok, v.clone(), None));
            Some(v)
        }
        Ok(Err(why)) => {
            checks.push(check("version", Level::Warn, why, None));
            None
        }
        Err(e) => {
            checks.push(check(
                "version",
                Level::Fail,
                format!("{} cannot be executed: {e}", run.display()),
                Some(install_fix(manifest)),
            ));
            None
        }
    };
    let knowledge = known(&manifest.name);
    match manifest.state.source {
        StateSource::Hooks => {
            checks.push(knowledge.as_ref().and_then(|k| k.hooks).map_or_else(
                || {
                    check(
                        "hooks",
                        Level::Info,
                        format!("wired by: {} — doctor has no check for this harness", manifest.state.hooks.install.trim()),
                        None,
                    )
                },
                |f| f(m),
            ));
            checks.push(daemon_check(m));
        }
        StateSource::Native => checks.push(daemon_check(m)),
        StateSource::Scrape => checks.push(check("signal", Level::Ok, "scraped from the pane — nothing to install", None)),
    }
    checks.push(knowledge.as_ref().and_then(|k| k.auth).map_or_else(
        || check("auth", Level::Info, "no non-interactive sign-in check for this harness", None),
        |f| f(m),
    ));
    (app_binary, version)
}

/// Diagnose one manifest.
#[must_use]
pub fn diagnose(manifest: &Manifest, m: &Machine) -> Diagnosis {
    let mut checks = Vec::new();
    let shim = which_unfiltered(manifest, &m.shell_path).filter(|p| is_shim(manifest, p));
    let binary = manifest.resolve_binary_in(&m.home, &m.shell_path);
    let (app_binary, version) = match &binary {
        None => {
            checks.push(missing_binary_check(manifest, shim.as_ref()));
            (None, None)
        }
        Some(bin) => {
            checks.push(check("binary", Level::Ok, bin.display().to_string(), None));
            if let Some(s) = &shim {
                checks.push(check(
                    "cmux_shim",
                    Level::Info,
                    format!(
                        "`which {}` is cmux's shim ({}); SmoothFlow skips it and runs {}",
                        manifest.binary.names[0],
                        s.display(),
                        bin.display()
                    ),
                    None,
                ));
            }
            installed_checks(manifest, m, bin, &mut checks)
        }
    };
    let verdict = if binary.is_none() && shim.is_none() {
        Verdict::NotInstalled
    } else if checks.iter().any(|c| c.level == Level::Fail) {
        Verdict::Degraded
    } else {
        Verdict::Works
    };
    let (reason, fix) = if verdict == Verdict::Works {
        (None, None)
    } else {
        checks
            .iter()
            .find(|c| c.level == Level::Fail || verdict == Verdict::NotInstalled)
            .map_or((None, None), |c| (Some(c.detail.clone()), c.fix.clone()))
    };
    Diagnosis {
        name: manifest.name.clone(),
        display_name: manifest.display_name.clone(),
        origin: manifest.origin.label().to_string(),
        state_source: manifest_source(manifest.state.source).to_string(),
        verdict,
        reason,
        fix,
        binary: binary.map(|p| p.display().to_string()),
        app_binary: app_binary.map(|p| p.display().to_string()),
        cmux_shim: shim.map(|p| p.display().to_string()),
        version,
        checks,
    }
}

/// Diagnose every manifest in `registry` (or just `only`).
///
/// # Errors
/// When `only` names no manifest.
pub fn diagnose_all(registry: &Registry, m: &Machine, only: Option<&str>) -> Result<Vec<Diagnosis>> {
    if let Some(name) = only {
        let Some(manifest) = registry.get(name) else {
            bail!("no harness named `{name}`\n  → th harness list");
        };
        return Ok(vec![diagnose(manifest, m)]);
    }
    Ok(registry.all().iter().map(|h| diagnose(h, m)).collect())
}

fn print_human(rows: &[Diagnosis], verbose: bool) {
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(8).max(8);
    for r in rows {
        let head = format!("{} {:<width$}  {:<13}", r.verdict.glyph(), r.name, r.verdict.label());
        let tail = [r.version.clone(), r.binary.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        if r.verdict == Verdict::NotInstalled {
            println!("{}", head.dimmed());
        } else {
            println!("{}  {}", head.bold(), tail.dimmed());
        }
        for c in &r.checks {
            let show = verbose || matches!(c.level, Level::Fail | Level::Warn) || c.id == "cmux_shim";
            if !show {
                continue;
            }
            let glyph = match c.level {
                Level::Ok => "●",
                Level::Info => "·",
                Level::Warn => "◐",
                Level::Fail => "○",
            };
            println!("    {glyph} {}: {}", c.id, c.detail);
            if let Some(fix) = &c.fix {
                if c.level != Level::Ok {
                    println!("      fix: {}", fix.cyan());
                }
            }
        }
    }
    let count = |v: Verdict| rows.iter().filter(|r| r.verdict == v).count();
    println!(
        "\n{} works · {} degraded · {} not installed  {}",
        count(Verdict::Works),
        count(Verdict::Degraded),
        count(Verdict::NotInstalled),
        "(read-only: doctor changed nothing)".dimmed()
    );
}

/// `th harness doctor [name] [--json] [--verbose]`.
///
/// # Errors
/// When `only` names no manifest.
pub fn run(home: &Path, only: Option<&str>, json: bool, verbose: bool) -> Result<()> {
    let project = std::env::current_dir().ok().map(|d| smooth_flow::engine::project_root(&d));
    let registry = Registry::load(home, project.as_deref());
    let machine = Machine::current(home.to_path_buf());
    let rows = diagnose_all(&registry, &machine, only)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "harnesses": rows, "app_path": machine.app_path.map(|p| p.to_string_lossy().into_owned()) }))?
        );
    } else {
        print_human(&rows, verbose);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    fn script(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn machine(root: &Path, shell_path: &[&Path], app_path: Option<&[&Path]>) -> Machine {
        Machine {
            home: root.join("home"),
            shell_path: std::env::join_paths(shell_path).unwrap(),
            app_path: app_path.map(|p| std::env::join_paths(p).unwrap()),
            env: BTreeMap::new(),
            keychain: false,
        }
    }

    fn manifest(name: &str) -> Manifest {
        Registry::builtin().get(name).cloned().unwrap()
    }

    #[cfg(unix)]
    fn by_id<'a>(d: &'a Diagnosis, id: &str) -> &'a Check {
        d.checks.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no {id} check in {d:#?}"))
    }

    #[test]
    fn missing_binary_is_not_installed_with_the_install_command() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        let d = diagnose(&manifest("claude"), &machine(tmp.path(), &[&empty], None));
        assert_eq!(d.verdict, Verdict::NotInstalled);
        assert!(d.reason.as_deref().unwrap().contains("not found at ~/.claude/local/claude"), "{d:#?}");
        assert_eq!(d.fix.as_deref(), Some("curl -fsSL https://claude.ai/install.sh | bash"));
        let custom = Manifest::parse("name = \"mytool\"\n[binary]\nnames = [\"mytool\"]\n[launch]\nargv = [\"{prompt}\"]\nprompt_as = \"argv\"\n").unwrap();
        assert_eq!(
            diagnose(&custom, &machine(tmp.path(), &[&empty], None)).fix.as_deref(),
            Some("install `mytool` on PATH")
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_a_cmux_shim_is_degraded_and_a_shim_in_front_of_a_real_install_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let shims = tmp.path().join("T/cmux-cli-shims/abc");
        script(&shims.join("codex"), "#!/bin/sh\necho shim\n");
        let d = diagnose(&manifest("codex"), &machine(tmp.path(), &[&shims], None));
        assert_eq!(d.verdict, Verdict::Degraded);
        assert!(d.reason.as_deref().unwrap().contains("only cmux's CLI shim"), "{d:#?}");
        assert!(d.cmux_shim.as_deref().unwrap().ends_with("cmux-cli-shims/abc/codex"));

        script(&tmp.path().join("home/.local/bin/codex"), "#!/bin/sh\necho 'codex-cli 0.153.0'\n");
        let d = diagnose(&manifest("codex"), &machine(tmp.path(), &[&shims], None));
        assert!(d.binary.as_deref().unwrap().ends_with("home/.local/bin/codex"));
        assert_eq!(d.version.as_deref(), Some("codex-cli 0.153.0"));
        assert!(by_id(&d, "cmux_shim").detail.contains("SmoothFlow skips it"), "{d:#?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_only_the_shell_can_find_is_degraded_with_a_prefer_path_symlink_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let mise = tmp.path().join("mise/installs/node/24/bin");
        script(&mise.join("claude"), "#!/bin/sh\necho '2.1.0 (Claude Code)'\n");
        let app = tmp.path().join("app-bin");
        std::fs::create_dir_all(&app).unwrap();
        let m = machine(tmp.path(), &[&mise], Some(&[&app]));
        let d = diagnose(&manifest("claude"), &m);
        assert_eq!(d.verdict, Verdict::Degraded, "{d:#?}");
        let c = by_id(&d, "app_env");
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("not from the SmoothFlow app's environment"), "{c:?}");
        let fix = c.fix.as_deref().unwrap();
        assert!(fix.contains("ln -s") && fix.ends_with("home/.claude/local/claude'"), "{fix}");
        assert_eq!(d.fix.as_deref(), Some(fix), "the row's fix is the first failing check's");
    }

    #[cfg(unix)]
    #[test]
    fn an_env_script_whose_interpreter_the_app_cannot_find_is_degraded() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("home/.local/bin/codex");
        script(&bin, "#!/usr/bin/env definitely-not-an-interpreter-xyz\n");
        let app = tmp.path().join("app-bin");
        std::fs::create_dir_all(&app).unwrap();
        let d = diagnose(&manifest("codex"), &machine(tmp.path(), &[], Some(&[&app])));
        let c = by_id(&d, "app_env");
        assert_eq!(c.level, Level::Fail, "{d:#?}");
        assert!(c.detail.contains("`definitely-not-an-interpreter-xyz` script"), "{c:?}");
        assert_eq!(interpreter(&bin), Some(("definitely-not-an-interpreter-xyz".into(), true)));
        script(&bin, "#!/bin/sh -e\necho v\n");
        assert_eq!(interpreter(&bin), Some(("/bin/sh".into(), false)));
    }

    #[cfg(unix)]
    #[test]
    fn codex_hooks_must_be_wired_and_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let m = machine(tmp.path(), &[], None);
        let codex = m.home.join(".codex");
        assert!(codex_hooks(&m).detail.contains("no SmoothFlow flow hook"));
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(
            codex.join("hooks.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"th prime"},{"type":"command","command":"/x/flow-hook.sh SessionStart codex"}]}],"Stop":[{"hooks":[{"type":"command","command":"/x/flow-hook.sh Stop codex"}]}]}}"#,
        )
        .unwrap();
        let hooks = codex.join("hooks.json").display().to_string();
        assert_eq!(
            codex_flow_hook_keys(&codex.join("hooks.json")),
            vec![format!("{hooks}:session_start:0:1"), format!("{hooks}:stop:0:0")]
        );
        std::fs::write(
            codex.join("config.toml"),
            format!("[hooks.state.\"{hooks}:session_start:0:1\"]\ntrusted_hash = \"abc\"\n"),
        )
        .unwrap();
        let c = codex_hooks(&m);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.starts_with("1 of 2 SmoothFlow hooks are not trusted"), "{c:?}");
        assert!(c.fix.as_deref().unwrap().contains("Trust all"));
        std::fs::write(
            codex.join("config.toml"),
            format!("[hooks.state.\"{hooks}:session_start:0:1\"]\ntrusted_hash = \"abc\"\n[hooks.state.\"{hooks}:stop:0:0\"]\ntrusted_hash = \"def\"\n"),
        )
        .unwrap();
        assert_eq!(codex_hooks(&m).level, Level::Ok, "a script that isn't there is not judged");
        assert_eq!(snake("UserPromptSubmit"), "user_prompt_submit");
        assert_eq!(codex_flow_hook_scripts(&codex.join("hooks.json")), vec![PathBuf::from("/x/flow-hook.sh")]);
        assert_eq!(flow_hook_script("th prime"), None);

        // th-91d032: a rendered flow-hook.sh that sends no token is refused.
        let script = tmp.path().join("pkg/flow-hook.sh");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "curl …/api/flow/hooks").unwrap();
        let wired = std::fs::read_to_string(codex.join("hooks.json"))
            .unwrap()
            .replace("/x/flow-hook.sh", &script.display().to_string());
        std::fs::write(codex.join("hooks.json"), wired).unwrap();
        let c = codex_hooks(&m);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("predates hook tokens"), "{c:?}");
        assert!(c.fix.as_deref().unwrap().starts_with("th harness enable codex"));
        std::fs::write(&script, "cat \"$SMOOTH_FLOW_HOOK_TOKEN_FILE\"").unwrap();
        assert!(!codex_hooks(&m).detail.contains("predates"));
    }

    /// A project-scoped pin older than the user install shadows it for every
    /// session started in that project, so doctor must not report Claude
    /// healthy. Pins for deleted directories are not ours to judge.
    #[test]
    fn stale_project_scoped_pins_degrade_claude_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let m = machine(tmp.path(), &[], None);
        let cache = m.home.join(".claude/plugins/cache/smooth/smooth-agent/0.51.1/hooks");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("hooks.json"), r#"{"x":"flow-hook.sh"}"#).unwrap();
        std::fs::write(cache.join("flow-hook.sh"), "cat \"$SMOOTH_FLOW_HOOK_TOKEN_FILE\"").unwrap();
        std::fs::write(m.home.join(".claude/settings.json"), r#"{"enabledPlugins":{"smooth-agent@smooth":true}}"#).unwrap();
        assert!(stale_project_pins(&m.home).is_empty(), "no installed_plugins.json ⇒ nothing to judge");
        assert_eq!(claude_hooks(&m).level, Level::Ok);

        let main = tmp.path().join("dev/smooth");
        let other = tmp.path().join("dev/smooai");
        let current = tmp.path().join("dev/current");
        for d in [&main, &other, &current] {
            std::fs::create_dir_all(d).unwrap();
        }
        let gone = tmp.path().join("dev/smooth-th-deleted");
        let pins = json!({"version": 2, "plugins": {"smooth-agent@smooth": [
            {"scope": "user", "projectPath": null, "version": "0.51.1"},
            {"scope": "project", "projectPath": main, "version": "0.31.1"},
            {"scope": "project", "projectPath": other, "version": "0.41.4"},
            {"scope": "project", "projectPath": current, "version": "0.51.1"},
            {"scope": "project", "projectPath": gone, "version": "0.9.0"},
        ]}});
        std::fs::write(m.home.join(".claude/plugins/installed_plugins.json"), pins.to_string()).unwrap();
        let stale = stale_project_pins(&m.home);
        assert_eq!(
            stale.iter().map(|p| (p.project.clone(), p.version.as_str())).collect::<Vec<_>>(),
            vec![(other.clone(), "0.41.4"), (main, "0.31.1")],
            "older, existing pins only; the current one and the deleted worktree are skipped"
        );
        let c = claude_hooks(&m);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("2 project-scoped pins are older"), "{c:?}");
        assert!(c.detail.contains("0.31.1") && c.detail.contains("0.41.4"));
        assert_eq!(
            c.fix.as_deref(),
            Some(format!("cd '{}' && claude plugin update smooth-agent@smooth --scope project", other.display()).as_str())
        );
        // Version order is numeric, not lexical: 0.9.0 < 0.51.1 < 0.100.0.
        assert!(version_key("0.9.0") < version_key("0.51.1") && version_key("0.51.1") < version_key("0.100.0"));
    }

    #[test]
    fn claude_hooks_need_an_enabled_plugin_that_ships_the_flow_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let m = machine(tmp.path(), &[], None);
        assert!(claude_hooks(&m).detail.contains("not installed"));
        let cache = m.home.join(".claude/plugins/cache/smooth/smooth-agent");
        std::fs::create_dir_all(cache.join("0.9.0/hooks")).unwrap();
        std::fs::create_dir_all(cache.join("0.41.4/hooks")).unwrap();
        std::fs::write(cache.join("0.41.4/hooks/hooks.json"), "{}").unwrap();
        std::fs::write(m.home.join(".claude/settings.json"), r#"{"enabledPlugins":{"smooth-agent@smooth":true}}"#).unwrap();
        let c = claude_hooks(&m);
        assert!(c.detail.contains("smooth-agent 0.41.4 predates"), "newest by version, not by name: {c:?}");
        std::fs::write(cache.join("0.41.4/hooks/hooks.json"), r#"{"x":"flow-hook.sh"}"#).unwrap();
        // th-91d032: a flow-hook.sh that sends no token is refused by the engine.
        std::fs::write(cache.join("0.41.4/hooks/flow-hook.sh"), "curl …/api/flow/hooks").unwrap();
        let c = claude_hooks(&m);
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("0.41.4's flow-hook.sh predates hook tokens"), "{c:?}");
        assert_eq!(c.fix.as_deref(), Some("th harness enable claude-code   # then restart Claude Code sessions"));
        std::fs::write(cache.join("0.41.4/hooks/flow-hook.sh"), "cat \"$SMOOTH_FLOW_HOOK_TOKEN_FILE\"").unwrap();
        assert_eq!(claude_hooks(&m).level, Level::Ok);
        std::fs::write(m.home.join(".claude/settings.json"), "{}").unwrap();
        assert!(claude_hooks(&m).detail.contains("not enabled"));
    }

    #[test]
    fn auth_checks_read_files_and_env_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = machine(tmp.path(), &[], None);
        assert_eq!(claude_auth(&m).level, Level::Fail);
        assert_eq!(codex_auth(&m).fix.as_deref(), Some("codex login"));
        assert_eq!(opencode_auth(&m).level, Level::Warn);
        assert_eq!(th_code_auth(&m).level, Level::Fail);
        m.env.insert("ANTHROPIC_API_KEY".into(), "k".into());
        assert_eq!(claude_auth(&m).level, Level::Ok);
        assert_eq!(opencode_auth(&m).level, Level::Ok);
        let codex_home = tmp.path().join("ch");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(codex_home.join("auth.json"), "{}").unwrap();
        m.env.insert("CODEX_HOME".into(), codex_home.display().to_string());
        assert_eq!(codex_auth(&m).level, Level::Ok, "CODEX_HOME is honored");
        std::fs::create_dir_all(m.home.join(".smooth")).unwrap();
        std::fs::write(m.home.join(".smooth/providers.json"), "{}").unwrap();
        assert_eq!(th_code_auth(&m).level, Level::Ok);
    }

    #[test]
    fn daemon_signal_is_a_warning_not_a_degradation() {
        let tmp = tempfile::tempdir().unwrap();
        let m = machine(tmp.path(), &[], None);
        let c = daemon_check(&m);
        assert_eq!((c.level, c.fix.as_deref()), (Level::Warn, Some("th up")));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::create_dir_all(m.home.join(".smooth")).unwrap();
        std::fs::write(m.home.join(".smooth/daemon.addr"), format!("{}\n", listener.local_addr().unwrap())).unwrap();
        assert_eq!(daemon_check(&m).level, Level::Ok);
        drop(listener);
        assert_eq!(daemon_check(&m).level, Level::Warn);
    }

    #[cfg(unix)]
    #[test]
    fn a_fully_set_up_scrape_harness_works_and_serializes_for_the_app() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        script(&bin.join("aider"), "#!/bin/sh\necho 'aider 0.86.2'\n");
        let man = Manifest::parse(
            // A scraped harness doctor has no knowledge row for (aider has one).
            "name = \"scrapy\"\n[binary]\nnames = [\"aider\"]\n[launch]\nargv = [\"{prompt}\"]\nprompt_as = \"argv\"\n[state]\nsource = \"scrape\"\n[state.scrape]\nidle = [\"> \"]\n",
        )
        .unwrap();
        let d = diagnose(&man, &machine(tmp.path(), &[&bin], Some(&[&bin])));
        assert_eq!(d.verdict, Verdict::Works, "{d:#?}");
        assert!(d.reason.is_none() && d.fix.is_none());
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["verdict"], "works");
        assert_eq!(v["state_source"], "scrape");
        assert_eq!(v["version"], "aider 0.86.2");
        assert!(v.get("fix").is_none(), "absent, not null: {v}");
        assert_eq!(by_id(&d, "auth").level, Level::Info);
    }

    #[cfg(unix)]
    #[test]
    fn a_version_flag_that_fails_warns_and_diagnose_all_filters_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        script(&tmp.path().join("home/.cargo/bin/th"), "#!/bin/sh\necho nope >&2\nexit 2\n");
        let m = machine(tmp.path(), &[], None);
        let reg = Registry::builtin();
        let rows = diagnose_all(&reg, &m, Some("th-code")).unwrap();
        assert_eq!(rows.len(), 1);
        let c = by_id(&rows[0], "version");
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains("exited 2: nope"), "{c:?}");
        assert!(diagnose_all(&reg, &m, Some("nope")).unwrap_err().to_string().contains("th harness list"));
        assert_eq!(diagnose_all(&reg, &m, None).unwrap().len(), reg.all().len());
    }

    /// th-5a2314: the scraped harnesses' provider checks read files and env
    /// only, and each failing row names its one fix.
    #[test]
    fn scraped_harness_auth_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = machine(tmp.path(), &[], None);
        let h = m.home.clone();
        let write = |rel: &str, text: &str| {
            let p = h.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        for name in ["aider", "goose", "crush", "cline"] {
            let k = known(name).unwrap();
            assert!(k.hooks.is_none(), "{name} is scraped");
            assert!(!k.install.is_empty());
        }

        let a = aider_auth(&m);
        assert_eq!((a.level, a.fix.is_some()), (Level::Warn, true));
        write(".aider.conf.yml", "model: sonnet\n# openai-api-key: commented\n");
        assert_eq!(aider_auth(&m).level, Level::Warn, "a commented key is no key");
        write(".aider.conf.yml", "model: sonnet\nanthropic-api-key: sk-x\n");
        assert_eq!(aider_auth(&m).level, Level::Ok);

        let g = goose_auth(&m);
        assert_eq!((g.level, g.fix.as_deref()), (Level::Fail, Some("goose configure")));
        write(".config/goose/config.yaml", "GOOSE_PROVIDER: \"\"\n");
        assert_eq!(goose_auth(&m).level, Level::Fail, "an empty provider is none");
        write(".config/goose/config.yaml", "GOOSE_PROVIDER: anthropic\nGOOSE_MODEL: x\n");
        assert!(goose_auth(&m).detail.contains("anthropic"));

        assert_eq!(crush_auth(&m).level, Level::Fail);
        write(".config/crush/crush.json", r#"{"providers":{}}"#);
        assert_eq!(crush_auth(&m).level, Level::Fail, "an empty providers object is none");
        write(".local/share/crush/crush.json", r#"{"providers":{"anthropic":{"api_key":"x"}}}"#);
        assert_eq!(crush_auth(&m).level, Level::Ok);

        assert_eq!(cline_auth(&m).level, Level::Fail);
        write(".cline/data/settings/providers.json", "{}");
        assert_eq!(cline_auth(&m).level, Level::Fail);
        write(".cline/data/settings/providers.json", r#"{"anthropic":{"apiKey":"x"}}"#);
        assert_eq!(cline_auth(&m).level, Level::Ok);

        m.env.insert("OPENAI_API_KEY".into(), "k".into());
        std::fs::remove_file(m.home.join(".aider.conf.yml")).unwrap();
        assert_eq!(aider_auth(&m).detail, "OPENAI_API_KEY is set");
    }

    #[cfg(unix)]
    #[test]
    fn opencode_plugin_must_be_linked_and_use_the_generic_event_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let m = machine(tmp.path(), &[], None);
        assert!(opencode_hooks(&m).detail.contains("not linked"));
        let src = tmp.path().join("plugin.js");
        std::fs::write(&src, "export default { event: ({event}) => fetch('/api/flow/hooks') }").unwrap();
        let link = m.home.join(".config/opencode/plugins/smooth-agent.js");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&src, &link).unwrap();
        let c = opencode_hooks(&m);
        assert!(c.detail.contains("predates hook tokens"), "a tokenless plugin is refused by the engine: {c:?}");
        assert_eq!(c.fix.as_deref(), Some("th harness enable opencode"));
        std::fs::write(
            &src,
            "// SMOOTH_FLOW_HOOK_TOKEN_FILE\nexport default { 'session.idle': () => fetch('/api/flow/hooks') }",
        )
        .unwrap();
        assert!(opencode_hooks(&m).detail.contains("never fires"));
        std::fs::write(
            &src,
            "// SMOOTH_FLOW_HOOK_TOKEN_FILE\nexport default { event: ({event}) => fetch('/api/flow/hooks') }",
        )
        .unwrap();
        assert_eq!(opencode_hooks(&m).level, Level::Ok);
    }
}
