//! Which mailbox `th msg` / `th agent` talk to — local SQLite, or the cloud.
//!
//! Pearl th-b02f63 (ADR-010). The default, and the only one that matters for
//! most people, is **SQLite on this machine** (`~/.smooth/mail.db`): no account,
//! no network, no configuration. Everything the CLI and the MCP tools do works
//! there, forever, for free.
//!
//! The cloud backend exists for exactly one thing local SQLite cannot do: agents
//! on **different machines** sharing one bus (your laptop and a build box). It
//! is user-scoped, authenticated with the `th auth login` user session, and it is
//! a paid feature with a 14-day trial. Nothing local depends on it.
//!
//! Two deliberate non-features, both because the alternative lies to the user:
//!
//! - **No silent fallback to SQLite.** If you selected `cloud` and you are
//!   offline or signed out, commands fail with the fix on one line. Quietly
//!   writing to a different mailbox looks exactly like success and loses mail.
//! - **No offline queue.** A send either landed or it didn't.
//!
//! ## Why an enum and not a trait
//!
//! [`Mail`] dispatches by match. Two implementations, one call site, no dynamic
//! dispatch and no generic threaded through `cmd_msg`/`cmd_agent`. If a third
//! backend ever shows up, that is the moment to reach for a trait — not before.
//!
//! ## Why no generated client
//!
//! `smooth-api-client` generates a typed progenitor client from a checked-in
//! `openapi.json` snapshot, but it also exposes `get`/`post`/`patch` returning
//! `serde_json::Value` — which is what every other `th api` command already
//! uses. Nine endpoints of `{"messages": [...]}` do not justify regenerating and
//! re-vendoring a 180KB spec.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use smooth_api_client::SmoothApiClient;
use smooth_pearls::mail_store::short_hostname;
use smooth_pearls::{AgentStatus, MailAgent, MailMessage, MailStore, MessageKind};

const PREFIX: &str = "/user/agent-mail";
const ENTITLEMENT_PATH: &str = "/user/cloud-features/entitlement";

/// Which mailbox to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// `~/.smooth/mail.db` on this machine. The default; works with no account.
    #[default]
    Sqlite,
    /// api.smoo.ai, shared across every machine you sign in from. Paid.
    Cloud,
}

impl Backend {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Cloud => "cloud",
        }
    }

    /// Parse a `th agent backend set` value.
    ///
    /// # Errors
    /// Returns an error naming the valid values for anything else.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sqlite" | "local" => Ok(Self::Sqlite),
            "cloud" | "smoo" => Ok(Self::Cloud),
            other => bail!("unknown backend '{other}' (expected sqlite|cloud)"),
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `~/.smooth/mail.toml` — the one thing that is configurable here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MailConfig {
    #[serde(default)]
    pub backend: Backend,
}

/// Where the backend selection lives. `$SMOOTH_MAIL_CONFIG` relocates it.
#[must_use]
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SMOOTH_MAIL_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".smooth").join("mail.toml")
}

/// Read the selection. A missing, empty, or unparseable file means the default —
/// `sqlite` is the safe answer, and refusing to run `th msg` because a config
/// file has a typo would be worse than ignoring it.
#[must_use]
pub fn load_config() -> MailConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Persist the selection.
///
/// # Errors
/// Returns an error if the file can't be written.
pub fn save_config(cfg: &MailConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, toml::to_string_pretty(cfg)?).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

// ── Cloud ──────────────────────────────────────────────────────────

/// The user's cloud mailbox on api.smoo.ai.
pub struct CloudMailStore {
    client: SmoothApiClient,
}

/// What the API says about the caller's access to cloud features.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entitlement {
    pub entitled: bool,
    pub reason: String,
    pub trial_days_left: Option<i64>,
    pub subscription_status: Option<String>,
}

impl Entitlement {
    /// One line for `th agent backend status`.
    #[must_use]
    pub fn summary(&self) -> String {
        let sub = self
            .subscription_status
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "none")
            .map(|s| format!(", subscription {s}"))
            .unwrap_or_default();
        match (self.entitled, self.trial_days_left) {
            (true, Some(d)) if self.reason == "trial" => format!("trial — {d} day(s) left{sub}"),
            (true, _) => format!("active ({}{sub})", self.reason),
            (false, _) => format!("not entitled ({}{sub})", self.reason),
        }
    }
}

impl CloudMailStore {
    /// Build a store from the signed-in **user** session.
    ///
    /// Org M2M credentials are deliberately not accepted: the routes are
    /// user-scoped and the API rejects a machine token with a 403, so failing
    /// here with the real fix beats a confusing server error later.
    ///
    /// # Errors
    /// Returns an error naming `th auth login` when there is no user session.
    pub async fn connect() -> Result<Self> {
        let client = crate::smooai::require_user_session()
            .await
            .context("the cloud mailbox is tied to your Smoo user account")?;
        Ok(Self { client })
    }

    /// Current entitlement. Reading it **starts** the 14-day trial if it hasn't
    /// started — that is the API's documented behaviour, and it is why the trial
    /// clock begins on first use rather than at signup.
    ///
    /// # Errors
    /// Returns an error if the request fails.
    pub async fn entitlement(&self) -> Result<Entitlement> {
        let v = self.get(ENTITLEMENT_PATH).await?;
        serde_json::from_value(v).context("parse entitlement")
    }

    async fn get(&self, path: &str) -> Result<serde_json::Value> {
        self.client.get(path).await.map_err(map_err)
    }

    async fn post(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
        self.client.post(path, Some(body)).await.map_err(map_err)
    }

    async fn patch(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
        self.client.patch(path, body).await.map_err(map_err)
    }

    async fn messages(&self, path: &str) -> Result<Vec<MailMessage>> {
        let v = self.get(path).await?;
        Ok(v.get("messages")
            .and_then(|m| m.as_array())
            .map(|a| a.iter().map(json_to_message).collect())
            .unwrap_or_default())
    }
}

/// Turn a raw API failure into one actionable line.
///
/// The client returns everything as `HTTP <status>: <body>`, and a raw 401/402
/// in front of a user is a dead end — each of these has exactly one fix, so say
/// it. The 402 body carries the upgrade URL, which is the only place it exists.
fn map_err(e: anyhow::Error) -> anyhow::Error {
    let text = e.to_string();
    if text.contains("401") || text.contains("Unauthorized") {
        return anyhow::anyhow!("not signed in to Smoo — run `th auth login` (or `th agent backend set sqlite` to go back to the local mailbox)");
    }
    if text.contains("403") {
        return anyhow::anyhow!(
            "the cloud mailbox is user-scoped and your session is an org machine token — run `th auth login` to sign in as yourself \
             (or `th agent backend set sqlite` to go back to the local mailbox)"
        );
    }
    if text.contains("402") {
        // The upgrade URL only exists in the response body.
        let upgrade = text
            .find("\"upgradeUrl\"")
            .and_then(|i| text[i..].split('"').nth(3).map(str::to_string))
            .unwrap_or_else(|| "https://smoo.ai/apps/start/product?feature=cloud_agent_features".to_string());
        return anyhow::anyhow!(
            "your cloud-features trial has ended — subscribe at {upgrade}\n(or `th agent backend set sqlite` to go back to the local mailbox, which is free)"
        );
    }
    if text.contains("error sending request") || text.contains("dns error") || text.contains("Connection refused") {
        return anyhow::anyhow!("can't reach api.smoo.ai — the cloud mailbox needs a network. `th agent backend set sqlite` uses the local one.");
    }
    e
}

fn s(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string).filter(|x| !x.is_empty())
}

fn dt(v: &serde_json::Value, key: &str) -> DateTime<Utc> {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|x| DateTime::parse_from_rfc3339(x).ok())
        .map_or_else(Utc::now, |d| d.with_timezone(&Utc))
}

fn json_to_agent(v: &serde_json::Value) -> MailAgent {
    MailAgent {
        name: s(v, "name").unwrap_or_default(),
        harness: s(v, "harness").unwrap_or_default(),
        machine: s(v, "machine"),
        pid: v.get("pid").and_then(serde_json::Value::as_i64),
        repo: s(v, "repo"),
        worktree: s(v, "worktree"),
        branch: s(v, "branch"),
        task: s(v, "task"),
        status: s(v, "status").and_then(|x| x.parse().ok()).unwrap_or(AgentStatus::Idle),
        registered_at: dt(v, "registeredAt"),
        last_seen: dt(v, "lastSeen"),
    }
}

fn json_to_message(v: &serde_json::Value) -> MailMessage {
    MailMessage {
        id: s(v, "id").unwrap_or_default(),
        from_agent: s(v, "fromAgent").unwrap_or_default(),
        to_agent: s(v, "toAgent").unwrap_or_default(),
        body: s(v, "body").unwrap_or_default(),
        thread_id: s(v, "threadId"),
        kind: s(v, "type").and_then(|x| x.parse().ok()).unwrap_or(MessageKind::Note),
        priority: v.get("priority").and_then(serde_json::Value::as_i64).unwrap_or(0),
        created_at: dt(v, "createdAt"),
        seq: v.get("seq").and_then(serde_json::Value::as_i64).unwrap_or(0),
        // The cloud inbox returns only what is unread when asked, and read state
        // is per-recipient server-side; there is no per-row `read_at` on the
        // wire. Rendering it as "not acked" is the honest default.
        read_at: None,
    }
}

// ── The dispatcher ─────────────────────────────────────────────────

/// The mailbox `th msg` / `th agent` are talking to.
pub enum Mail {
    Sqlite(Box<MailStore>),
    Cloud(Box<CloudMailStore>),
}

impl Mail {
    /// Open whichever backend `~/.smooth/mail.toml` selects.
    ///
    /// # Errors
    /// Returns an error if the local store can't be opened, or — on `cloud` —
    /// if there is no signed-in user session.
    pub async fn open() -> Result<Self> {
        match load_config().backend {
            Backend::Sqlite => Ok(Self::Sqlite(Box::new(MailStore::open_default()?))),
            Backend::Cloud => Ok(Self::Cloud(Box::new(CloudMailStore::connect().await?))),
        }
    }

    /// Which backend this is.
    #[must_use]
    pub const fn backend(&self) -> Backend {
        match self {
            Self::Sqlite(_) => Backend::Sqlite,
            Self::Cloud(_) => Backend::Cloud,
        }
    }

    /// Where mail actually lives, for `th agent whoami`.
    #[must_use]
    pub fn location(&self) -> String {
        match self {
            Self::Sqlite(_) => smooth_pearls::mail_store::default_path().display().to_string(),
            Self::Cloud(_) => format!("{}{PREFIX} (cloud)", smooth_api_client::base_url()),
        }
    }

    // ── Agents ─────────────────────────────────────────────────

    /// # Errors
    /// Propagates store/API failures.
    pub async fn register(&self, name: &str, harness: &str, pid: Option<i64>, cwd: &Path) -> Result<()> {
        match self {
            Self::Sqlite(s) => s.register(name, harness, pid, cwd),
            Self::Cloud(c) => {
                let (repo, worktree, branch) = git_context(cwd);
                c.post(
                    &format!("{PREFIX}/agents"),
                    &serde_json::json!({
                        "name": name.trim(),
                        "harness": harness.trim(),
                        "machine": short_hostname(),
                        "repo": repo,
                        "worktree": worktree,
                        "branch": branch,
                        "pid": pid,
                    }),
                )
                .await
                .map(|_| ())
            }
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn touch(&self, name: &str) -> Result<()> {
        match self {
            Self::Sqlite(s) => s.touch(name),
            // Any PATCH bumps `lastSeen` server-side; an empty one is the
            // heartbeat. Not worth a round-trip on every inbox read, but the
            // caller already treats touch as best-effort.
            Self::Cloud(c) => c.patch(&agent_path(name), &serde_json::json!({})).await.map(|_| ()),
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn rename(&self, old: &str, new: &str) -> Result<()> {
        match self {
            Self::Sqlite(s) => s.rename(old, new),
            Self::Cloud(c) => c
                .post(&format!("{}/rename", agent_path(old)), &serde_json::json!({ "name": new.trim() }))
                .await
                .map(|_| ()),
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn set_status(&self, name: &str, status: AgentStatus, task: Option<&str>) -> Result<()> {
        match self {
            Self::Sqlite(s) => s.set_status(name, status, task),
            Self::Cloud(c) => {
                let mut body = serde_json::json!({ "status": status.as_str() });
                if let Some(t) = task {
                    // An empty task clears it, matching the SQLite semantics.
                    body["task"] = if t.trim().is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(t.trim().to_string())
                    };
                }
                c.patch(&agent_path(name), &body).await.map(|_| ())
            }
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn list_agents(&self) -> Result<Vec<MailAgent>> {
        match self {
            Self::Sqlite(s) => s.list_agents(),
            Self::Cloud(c) => {
                let v = c.get(&format!("{PREFIX}/agents")).await?;
                Ok(v.get("agents")
                    .and_then(|a| a.as_array())
                    .map(|a| a.iter().map(json_to_agent).collect())
                    .unwrap_or_default())
            }
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn get_agent(&self, name: &str) -> Result<Option<MailAgent>> {
        match self {
            Self::Sqlite(s) => s.get_agent(name),
            // No single-agent GET on the API; the roster is small and this is
            // not a hot path.
            Self::Cloud(_) => Ok(self.list_agents().await?.into_iter().find(|a| a.name == name.trim())),
        }
    }

    // ── Mail ───────────────────────────────────────────────────

    /// # Errors
    /// Propagates store/API failures.
    pub async fn send(&self, from: &str, to: &str, body: &str, kind: MessageKind, priority: i64, thread_id: Option<&str>) -> Result<String> {
        match self {
            Self::Sqlite(s) => s.send(from, to, body, kind, priority, thread_id),
            Self::Cloud(c) => {
                let v = c
                    .post(
                        &format!("{PREFIX}/messages"),
                        &serde_json::json!({
                            "fromAgent": from.trim(),
                            "toAgent": to.trim(),
                            "body": body.trim(),
                            "type": kind.as_str(),
                            "priority": priority,
                            "threadId": thread_id,
                        }),
                    )
                    .await?;
                Ok(s(&v, "id").unwrap_or_default())
            }
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn inbox(&self, agent: &str, unread_only: bool, limit: usize) -> Result<Vec<MailMessage>> {
        match self {
            Self::Sqlite(s) => s.inbox(agent, unread_only, limit),
            Self::Cloud(c) => {
                c.messages(&format!(
                    "{PREFIX}/messages?agent={}&unread_only={unread_only}&limit={}",
                    urlencoding::encode(agent.trim()),
                    limit.clamp(1, 200),
                ))
                .await
            }
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn get_message(&self, id: &str) -> Result<Option<MailMessage>> {
        match self {
            Self::Sqlite(s) => s.get_message(id),
            // The API has no get-by-id; a message is either in this user's inbox
            // or in their sent mail, and both are already paginated endpoints.
            Self::Cloud(_) => Ok(None),
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn thread(&self, root_id: &str) -> Result<Vec<MailMessage>> {
        match self {
            Self::Sqlite(s) => s.thread(root_id),
            Self::Cloud(c) => c.messages(&format!("{PREFIX}/threads/{}", urlencoding::encode(root_id.trim()))).await,
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn ack(&self, agent: &str, message_id: &str) -> Result<()> {
        match self {
            Self::Sqlite(s) => s.ack(agent, message_id),
            Self::Cloud(c) => c
                .post(
                    &format!("{PREFIX}/messages/ack"),
                    &serde_json::json!({ "agent": agent.trim(), "messageIds": [message_id.trim()] }),
                )
                .await
                .map(|_| ()),
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn ack_all(&self, agent: &str) -> Result<usize> {
        match self {
            Self::Sqlite(s) => s.ack_all(agent),
            Self::Cloud(c) => {
                let v = c
                    .post(&format!("{PREFIX}/messages/ack"), &serde_json::json!({ "agent": agent.trim(), "all": true }))
                    .await?;
                Ok(usize::try_from(v.get("acked").and_then(serde_json::Value::as_i64).unwrap_or(0)).unwrap_or(0))
            }
        }
    }

    /// # Errors
    /// Propagates store/API failures.
    pub async fn unread_count(&self, agent: &str) -> Result<usize> {
        match self {
            Self::Sqlite(s) => s.unread_count(agent),
            Self::Cloud(c) => {
                // The inbox response carries the count, so ask for the smallest
                // page rather than adding a second endpoint.
                let v = c.get(&format!("{PREFIX}/messages?agent={}&limit=1", urlencoding::encode(agent.trim()))).await?;
                Ok(usize::try_from(v.get("unreadCount").and_then(serde_json::Value::as_i64).unwrap_or(0)).unwrap_or(0))
            }
        }
    }
}

fn agent_path(name: &str) -> String {
    format!("{PREFIX}/agents/{}", urlencoding::encode(name.trim()))
}

/// `git rev-parse` context of `cwd` — (repo, worktree, branch), best-effort.
/// Mirrors what `MailStore::register` records locally, so the two backends put
/// the same thing in the same fields.
fn git_context(cwd: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let run = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").args(args).current_dir(cwd).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let worktree = run(&["rev-parse", "--show-toplevel"]);
    let repo = run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(|p| p.to_string_lossy().into_owned()))
        .or_else(|| worktree.clone());
    (repo, worktree, run(&["rev-parse", "--abbrev-ref", "HEAD"]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_pearls::mail_store::BROADCAST;

    #[test]
    fn backend_parses_its_spellings_and_defaults_to_local() {
        assert_eq!(Backend::default(), Backend::Sqlite);
        assert_eq!(Backend::parse("sqlite").unwrap(), Backend::Sqlite);
        assert_eq!(Backend::parse(" LOCAL ").unwrap(), Backend::Sqlite);
        assert_eq!(Backend::parse("cloud").unwrap(), Backend::Cloud);
        assert!(Backend::parse("dolt").is_err(), "the Dolt backend was deliberately not built");
    }

    #[test]
    fn config_round_trips_and_a_broken_file_is_the_default() {
        let cfg: MailConfig = toml::from_str("backend = \"cloud\"").unwrap();
        assert_eq!(cfg.backend, Backend::Cloud);
        assert!(toml::to_string_pretty(&cfg).unwrap().contains("cloud"));
        // Missing key, empty file, garbage: all mean sqlite. Refusing to run
        // `th msg` over a typo in a config file would be worse than ignoring it.
        assert_eq!(toml::from_str::<MailConfig>("").unwrap().backend, Backend::Sqlite);
        assert!(toml::from_str::<MailConfig>("backend = \"dolt\"").is_err());
    }

    /// The error mapping IS the feature here: a raw `HTTP 402: {...}` in front of
    /// a user is a dead end, and each of these has exactly one fix.
    #[test]
    fn api_errors_become_one_actionable_line() {
        let unauth = map_err(anyhow::anyhow!("{}", r#"GET /user/agent-mail/agents returned HTTP 401 Unauthorized: {}"#)).to_string();
        assert!(unauth.contains("th auth login"), "{unauth}");

        let m2m = map_err(anyhow::anyhow!("{}", r#"GET /x returned HTTP 403 Forbidden: {"message":"user-scoped"}"#)).to_string();
        assert!(m2m.contains("th auth login"), "{m2m}");

        let expired = map_err(anyhow::anyhow!(
            "{}",
            r#"POST /x returned HTTP 402 Payment Required: {"error":"PaymentRequired","upgradeUrl":"https://smoo.ai/apps/start/product?feature=cloud_agent_features","reason":"trial_expired"}"#
        ))
        .to_string();
        assert!(
            expired.contains("https://smoo.ai/apps/start/product?feature=cloud_agent_features"),
            "the upgrade URL exists ONLY in the 402 body — losing it strands the user: {expired}"
        );
        assert!(expired.contains("trial has ended"), "{expired}");

        // A 402 whose body we couldn't parse still has to offer somewhere to go.
        let bare = map_err(anyhow::anyhow!("POST /x returned HTTP 402 Payment Required: ")).to_string();
        assert!(bare.contains("smoo.ai"), "{bare}");

        let offline = map_err(anyhow::anyhow!("GET https://api.smoo.ai/x: error sending request")).to_string();
        assert!(offline.contains("network") && offline.contains("sqlite"), "{offline}");

        // Every mapped error offers the free local backend as a way out.
        for e in [&unauth, &m2m, &expired, &offline] {
            assert!(e.contains("sqlite"), "should name the local escape hatch: {e}");
        }

        // Anything unrecognised passes through rather than being mislabelled.
        let other = map_err(anyhow::anyhow!("GET /x returned HTTP 500: boom")).to_string();
        assert!(other.contains("500") && other.contains("boom"));
    }

    #[test]
    fn wire_agents_map_onto_the_local_shape() {
        let v = serde_json::json!({
            "name": "laptop-fixer", "harness": "claude-code", "machine": "brents-mbp",
            "repo": "/r", "worktree": "/r/wt", "branch": "main", "task": "shipping",
            "status": "working", "pid": 42,
            "lastSeen": "2026-08-10T12:00:00Z", "registeredAt": "2026-08-09T12:00:00Z"
        });
        let a = json_to_agent(&v);
        assert_eq!(a.name, "laptop-fixer");
        assert_eq!(a.machine.as_deref(), Some("brents-mbp"), "machine is the whole point of the cloud backend");
        assert_eq!(a.status, AgentStatus::Working);
        assert_eq!(a.pid, Some(42));
        assert_eq!(a.last_seen.to_rfc3339(), "2026-08-10T12:00:00+00:00");

        // Nulls everywhere (the API declares most fields nullable) must not panic
        // or produce empty-string fields that render as `  ` in the roster.
        let sparse = json_to_agent(&serde_json::json!({ "name": "x", "harness": null, "machine": null, "status": "bogus" }));
        assert_eq!(sparse.harness, "");
        assert_eq!(sparse.machine, None);
        assert_eq!(sparse.status, AgentStatus::Idle, "an unknown status degrades, it does not fail the listing");
    }

    #[test]
    fn wire_messages_map_onto_the_local_shape() {
        let m = json_to_message(&serde_json::json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "fromAgent": "a", "toAgent": "b", "body": "hi",
            "threadId": null, "type": "handoff", "priority": 3, "seq": 7,
            "createdAt": "2026-08-10T12:00:00Z"
        }));
        assert_eq!(m.kind, MessageKind::Handoff);
        assert_eq!(m.priority, 3);
        assert_eq!(m.seq, 7);
        assert_eq!(m.thread_id, None);
        assert_eq!(m.thread_root(), m.id, "a message with no thread is its own root");

        let broadcast = json_to_message(&serde_json::json!({ "id": "i", "fromAgent": "a", "toAgent": BROADCAST, "body": "b", "type": "nonsense" }));
        assert_eq!(broadcast.to_agent, BROADCAST);
        assert_eq!(broadcast.kind, MessageKind::Note, "an unknown type degrades to note");
    }

    #[test]
    fn entitlement_summaries_read_like_english() {
        let trial = Entitlement {
            entitled: true,
            reason: "trial".into(),
            trial_days_left: Some(12),
            subscription_status: None,
        };
        assert_eq!(trial.summary(), "trial — 12 day(s) left");

        let subbed = Entitlement {
            entitled: true,
            reason: "subscription".into(),
            trial_days_left: None,
            subscription_status: Some("active".into()),
        };
        assert!(subbed.summary().starts_with("active"));

        let done = Entitlement {
            entitled: false,
            reason: "trial_expired".into(),
            trial_days_left: Some(0),
            subscription_status: Some("none".into()),
        };
        assert!(done.summary().contains("not entitled"));
    }

    #[test]
    fn entitlement_parses_the_api_shape() {
        let e: Entitlement = serde_json::from_value(serde_json::json!({
            "feature": "cloud_agent_features", "entitled": true, "reason": "trial",
            "trialEndsAt": "2026-08-24T00:00:00Z", "trialDaysLeft": 14, "subscriptionStatus": "none"
        }))
        .expect("parse");
        assert!(e.entitled);
        assert_eq!(e.trial_days_left, Some(14));
    }

    #[test]
    fn agent_paths_are_url_encoded() {
        // Handles allow `@` and `.`; a bare `user@host` handle in a path would
        // otherwise produce a request the router reads differently.
        assert_eq!(agent_path("brent@mbp"), "/user/agent-mail/agents/brent%40mbp");
        assert_eq!(agent_path("  fix-auth  "), "/user/agent-mail/agents/fix-auth");
    }

    #[test]
    fn git_context_matches_what_the_local_store_records() {
        // Run from inside this repo, so all three resolve.
        let (repo, worktree, branch) = git_context(Path::new("."));
        assert!(repo.is_some() && worktree.is_some() && branch.is_some());
        // Outside a repo: all None, never an error.
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(git_context(tmp.path()), (None, None, None));
    }
}
