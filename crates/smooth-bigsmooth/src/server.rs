//! Axum HTTP server — all REST routes, middleware, CORS.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use smooth_cast::provider_migration::load_providers_with_migration;
use smooth_operator::AgentEvent;
use tokio::sync::broadcast;
use tower_http::trace::TraceLayer;

use crate::events::{ClientEvent, ServerEvent};

/// Default idle timeout: 24 hours.
///
/// Was 30 minutes. Bumped under pearl `th-1b9b3e` after bench evidence
/// showed Big Smooth was silently shutting itself down mid-session —
/// pi + opencode (the bench's reference backends) have no daemon and
/// therefore no auto-shutdown, so smooth's 30-min cliff was a
/// competitive-parity loss masquerading as a "crashes unprompted"
/// symptom.
///
/// 24h keeps a safety net for forgotten-running dev sessions but
/// doesn't fire during a single work session. Override at boot via
/// `SMOOTH_BIGSMOOTH_IDLE_TIMEOUT_SECS=<seconds>` (set to `0` to
/// disable entirely; only honored when set in the daemon process's
/// own environment, which in sandboxed mode is the safehouse VM —
/// see project memory on env propagation).
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 24 * 60 * 60;

/// Read the idle-timeout env override. `None` = use default. `Some(0)`
/// = disabled (timeout never fires).
fn idle_timeout_from_env() -> Option<Duration> {
    let raw = std::env::var("SMOOTH_BIGSMOOTH_IDLE_TIMEOUT_SECS").ok()?;
    let secs: u64 = raw.parse().ok()?;
    Some(Duration::from_secs(secs))
}

/// Default broadcast channel capacity.
const BROADCAST_CHANNEL_CAPACITY: usize = 256;

/// Default max concurrent Smooth Operators. Each is a real microVM
/// with its own RAM allocation, so the conservative default keeps a
/// dev laptop from thrashing. Override via `SMOOTH_SANDBOX_MAX_CONCURRENCY`
/// env var (or `th up --max-operators N` on the CLI, which sets it).
const DEFAULT_SANDBOX_MAX_CONCURRENCY: usize = 3;

/// Resolve the sandbox pool cap from `SMOOTH_SANDBOX_MAX_CONCURRENCY`,
/// falling back to the default. Values <= 0 or unparseable are treated
/// as unset.
fn max_sandbox_concurrency() -> usize {
    match std::env::var("SMOOTH_SANDBOX_MAX_CONCURRENCY").ok().and_then(|v| v.parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => DEFAULT_SANDBOX_MAX_CONCURRENCY,
    }
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub pearl_store: smooth_pearls::PearlStore,
    /// Per-project [`PearlStore`]s, each backed by a long-running
    /// `smooth-dolt serve` subprocess spawned in [`AppState::new`]. See
    /// `smooth_pearls::SmoothDoltServer` for why we don't open fresh
    /// stores from inside tokio handlers (pearl `th-1a61a7`).
    pub project_pearl_stores: Arc<HashMap<std::path::PathBuf, smooth_pearls::PearlStore>>,
    /// `SmoothDoltServer` handles for each cached project. Held to keep
    /// the spawned children alive — `Drop` SIGTERMs them and removes
    /// their socket files. The `PearlStore`s above route their queries
    /// through these via `SmoothDolt::from_server`.
    pub project_dolt_servers: Arc<HashMap<std::path::PathBuf, Arc<smooth_pearls::SmoothDoltServer>>>,
    pub session_store: Arc<crate::session::DoltSessionStore>,
    pub start_time: Instant,
    pub last_activity: Arc<Mutex<Instant>>,
    pub idle_timeout: Duration,
    /// Broadcast channel for pushing [`ServerEvent`]s to all connected WebSocket clients.
    pub event_tx: broadcast::Sender<ServerEvent>,
    /// Diver client — when present, dispatch/complete go through Diver's
    /// HTTP API (with Jira sync, cost tracking, etc.) instead of direct
    /// PearlStore calls.
    pub diver: Option<crate::diver_client::DiverClient>,
    /// The orchestration state machine. Runs as a background loop picking up
    /// ready pearls and dispatching operators. Behind `Arc<tokio::sync::Mutex<>>`
    /// since the background loop and API handlers both need access.
    pub orchestrator: Arc<tokio::sync::Mutex<crate::orchestrator::Orchestrator>>,
    /// Safehouse Narc — central LLM-judge-backed access arbiter. Every
    /// per-VM Wonk escalates to this when its local policy can't
    /// auto-approve a `/check/*` request. Always present (constructed with
    /// or without an LLM backend) so the `/api/narc/*` routes can unwrap
    /// unconditionally.
    pub safehouse_narc: crate::safehouse_narc::SafehouseNarc,
    /// Registry of live teammates (operators) — populated on dispatch,
    /// idled when the comment-tap sees `[IDLE]`. Powers `/api/teammates`
    /// and the chat-agent's `teammate_list` tool. See `crate::teammates`.
    pub teammates: Arc<crate::teammates::OperativeRegistry>,
    /// Pending access-request queue + event bus. When Safehouse Narc
    /// returns [`smooth_narc::judge::Decision::Ask`], the originating
    /// tool call is held open here while a human resolves it via the
    /// `/api/access/{approve,deny}` routes (or the TUI inline card).
    /// See [`crate::access`] for the protocol.
    pub access: crate::access::AccessStore,
    /// Persistent user / project permission grants loaded from
    /// `wonk-allow.toml`. Consulted by [`crate::safehouse_narc`] after
    /// the rule engine and before the LLM judge; new grants are
    /// written back here when a `/api/access/approve` resolution
    /// lands at scope `user` or `project`. Pearl th-38b72c.
    pub wonk_grants: crate::wonk_grants::SharedWonkGrants,
    /// Per-process host-tool bearer token. The `/api/host/exec` handler
    /// checks presented bearers against this; dispatch threads it into the
    /// operative's child env. Held on state (not a process env var) because
    /// mutating `std::env` from inside the tokio runtime is UB-prone —
    /// `set_var` racing a `getenv` on another thread can segfault, and
    /// Rust 2024 marks `set_var` unsafe for exactly this reason (pearl
    /// th-87dfee). Seeded from an inherited `SMOOTH_HOST_TOKEN` (sandbox
    /// dispatch passes one in) or freshly generated.
    pub host_token: Arc<str>,
}

impl AppState {
    /// Create a new `AppState` with default idle timeout.
    ///
    /// Reads `SMOOTH_SANDBOX_MAX_CONCURRENCY` from the environment to
    /// size the sandbox pool (defaults to 3 — each microVM eats real
    /// RAM so the conservative default keeps dev laptops happy).
    pub fn new(pearl_store: smooth_pearls::PearlStore) -> Self {
        // Bootstrap the per-process host-tool bearer token. Sandbox
        // teammates use this when calling /api/host/exec so we know the
        // call is from a legit dispatch and not a stray network reach.
        // Store it on state (below) rather than in a process env var:
        // `std::env::set_var` from inside the tokio runtime is UB-prone
        // and unsafe in Rust 2024 (pearl th-87dfee). Reading an inherited
        // value once here is fine (getenv, not setenv); sandbox dispatch
        // may pass one in, otherwise generate a fresh per-process token.
        let host_token: Arc<str> = std::env::var("SMOOTH_HOST_TOKEN")
            .unwrap_or_else(|_| crate::host_tools::generate_host_token())
            .into();
        let max_operators = max_sandbox_concurrency();
        let session_store = Arc::new(crate::session::DoltSessionStore::new(&pearl_store));
        let (event_tx, _) = broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
        let orchestrator = crate::orchestrator::Orchestrator::new(max_operators, pearl_store.clone()).with_event_tx(event_tx.clone());

        // Pre-spawn a `smooth-dolt serve` per registered project, wrap each
        // in a server-mode `PearlStore`, cache. Synchronous code; runs
        // BEFORE axum starts so the smooth-dolt subprocess hang documented
        // in pearl `th-1a61a7` doesn't fire here. Failures are logged
        // and skipped — a single broken project shouldn't take the
        // service down.
        // The caller already opened a PearlStore for some dolt
        // dir (typically the cwd's). Don't spawn a SECOND
        // smooth-dolt server for the same dir — that causes
        // "manifest read only" lock contention because both
        // writers try to flush at once. The caller's store is
        // already in `pearl_store`; we only spawn for OTHER
        // registry entries here. Pearl th-67c96b benchmark fix.
        let caller_dolt_dir = pearl_store.dolt().data_dir().to_path_buf();
        let mut stores: HashMap<std::path::PathBuf, smooth_pearls::PearlStore> = HashMap::new();
        let mut servers: HashMap<std::path::PathBuf, Arc<smooth_pearls::SmoothDoltServer>> = HashMap::new();

        // Seed the cache with the caller's own project FIRST,
        // independent of the global registry. The caller already
        // handed us a working PearlStore for this dolt_dir, so we
        // don't need Registry::load() to round-trip its entry back
        // before we know about it. Without this, integration tests
        // (each creating a tempdir + initializing a PearlStore +
        // calling AppState::new) race the global ~/.smooth/registry.json
        // load/save under nextest's process-per-test fan-out — and
        // when one test's save loses to another's, the loser's
        // tempdir is missing from Registry::list() and falls out of
        // this cache. Pearls th-96e525, th-9799fa, th-e392d9.
        if let Some(project_root) = caller_dolt_dir.parent().and_then(|p| p.parent()) {
            let key = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
            stores.insert(key, pearl_store.clone());
        }

        // Pre-spawn a `smooth-dolt serve` per OTHER registered
        // project, wrap each in a server-mode `PearlStore`, cache.
        // Synchronous code; runs BEFORE axum starts so the
        // smooth-dolt subprocess hang documented in pearl
        // `th-1a61a7` doesn't fire here. Failures are logged + skipped —
        // a single broken project shouldn't take the service down.
        // Don't spawn a SECOND smooth-dolt server for the caller's
        // own dir — that causes "manifest read only" lock contention
        // because both writers try to flush at once. The caller's
        // store is already seeded above. Pearl th-67c96b.
        match smooth_pearls::Registry::load() {
            Ok(registry) => {
                for entry in registry.list() {
                    let path_str = entry.path.to_string_lossy().to_string();
                    if is_invalid_project(&path_str) {
                        continue;
                    }
                    // Canonicalize so /var/folders/... and
                    // /private/var/folders/... (macOS) — and any
                    // symlink / trailing-slash variant — resolve to
                    // the same key for both insert and the
                    // project_pearls_handler lookup. Pearl
                    // th-6db839.
                    let key = entry.path.canonicalize().unwrap_or_else(|_| entry.path.clone());
                    let dolt_dir = entry.path.join(".smooth").join("dolt");
                    if dolt_dir == caller_dolt_dir {
                        // Already seeded above. Skip — don't
                        // double-insert and don't spawn a duplicate
                        // smooth-dolt for the caller's dir.
                        continue;
                    }
                    if stores.contains_key(&key) {
                        // Two registry paths canonicalize to the same
                        // key (eg /var/folders/X and /private/var/folders/X
                        // on macOS). First insert wins; the duplicate
                        // would just be a redundant smooth-dolt.
                        continue;
                    }
                    match smooth_pearls::SmoothDoltServer::spawn(&dolt_dir) {
                        Ok(server) => {
                            let server = Arc::new(server);
                            let dolt = smooth_pearls::SmoothDolt::from_server(server.clone(), &dolt_dir);
                            let store = smooth_pearls::PearlStore::from_dolt(dolt);
                            tracing::info!(path = %path_str, "spawned smooth-dolt serve for project");
                            stores.insert(key.clone(), store);
                            servers.insert(key, server);
                        }
                        Err(e) => {
                            tracing::warn!(path = %path_str, error = %e, "failed to spawn smooth-dolt serve; project unavailable until restart");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load project registry");
            }
        }
        let project_pearl_stores = Arc::new(stores);
        let project_dolt_servers = Arc::new(servers);

        // Construct the Safehouse Narc. If the host has an LLM provider
        // configured, Narc uses the default provider for its judge; otherwise
        // it runs rule-engine-only and escalates any unhandled request to a
        // human. Load is best-effort — a missing providers.json is fine in
        // dev + tests.
        let narc_llm_config = dirs_next::home_dir().and_then(|home| {
            let providers_path = home.join(".smooth/providers.json");
            if !providers_path.exists() {
                return None;
            }
            match load_providers_with_migration(&providers_path) {
                // Route the Narc arbiter through the `Judge` slot (smooth-judge).
                // This is what the slot was named for — a cheap, fast
                // judge-class model. Falls back to the Default slot when the
                // Judge slot isn't configured (older providers.json files
                // without routing).
                Ok(registry) => match registry
                    .llm_config_for(smooth_operator::providers::Activity::Judge)
                    .or_else(|_| registry.default_llm_config())
                {
                    Ok(cfg) => Some(cfg),
                    Err(e) => {
                        tracing::warn!(error = %e, "safehouse narc: no judge/default LLM provider; Narc will escalate unknown requests to humans");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "safehouse narc: failed to load providers.json; Narc will escalate unknown requests to humans");
                    None
                }
            }
        });
        let access = crate::access::AccessStore::new();

        // Load persistent grants from ~/.smooth/wonk-allow.toml so
        // approvals from prior sessions survive a Big Smooth restart.
        // Best-effort: a parse error logs and falls back to empty
        // rather than blocking startup — a broken file shouldn't take
        // the service down. Project-scoped grants are loaded
        // lazily per-pearl when a dispatch picks them up; here we
        // just seed the user layer. Pearl th-38b72c.
        let initial_grants = match crate::wonk_grants::user_grants_path() {
            Some(path) => match crate::wonk_grants::WonkGrants::load_from_path(&path) {
                Ok(g) => {
                    tracing::info!(
                        path = %path.display(),
                        hosts = g.network.allow_hosts.len(),
                        tools = g.tools.allow.len(),
                        bash = g.bash.allow_patterns.len(),
                        "loaded user wonk-allow.toml"
                    );
                    g
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to load user wonk-allow.toml; starting empty");
                    crate::wonk_grants::WonkGrants::new()
                }
            },
            None => crate::wonk_grants::WonkGrants::new(),
        };
        let wonk_grants = crate::wonk_grants::SharedWonkGrants::new(initial_grants);

        // Wire the same AccessStore into Narc so an `Ask` verdict can
        // file_pending + await_resolution on the live, shared queue.
        // Pass the SharedWonkGrants so the judge can short-circuit on
        // a persisted grant before falling through to the LLM.
        let safehouse_narc = crate::safehouse_narc::SafehouseNarc::new(narc_llm_config, access.clone()).with_grants(wonk_grants.clone());

        Self {
            pearl_store,
            project_pearl_stores,
            project_dolt_servers,
            session_store,
            start_time: Instant::now(),
            last_activity: Arc::new(Mutex::new(Instant::now())),
            idle_timeout: idle_timeout_from_env().unwrap_or_else(|| Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS)),
            event_tx,
            diver: None,
            orchestrator: Arc::new(tokio::sync::Mutex::new(orchestrator)),
            safehouse_narc,
            teammates: Arc::new(crate::teammates::OperativeRegistry::new()),
            access,
            wonk_grants,
            host_token,
        }
    }

    /// Touch the activity timestamp — call from every handler.
    fn touch(&self) {
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
    }
}

// ── Response types ─────────────────────────────────────────

#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
    pub ok: bool,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub service: String,
    pub version: String,
    pub uptime: f64,
    pub timestamp: String,
}

#[derive(Serialize)]
pub struct SystemHealth {
    pub leader: LeaderHealth,
    pub database: DatabaseHealth,
    pub sandbox: SandboxHealth,
    pub tailscale: TailscaleHealth,
    pub pearls: PearlsHealth,
    pub orchestrator: OrchestratorHealth,
}

#[derive(Serialize)]
pub struct OrchestratorHealth {
    pub state: String,
    pub active_workers: u32,
    pub completed: u32,
}

#[derive(Serialize)]
pub struct LeaderHealth {
    pub status: String,
    pub uptime: f64,
}

#[derive(Serialize)]
pub struct DatabaseHealth {
    pub status: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct SandboxHealth {
    pub status: String,
    pub backend: String,
    pub active_sandboxes: u32,
    pub max_concurrency: u32,
}

#[derive(Serialize)]
pub struct TailscaleHealth {
    pub status: String,
    pub hostname: Option<String>,
}

#[derive(Serialize)]
pub struct PearlsHealth {
    pub status: String,
    pub open_pearls: u32,
}

// ── Query params ───────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchParams {
    q: Option<String>,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    search_type: Option<String>,
}

#[derive(Deserialize)]
pub struct ChatBody {
    content: String,
    /// Optional override for the chat agent's LLM. When unset the agent
    /// runs on the reasoning slot (smooth-reasoning-kimi by default).
    /// Set to e.g. "smooth-fast-gemini" for one-liner queries.
    #[serde(default)]
    model: Option<String>,
    /// Optional per-request USD cap on the chat agent. Stops the
    /// inner Agent loop when total cost exceeds this. Useful for
    /// keeping a bounded ceiling on tool-call recursion (chat-agent
    /// dispatches teammate → teammate ask_smooths → chat-agent
    /// answers → recurse). Defaults to no cap.
    #[serde(default)]
    budget_usd: Option<f64>,
}

#[derive(Deserialize)]
pub struct ConfigBody {
    key: String,
    value: serde_json::Value,
}

#[derive(Deserialize)]
pub struct SteerBody {
    message: Option<String>,
}

// ── Task request/types ────────────────────────────────────

#[derive(Deserialize)]
pub struct TaskRequest {
    pub message: String,
    pub model: Option<String>,
    pub budget: Option<f64>,
    pub working_dir: Option<String>,
    /// OCI image for the operator VM. Overrides the server's
    /// `SMOOTH_OPERATIVE_IMAGE` env default. Usual value is
    /// `smooai/smooth-operative:latest` (unified image; agent
    /// installs toolchains at runtime via mise).
    #[serde(default)]
    pub image: Option<String>,
    /// Keep the operator VM alive after the agent emits Completed so
    /// the user can poke at a running dev server / REPL. Must be
    /// explicitly torn down via `th operators stop <id>`.
    #[serde(default)]
    pub keep_alive: bool,
    /// Per-run memory override in MB. `None` falls back to the
    /// `SandboxConfig::default()` 4096. Next.js + a couple workers on
    /// a big monorepo want 6–8 GB; smaller tasks can stay at 4.
    #[serde(default)]
    pub memory_mb: Option<u32>,
    /// Lead role to run under. Mapped directly to
    /// [`DispatchOptions::agent`]; the runner applies the
    /// corresponding [`smooth_operator::Clearance`].
    #[serde(default)]
    pub agent: Option<String>,
}

// ── Router ─────────────────────────────────────────────────

/// Build the axum router with all routes.
///
/// The embedded web UI (SPA) is served as a fallback so that API routes
/// take priority and unknown paths return index.html for client-side routing.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Health
        .route("/health", get(health_handler))
        // System
        .route("/api/system/health", get(system_health_handler))
        .route("/api/system/config", get(get_config_handler).put(set_config_handler))
        // Tasks (headless agent execution)
        .route("/api/tasks", post(run_task_handler))
        // Projects (multi-project pearl support)
        .route("/api/projects", get(list_projects_handler))
        .route("/api/projects/pearls", get(project_pearls_handler))
        // Pearls — the only spelling. No /api/issues, no /api/beads.
        .route("/api/pearls", get(list_pearls_handler).post(create_pearl_handler))
        .route("/api/pearls/ready", get(ready_pearls_handler))
        .route("/api/pearls/stats", get(stats_handler))
        .route("/api/pearls/{id}", get(get_pearl_handler).patch(update_pearl_handler))
        .route("/api/pearls/{id}/close", post(close_pearl_handler))
        // Workers
        .route("/api/workers", get(list_workers_handler))
        .route("/api/workers/{id}", get(get_worker_handler).delete(kill_worker_handler))
        // Messages / Sessions
        .route("/api/messages/inbox", get(inbox_handler))
        .route("/api/sessions/{id}/messages", get(session_messages_handler))
        // Reviews
        .route("/api/reviews", get(list_reviews_handler))
        .route("/api/reviews/{bead_id}/approve", post(approve_review_handler))
        .route("/api/reviews/{bead_id}/reject", post(reject_review_handler))
        // Chat
        .route("/api/chat", post(chat_handler))
        .route("/api/chat/sessions", get(list_chat_sessions_handler).post(create_chat_session_handler))
        .route("/api/chat/sessions/{id}", get(get_chat_session_handler).delete(delete_chat_session_handler))
        .route(
            "/api/chat/sessions/{id}/messages",
            get(get_chat_messages_handler).post(post_chat_message_handler),
        )
        // SSE streaming variant — perceived-latency win: tokens land in
        // the UI as the model produces them instead of after the full
        // turn buffers. Pearl th-26d708.
        .route("/api/chat/sessions/{id}/messages/stream", post(post_chat_message_stream_handler))
        // Search
        .route("/api/search", get(search_handler))
        // Web search — native tool backed by DuckDuckGo HTML. Pearl th-70b68b.
        .route("/api/web_search", get(web_search_handler))
        // Credential broker — mints short-lived creds for the sandbox
        // after a human approves. Pearl th-08b65f.
        // Steering
        .route("/api/steering/{bead_id}/pause", post(pause_handler))
        .route("/api/steering/{bead_id}/resume", post(resume_handler))
        .route("/api/steering/{bead_id}/steer", post(steer_handler))
        .route("/api/steering/{bead_id}/cancel", post(cancel_handler))
        // Phase 4: live-teammate registry + direct chat with a selected teammate.
        .route("/api/teammates", get(list_teammates_handler))
        .route(
            "/api/teammates/{name}/messages",
            get(get_teammate_messages_handler).post(post_teammate_message_handler),
        )
        .route("/api/teammates/{name}/shutdown", post(shutdown_teammate_handler))
        // Delegation — operator-to-operator delegation via sub-pearls
        .route("/api/delegate", post(delegate_handler))
        .route("/api/delegate/{id}/status", get(delegate_status_handler))
        // Orchestrator
        .route("/api/orchestrator/status", get(orchestrator_status_handler))
        // Jira
        .route("/api/jira/status", get(jira_status_handler))
        .route("/api/jira/sync", post(jira_sync_handler))
        // Safehouse Narc — central LLM-judge access arbiter. Per-VM Wonks
        // POST their uncertain /check/* decisions here; Narc applies the
        // rule engine, its decision cache, and (when unresolved) the LLM
        // judge, then returns an approve/deny/escalate verdict.
        .route("/api/narc/judge", post(narc_judge_handler))
        // Access — the Claude-Code-style auto-mode pending-request queue.
        // When Narc returns `Ask`, the tool call holds inside `judge()`
        // while the human resolves it through these routes. Pearl th-49b4aa.
        .route("/api/access/pending", get(access_pending_handler))
        .route("/api/access/approve", post(access_approve_handler))
        .route("/api/access/deny", post(access_deny_handler))
        .route("/api/access/stream", get(access_stream_handler))
        .route("/api/host/exec", post(crate::host_tools::host_exec_handler))
        // WebSocket — primary real-time channel
        .route("/ws", get(ws_handler))
        // Embedded web UI (SPA fallback — must be last)
        .fallback_service(smooth_web::web_router())
        // Middleware
        //
        // CORS: same-origin only. The embedded web SPA is served by
        // `smooth_web::web_router()` as the fallback on this same
        // origin, so it never needs cross-origin headers. CLI clients
        // (`th`, curl, reqwest) ignore CORS. `CorsLayer::permissive()`
        // used to be here, but combined with `--bind 0.0.0.0` (now
        // off by default per pearl `th-6db839`) it let any malicious
        // website the user visited drive arbitrary API calls into
        // their local Big Smooth via fetch(). Restricting to
        // same-origin (default Axum behavior, no layer) closes the
        // browser-side hole independently of the bind fix.
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Start the leader HTTP server.
pub async fn start(state: AppState, addr: SocketAddr) -> anyhow::Result<()> {
    // Spawn idle timeout checker (pearl th-1b9b3e). Skip entirely when
    // the timeout is zero — bench harness + long-running dev sessions
    // set `SMOOTH_BIGSMOOTH_IDLE_TIMEOUT_SECS=0` to opt out of the
    // 30-min auto-shutdown. Pi + OpenCode (the bench's reference
    // backends) have no daemon timeout because they have no daemon —
    // smooth's daemon model means every loop pause auto-killed the
    // process before this knob existed.
    if state.idle_timeout.is_zero() {
        tracing::info!("Idle timeout disabled (SMOOTH_BIGSMOOTH_IDLE_TIMEOUT_SECS=0)");
    } else {
        let idle_state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let elapsed = {
                    let Ok(last) = idle_state.last_activity.lock() else {
                        continue;
                    };
                    last.elapsed()
                };
                if elapsed > idle_state.idle_timeout {
                    tracing::info!("Idle timeout reached ({:.0}s), shutting down", idle_state.idle_timeout.as_secs_f64());
                    std::process::exit(0);
                }
            }
        });
    }

    // Spawn orchestrator loop — continuously picks up ready pearls and
    // dispatches operators. Skipped in direct mode: the orchestrator
    // only knows how to spawn microVMs through Bootstrap Bill, so when
    // SMOOTH_WORKFLOW_DIRECT=1 it would silently fall back to sandboxed
    // dispatch even though every other path is direct. In dev / bench
    // / smoo-hub setups that's exactly the wrong thing — the only
    // dispatch we want is the explicit one driven by the chat-agent's
    // teammate_spawn (which honours direct mode). Re-enable when
    // sandbox dispatch is reliable again.
    // Periodic dolt-serve health check. Pings every smooth-dolt
    // companion (project servers + global) every 30 s; respawns any
    // that don't answer within 3 s. Closes the macOS-overnight-sleep
    // wedge where the child stays alive but the socket goes silent
    // and any subsequent dolt op blocks forever.
    {
        let project_servers = state.project_dolt_servers.clone();
        let global_server = state.pearl_store.dolt_server().cloned();
        tokio::spawn(async move {
            // Initial 60s grace so freshly-spawned children settle.
            tokio::time::sleep(Duration::from_secs(60)).await;
            loop {
                let mut all: Vec<(std::path::PathBuf, std::sync::Arc<smooth_pearls::SmoothDoltServer>)> =
                    project_servers.iter().map(|(p, s)| (p.clone(), s.clone())).collect();
                if let Some(ref g) = global_server {
                    all.push((std::path::PathBuf::from("global"), g.clone()));
                }
                for (path, server) in all {
                    let server2 = server.clone();
                    let res = tokio::task::spawn_blocking(move || server2.ensure_healthy()).await;
                    match res {
                        Ok(Ok(())) => tracing::trace!(path = %path.display(), "dolt healthcheck ok"),
                        Ok(Err(e)) => tracing::error!(path = %path.display(), error = %e, "dolt healthcheck: respawn failed"),
                        Err(join_err) => tracing::warn!(path = %path.display(), error = %join_err, "dolt healthcheck task join failed"),
                    }
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        tracing::info!("dolt healthcheck loop started (30s interval)");
    }

    // The autonomous orchestrator loop drove per-task microVM dispatch,
    // removed 2026-07 (pearl th-f4a801). Dispatch is now WebSocket-driven
    // (TaskStart → dispatch_ws_task_direct); the orchestrator only reports
    // state for status/TUI/web, so there's no background loop to run.

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // Print runtime-identifying facts on startup so anyone staring
    // at a long-running Big Smooth can confirm which binary + modes
    // are in effect without having to inspect the process env.
    let workflow_mode = if std::env::var("SMOOTH_WORKFLOW")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        "disabled (SMOOTH_WORKFLOW=0)"
    } else {
        "enabled (default)"
    };
    let skip_test = std::env::var("SMOOTH_WORKFLOW_SKIP_TEST")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let dispatch_mode = if std::env::var("SMOOTH_WORKFLOW_DIRECT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        "direct (no sandbox; agent runs as host subprocess)"
    } else {
        "sandboxed (microVM per task)"
    };
    tracing::info!(
        version = env!("TH_VERSION"),
        workflow = workflow_mode,
        skip_test_phase = skip_test,
        dispatch = dispatch_mode,
        "Smooth leader running at http://{addr}"
    );

    // Preflight the direct-dispatch runner binary so a missing /
    // moved smooth-operative shows up as a loud startup warn,
    // not as silently-failed pearls. Pearl from the post-loop sweep
    // diagnostic: the entire shared-target/release dir got nuked
    // between build and sweep, every dispatch hit the
    // "native smooth-operative not found" error path, and
    // the pearls closed in milliseconds with no METRICS. Hard to
    // spot without grepping the daemon log.
    let direct_mode = std::env::var("SMOOTH_WORKFLOW_DIRECT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if direct_mode {
        match find_native_operative_binary() {
            Some(p) => tracing::info!(runner_bin = %p.display(), "direct dispatch: resolved native smooth-operative"),
            None => tracing::warn!(
                env_var = ?std::env::var("SMOOTH_OPERATIVE_NATIVE").ok(),
                "direct dispatch: native smooth-operative NOT FOUND — every dispatch will silently close its pearl with cost_usd=0 until this is fixed. Run `cargo build --release -p smooai-smooth-operative` or set SMOOTH_OPERATIVE_NATIVE=/absolute/path."
            ),
        }
    }

    axum::serve(listener, app).await?;
    Ok(())
}

// ── WebSocket ─────────────────────────────────────────────

/// Heartbeat interval for WebSocket connections.
const WS_HEARTBEAT_SECS: u64 = 30;

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    use futures_util::{SinkExt, StreamExt};

    let session_id = uuid::Uuid::new_v4().to_string();
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Send Connected event
    let connected = ServerEvent::Connected {
        session_id: session_id.clone(),
    };
    if let Ok(json) = serde_json::to_string(&connected) {
        let _ = ws_tx.send(Message::Text(json.into())).await;
    }

    // Subscribe to broadcast channel for server events
    let mut event_rx = state.event_tx.subscribe();

    // Spawn a task that forwards broadcast events and heartbeats to the client
    let (internal_tx, mut internal_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    // Forward broadcast → internal_tx
    let broadcast_tx = internal_tx.clone();
    let broadcast_handle = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    if let Ok(json) = serde_json::to_string(&event) {
                        if broadcast_tx.send(Message::Text(json.into())).is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(lagged = n, "WebSocket client lagged behind broadcast");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Heartbeat → internal_tx
    let heartbeat_tx = internal_tx;
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(WS_HEARTBEAT_SECS));
        loop {
            interval.tick().await;
            let pong = ServerEvent::Pong;
            if let Ok(json) = serde_json::to_string(&pong) {
                if heartbeat_tx.send(Message::Text(json.into())).is_err() {
                    break;
                }
            }
        }
    });

    // Write loop: drain internal_rx into WebSocket
    let write_handle = tokio::spawn(async move {
        while let Some(msg) = internal_rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Read loop: process incoming client messages
    while let Some(Ok(msg)) = ws_rx.next().await {
        state.touch();
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };

        let Ok(event) = serde_json::from_str::<ClientEvent>(&text) else {
            let err = ServerEvent::Error {
                message: "invalid event JSON".into(),
            };
            if let Ok(json) = serde_json::to_string(&err) {
                let _ = state.event_tx.send(err);
                // Also try direct send (may fail if no subscribers, that's fine)
                let _ = json;
            }
            continue;
        };

        handle_client_event(&state, event).await;
    }

    // Client disconnected — clean up
    broadcast_handle.abort();
    heartbeat_handle.abort();
    write_handle.abort();
    tracing::debug!(session_id, "WebSocket client disconnected");
}

/// Dispatch a single [`ClientEvent`] received over WebSocket.
async fn handle_client_event(state: &AppState, event: ClientEvent) {
    match event {
        ClientEvent::Ping => {
            let _ = state.event_tx.send(ServerEvent::Pong);
        }
        ClientEvent::TaskStart {
            message,
            model,
            budget,
            working_dir,
            agent,
            prior_messages,
        } => {
            // WebSocket callers don't currently carry image / keep_alive /
            // memory_mb; HTTP /api/tasks is the dispatch path for those.
            dispatch_ws_task(
                state,
                DispatchOptions {
                    message,
                    model,
                    budget,
                    working_dir,
                    agent,
                    prior_messages,
                    ..DispatchOptions::default()
                },
            )
            .await;
        }
        ClientEvent::TaskCancel { task_id } => {
            tracing::info!(task_id, "Task cancel requested via WebSocket");
            // Cancellation is fire-and-forget for now; agent loop will
            // be extended with a cancellation token in a future PR.
        }
        ClientEvent::Steer { task_id, action, message } => {
            tracing::info!(task_id, action, "Steer via WebSocket");
            let comment = format!("[STEERING:{action}] {}", message.unwrap_or_default());
            let _ = state.pearl_store.add_comment(&task_id, &comment);
        }
        ClientEvent::PearlCreate {
            title,
            description,
            pearl_type,
            priority,
        } => {
            let desc = description.as_deref().unwrap_or("");
            let itype = pearl_type.as_deref().unwrap_or("task");
            let prio = priority.unwrap_or(2);
            match crate::pearls::create_pearl(&state.pearl_store, &title, desc, itype, prio) {
                Ok(issue) => {
                    let _ = state.event_tx.send(ServerEvent::PearlCreated { id: issue.id, title });
                }
                Err(e) => {
                    let _ = state.event_tx.send(ServerEvent::Error { message: e.to_string() });
                }
            }
        }
        ClientEvent::PearlUpdate { id, status, priority } => {
            let update = smooth_pearls::PearlUpdate {
                status: status.as_deref().and_then(smooth_pearls::PearlStatus::from_str_loose),
                priority: priority.and_then(smooth_pearls::Priority::from_u8),
                ..Default::default()
            };
            match state.pearl_store.update(&id, &update) {
                Ok(_issue) => {
                    let _ = state.event_tx.send(ServerEvent::PearlUpdated {
                        id,
                        status: status.unwrap_or_else(|| "updated".into()),
                    });
                }
                Err(e) => {
                    let _ = state.event_tx.send(ServerEvent::Error { message: e.to_string() });
                }
            }
        }
        ClientEvent::PearlClose { ids } => {
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            match state.pearl_store.close(&refs) {
                Ok(count) => {
                    for id in &ids {
                        let _ = state.event_tx.send(ServerEvent::PearlUpdated {
                            id: id.clone(),
                            status: "closed".into(),
                        });
                    }
                    tracing::info!(count, "Closed issues via WebSocket");
                }
                Err(e) => {
                    let _ = state.event_tx.send(ServerEvent::Error { message: e.to_string() });
                }
            }
        }
    }
}

/// Spawn an agent task from a WebSocket `TaskStart` event, broadcasting
/// [`ServerEvent`]s as the agent progresses.
///
/// ALL dispatch goes through the sandboxed path — Big Smooth stays
/// READ-ONLY. The operative inside the microVM hosts the real tools
/// (read_file, write_file, edit_file, grep, bash, etc.) with the full
/// security cast (Wonk/Goalie/Narc/Scribe) watching every call.
/// Options for dispatching an agent task. Bundled so the dispatch
/// helpers don't balloon past clippy's argument limit and so new
/// knobs (image, keep-alive, memory, …) can be added without
/// touching every call site.
#[derive(Debug, Clone, Default)]
pub struct DispatchOptions {
    pub message: String,
    pub model: Option<String>,
    pub budget: Option<f64>,
    pub working_dir: Option<String>,
    pub image: Option<String>,
    pub keep_alive: bool,
    pub memory_mb: Option<u32>,
    /// Lead role to run under. Propagated to the microVM runner
    /// as `SMOOTH_AGENT`; the runner resolves it against
    /// `Cast::builtin()` and installs a `PermissionHook`
    /// that blocks denied tool calls before they execute.
    pub agent: Option<String>,
    /// Reuse an existing pearl instead of creating a fresh one.
    /// The chat-agent's `teammate_spawn` tool sets this to the pearl
    /// id that `pearls_create` just returned, so dispatch doesn't
    /// duplicate it. When set:
    ///   - Diver (when present) reconciles status to `in_progress`
    ///     instead of dispatching a new entry.
    ///   - PearlStore-only path updates status to `in_progress` so
    ///     the orchestrator's "ready pearls" sweep doesn't double-
    ///     dispatch the same pearl.
    pub pearl_id: Option<String>,
    /// Prior turns of the calling client's session. Written to the
    /// sandbox's policy bind-mount as `prior_history.json`; the runner
    /// reads `SMOOTH_PRIOR_HISTORY_FILE` and pre-populates its
    /// `Conversation` with these as native role-tagged messages
    /// before the current turn. Empty / `None` means "no history,
    /// fresh agent" (pearl th-422b93).
    pub prior_messages: Vec<crate::events::PriorMessage>,
}

pub async fn dispatch_ws_task(state: &AppState, opts: DispatchOptions) {
    // The microVM dispatch path was removed 2026-07 (pearl th-f4a801;
    // git history has the sandboxed variant). Dispatch now always runs
    // the runner as a host subprocess against the host filesystem.
    dispatch_ws_task_direct(state, opts).await;
}

// Pearl th-7b95ef: `strip_ansi_escapes` was used to flatten the
// runner's colorized tracing output before pattern-matching to suppress
// it from the chat-stream forward. The forwarder now drops runner
// stderr unconditionally (it's never model content), so the ANSI
// matcher is obsolete and was removed.

/// Outcome of inspecting a single line from the operative's
/// stdout stream. Stdout is contractually the JSON `AgentEvent`
/// transport; anything else is a contract violation and must NOT be
/// forwarded to the chat-token stream (where it would be persisted
/// as fake `role: assistant` content). Pearl th-7b95ef.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunnerStdoutLine {
    /// Line parsed as JSON. `serde_json::Value` for downstream
    /// dispatch; tag/payload extraction happens at the call site so
    /// this helper stays cheap.
    Json,
    /// Empty/whitespace-only line — skip silently.
    Empty,
    /// Anything else. The caller MUST drop it (warn-log only); do not
    /// forward as a `TokenDelta`.
    NonJson,
}

/// Classify a single runner-stdout line for downstream dispatch.
/// Pulled out so the "drop non-JSON, never forward as chat content"
/// invariant has a direct unit test. See pearl th-7b95ef.
pub(crate) fn classify_runner_stdout_line(line: &str) -> RunnerStdoutLine {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return RunnerStdoutLine::Empty;
    }
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        RunnerStdoutLine::Json
    } else {
        RunnerStdoutLine::NonJson
    }
}

/// Find the NATIVE operative binary (built for the host
/// triple, not cross-compiled to `aarch64-unknown-linux-musl`).
/// Used by the direct-dispatch path where we exec the runner as
/// a regular subprocess rather than inside a microVM.
///
/// Resolution order:
/// 1. `SMOOTH_OPERATIVE_NATIVE` env var (explicit override).
/// 2. Walk up from `CARGO_MANIFEST_DIR` looking for
///    `target/release/smooth-operative`, then
///    `target/debug/smooth-operative`.
/// 3. Same walk from `std::env::current_dir`.
fn find_native_operative_binary() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("SMOOTH_OPERATIVE_NATIVE") {
        let p = std::path::PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    let check = |base: &std::path::Path| -> Option<std::path::PathBuf> {
        for profile in ["release", "debug"] {
            let candidate = base.join("target").join(profile).join("smooth-operative");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    };
    let manifest = env!("CARGO_MANIFEST_DIR");
    let mut dir = std::path::PathBuf::from(manifest);
    for _ in 0..5 {
        if let Some(p) = check(&dir) {
            return Some(p);
        }
        if !dir.pop() {
            break;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(p) = check(&cwd) {
            return Some(p);
        }
    }
    // Pearl th-92dac3: `pnpm install:th` now runs
    // `cargo install --path crates/smooth-operative`, which drops
    // the native binary at $CARGO_HOME/bin/smooth-operative (or
    // ~/.cargo/bin by default). This lookup must succeed when
    // `th up` is invoked from outside the smooth repo (e.g. from
    // ~/dev/smooai/smooai/) — neither the CARGO_MANIFEST_DIR
    // walk-up nor the cwd check finds it there.
    cargo_bin_native_operative(
        std::env::var("CARGO_INSTALL_ROOT").ok().as_deref(),
        std::env::var("CARGO_HOME").ok().as_deref(),
        dirs_next::home_dir().as_deref(),
    )
}

/// Pure helper for `find_native_operative_binary`'s ~/.cargo/bin
/// lookup. Split out for testing without touching process env.
/// Pearl th-92dac3.
fn cargo_bin_native_operative(cargo_install_root: Option<&str>, cargo_home: Option<&str>, home_dir: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    let bin_dir = if let Some(root) = cargo_install_root {
        std::path::PathBuf::from(root).join("bin")
    } else if let Some(home) = cargo_home {
        std::path::PathBuf::from(home).join("bin")
    } else {
        home_dir?.join(".cargo").join("bin")
    };
    let candidate = bin_dir.join("smooth-operative");
    candidate.is_file().then_some(candidate)
}

/// Build a human-readable resumption context block from prior session
/// messages. Empty string if `pearl_id` is None or the pearl has no
/// prior messages — caller treats empty as "no resume".
///
/// Capped at `max_messages` so the context doesn't grow unbounded
/// across many iterations on the same pearl. Messages are tagged with
/// role + timestamp so the agent can see the sequence.
fn build_resumption_context(store: &crate::session::DoltSessionStore, pearl_id: Option<&str>, max_messages: usize) -> String {
    use crate::session::SessionStore;
    let Some(pearl_id) = pearl_id else {
        return String::new();
    };
    let Ok(messages) = store.get_messages(pearl_id, max_messages) else {
        return String::new();
    };
    if messages.is_empty() {
        return String::new();
    }
    let mut ctx = String::new();
    ctx.push_str("## Resumption context\n\n");
    ctx.push_str("You are continuing work on this pearl. The following is a condensed log of what happened in prior sessions on this same pearl. Use it to understand what has already been done and avoid repeating yourself. The workspace files persist between sessions, so anything you see referenced should already exist on disk — verify with read_file before making assumptions.\n\n");
    for msg in messages.iter().rev().take(max_messages).rev() {
        let trimmed = if msg.content.chars().count() > 400 {
            let truncated: String = msg.content.chars().take(400).collect();
            format!("{truncated}…")
        } else {
            msg.content.clone()
        };
        ctx.push_str(&format!(
            "- [{}] {} → {}: {}\n",
            msg.timestamp.format("%Y-%m-%d %H:%M"),
            msg.from,
            msg.to,
            trimmed
        ));
    }
    ctx
}

/// Spawn the operative as a direct subprocess against the host
/// workspace. No microVM, no bind mounts — the agent has host-level
/// tool access (Narc tool surveillance still runs in-process). This
/// is the only dispatch path; the microVM variant was removed 2026-07
/// (pearl th-f4a801, see git history).
///
/// Wiring: we stream stdout line-by-line via tokio's async pipe
/// reader and translate each `AgentEvent` JSON line to a
/// `ServerEvent`.
#[allow(clippy::too_many_lines)]
async fn dispatch_ws_task_direct(state: &AppState, opts: DispatchOptions) {
    use std::process::Stdio;
    use tokio::io::AsyncBufReadExt;

    let DispatchOptions {
        message,
        model,
        budget,
        working_dir,
        image: _,
        keep_alive: _,
        memory_mb: _,
        agent,
        pearl_id: pearl_id_in,
        prior_messages,
    } = opts;

    let task_id = uuid::Uuid::new_v4().to_string();
    let event_tx = state.event_tx.clone();
    let pearl_store = state.pearl_store.clone();
    let last_activity = state.last_activity.clone();

    // Pearl bookkeeping — mirrors the sandboxed path so runs show
    // up in `th pearls` the same way. Caller-supplied pearl wins
    // (the chat-agent's teammate_spawn passes the pearls_create id);
    // otherwise Diver, then direct PearlStore.
    let diver = state.diver.clone();
    let pearl_id: Option<String> = if let Some(supplied) = pearl_id_in {
        tracing::info!(pearl_id = %supplied, "direct dispatch: reusing caller-supplied pearl");
        let _ = pearl_store.update(
            &supplied,
            &smooth_pearls::PearlUpdate {
                status: Some(smooth_pearls::PearlStatus::InProgress),
                ..Default::default()
            },
        );
        Some(supplied)
    } else if let Some(ref diver_client) = diver {
        match diver_client.dispatch(&format!("Task: {}", truncate_str(&message, 60)), &message, None).await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(error = %e, "direct dispatch: Diver dispatch failed, falling back to direct PearlStore");
                crate::pearls::create_pearl(&pearl_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
                    .ok()
                    .map(|i| i.id)
            }
        }
    } else {
        let id = crate::pearls::create_pearl(&pearl_store, &format!("Task: {}", truncate_str(&message, 60)), &message, "task", 2)
            .ok()
            .map(|i| i.id);
        if let Some(ref id) = id {
            let _ = pearl_store.update(
                id,
                &smooth_pearls::PearlUpdate {
                    status: Some(smooth_pearls::PearlStatus::InProgress),
                    ..Default::default()
                },
            );
        }
        id
    };

    let pearl_store_for_abort = pearl_store.clone();
    let pearl_id_for_abort = pearl_id.clone();
    let close_pearl_on_abort = |reason: &str| {
        if let Some(ref id) = pearl_id_for_abort {
            tracing::warn!(pearl_id = %id, reason, "closing task pearl (direct dispatch early return)");
            let _ = pearl_store_for_abort.close(&[id]);
        }
    };

    // Resolve runner binary + workspace as host paths.
    let runner_bin = match find_native_operative_binary() {
        Some(p) => p,
        None => {
            let err = "native smooth-operative not found. Run `cargo build -p smooai-smooth-operative` (debug) or `--release`, or set SMOOTH_OPERATIVE_NATIVE=/absolute/path.";
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: err.into(),
            });
            tracing::error!("direct dispatch: {err}");
            close_pearl_on_abort(err);
            return;
        }
    };

    // Inside the Safehouse, the user's repo is bind-mounted at
    // `/workspace` (see `start_sandboxed_vm` on the host side). Any
    // host-shaped `working_dir` the TUI sends is meaningless in here
    // — that path doesn't exist in this VM's filesystem. Translate
    // it to `/workspace`. SMOOTH_HOST_WORKSPACE is the host path the
    // mount came from, set by `start_sandboxed_vm` for the rare
    // diagnostic case where the agent needs to know.
    //
    // EXCEPT: when the TUI sends a working_dir that lives under the
    // outer host's `~/.smooth/` (the bench harness does this — its
    // per-task work_dirs are at ~/.smooth/bench-runs/<id>/<task>/),
    // we DO have a way to reach it. `th up` also bind-mounts the
    // outer ~/.smooth at /root/.smooth (RO), so we can translate the
    // outer-host prefix to the safehouse-visible prefix and bind
    // THAT into the operator VM's /workspace. Without this, every
    // bench task gets the SAME workspace contents (= cwd at
    // `th up` time, usually the smooth repo), and read_file calls
    // for task fixtures all fail. Pearl th-14d773.
    let safehouse_mode = std::env::var("SMOOTH_SAFEHOUSE_MODE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let host_workspace: std::path::PathBuf = if safehouse_mode {
        let tui_working_dir = working_dir.as_deref();
        let host_home_smooth = std::env::var("SMOOTH_HOME_HOST_PATH").ok();
        match (tui_working_dir, host_home_smooth.as_deref()) {
            (Some(wd), Some(home_smooth)) => {
                let prefix = format!("{}/", home_smooth.trim_end_matches('/'));
                if wd.starts_with(&prefix) {
                    // Outer ~/.smooth/foo → inside safehouse /root/.smooth/foo
                    let suffix = &wd[prefix.len()..];
                    let translated = std::path::PathBuf::from("/root/.smooth").join(suffix);
                    if translated.exists() {
                        tracing::info!(
                            task_id = %task_id,
                            host_path = %wd,
                            translated = %translated.display(),
                            "safehouse: translated host-side ~/.smooth working_dir to /root/.smooth"
                        );
                        translated
                    } else {
                        tracing::warn!(
                            task_id = %task_id,
                            host_path = %wd,
                            translated = %translated.display(),
                            "safehouse: translated path does not exist; falling back to /workspace"
                        );
                        std::path::PathBuf::from("/workspace")
                    }
                } else {
                    std::path::PathBuf::from("/workspace")
                }
            }
            _ => std::path::PathBuf::from("/workspace"),
        }
    } else {
        working_dir
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
    };
    if !host_workspace.exists() {
        if let Err(e) = std::fs::create_dir_all(&host_workspace) {
            let msg = format!("failed to create workspace {}: {e}", host_workspace.display());
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: msg.clone(),
            });
            close_pearl_on_abort(&msg);
            return;
        }
    }
    let workspace_canon = host_workspace.canonicalize().unwrap_or_else(|_| host_workspace.clone());

    let (api_url, api_key, final_model) = match load_llm_config_for_runner(&model) {
        Ok(x) => x,
        Err(e) => {
            let msg = format!("no LLM provider configured: {e}");
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: msg.clone(),
            });
            close_pearl_on_abort(&msg);
            return;
        }
    };

    // Build a host tempdir that carries the task message + routing
    // config. Tempdir's Drop removes it when this function returns,
    // which is AFTER we've waited for the subprocess, so the files
    // are safe for the runner's lifetime.
    let control_dir = match tempfile::Builder::new().prefix("smooth-direct-").tempdir() {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("failed to create control tempdir: {e}");
            let _ = event_tx.send(ServerEvent::TaskError {
                task_id: task_id.clone(),
                message: msg.clone(),
            });
            close_pearl_on_abort(&msg);
            return;
        }
    };

    let resumption_context = build_resumption_context(&state.session_store, pearl_id.as_deref(), 20);
    let full_task_message = if resumption_context.is_empty() {
        message.clone()
    } else {
        format!("{message}\n\n{resumption_context}")
    };

    let task_path = control_dir.path().join("task.txt");
    if let Err(e) = std::fs::write(&task_path, full_task_message.as_bytes()) {
        let msg = format!("failed to write task file: {e}");
        let _ = event_tx.send(ServerEvent::TaskError {
            task_id: task_id.clone(),
            message: msg.clone(),
        });
        close_pearl_on_abort(&msg);
        return;
    }

    // Write prior conversation history (pearl th-422b93). Same shape
    // as the sandboxed dispatch — JSON array of {role, content} that
    // the runner replays into its Conversation before the current turn.
    let prior_history_path: Option<std::path::PathBuf> = if prior_messages.is_empty() {
        None
    } else {
        let p = control_dir.path().join("prior_history.json");
        match serde_json::to_vec(&prior_messages) {
            Ok(bytes) => match std::fs::write(&p, &bytes) {
                Ok(()) => {
                    tracing::info!(messages = prior_messages.len(), "direct dispatch: wrote prior_history.json");
                    Some(p)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "direct dispatch: failed to write prior_history.json");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "direct dispatch: failed to serialize prior_messages");
                None
            }
        }
    };

    // Write routing.json so the runner's workflow path picks up
    // the provider registry (needed for the coding slot).
    //
    // When the caller passes `opts.model`, override the `coding` slot's
    // model before serializing. The runner's `coding_workflow` resolves
    // the LLM via `routing.coding`, so without this override the model
    // override is ignored and the runner uses the default coding model
    // (smooth-coding) regardless of what `teammate_spawn(model=...)`
    // requested. That made simple lookups dispatch on a slow reasoning
    // model and look like a hang.
    let routing_path = control_dir.path().join("routing.json");
    if let Some(home) = dirs_next::home_dir() {
        let providers_path = home.join(".smooth/providers.json");
        match load_providers_with_migration(&providers_path) {
            Ok(mut registry) => {
                if let Some(ref m) = model {
                    registry.routing.coding.model = m.clone();
                    tracing::info!(model = %m, "direct dispatch: overrode routing.coding.model from opts.model");
                }
                match registry.to_json() {
                    Ok(json) => {
                        if let Err(e) = std::fs::write(&routing_path, json) {
                            tracing::warn!(error = %e, "direct dispatch: failed to write routing.json; workflow will fall back to classic agent");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "direct dispatch: failed to serialize routing config"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "direct dispatch: failed to load providers.json"),
        }
    }

    let tid = task_id.clone();
    // Clone the host-tool bearer out of state before the 'static spawn —
    // Arc<str> clone is cheap and avoids borrowing `state` into the task
    // (pearl th-87dfee).
    let host_token = state.host_token.clone();

    tokio::spawn(async move {
        let _control_dir = control_dir; // keep alive

        // Build the runner's environment. Deliberately minimal —
        // the direct path doesn't need Narc/Wonk/Goalie URLs.
        let mut cmd = tokio::process::Command::new(&runner_bin);
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()))
            .env("HOME", std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
            .env("SMOOTH_API_URL", &api_url)
            .env("SMOOTH_API_KEY", &api_key)
            .env("SMOOTH_MODEL", &final_model)
            .env("SMOOTH_OPERATOR_ID", &tid)
            .env("SMOOTH_WORKSPACE", workspace_canon.to_string_lossy().as_ref())
            .env("SMOOTH_TASK_FILE", task_path.to_string_lossy().as_ref())
            .env("SMOOTH_WORKFLOW", "1")
            .env("SMOOTH_ROUTING_JSON_FILE", routing_path.to_string_lossy().as_ref());
        // Per-provider output-token cap (small local-model context
        // windows): look the resolved api_url up in providers.json —
        // via the raw-JSON reader so the typed loader doesn't drop it —
        // and pass it through so the operative caps max_tokens instead
        // of asking for the hardcoded 32768. Pearl th-f4a0fb.
        if let Some(mt) = dirs_next::home_dir()
            .map(|h| h.join(".smooth/providers.json"))
            .and_then(|p| smooth_cast::providers::max_tokens_for_api_url_from_file(&p, &api_url))
        {
            cmd.env("SMOOTH_MAX_TOKENS", mt.to_string());
        }
        if let Some(ref p) = prior_history_path {
            cmd.env("SMOOTH_PRIOR_HISTORY_FILE", p.to_string_lossy().as_ref());
        }
        if let Some(b) = budget {
            cmd.env("SMOOTH_BUDGET_USD", b.to_string());
        }
        if let Some(ref agent_name) = agent {
            if !agent_name.trim().is_empty() {
                cmd.env("SMOOTH_AGENT", agent_name);
            }
        }
        if let Ok(skip) = std::env::var("SMOOTH_WORKFLOW_SKIP_TEST") {
            cmd.env("SMOOTH_WORKFLOW_SKIP_TEST", skip);
        }
        if let Ok(iters) = std::env::var("SMOOTH_WORKFLOW_MAX_ITERATIONS") {
            cmd.env("SMOOTH_WORKFLOW_MAX_ITERATIONS", iters);
        }
        if let Ok(iters) = std::env::var("SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS") {
            cmd.env("SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS", iters);
        }
        // Pearl th-393aed: SMOOTH_VERIFY_TESTS=1 tells the operator-
        // runner to append a "no final response until tests pass"
        // rule to the agent's system prompt. Default off so general
        // `th code` users are unaffected; bench dispatch flips it on
        // before booting Big Smooth so all in-bench operator runs
        // see the rule.
        if let Ok(v) = std::env::var("SMOOTH_VERIFY_TESTS") {
            cmd.env("SMOOTH_VERIFY_TESTS", v);
        }
        if let Some(ref pid) = pearl_id {
            cmd.env("SMOOTH_PEARL_ID", pid);
        }
        // Thread the per-process host-tool bearer into the operative's
        // env so its host_tool proxy calls authenticate. Setting a child
        // Command's env is sound; the token lives on state, not the
        // parent's process env (pearl th-87dfee).
        cmd.env("SMOOTH_HOST_TOKEN", host_token.as_ref());
        if let Some(home) = dirs_next::home_dir() {
            let smooth_home = home.join(".smooth");
            if smooth_home.exists() {
                cmd.env("SMOOTH_HOME", smooth_home.to_string_lossy().as_ref());
            }
        }
        // Point the runner's in-process Wonk at Big Smooth's
        // Safehouse Narc so the direct path gets parity with the
        // sandboxed path. Without this the runner's Wonk has no
        // arbiter and hard-denies every request its local policy
        // can't auto-approve — the agent then has no path to the
        // Claude-Code-style auto-mode prompts because the call dies
        // before reaching the AccessStore. Pearl th-e96aeb.
        //
        // Localhost is fine here because the runner is running
        // directly on the host (no microVM); SMOOTH_BIGSMOOTH_URL
        // overrides if Big Smooth is reachable at a non-default
        // location (test harnesses, dev cluster setups).
        let narc_url_for_direct = resolve_direct_dispatch_narc_url(
            std::env::var("SMOOTH_NARC_URL").ok().as_deref(),
            std::env::var("SMOOTH_BIGSMOOTH_URL").ok().as_deref(),
        );
        cmd.env("SMOOTH_NARC_URL", &narc_url_for_direct);
        tracing::info!(task_id = %tid, narc_url = %narc_url_for_direct, "direct dispatch: runner Wonk wired to Safehouse Narc");
        cmd.current_dir(&workspace_canon)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: format!("failed to spawn runner: {e}"),
                });
                return;
            }
        };
        // Make runner spawns visible in service.log so a silent wedge
        // can be diagnosed after the fact. Without this the only
        // server-log entry for a dispatched runner was the "reusing
        // pearl" line and we couldn't tell whether the runner ever
        // launched / what bin / what cwd.
        tracing::info!(
            task_id = %tid,
            runner_pid = child.id().unwrap_or(0),
            runner_bin = %runner_bin.display(),
            workspace = %workspace_canon.display(),
            model = %final_model,
            "direct dispatch: spawned runner"
        );

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let mut out_reader = tokio::io::BufReader::new(stdout).lines();
        let mut err_reader = tokio::io::BufReader::new(stderr).lines();

        let event_tx_out = event_tx.clone();
        let tid_out = tid.clone();
        let last_activity_for_stdout = last_activity.clone();
        let stdout_task = tokio::spawn(async move {
            let mut agent_iterations: u32 = 0;
            let mut final_cost_usd: f64 = 0.0;
            let mut final_prompt_tokens: u64 = 0;
            let mut final_completion_tokens: u64 = 0;
            let mut final_cached_tokens: u64 = 0;
            let mut first_line_logged = false;
            while let Ok(Some(line)) = out_reader.next_line().await {
                // Pearl th-7b95ef: classify first, drop non-JSON so it
                // can't be forwarded as fake assistant chat content.
                let trimmed = match classify_runner_stdout_line(&line) {
                    RunnerStdoutLine::Empty => continue,
                    RunnerStdoutLine::Json => line.trim(),
                    RunnerStdoutLine::NonJson => {
                        tracing::warn!(task_id = %tid_out, line = %line, "non-JSON line on runner stdout (dropped)");
                        continue;
                    }
                };
                if !first_line_logged {
                    first_line_logged = true;
                    let preview: String = trimmed.chars().take(160).collect();
                    tracing::info!(task_id = %tid_out, preview = %preview, "direct dispatch: runner produced first stdout line");
                }
                if let Ok(mut la) = last_activity_for_stdout.lock() {
                    *la = std::time::Instant::now();
                }
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(event) => {
                        let Some(ty) = event.get("type").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        match ty {
                            "TokenDelta" => {
                                if let Some(content) = event.get("content").and_then(|v| v.as_str()) {
                                    let _ = event_tx_out.send(ServerEvent::TokenDelta {
                                        task_id: tid_out.clone(),
                                        content: content.to_string(),
                                    });
                                }
                            }
                            "ToolCallStart" => {
                                let tool_name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                // Read arguments from the runner's
                                // JSON-line emit (pearl th-7a5106).
                                // Older runner builds don't populate
                                // this field; fall back to "" so the
                                // legacy "tool name only" behavior
                                // still works.
                                let arguments = event.get("arguments").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let _ = event_tx_out.send(ServerEvent::ToolCallStart {
                                    task_id: tid_out.clone(),
                                    tool_name,
                                    arguments,
                                });
                            }
                            "ToolCallComplete" => {
                                let tool_name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let is_error = event.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false);
                                let result = event.get("result").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let duration_ms = event.get("duration_ms").and_then(serde_json::Value::as_u64).unwrap_or(0);
                                let _ = event_tx_out.send(ServerEvent::ToolCallComplete {
                                    task_id: tid_out.clone(),
                                    tool_name,
                                    result,
                                    is_error,
                                    duration_ms,
                                });
                            }
                            "Completed" => {
                                // Use max for iterations, cost, AND tokens so
                                // a later "fallback" Completed event (the
                                // runner emits one with zeros at exit as
                                // belt-and-suspenders, see operative
                                // main.rs) can't clobber the real numbers the
                                // workflow/agent posted earlier. Pearl
                                // th-46bc94 (iterations + cost), pearl
                                // th-eff0d0 (tokens for the cost-is-zero
                                // diagnostic).
                                if let Some(iters) = event.get("iterations").and_then(serde_json::Value::as_u64) {
                                    let iters_u32 = u32::try_from(iters).unwrap_or(u32::MAX);
                                    if iters_u32 > agent_iterations {
                                        agent_iterations = iters_u32;
                                    }
                                }
                                if let Some(c) = event.get("cost_usd").and_then(serde_json::Value::as_f64) {
                                    if c > final_cost_usd {
                                        final_cost_usd = c;
                                    }
                                }
                                if let Some(t) = event.get("prompt_tokens").and_then(serde_json::Value::as_u64) {
                                    if t > final_prompt_tokens {
                                        final_prompt_tokens = t;
                                    }
                                }
                                if let Some(t) = event.get("completion_tokens").and_then(serde_json::Value::as_u64) {
                                    if t > final_completion_tokens {
                                        final_completion_tokens = t;
                                    }
                                }
                                // Pearl th-litellm-caching-client: read cached
                                // prompt tokens (Anthropic prompt-cache hits)
                                // from the runner's Completed event so the
                                // dispatch's `[METRICS]` line can surface the
                                // session's cache-hit ratio. Defaults to 0 for
                                // older runner builds.
                                if let Some(t) = event.get("cached_tokens").and_then(serde_json::Value::as_u64) {
                                    if t > final_cached_tokens {
                                        final_cached_tokens = t;
                                    }
                                }
                            }
                            "Error" => {
                                if let Some(msg) = event.get("message").and_then(|v| v.as_str()) {
                                    let _ = event_tx_out.send(ServerEvent::TaskError {
                                        task_id: tid_out.clone(),
                                        message: msg.to_string(),
                                    });
                                }
                            }
                            // Pearl th-486bd0: bridge LlmRequest / PhaseStart
                            // as ServerEvent::LlmIteration so the TUI can
                            // reset its streaming bubble at iteration
                            // boundaries (direct-dispatch sibling of the
                            // sandboxed parser above).
                            "LlmRequest" | "PhaseStart" => {
                                let iteration = event.get("iteration").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                                let _ = event_tx_out.send(ServerEvent::LlmIteration {
                                    task_id: tid_out.clone(),
                                    iteration,
                                });
                            }
                            _ => {} // informational; not forwarded
                        }
                    }
                    Err(e) => {
                        // Unreachable in practice: classify_runner_stdout_line
                        // above already drops non-JSON. Kept as defense-in-
                        // depth so a future refactor that bypasses the
                        // classifier can't accidentally forward bad lines.
                        tracing::warn!(task_id = %tid_out, line = %trimmed, error = %e, "non-JSON line slipped past classifier (dropped)");
                    }
                }
            }
            (
                agent_iterations,
                final_cost_usd,
                final_prompt_tokens,
                final_completion_tokens,
                final_cached_tokens,
            )
        });

        let tid_err = tid.clone();
        let stderr_task = tokio::spawn(async move {
            while let Ok(Some(line)) = err_reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // Mirror EVERY stderr line to tracing so the host's
                // service.log keeps a full post-mortem record — a
                // wedged runner that printed "panic:" before dying
                // will still show up there.
                tracing::warn!(task_id = %tid_err, line = %line, "runner stderr");

                // Pearl th-7b95ef: runner stderr is diagnostic
                // tracing output + NarcHook summaries — it is NEVER
                // model-authored chat content. Everything stderr says
                // is already mirrored to service.log via the
                // `tracing::warn!` above, so we drop it from the
                // session transcript entirely. Previously a subset of
                // stderr lines was forwarded as `[runner] {line}`
                // TokenDelta, which made every policy/role/history
                // diagnostic land in the user's chat as `role:
                // assistant` content (visible as `[runner] [runner]
                // SMOOTH_POLICY_FILE env var not set` etc. in
                // `~/.smooth/coding-sessions/*.json`).
            }
        });

        let exit = child.wait().await;
        let (iters, cost, prompt_tokens, completion_tokens, cached_tokens) = stdout_task.await.unwrap_or((0, 0.0, 0, 0, 0));
        let _ = stderr_task.await;

        // Mirror sandboxed dispatch's `[METRICS]` breadcrumb on
        // BOTH success and failure paths so the bench harness can
        // distinguish "ran and failed" (METRICS with partial
        // numbers) from "never ran" (no METRICS line). Includes
        // prompt/completion token counts so a cost_usd=0 result
        // is diagnosable. Pearl th-eff0d0. Also includes
        // `cached_tokens` (subset of prompt_tokens that hit
        // Anthropic's prompt cache) — pearl th-litellm-caching-client.
        if let Some(ref id) = pearl_id {
            let body = format!(
                "[METRICS] cost_usd={cost:.8} iterations={iters} prompt_tokens={prompt_tokens} completion_tokens={completion_tokens} cached_tokens={cached_tokens}"
            );
            if let Err(e) = pearl_store.add_comment(id, &body) {
                tracing::warn!(pearl_id = %id, error = %e, "[METRICS] write failed (direct dispatch)");
            }
        }

        match exit {
            Ok(status) if status.success() => {
                let _ = event_tx.send(ServerEvent::TaskComplete {
                    task_id: tid.clone(),
                    iterations: iters,
                    cost_usd: cost,
                });
            }
            Ok(status) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: format!("runner exited non-zero: {status}"),
                });
            }
            Err(e) => {
                let _ = event_tx.send(ServerEvent::TaskError {
                    task_id: tid.clone(),
                    message: format!("runner wait failed: {e}"),
                });
            }
        }

        // Pearl bookkeeping on exit — mark the task pearl done.
        if let Some(ref id) = pearl_id {
            let _ = pearl_store.close(&[id.as_str()]);
        }
    });
}

/// Load LLM config for the in-VM runner. Big Smooth reads its own
/// providers.json (which it already does for the in-process path) and
/// projects the relevant fields into env vars the runner can consume.
/// Pull the bare hostname out of an HTTP(S) URL for the secrets
/// allowed-hosts list. Intentionally minimal — we don't need a
/// full URL crate for the shape providers.json produces
/// (`https://llm.smoo.ai/v1`, `http://127.0.0.1:11434/v1`, etc.).
/// Strips scheme, port, userinfo, and path. Returns an empty
/// string on any parse failure; callers treat empty as "no
/// substitution" (the placeholder never gets expanded on the wire,
/// which is safer than expanding on the wrong host).
/// Pick the URL to point the direct-dispatch runner's in-process Wonk
/// at for Narc escalations. Precedence:
///   1. `SMOOTH_NARC_URL` (caller override — test harnesses, shared
///      Narc setups)
///   2. `SMOOTH_BIGSMOOTH_URL` (general Big Smooth address override)
///   3. `http://127.0.0.1:4400` (the default Big Smooth bind)
///
/// Empty / whitespace-only strings are treated as "unset" rather than
/// blowing away the fallback — a common mistake in shell scripts that
/// `export FOO=` without a value.
///
/// Pearl th-e96aeb.
fn resolve_direct_dispatch_narc_url(narc_url: Option<&str>, bigsmooth_url: Option<&str>) -> String {
    let non_empty = |s: &str| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    narc_url
        .and_then(non_empty)
        .or_else(|| bigsmooth_url.and_then(non_empty))
        .unwrap_or_else(|| "http://127.0.0.1:4400".into())
}

fn load_llm_config_for_runner(model_override: &Option<String>) -> anyhow::Result<(String, String, String)> {
    let providers_path = dirs_next::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?
        .join(".smooth/providers.json");
    let registry = load_providers_with_migration(&providers_path).map_err(|e| anyhow::anyhow!("reading {}: {e}", providers_path.display()))?;
    let llm = registry.default_llm_config().map_err(|e| anyhow::anyhow!("default provider: {e}"))?;
    let model = model_override.clone().unwrap_or(llm.model);
    Ok((llm.api_url, llm.api_key, model))
}

// ── Health ─────────────────────────────────────────────────

async fn health_handler(State(state): State<AppState>) -> Json<HealthResponse> {
    state.touch();
    Json(HealthResponse {
        ok: true,
        service: "big-smooth".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime: state.start_time.elapsed().as_secs_f64(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

async fn system_health_handler(State(state): State<AppState>) -> Json<ApiResponse<SystemHealth>> {
    state.touch();
    // Round-trip a query against Dolt to confirm the store is responsive.
    let db_ok = state.pearl_store.get_config("__health_check").is_ok();
    let ts = crate::tailscale::get_status();

    let orch = state.orchestrator.lock().await;
    let orch_health = OrchestratorHealth {
        state: orch.state_name().to_string(),
        active_workers: orch.active_worker_count() as u32,
        completed: orch.completed_beads.len() as u32,
    };
    let sandbox_active = u32::try_from(orch.active_worker_count()).unwrap_or(u32::MAX);
    let sandbox_max = u32::try_from(orch.max_operators).unwrap_or(u32::MAX);
    drop(orch);

    Json(ApiResponse {
        data: SystemHealth {
            leader: LeaderHealth {
                status: "healthy".into(),
                uptime: state.start_time.elapsed().as_secs_f64(),
            },
            database: DatabaseHealth {
                status: if db_ok { "healthy" } else { "down" }.into(),
                path: state.pearl_store.dolt_path().display().to_string(),
            },
            sandbox: SandboxHealth {
                status: "healthy".into(),
                backend: "in-process".into(),
                active_sandboxes: sandbox_active,
                max_concurrency: sandbox_max,
            },
            tailscale: TailscaleHealth {
                status: if ts.connected { "connected" } else { "disconnected" }.into(),
                hostname: ts.hostname,
            },
            pearls: PearlsHealth {
                status: "healthy".into(),
                open_pearls: state.pearl_store.stats().map_or(0, |s| (s.open + s.in_progress) as u32),
            },
            orchestrator: orch_health,
        },
        ok: true,
    })
}

// ── Orchestrator ──────────────────────────────────────────

async fn orchestrator_status_handler(State(state): State<AppState>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let orch = state.orchestrator.lock().await;
    let status = serde_json::json!({
        "state": orch.state_name(),
        "active_workers": orch.active_worker_count(),
        "completed": orch.completed_beads.len(),
        "pool_max_concurrency": orch.max_operators,
        "pool_active": orch.active_worker_count(),
    });
    Json(ApiResponse { data: status, ok: true })
}

// ── Config ─────────────────────────────────────────────────

async fn get_config_handler(State(state): State<AppState>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let pairs = state.pearl_store.list_config().unwrap_or_default();
    let mut obj = serde_json::Map::new();
    for (k, v) in pairs {
        // Values were set as JSON-stringified; parse back if possible,
        // otherwise return the raw string.
        let parsed: serde_json::Value = serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v));
        obj.insert(k, parsed);
    }
    Json(ApiResponse {
        data: serde_json::Value::Object(obj),
        ok: true,
    })
}

async fn set_config_handler(State(state): State<AppState>, Json(body): Json<ConfigBody>) -> Json<ApiResponse<()>> {
    state.touch();
    let value_str = serde_json::to_string(&body.value).unwrap_or_default();
    let ok = state.pearl_store.set_config(&body.key, &value_str).is_ok();
    Json(ApiResponse { data: (), ok })
}

// ── Tasks (headless agent execution via SSE) ──────────────

async fn run_task_handler(State(state): State<AppState>, Json(req): Json<TaskRequest>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    state.touch();

    // Subscribe to the broadcast channel BEFORE dispatching so we don't miss
    // events. The dispatched task broadcasts ServerEvents which we forward as
    // AgentEvent SSE chunks for clients.
    let mut event_rx = state.event_tx.subscribe();
    let (sse_tx, sse_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

    // Dispatch via the unified ws task path — sandboxed if SMOOTH_SANDBOXED is
    // set, in-process otherwise. Sandboxed is the security architecture path:
    // operator runs inside a microVM with Wonk/Goalie/Narc enforcement.
    let state_clone = state.clone();
    let opts = DispatchOptions {
        message: req.message.clone(),
        model: req.model.clone(),
        budget: req.budget,
        working_dir: req.working_dir.clone(),
        image: req.image.clone(),
        keep_alive: req.keep_alive,
        memory_mb: req.memory_mb,
        agent: req.agent.clone(),
        pearl_id: None,
        prior_messages: Vec::new(),
    };

    tokio::spawn(async move {
        dispatch_ws_task(&state_clone, opts).await;
    });

    // Bridge ServerEvent broadcast → AgentEvent SSE stream
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    let agent_event = match event {
                        ServerEvent::TokenDelta { content, .. } => Some(AgentEvent::TokenDelta { content }),
                        ServerEvent::ToolCallStart { tool_name, arguments, .. } => Some(AgentEvent::ToolCallStart {
                            iteration: 0,
                            tool_name,
                            arguments,
                        }),
                        ServerEvent::ToolCallComplete {
                            tool_name,
                            is_error,
                            result,
                            duration_ms,
                            ..
                        } => Some(AgentEvent::ToolCallComplete {
                            iteration: 0,
                            tool_name,
                            is_error,
                            result,
                            duration_ms,
                        }),
                        ServerEvent::TaskComplete { iterations, cost_usd, .. } => {
                            let _ = sse_tx.send(AgentEvent::Completed {
                                agent_id: "task".into(),
                                iterations,
                                cost_usd,
                                prompt_tokens: 0,
                                completion_tokens: 0,
                                cached_tokens: 0,
                            });
                            break;
                        }
                        ServerEvent::TaskError { message, .. } => {
                            let _ = sse_tx.send(AgentEvent::Error { message });
                            break;
                        }
                        _ => None,
                    };
                    if let Some(e) = agent_event {
                        if sse_tx.send(e).is_err() {
                            break;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(sse_rx);
    let sse_stream = futures_util::StreamExt::map(stream, |event| {
        let data = serde_json::to_string(&event).unwrap_or_else(|_| r#"{"type":"Error","message":"serialization failed"}"#.into());
        Ok(Event::default().data(data))
    });

    Sse::new(sse_stream)
}

/// Truncate a string to at most `max_len` characters, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Derive a stable cache key from a workspace path. Produces
/// `<basename>-<6hex>` where the hex is the first 6 nibbles of an FNV-1a
/// hash of the canonicalized path — stable across runs, distinguishes
/// siblings sharing a basename. Returns `None` for empty inputs.
///
/// Why FNV rather than SHA: we only need bucket-level collision
/// resistance across the user's own projects, and avoiding the
/// `sha2` dep keeps this hot path free of cost.
pub fn project_cache_key(workspace: &str) -> Option<String> {
    let ws = workspace.trim();
    if ws.is_empty() {
        return None;
    }

    // FNV-1a 64-bit.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in ws.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }

    let basename = std::path::Path::new(ws)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "workspace".to_string());

    // Keep keys filesystem-safe: alphanum + dashes. Collapse anything
    // else to dashes so weird paths ("my project (copy)/") don't
    // produce pathological directory names.
    let safe: String = basename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();

    Some(format!("{safe}-{:06x}", hash & 0x00ff_ffff))
}

// ── Projects (multi-project pearl support) ─────────────────

#[derive(Serialize, Default)]
struct ProjectPearlCounts {
    open: usize,
    in_progress: usize,
    closed: usize,
}

#[derive(Serialize)]
struct ProjectInfo {
    path: String,
    name: String,
    pearl_counts: ProjectPearlCounts,
}

/// Returns `true` if a registry entry should be filtered out (temp dirs, invalid roots,
/// or missing `.smooth/dolt/` directory).
fn is_invalid_project(path: &str) -> bool {
    let p = std::path::Path::new(path);
    is_temp_path(p)
        || path == "/"
        || path == "/root"
        || p.components().count() <= 3 // filter bare home dirs like /Users/username
        || !p.join(".smooth/dolt").exists()
}

/// True if `p` lives under a well-known temp root. CI test runs register
/// pearl projects in tempdirs; those entries must not linger in the
/// registry (each phantom entry spawns a `smooth-dolt serve` at startup).
/// macOS puts tempdirs under `/var/folders`; Linux uses `/tmp` or
/// `/run/user/<uid>`. `std::env::temp_dir()` is the authoritative prefix;
/// the literals are fallbacks for paths recorded under a different mount
/// alias (e.g. macOS `/tmp` → `/private/tmp`). Pearl th-8bfbf4.
fn is_temp_path(p: &std::path::Path) -> bool {
    let mut roots: Vec<std::path::PathBuf> = ["/tmp", "/private/tmp", "/var/folders", "/private/var/folders", "/run/user"]
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    let sys = std::env::temp_dir();
    if let Ok(canon) = sys.canonicalize() {
        roots.push(canon);
    }
    roots.push(sys);
    // Component-wise prefix match, so `/tmpfoo` does not match `/tmp`.
    roots.iter().any(|root| p.starts_with(root))
}

async fn list_projects_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<ProjectInfo>>> {
    state.touch();

    // Use the per-project `PearlStore`s pre-spawned at startup, each
    // routed through its own long-running `smooth-dolt serve`. See
    // `AppState::project_pearl_stores` for why we don't open fresh.
    let stores = state.project_pearl_stores.clone();

    let projects = tokio::task::spawn_blocking(move || -> Vec<ProjectInfo> {
        let registry = match smooth_pearls::Registry::load() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load project registry");
                return Vec::new();
            }
        };

        let mut projects = Vec::new();
        for entry in registry.list() {
            let path_str = entry.path.to_string_lossy().to_string();
            if is_invalid_project(&path_str) {
                continue;
            }

            // Same canonicalize-on-lookup as AppState::new's insert.
            // Pearl th-6db839.
            let lookup_key = entry.path.canonicalize().unwrap_or_else(|_| entry.path.clone());
            let counts = match stores.get(&lookup_key) {
                Some(store) => store
                    .stats()
                    .map(|stats| ProjectPearlCounts {
                        open: stats.open,
                        in_progress: stats.in_progress,
                        closed: stats.closed,
                    })
                    .unwrap_or_default(),
                None => {
                    // Project registered after startup, or its serve
                    // child failed to spawn. Surface the entry with
                    // zero counts so it still appears in the picker.
                    tracing::debug!(path = %path_str, "project not in pre-spawned cache; restart service to populate");
                    ProjectPearlCounts::default()
                }
            };

            projects.push(ProjectInfo {
                path: path_str,
                name: entry.name.clone(),
                pearl_counts: counts,
            });
        }
        projects
    })
    .await
    .unwrap_or_default();

    Json(ApiResponse { data: projects, ok: true })
}

#[derive(Deserialize)]
pub struct ProjectPearlsParams {
    path: String,
    status: Option<String>,
    /// Optional cap on returned pearls. Defaults to `0` = "no limit" so
    /// the dashboard / pearls page get the full set for client-side
    /// counting and bucketing. Pass an explicit value to paginate.
    #[serde(default)]
    limit: usize,
}

async fn project_pearls_handler(State(state): State<AppState>, Query(params): Query<ProjectPearlsParams>) -> Json<ApiResponse<Vec<smooth_pearls::Pearl>>> {
    state.touch();

    // Use the pre-spawned (server-mode) store for this project. See
    // `AppState::project_pearl_stores` for why we don't open fresh.
    // Canonicalize so `?path=/var/folders/...` and
    // `?path=/private/var/folders/...` resolve to the same map key
    // we stored under in `AppState::new` (also canonicalized).
    // Pearl th-6db839.
    let raw_path = std::path::PathBuf::from(&params.path);
    let project_path = raw_path.canonicalize().unwrap_or(raw_path);
    let stores = state.project_pearl_stores.clone();
    let limit = params.limit;
    let status = params.status.clone();
    let path_for_log = params.path.clone();
    let result: Result<Vec<smooth_pearls::Pearl>, anyhow::Error> = tokio::task::spawn_blocking(move || {
        let store = stores
            .get(&project_path)
            .ok_or_else(|| anyhow::anyhow!("project not in pre-spawned cache (restart service to populate): {}", project_path.display()))?;
        let mut query = smooth_pearls::PearlQuery::new().with_limit(limit);
        if let Some(ref s) = status {
            query = query.with_status(smooth_pearls::PearlStatus::from_str_loose(s).unwrap_or(smooth_pearls::PearlStatus::Open));
        }
        Ok(store.list(&query).unwrap_or_default())
    })
    .await
    .unwrap_or_else(|join_err| Err(anyhow::anyhow!("blocking task join failed: {join_err}")));

    match result {
        Ok(pearls) => Json(ApiResponse { data: pearls, ok: true }),
        Err(e) => {
            tracing::warn!(error = %e, path = %path_for_log, "failed to load pearls for project");
            Json(ApiResponse { data: vec![], ok: false })
        }
    }
}

// ── Issues ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListPearlsParams {
    status: Option<String>,
    /// Optional cap. Defaults to `0` = "no limit" so the web UI gets
    /// the full set; pass a value to paginate.
    #[serde(default)]
    limit: usize,
}

#[derive(Deserialize)]
pub struct CreatePearlBody {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "type", default = "default_pearl_type")]
    pearl_type: String,
    #[serde(default = "default_priority")]
    priority: u8,
}

fn default_pearl_type() -> String {
    "task".into()
}

const fn default_priority() -> u8 {
    2
}

#[derive(Deserialize)]
pub struct UpdatePearlBody {
    status: Option<String>,
    title: Option<String>,
    description: Option<String>,
    priority: Option<u8>,
    #[serde(rename = "type")]
    pearl_type: Option<String>,
}

async fn list_pearls_handler(State(state): State<AppState>, Query(params): Query<ListPearlsParams>) -> Json<ApiResponse<Vec<smooth_pearls::Pearl>>> {
    state.touch();
    let issues = crate::pearls::list_pearls_with_limit(&state.pearl_store, params.status.as_deref(), params.limit).unwrap_or_default();
    Json(ApiResponse { data: issues, ok: true })
}

async fn get_pearl_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let issue = crate::pearls::get_pearl(&state.pearl_store, &id).unwrap_or(None);
    let data = match issue {
        Some(i) => serde_json::to_value(i).unwrap_or(serde_json::json!(null)),
        None => serde_json::json!(null),
    };
    Json(ApiResponse { data, ok: true })
}

async fn ready_pearls_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<smooth_pearls::Pearl>>> {
    state.touch();
    let issues = crate::pearls::get_ready(&state.pearl_store).unwrap_or_default();
    Json(ApiResponse { data: issues, ok: true })
}

async fn create_pearl_handler(State(state): State<AppState>, Json(body): Json<CreatePearlBody>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    match crate::pearls::create_pearl(&state.pearl_store, &body.title, &body.description, &body.pearl_type, body.priority) {
        Ok(issue) => Json(ApiResponse {
            data: serde_json::to_value(issue).unwrap_or(serde_json::json!(null)),
            ok: true,
        }),
        Err(e) => Json(ApiResponse {
            data: serde_json::json!({"error": e.to_string()}),
            ok: false,
        }),
    }
}

async fn update_pearl_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdatePearlBody>,
) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let update = smooth_pearls::PearlUpdate {
        title: body.title,
        description: body.description,
        status: body.status.as_deref().and_then(smooth_pearls::PearlStatus::from_str_loose),
        priority: body.priority.and_then(smooth_pearls::Priority::from_u8),
        pearl_type: body.pearl_type.as_deref().and_then(smooth_pearls::PearlType::from_str_loose),
        ..Default::default()
    };
    match state.pearl_store.update(&id, &update) {
        Ok(issue) => Json(ApiResponse {
            data: serde_json::to_value(issue).unwrap_or(serde_json::json!(null)),
            ok: true,
        }),
        Err(e) => Json(ApiResponse {
            data: serde_json::json!({"error": e.to_string()}),
            ok: false,
        }),
    }
}

async fn close_pearl_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    match state.pearl_store.close(&[&id]) {
        Ok(count) => Json(ApiResponse {
            data: serde_json::json!({"closed": count}),
            ok: true,
        }),
        Err(e) => Json(ApiResponse {
            data: serde_json::json!({"error": e.to_string()}),
            ok: false,
        }),
    }
}

async fn stats_handler(State(state): State<AppState>) -> Json<ApiResponse<smooth_pearls::PearlStats>> {
    state.touch();
    let stats = crate::pearls::stats(&state.pearl_store).unwrap_or_default();
    Json(ApiResponse { data: stats, ok: true })
}

// ── Workers ────────────────────────────────────────────────

// Live operators are tracked in the teammates registry now that
// dispatch runs in-process (the VM sandbox handles these routes used
// to read were removed 2026-07, pearl th-f4a801). `operator_id` maps
// to the teammate slug and `bead_id` to its pearl.
async fn list_workers_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    let data: Vec<serde_json::Value> = state
        .teammates
        .list()
        .await
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "operator_id": t.name,
                "bead_id": t.pearl_id,
                "title": t.title,
                "status": t.status,
                "created_at": t.started_at,
            })
        })
        .collect();
    Json(ApiResponse { data, ok: true })
}

async fn get_worker_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();
    let data = state.teammates.get(&id).await.map_or_else(
        || serde_json::json!({"id": id, "status": "unknown"}),
        |t| {
            serde_json::json!({
                "operator_id": t.name,
                "bead_id": t.pearl_id,
                "title": t.title,
                "status": t.status,
                "created_at": t.started_at,
            })
        },
    );
    Json(ApiResponse { data, ok: true })
}

async fn kill_worker_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    if state.teammates.get(&id).await.is_some() {
        state.teammates.mark_status(&id, "ended").await;
        tracing::info!(operator = %id, "kill_worker: teammate marked ended");
        Json(ApiResponse { data: (), ok: true })
    } else {
        tracing::warn!(operator = %id, "kill_worker: no active operator with that id");
        Json(ApiResponse { data: (), ok: false })
    }
}

// ── Messages ───────────────────────────────────────────────

async fn inbox_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    Json(ApiResponse { data: vec![], ok: true })
}

async fn session_messages_handler(
    State(state): State<AppState>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    use crate::session::SessionStore;
    let msgs = state.session_store.get_messages(&session_id, 100).unwrap_or_default();
    let data: Vec<serde_json::Value> = msgs
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "session_id": m.session_id,
                "from": m.from,
                "to": m.to,
                "content": m.content,
                "message_type": format!("{:?}", m.message_type),
                "timestamp": m.timestamp.to_rfc3339(),
            })
        })
        .collect();
    Json(ApiResponse { data, ok: true })
}

// ── Reviews ────────────────────────────────────────────────

async fn list_reviews_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<serde_json::Value>>> {
    state.touch();
    Json(ApiResponse { data: vec![], ok: true })
}

async fn approve_review_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    tracing::info!("Approve review for {bead_id}");
    let _ = state.pearl_store.close(&[&bead_id]);
    Json(ApiResponse { data: (), ok: true })
}

async fn reject_review_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    tracing::info!("Reject review for {bead_id}");
    Json(ApiResponse { data: (), ok: true })
}

// ── Chat ───────────────────────────────────────────────────

async fn chat_handler(State(state): State<AppState>, Json(body): Json<ChatBody>) -> Json<ApiResponse<String>> {
    state.touch();

    // The chat agent IS the team lead. It searches pearls, creates them
    // with smooth-summarize-generated titles, dispatches teammates
    // (operators), nudges them with steering messages, and reads back
    // their progress. Default model is `smooth-reasoning-kimi` —
    // capability for orchestration beats raw speed; per-request model
    // override is a Phase 6 polish (see plan).
    let system_prompt = include_str!("chat_tools_system_prompt.txt");

    async fn chat_inner(
        state: AppState,
        system_prompt: &str,
        user_content: &str,
        model_override: Option<&str>,
        budget_usd: Option<f64>,
    ) -> anyhow::Result<String> {
        use smooth_operator::agent::{Agent, AgentConfig, AgentEvent};
        use smooth_operator::cost::CostBudget;

        let providers_path = dirs_next::home_dir().unwrap_or_default().join(".smooth/providers.json");
        let registry = load_providers_with_migration(&providers_path).map_err(|e| anyhow::anyhow!("no LLM providers configured: {e}"))?;

        // Resolve the chat agent's LLM. Default is the CODING slot
        // (MiniMax via smooth-coding) — fast AND tool-call-capable.
        // The previous defaults all had problems: smooth-reasoning-kimi
        // was too slow, smooth-fast-gemini hallucinated Python-style
        // `tool_code` blocks instead of using native function calling.
        // smooth-coding is explicitly trained on tool use and runs at
        // ~1-2s per turn on the gateway, which is the right balance for
        // an orchestration chat. Per-request `model` still flips up
        // (e.g. smooth-reasoning) for hard requests, or down (e.g.
        // smooth-fast-gemini) when the user is willing to risk less
        // capability for more speed.
        let llm_config = if let Some(m) = model_override.filter(|s| !s.trim().is_empty()) {
            let mut cfg = registry.default_llm_config().map_err(|e| anyhow::anyhow!("no default provider: {e}"))?;
            cfg.model = m.to_string();
            cfg
        } else {
            registry
                .llm_config_for(smooth_operator::providers::Activity::Coding)
                .map_err(|e| anyhow::anyhow!("resolving coding slot for chat: {e}"))?
        };

        let registry_arc = std::sync::Arc::new(registry);
        let tools = crate::chat_tools::build_chat_tools(state.clone(), registry_arc.clone());

        // 20-iteration cap: enough for the chat-agent to drive a task
        // to completion via teammate_wait (poll + retry + format). Each
        // teammate_wait burns just one iteration even if the underlying
        // wait is a minute. Five was too few — the agent ran out of
        // turns mid-poll and returned an empty assistant message.
        let mut agent_cfg = AgentConfig::new("big-smooth-chat", system_prompt, llm_config).with_max_iterations(20);
        if let Some(cap) = budget_usd {
            agent_cfg = agent_cfg.with_budget(CostBudget {
                max_cost_usd: Some(cap),
                max_tokens: None,
            });
        }
        let agent = Agent::new(agent_cfg, tools);

        let thoughts = crate::thoughts::ThoughtStreamer::new(&registry_arc, state.event_tx.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        // Heartbeat: if a single tool sits running for > 8s without
        // emitting any new ToolCallStart (e.g. teammate_wait polling),
        // fire a synthesized "still working" thought so the UI bubbles
        // don't go quiet. Tracks the last tool name + the start instant
        // so the heartbeat thought references it.
        let last_action: std::sync::Arc<tokio::sync::Mutex<(String, std::time::Instant)>> =
            std::sync::Arc::new(tokio::sync::Mutex::new((String::from("thinking"), std::time::Instant::now())));
        let last_action_drain = last_action.clone();
        let thoughts_drain = thoughts.clone();
        let drain = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    AgentEvent::ToolCallStart { tool_name, .. } => {
                        {
                            let mut la = last_action_drain.lock().await;
                            *la = (tool_name.clone(), std::time::Instant::now());
                        }
                        thoughts_drain.emit(crate::thoughts::ThoughtContext::ToolCall { tool_name });
                    }
                    AgentEvent::ToolCallComplete { .. } => {
                        let mut la = last_action_drain.lock().await;
                        la.1 = std::time::Instant::now(); // reset so heartbeat doesn't fire on the next iteration's gap
                    }
                    AgentEvent::LlmResponse { content_preview, .. } if !content_preview.trim().is_empty() => {
                        thoughts_drain.emit(crate::thoughts::ThoughtContext::AssistantPreview { snippet: content_preview });
                    }
                    _ => {}
                }
            }
        });
        let last_action_hb = last_action.clone();
        let thoughts_hb = thoughts.clone();
        let heartbeat = tokio::spawn(async move {
            // First beat at 8s so quick chats don't get a heartbeat at
            // all. After that, every 9-13s while the same action is
            // still in flight.
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            loop {
                let (action, since) = {
                    let la = last_action_hb.lock().await;
                    (la.0.clone(), la.1)
                };
                let elapsed = since.elapsed().as_secs();
                if elapsed >= 8 {
                    thoughts_hb.emit(crate::thoughts::ThoughtContext::Heartbeat {
                        last_action: action,
                        seconds: u32::try_from(elapsed).unwrap_or(u32::MAX),
                    });
                }
                tokio::time::sleep(std::time::Duration::from_secs(11)).await;
            }
        });

        let conversation = agent
            .run_with_channel(user_content.to_string(), tx)
            .await
            .map_err(|e| anyhow::anyhow!("chat agent: {e}"))?;
        heartbeat.abort();
        drain.abort();

        // Final assistant message is the user-facing reply.
        let last_assistant = conversation.last_assistant_content().unwrap_or("(no response)").to_string();
        Ok(last_assistant)
    }

    // Same 5-minute ceiling as the session-bound chat path. Anything
    // past this is a wedge — return an actionable error so the user
    // can retry instead of watching a spinner indefinitely.
    const CHAT_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    let result: anyhow::Result<String> = match tokio::time::timeout(
        CHAT_TURN_TIMEOUT,
        chat_inner(state, system_prompt, &body.content, body.model.as_deref(), body.budget_usd),
    )
    .await
    {
        Ok(inner) => inner,
        Err(_) => Err(anyhow::anyhow!("chat turn exceeded {CHAT_TURN_TIMEOUT:?} ceiling")),
    };

    match result {
        Ok(response) => Json(ApiResponse { data: response, ok: true }),
        Err(e) => Json(ApiResponse {
            data: format!("Error: {e}"),
            ok: true,
        }),
    }
}

// ── Chat sessions ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateChatSessionBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
pub struct PostChatMessageBody {
    content: String,
}

#[derive(Serialize)]
pub struct ChatMessageView {
    id: String,
    role: String, // "user" | "assistant"
    content: String,
    created_at: String,
    /// Tool calls executed while producing this message. Empty for
    /// user messages and for assistant turns that didn't invoke any
    /// tools. Always present (rather than omitted) so web clients
    /// don't have to guard against undefined — empty array is the
    /// "no tools" signal. Pearl th-880f2c.
    tool_calls: Vec<ToolCallView>,
}

/// Wire shape for one tool call. Mirrors `SessionToolCall` in
/// `session.rs` 1:1 but lives here so the API surface stays
/// independent of the storage layer if we ever swap stores.
#[derive(Serialize, Deserialize, Clone)]
pub struct ToolCallView {
    id: String,
    tool_name: String,
    arguments: String,
    output: Option<String>,
    status: String, // "running" | "done" | "error"
    duration_ms: Option<u64>,
}

impl From<crate::session::SessionToolCall> for ToolCallView {
    fn from(t: crate::session::SessionToolCall) -> Self {
        Self {
            id: t.id,
            tool_name: t.tool_name,
            arguments: t.arguments,
            output: t.output,
            status: t.status,
            duration_ms: t.duration_ms,
        }
    }
}

async fn create_chat_session_handler(State(state): State<AppState>, Json(body): Json<CreateChatSessionBody>) -> Json<ApiResponse<crate::session::ChatSession>> {
    state.touch();
    let title = body.title.unwrap_or_else(|| "New chat".to_string());
    let model = body.model.unwrap_or_else(chat_default_model);
    match state.session_store.create_chat_session(&title, &model) {
        Ok(session) => Json(ApiResponse { data: session, ok: true }),
        Err(e) => {
            tracing::warn!(error = %e, "failed to create chat session");
            Json(ApiResponse {
                data: crate::session::ChatSession {
                    id: String::new(),
                    title: String::new(),
                    model: String::new(),
                    started_at: chrono::Utc::now(),
                    message_count: 0,
                    token_count: 0,
                },
                ok: false,
            })
        }
    }
}

async fn list_chat_sessions_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<crate::session::ChatSession>>> {
    state.touch();
    let sessions = state.session_store.list_chat_sessions().unwrap_or_default();
    Json(ApiResponse { data: sessions, ok: true })
}

async fn get_chat_session_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<Option<crate::session::ChatSession>>> {
    state.touch();
    let session = state.session_store.get_chat_session(&id).ok().flatten();
    Json(ApiResponse { data: session, ok: true })
}

async fn delete_chat_session_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<()>> {
    state.touch();
    let ok = state.session_store.delete_chat_session(&id).is_ok();
    Json(ApiResponse { data: (), ok })
}

async fn get_chat_messages_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<Vec<ChatMessageView>>> {
    use crate::session::SessionStore;
    state.touch();
    let msgs = state.session_store.get_messages(&id, 1000).unwrap_or_default();
    let views: Vec<ChatMessageView> = msgs
        .into_iter()
        .map(|m| ChatMessageView {
            id: m.id,
            role: if m.from == "user" { "user".to_string() } else { "assistant".to_string() },
            content: m.content,
            created_at: m.timestamp.to_rfc3339(),
            tool_calls: m.tool_calls.into_iter().map(ToolCallView::from).collect(),
        })
        .collect();
    Json(ApiResponse { data: views, ok: true })
}

async fn post_chat_message_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PostChatMessageBody>,
) -> Json<ApiResponse<ChatMessageView>> {
    use crate::session::SessionStore;
    state.touch();

    let user_content = body.content;
    let user_msg_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let user_msg = crate::session::SessionMessage {
        id: user_msg_id.clone(),
        session_id: id.clone(),
        from: "user".into(),
        to: "bigsmooth".into(),
        content: user_content.clone(),
        timestamp: chrono::Utc::now(),
        message_type: crate::session::MessageType::Command,
        tool_calls: Vec::new(),
    };
    if let Err(e) = state.session_store.save_message(user_msg) {
        tracing::warn!(error = %e, "failed to save user chat message");
    }

    // If this is the first message, kick off an async auto-name via
    // `smooth-fast` — the Haiku-class utility slot. We spawn the LLM
    // call into a detached tokio task so the chat response isn't gated
    // on it; the title lands on the session row via `rename_chat_session`
    // whenever the completion comes back. If the fast slot isn't
    // configured or the call fails, we fall back to the legacy
    // truncate-first-60-chars behaviour so a session is never left
    // literally named "New chat".
    if let Ok(Some(session)) = state.session_store.get_chat_session(&id) {
        if session.title == "New chat" {
            let session_store = state.session_store.clone();
            let id_for_spawn = id.clone();
            let prompt_for_spawn = user_content.clone();
            tokio::spawn(async move {
                let title = auto_name_session(&prompt_for_spawn).await.unwrap_or_else(|| {
                    // Fallback to the legacy behaviour.
                    let short: String = prompt_for_spawn.chars().take(60).collect();
                    short.trim().to_string()
                });
                if !title.is_empty() {
                    let _ = session_store.rename_chat_session(&id_for_spawn, &title);
                }
            });
        }
    }

    // Pull recent history to feed the LLM (oldest first).
    let history = state.session_store.get_messages(&id, 50).unwrap_or_default();

    // Goal-first system prompt shared with /api/chat — the session-bound
    // path is the one the web UI hits, so this is what the user actually
    // sees as Big Smooth's persona.
    let system_prompt = include_str!("chat_tools_system_prompt.txt");
    // Hard ceiling on a single chat turn. The chat agent itself caps
    // iterations at 20 with per-tool timeouts (bash 10s, teammate_wait
    // 60s ×3), so even a worst-case run completes inside 4 minutes.
    // 5 minutes is a generous buffer; anything past it is a wedge
    // (dolt deadlock, gateway hang, etc.) and we're better off
    // returning an error to the user than leaving them watching the
    // thinking spinner forever.
    const CHAT_TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    let (assistant_text, tool_calls) = match tokio::time::timeout(CHAT_TURN_TIMEOUT, run_chat_with_history(&state, system_prompt, &history, &user_content))
        .await
    {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => (format!("Error: {e}"), Vec::new()),
        Err(_) => {
            tracing::warn!(session = %id, "chat turn exceeded {CHAT_TURN_TIMEOUT:?} ceiling — aborting");
            (
                "_(Big Smooth ran into a wall — the chat turn went past 5 minutes without a real answer. Try sending the message again; the next turn starts fresh.)_".to_string(),
                Vec::new(),
            )
        }
    };

    let assistant_msg_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let assistant_msg = crate::session::SessionMessage {
        id: assistant_msg_id.clone(),
        session_id: id.clone(),
        from: "bigsmooth".into(),
        to: "user".into(),
        content: assistant_text.clone(),
        timestamp: chrono::Utc::now(),
        message_type: crate::session::MessageType::Response,
        tool_calls: tool_calls.clone(),
    };
    if let Err(e) = state.session_store.save_message(assistant_msg) {
        tracing::warn!(error = %e, "failed to save assistant chat message");
    }

    let _ = state.session_store.bump_message_count(&id, 2);

    Json(ApiResponse {
        data: ChatMessageView {
            id: assistant_msg_id,
            role: "assistant".into(),
            content: assistant_text,
            created_at: chrono::Utc::now().to_rfc3339(),
            tool_calls: tool_calls.into_iter().map(ToolCallView::from).collect(),
        },
        ok: true,
    })
}

/// SSE-streaming variant of [`post_chat_message_handler`] (pearl
/// th-26d708). End-to-end wallclock is identical, but the user sees
/// the model's tokens arriving incrementally instead of staring at a
/// blank panel for 20-30s.
///
/// Wire format mirrors `/api/tasks`: each SSE event carries a
/// JSON-serialized `AgentEvent` so the web client can use the same
/// stream parser. Persistence (user msg + final assistant msg with
/// tool_calls) still runs server-side; the final `Completed` event
/// signals the client that the message is durable.
async fn post_chat_message_stream_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PostChatMessageBody>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    use crate::session::SessionStore;
    use smooth_operator::agent::{Agent, AgentConfig, AgentEvent};

    state.touch();

    let user_content = body.content;
    let user_msg_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let user_msg = crate::session::SessionMessage {
        id: user_msg_id.clone(),
        session_id: id.clone(),
        from: "user".into(),
        to: "bigsmooth".into(),
        content: user_content.clone(),
        timestamp: chrono::Utc::now(),
        message_type: crate::session::MessageType::Command,
        tool_calls: Vec::new(),
    };
    if let Err(e) = state.session_store.save_message(user_msg) {
        tracing::warn!(error = %e, "failed to save user chat message");
    }

    // Auto-name the session — same fire-and-forget behaviour as the
    // buffered handler. Detached so streaming latency isn't gated on it.
    if let Ok(Some(session)) = state.session_store.get_chat_session(&id) {
        if session.title == "New chat" {
            let session_store = state.session_store.clone();
            let id_for_spawn = id.clone();
            let prompt_for_spawn = user_content.clone();
            tokio::spawn(async move {
                let title = auto_name_session(&prompt_for_spawn).await.unwrap_or_else(|| {
                    let short: String = prompt_for_spawn.chars().take(60).collect();
                    short.trim().to_string()
                });
                if !title.is_empty() {
                    let _ = session_store.rename_chat_session(&id_for_spawn, &title);
                }
            });
        }
    }

    let history = state.session_store.get_messages(&id, 50).unwrap_or_default();

    // Build the SSE event channel. The agent task forwards every
    // AgentEvent it sees, then sends a final `Completed` after
    // persisting the assistant message.
    let (sse_tx, sse_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

    let state_for_agent = state.clone();
    let session_id = id.clone();
    tokio::spawn(async move {
        // Resolve provider config — fail fast with an Error event if
        // the user hasn't configured an LLM provider.
        let providers_path = dirs_next::home_dir().unwrap_or_default().join(".smooth/providers.json");
        let registry = match load_providers_with_migration(&providers_path) {
            Ok(r) => r,
            Err(e) => {
                let _ = sse_tx.send(AgentEvent::Error {
                    message: format!("no LLM providers configured: {e}"),
                });
                return;
            }
        };
        let llm_config = match registry.llm_config_for(smooth_operator::providers::Activity::Coding) {
            Ok(c) => c,
            Err(e) => {
                let _ = sse_tx.send(AgentEvent::Error {
                    message: format!("resolving coding slot for chat: {e}"),
                });
                return;
            }
        };

        let registry_arc = std::sync::Arc::new(registry);
        let tools = crate::chat_tools::build_chat_tools(state_for_agent.clone(), registry_arc.clone());

        let mut history_block = String::new();
        for m in &history {
            let speaker = if m.from == "user" { "User" } else { "Big Smooth" };
            history_block.push_str(&format!("{speaker}: {}\n\n", m.content));
        }
        let user_payload = if history_block.is_empty() {
            user_content.clone()
        } else {
            format!("Recent conversation history (read-only context):\n\n{history_block}---\n\nNew user message:\n\n{user_content}")
        };

        let system_prompt = include_str!("chat_tools_system_prompt.txt");
        let agent_cfg = AgentConfig::new("big-smooth-chat-stream", system_prompt, llm_config).with_max_iterations(20);
        let agent = Agent::new(agent_cfg, tools);

        // Two-pronged forward: every AgentEvent is forwarded to the
        // SSE channel, AND ToolCallStart/Complete are accumulated for
        // persistence with the assistant message. We tap the same
        // channel rather than two channels because the agent driver
        // takes a single sender.
        let (agent_tx, mut agent_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

        let captured_tools: std::sync::Arc<tokio::sync::Mutex<Vec<crate::session::SessionToolCall>>> = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let captured_drain = captured_tools.clone();
        let sse_tx_drain = sse_tx.clone();
        let drain = tokio::spawn(async move {
            while let Some(ev) = agent_rx.recv().await {
                match &ev {
                    AgentEvent::ToolCallStart { tool_name, arguments, .. } => {
                        let mut buf = captured_drain.lock().await;
                        buf.push(crate::session::SessionToolCall {
                            id: format!("tc-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
                            tool_name: tool_name.clone(),
                            arguments: arguments.clone(),
                            output: None,
                            status: "running".to_string(),
                            duration_ms: None,
                        });
                    }
                    AgentEvent::ToolCallComplete {
                        tool_name,
                        is_error,
                        result,
                        duration_ms,
                        ..
                    } => {
                        let mut buf = captured_drain.lock().await;
                        if let Some(slot) = buf.iter_mut().rev().find(|t| t.tool_name == *tool_name && t.status == "running") {
                            slot.output = Some(result.clone());
                            slot.status = if *is_error { "error" } else { "done" }.to_string();
                            slot.duration_ms = Some(*duration_ms);
                        }
                    }
                    _ => {}
                }
                // Forward to SSE — drop on receiver hangup (client
                // closed). The agent will keep running and still
                // persist the result when it finishes.
                let _ = sse_tx_drain.send(ev);
            }
        });

        let result = agent.run_with_channel(user_payload, agent_tx).await;
        // Drain task closes when its sender is dropped (above).
        let _ = drain.await;

        let tool_calls = std::sync::Arc::try_unwrap(captured_tools)
            .map(tokio::sync::Mutex::into_inner)
            .unwrap_or_else(|arc| arc.try_lock().map(|g| g.clone()).unwrap_or_default());

        match result {
            Ok(conversation) => {
                let assistant_text = conversation.last_assistant_content().unwrap_or("(no response)").to_string();
                let assistant_msg_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                let assistant_msg = crate::session::SessionMessage {
                    id: assistant_msg_id.clone(),
                    session_id: session_id.clone(),
                    from: "bigsmooth".into(),
                    to: "user".into(),
                    content: assistant_text,
                    timestamp: chrono::Utc::now(),
                    message_type: crate::session::MessageType::Response,
                    tool_calls,
                };
                if let Err(e) = state_for_agent.session_store.save_message(assistant_msg) {
                    tracing::warn!(error = %e, "failed to save assistant chat stream message");
                }
                let _ = state_for_agent.session_store.bump_message_count(&session_id, 2);
            }
            Err(e) => {
                let _ = sse_tx.send(AgentEvent::Error {
                    message: format!("chat agent: {e}"),
                });
            }
        }
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(sse_rx);
    let sse_stream = futures_util::StreamExt::map(stream, |event| {
        let data = serde_json::to_string(&event).unwrap_or_else(|_| r#"{"type":"Error","message":"serialization failed"}"#.into());
        Ok(Event::default().data(data))
    });

    Sse::new(sse_stream)
}

/// Generate a short (3–6 word) title summarizing the user's first
/// message, using the `smooth-fast` routing slot (Haiku-class). Runs
/// in a detached task so chat latency isn't gated on it. Returns
/// `None` if the slot isn't configured or the call fails — caller
/// falls back to a literal truncation of the prompt.
///
/// Trims quotes, trailing punctuation, and clamps to 60 chars so a
/// chatty model can't silently fill the UI with a paragraph.
async fn auto_name_session(user_prompt: &str) -> Option<String> {
    let providers_path = dirs_next::home_dir()?.join(".smooth/providers.json");
    let registry = load_providers_with_migration(&providers_path).ok()?;
    let cast = smooth_cast::cast::builtin();
    let agent = cast.get("tagger")?;
    let config = registry.llm_config_for(agent.slot).ok()?;
    let llm = smooth_operator::llm::LlmClient::new(config);

    let system = smooth_operator::conversation::Message::system(&agent.prompt);
    let user = smooth_operator::conversation::Message::user(user_prompt);
    let resp = llm.chat(&[&system, &user], &[]).await.ok()?;

    let cleaned = resp
        .content
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c == '\n')
        .chars()
        .take(60)
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn chat_default_model() -> String {
    let providers_path = dirs_next::home_dir().unwrap_or_default().join(".smooth/providers.json");
    load_providers_with_migration(&providers_path)
        .ok()
        .and_then(|r| r.default_llm_config().ok())
        .map(|c| c.model)
        .unwrap_or_else(|| "default".to_string())
}

/// Run an agentic chat turn for the session-bound endpoint
/// (`POST /api/chat/sessions/{id}/messages`).
///
/// Same Agent + tools as the bare `/api/chat` handler — the session
/// endpoint just adds prior-conversation context. The web UI uses this
/// endpoint, so making this agentic is what makes the chat actually
/// orchestrate (search/create pearls, spawn teammates, message them)
/// instead of returning Haiku-class one-shots.
/// Run the chat agent and return both the final assistant text and the
/// ordered list of tool calls captured along the way. Tool-call
/// persistence is what backs the web UI's tool-call timeline (pearl
/// th-880f2c) — without these we can't render the tool cards next to
/// the assistant turn.
async fn run_chat_with_history(
    state: &AppState,
    system_prompt: &str,
    history: &[crate::session::SessionMessage],
    user_content: &str,
) -> anyhow::Result<(String, Vec<crate::session::SessionToolCall>)> {
    use smooth_operator::agent::{Agent, AgentConfig, AgentEvent};

    let providers_path = dirs_next::home_dir().unwrap_or_default().join(".smooth/providers.json");
    let registry = load_providers_with_migration(&providers_path).map_err(|e| anyhow::anyhow!("no LLM providers configured: {e}"))?;

    // Coding slot (MiniMax) — fast AND tool-call-capable. See the
    // chat_handler comment for why we pick coding over fast/reasoning.
    let llm_config = registry
        .llm_config_for(smooth_operator::providers::Activity::Coding)
        .map_err(|e| anyhow::anyhow!("resolving coding slot for chat: {e}"))?;

    let registry_arc = std::sync::Arc::new(registry);
    let tools = crate::chat_tools::build_chat_tools(state.clone(), registry_arc.clone());

    // Fold prior history into a single read-only context block prepended to
    // the new user-turn. Avoids the user/assistant alternation requirement
    // some OpenAI-compat providers enforce on the live message array, and
    // keeps the agent's compaction strategy from churning on old turns.
    let mut history_block = String::new();
    for m in history {
        let speaker = if m.from == "user" { "User" } else { "Big Smooth" };
        history_block.push_str(&format!("{speaker}: {}\n\n", m.content));
    }
    let user_payload = if history_block.is_empty() {
        user_content.to_string()
    } else {
        format!("Recent conversation history (read-only context):\n\n{history_block}---\n\nNew user message:\n\n{user_content}")
    };

    // 20 iterations so the agent can spawn → wait → format without
    // running out of turns. teammate_wait absorbs the long wait into
    // one iteration so this stays responsive.
    let agent_cfg = AgentConfig::new("big-smooth-chat-session", system_prompt, llm_config).with_max_iterations(20);
    let agent = Agent::new(agent_cfg, tools);

    // Thought stream — Gemini Flash Lite summarizes each tool call /
    // assistant snippet into a one-line first-person thought and
    // broadcasts it to the chat WS so the UI can float it next to the
    // BS face. Non-blocking (Semaphore-capped) so the agent never
    // waits on the summarizer.
    let thoughts = crate::thoughts::ThoughtStreamer::new(&registry_arc, state.event_tx.clone());
    let last_action: std::sync::Arc<tokio::sync::Mutex<(String, std::time::Instant)>> =
        std::sync::Arc::new(tokio::sync::Mutex::new((String::from("thinking"), std::time::Instant::now())));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let last_action_drain = last_action.clone();
    let thoughts_drain = thoughts.clone();
    // Accumulate tool calls in a shared buffer so the chat handler can
    // persist them with the assistant message. We pair Start with
    // Complete by walking the buffer backwards looking for the first
    // still-`running` entry whose tool_name matches — chat agents in
    // this server are sequential (one tool at a time), so this is
    // unambiguous in practice. Pearl th-880f2c.
    let captured_tools: std::sync::Arc<tokio::sync::Mutex<Vec<crate::session::SessionToolCall>>> = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured_drain = captured_tools.clone();
    let drain = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ToolCallStart { tool_name, arguments, .. } => {
                    {
                        let mut la = last_action_drain.lock().await;
                        *la = (tool_name.clone(), std::time::Instant::now());
                    }
                    {
                        let mut buf = captured_drain.lock().await;
                        buf.push(crate::session::SessionToolCall {
                            id: format!("tc-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
                            tool_name: tool_name.clone(),
                            arguments,
                            output: None,
                            status: "running".to_string(),
                            duration_ms: None,
                        });
                    }
                    thoughts_drain.emit(crate::thoughts::ThoughtContext::ToolCall { tool_name });
                }
                AgentEvent::ToolCallComplete {
                    tool_name,
                    is_error,
                    result,
                    duration_ms,
                    ..
                } => {
                    {
                        let mut la = last_action_drain.lock().await;
                        la.1 = std::time::Instant::now();
                    }
                    {
                        let mut buf = captured_drain.lock().await;
                        // Match the most-recent Running entry with the
                        // same tool_name.
                        if let Some(slot) = buf.iter_mut().rev().find(|t| t.tool_name == tool_name && t.status == "running") {
                            slot.output = Some(result);
                            slot.status = if is_error { "error" } else { "done" }.to_string();
                            slot.duration_ms = Some(duration_ms);
                        }
                    }
                }
                AgentEvent::LlmResponse { content_preview, .. } if !content_preview.trim().is_empty() => {
                    thoughts_drain.emit(crate::thoughts::ThoughtContext::AssistantPreview { snippet: content_preview });
                }
                _ => {}
            }
        }
    });
    let last_action_hb = last_action.clone();
    let thoughts_hb = thoughts.clone();
    let heartbeat = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        loop {
            let (action, since) = {
                let la = last_action_hb.lock().await;
                (la.0.clone(), la.1)
            };
            let elapsed = since.elapsed().as_secs();
            if elapsed >= 8 {
                thoughts_hb.emit(crate::thoughts::ThoughtContext::Heartbeat {
                    last_action: action,
                    seconds: u32::try_from(elapsed).unwrap_or(u32::MAX),
                });
            }
            tokio::time::sleep(std::time::Duration::from_secs(11)).await;
        }
    });

    let conversation = agent.run_with_channel(user_payload, tx).await.map_err(|e| anyhow::anyhow!("chat agent: {e}"))?;
    heartbeat.abort();
    // Wait for the drain to finish processing whatever events are
    // already queued before we abort it — otherwise a tool's
    // Complete event can race with the abort and we lose its output
    // from the persisted record. The drain channel was closed when
    // the agent dropped its `tx`, so this finishes promptly.
    let _ = drain.await;

    let tool_calls = std::sync::Arc::try_unwrap(captured_tools)
        .map(tokio::sync::Mutex::into_inner)
        .unwrap_or_else(|arc| {
            // Fallback: clone out of the Arc if some other reference is
            // still held. Practically unreachable — only the drain task
            // holds the second clone and we awaited it above.
            arc.try_lock().map(|g| g.clone()).unwrap_or_default()
        });
    Ok((conversation.last_assistant_content().unwrap_or("(no response)").to_string(), tool_calls))
}

// ── Safehouse Narc — POST /api/narc/judge ─────────────────

/// Arbitrate a runtime access request escalated from a per-VM Wonk.
///
/// Wonk calls this when its local policy can't auto-approve a `/check/*`
/// request. Narc applies its rule engine, cache, and (when nothing else
/// resolves the request) LLM judge, then returns an approve / deny /
/// escalate_to_human verdict. Returns the decision directly as JSON — no
/// `ApiResponse` envelope, because Wonk speaks the raw `JudgeDecision`
/// wire format shared with `smooth-narc::judge`.
async fn narc_judge_handler(State(state): State<AppState>, Json(request): Json<smooth_narc::judge::JudgeRequest>) -> Json<smooth_narc::judge::JudgeDecision> {
    state.touch();
    let decision = state.safehouse_narc.judge(request).await;
    Json(decision)
}

// ── Access — auto-mode-style pending-request queue ────────────
//
// Wonk-the-binary today calls into SafehouseNarc; when Narc returns
// `Decision::Ask`, the call holds open inside `judge()` while a human
// resolves it via these four routes. The TUI subscribes to the SSE
// stream to render inline approval cards (pearl th-670fb2).

/// `GET /api/access/pending` — snapshot of every currently-pending
/// access request, oldest first. Returns the raw list (no
/// `ApiResponse` envelope) — the TUI consumes this directly.
async fn access_pending_handler(State(state): State<AppState>) -> Json<Vec<crate::access::PendingAccessRequest>> {
    state.touch();
    Json(state.access.list_pending())
}

#[derive(Deserialize)]
struct AccessResolveBody {
    /// The pending request id (UUID) returned in the `pending` event.
    id: String,
    /// One of `once` / `session` / `project` / `user` (case-insensitive).
    scope: String,
    /// Optional glob the human bound the approval to (e.g.
    /// `*.openai.com`). When set, Wonk caches the entire glob in its
    /// runtime allowlist instead of just the exact resource. Ignored
    /// when denying.
    #[serde(default)]
    glob_override: Option<String>,
}

/// Map a `(verdict, scope)` resolution onto the [`crate::access::AccessStore`].
/// Shared between the approve + deny handlers so we don't duplicate the
/// scope parsing + error handling.
async fn resolve_access(
    state: AppState,
    body: AccessResolveBody,
    verdict: crate::access::ResolutionVerdict,
) -> Result<Json<crate::access::AccessResolution>, (axum::http::StatusCode, String)> {
    state.touch();
    let scope = smooth_narc::judge::Scope::parse(&body.scope).ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        format!("unknown scope '{}': expected once|session|project|user", body.scope),
    ))?;
    // Capture the pending request shape BEFORE resolving — `resolve()`
    // removes the entry from the pending map, but we need its kind +
    // resource to write a persistent grant.
    let pending_snapshot = state.access.list_pending().into_iter().find(|r| r.id == body.id);
    let glob_override = body.glob_override.clone();
    let resolution = state.access.resolve(&body.id, verdict, scope, body.glob_override).map_err(|e| match e {
        crate::access::AccessError::NotFound(id) => (axum::http::StatusCode::NOT_FOUND, format!("no pending request with id {id}")),
        crate::access::AccessError::Poisoned => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "access store mutex was poisoned".to_string()),
    })?;

    // Persist Approve verdicts at project/user scope into the right
    // wonk-allow.toml AND merge into the live SharedWonkGrants so the
    // next check_persistent_grants call sees it. Once / Session never
    // touch the filesystem. Pearl th-38b72c.
    if matches!(verdict, crate::access::ResolutionVerdict::Approve) {
        if let Some(req) = pending_snapshot {
            let target_path = persistent_grant_path(&state, scope);
            if let Some(path) = target_path {
                match crate::wonk_grants::append_grant(&path, &req.kind, &req.resource, glob_override.as_deref()) {
                    Ok(()) => {
                        if let Ok(fresh) = crate::wonk_grants::WonkGrants::load_from_path(&path) {
                            state.wonk_grants.merge_in(fresh);
                        }
                        tracing::info!(
                            scope = scope.as_str(),
                            path = %path.display(),
                            kind = %req.kind,
                            resource = %req.resource,
                            "persisted permission grant"
                        );
                    }
                    Err(e) => {
                        // Don't fail the HTTP request — the
                        // resolution still took effect for the live
                        // tool call. The persistence layer is a
                        // best-effort durability boost.
                        tracing::warn!(
                            scope = scope.as_str(),
                            path = %path.display(),
                            error = %e,
                            "failed to persist permission grant"
                        );
                    }
                }
            }
        }
    }

    Ok(Json(resolution))
}

/// Pick the file path to persist a grant at, given the resolution
/// scope. Returns `None` for non-persistent scopes (Once / Session).
///
/// Project-scope grants would ideally go to the requesting pearl's
/// workspace, but the bead_id → workspace mapping isn't yet wired
/// through the access flow — for v1 we route project grants to the
/// user file so the grant still survives a restart. Refining the
/// project routing is a sub-pearl.
fn persistent_grant_path(_state: &AppState, scope: smooth_narc::judge::Scope) -> Option<std::path::PathBuf> {
    match scope {
        smooth_narc::judge::Scope::User | smooth_narc::judge::Scope::PearlProject => crate::wonk_grants::user_grants_path(),
        smooth_narc::judge::Scope::Once | smooth_narc::judge::Scope::Session => None,
    }
}

/// `POST /api/access/approve` — resolve a pending request as Approve.
/// Body: `{ id, scope, glob_override? }`.
async fn access_approve_handler(
    State(state): State<AppState>,
    Json(body): Json<AccessResolveBody>,
) -> Result<Json<crate::access::AccessResolution>, (axum::http::StatusCode, String)> {
    resolve_access(state, body, crate::access::ResolutionVerdict::Approve).await
}

/// `POST /api/access/deny` — resolve a pending request as Deny.
async fn access_deny_handler(
    State(state): State<AppState>,
    Json(body): Json<AccessResolveBody>,
) -> Result<Json<crate::access::AccessResolution>, (axum::http::StatusCode, String)> {
    resolve_access(state, body, crate::access::ResolutionVerdict::Deny).await
}

/// `GET /api/access/stream` — Server-Sent Events stream of every
/// access-store event. New subscribers should hit `/api/access/pending`
/// once on connect to catch up; this stream only sends events from the
/// subscription point forward.
///
/// Wire format: `data: <json>` for every event, where `<json>` is the
/// serde-tagged [`crate::access::AccessEvent`] form
/// (`{"event":"pending",...}` / `{"event":"resolved",...}` /
/// `{"event":"expired",...}`). The connection includes a 15s keepalive
/// so reverse proxies don't drop idle SSEs.
async fn access_stream_handler(State(state): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    use futures_util::StreamExt;
    state.touch();
    let rx = state.access.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(event) => {
                let json = serde_json::to_string(&event).ok()?;
                Some(Ok(Event::default().data(json)))
            }
            Err(_lagged) => {
                // Subscriber fell behind. They can resync via
                // `/api/access/pending`; we don't try to replay missed
                // events from the broadcast.
                None
            }
        }
    });
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)))
}

// ── Web search ─────────────────────────────────────────────
//
// Native web search backed by the DuckDuckGo HTML endpoint. Runners
// hit this through their already-allowed Big Smooth route instead of
// each sandbox carrying a TLS-capable HTTP client + outbound network
// permission for `html.duckduckgo.com`. Pearl th-70b68b.

#[derive(Deserialize)]
struct WebSearchParams {
    /// Search query. Required.
    q: Option<String>,
    /// Number of results. Clamped to [`crate::web_search::MAX_RESULTS`].
    /// Defaults to 5.
    n: Option<usize>,
    /// Run injection redaction on the results before returning.
    /// Defaults to `true` — callers that need raw text (debugging,
    /// fuzzing) can set `redact=false`.
    redact: Option<bool>,
}

#[derive(Serialize)]
struct WebSearchResponse {
    results: Vec<crate::web_search::SearchResult>,
    /// Number of injection patterns redacted. Zero on the happy path.
    redacted_count: usize,
}

async fn web_search_handler(
    State(state): State<AppState>,
    Query(params): Query<WebSearchParams>,
) -> Result<Json<WebSearchResponse>, (axum::http::StatusCode, String)> {
    state.touch();
    let query = params.q.unwrap_or_default();
    let n = params.n.unwrap_or(5);
    let redact = params.redact.unwrap_or(true);

    let results = crate::web_search::search(&query, n).await.map_err(|e| {
        let status = match &e {
            crate::web_search::SearchError::EmptyQuery => axum::http::StatusCode::BAD_REQUEST,
            // Network / Parse / BadStatus all surface as 502 — the
            // upstream is misbehaving, not the caller.
            crate::web_search::SearchError::Network { .. }
            | crate::web_search::SearchError::Parse { .. }
            | crate::web_search::SearchError::BadStatus { .. } => axum::http::StatusCode::BAD_GATEWAY,
        };
        (status, e.to_string())
    })?;

    let (final_results, redacted_count) = if redact {
        crate::web_search::redact_injections(results)
    } else {
        (results, 0)
    };

    if redacted_count > 0 {
        tracing::warn!(redacted = redacted_count, query = %query, "web_search: injection patterns redacted from results");
    }

    Ok(Json(WebSearchResponse {
        results: final_results,
        redacted_count,
    }))
}

// ── Search ─────────────────────────────────────────────────

async fn search_handler(State(state): State<AppState>, Query(params): Query<SearchParams>) -> Json<ApiResponse<Vec<crate::search::SearchResult>>> {
    state.touch();
    let query = params.q.unwrap_or_default();
    if query.is_empty() {
        return Json(ApiResponse { data: vec![], ok: true });
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let results = crate::search::search_all(&query, &cwd, &state.pearl_store);
    Json(ApiResponse { data: results, ok: true })
}

// ── Steering ───────────────────────────────────────────────

async fn pause_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Pause operator on {bead_id}");
    let _ = state.pearl_store.add_comment(&bead_id, "[STEERING:PAUSE] Operator paused by human.");
    Json(ApiResponse {
        data: "paused".into(),
        ok: true,
    })
}

async fn resume_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Resume operator on {bead_id}");
    let _ = state.pearl_store.add_comment(&bead_id, "[STEERING:RESUME] Operator resumed.");
    Json(ApiResponse {
        data: "resumed".into(),
        ok: true,
    })
}

async fn steer_handler(State(state): State<AppState>, Path(bead_id): Path<String>, Json(body): Json<SteerBody>) -> Json<ApiResponse<String>> {
    state.touch();
    let msg = body.message.unwrap_or_default();
    tracing::info!("Steer operator on {bead_id}: {msg}");
    let _ = state.pearl_store.add_comment(&bead_id, &format!("[STEERING:GUIDANCE] {msg}"));
    Json(ApiResponse {
        data: "steered".into(),
        ok: true,
    })
}

async fn cancel_handler(State(state): State<AppState>, Path(bead_id): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    tracing::info!("Cancel operator on {bead_id}");
    let _ = state.pearl_store.add_comment(&bead_id, "[STEERING:CANCEL] Operator cancelled.");
    Json(ApiResponse {
        data: "cancelled".into(),
        ok: true,
    })
}

// ── Teammates (Phase 4) ────────────────────────────────────

#[derive(Deserialize)]
pub struct PostTeammateMessageBody {
    content: String,
}

async fn list_teammates_handler(State(state): State<AppState>) -> Json<ApiResponse<Vec<crate::teammates::TeammateView>>> {
    state.touch();
    let mut list = state.teammates.list().await;
    list.sort_by_key(|t| std::cmp::Reverse(t.last_event_at));
    Json(ApiResponse { data: list, ok: true })
}

async fn get_teammate_messages_handler(State(state): State<AppState>, Path(name): Path<String>) -> Json<ApiResponse<Vec<crate::session::SessionMessage>>> {
    state.touch();
    let Some(view) = state.teammates.get(&name).await else {
        return Json(ApiResponse { data: Vec::new(), ok: false });
    };
    // Return the pearl's recent comments cast as session-message-shaped
    // records so the chat panel can render them uniformly.
    let comments = state.pearl_store.get_comments(&view.pearl_id).unwrap_or_default();
    let msgs: Vec<crate::session::SessionMessage> = comments
        .into_iter()
        .map(|c| crate::session::SessionMessage {
            id: c.id,
            session_id: view.pearl_id.clone(),
            from: actor_for_comment(&c.content).to_string(),
            to: "user".to_string(),
            content: c.content,
            message_type: crate::session::MessageType::Response,
            timestamp: c.created_at,
            tool_calls: Vec::new(),
        })
        .collect();
    Json(ApiResponse { data: msgs, ok: true })
}

async fn post_teammate_message_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PostTeammateMessageBody>,
) -> Json<ApiResponse<String>> {
    state.touch();
    let Some(view) = state.teammates.get(&name).await else {
        return Json(ApiResponse {
            data: format!("teammate {name} not found"),
            ok: false,
        });
    };
    let comment = format!("[CHAT:USER] {}", body.content);
    if let Err(e) = state.pearl_store.add_comment(&view.pearl_id, &comment) {
        return Json(ApiResponse {
            data: format!("posting message failed: {e}"),
            ok: false,
        });
    }
    Json(ApiResponse {
        data: "Message queued for the teammate.".into(),
        ok: true,
    })
}

async fn shutdown_teammate_handler(State(state): State<AppState>, Path(name): Path<String>) -> Json<ApiResponse<String>> {
    state.touch();
    let Some(view) = state.teammates.get(&name).await else {
        return Json(ApiResponse {
            data: format!("teammate {name} not found"),
            ok: false,
        });
    };
    let _ = state
        .pearl_store
        .add_comment(&view.pearl_id, "[STEERING:SHUTDOWN] graceful shutdown requested by user");
    state.teammates.mark_status(&name, "ended").await;
    Json(ApiResponse {
        data: "shutdown requested".into(),
        ok: true,
    })
}

fn actor_for_comment(body: &str) -> &'static str {
    let t = body.trim_start();
    if t.starts_with("[CHAT:USER]") {
        "user"
    } else if t.starts_with("[CHAT:TEAMMATE]") || t.starts_with("[PROGRESS]") || t.starts_with("[QUESTION:TEAMMATE") || t.starts_with("[IDLE]") {
        "teammate"
    } else if t.starts_with("[STEERING:") || t.starts_with("[ANSWER:") {
        "lead"
    } else {
        "system"
    }
}

// ── Jira ───────────────────────────────────────────────────

async fn jira_status_handler(State(state): State<AppState>) -> Json<ApiResponse<crate::jira::SyncStatus>> {
    state.touch();
    let config = crate::jira::JiraConfig::from_pearl_store(&state.pearl_store);
    let connected = if let Some(ref c) = config {
        crate::jira::check_connection(c).await
    } else {
        false
    };
    Json(ApiResponse {
        data: crate::jira::SyncStatus {
            connected,
            last_sync: None,
            pending_changes: 0,
        },
        ok: true,
    })
}

// ── Delegation ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DelegateRequest {
    /// The operator requesting delegation.
    pub parent_operator_id: String,
    /// The task to delegate.
    pub task: String,
    /// Optional model override; if absent the orchestrator picks one.
    pub model: Option<String>,
}

#[derive(Serialize)]
pub struct DelegateResponse {
    pub delegation_id: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct DelegateStatusResponse {
    pub delegation_id: String,
    pub status: String,
    /// Last comment on the pearl, if completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

async fn delegate_handler(State(state): State<AppState>, Json(body): Json<DelegateRequest>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();

    // 1. Create a sub-pearl (subtask type) linked to the parent operator.
    let title = format!("[delegated] {}", truncate_str(&body.task, 80));
    let pearl = match crate::pearls::create_pearl(&state.pearl_store, &title, &body.task, "subtask", 1) {
        Ok(p) => p,
        Err(e) => {
            return Json(ApiResponse {
                data: serde_json::json!({"error": e.to_string()}),
                ok: false,
            });
        }
    };
    let pearl_id = pearl.id.clone();

    // 2. Leave as Open so the orchestrator's `ready()` picks it up on the
    //    next scheduling cycle. The orchestrator will transition it to
    //    InProgress when it dispatches an operator.

    // 3. Add a comment noting delegation origin.
    let comment = format!(
        "[DELEGATION] Delegated by operator {} | model: {}",
        body.parent_operator_id,
        body.model.as_deref().unwrap_or("inherit")
    );
    let _ = state.pearl_store.add_comment(&pearl_id, &comment);

    // 4. Notify the orchestrator so it can schedule dispatch.
    {
        let mut orch = state.orchestrator.lock().await;
        orch.nudge();
    }

    let resp = DelegateResponse {
        delegation_id: pearl_id,
        status: "dispatched".into(),
    };
    Json(ApiResponse {
        data: serde_json::to_value(resp).unwrap_or(serde_json::json!(null)),
        ok: true,
    })
}

async fn delegate_status_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    state.touch();

    // Look up the pearl.
    let pearl = match crate::pearls::get_pearl(&state.pearl_store, &id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Json(ApiResponse {
                data: serde_json::json!({"error": "delegation not found"}),
                ok: false,
            });
        }
        Err(e) => {
            return Json(ApiResponse {
                data: serde_json::json!({"error": e.to_string()}),
                ok: false,
            });
        }
    };

    let (status_str, result) = match pearl.status {
        smooth_pearls::PearlStatus::Closed => {
            // Grab the last comment as the result.
            let last_comment = state
                .pearl_store
                .get_comments(&id)
                .ok()
                .and_then(|comments| comments.last().map(|c| c.content.clone()));
            ("completed".to_string(), last_comment)
        }
        smooth_pearls::PearlStatus::InProgress => ("in_progress".to_string(), None),
        smooth_pearls::PearlStatus::Open => ("in_progress".to_string(), None),
        smooth_pearls::PearlStatus::Deferred => ("failed".to_string(), None),
    };

    let resp = DelegateStatusResponse {
        delegation_id: id,
        status: status_str,
        result,
    };
    Json(ApiResponse {
        data: serde_json::to_value(resp).unwrap_or(serde_json::json!(null)),
        ok: true,
    })
}

async fn jira_sync_handler(State(state): State<AppState>) -> Json<ApiResponse<crate::jira::SyncResult>> {
    state.touch();
    Json(ApiResponse {
        data: crate::jira::SyncResult {
            pulled: 0,
            pushed: 0,
            conflicts: 0,
        },
        ok: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[test]
    fn is_temp_path_matches_cross_platform_roots_th_8bfbf4() {
        // (path, expected_is_temp)
        let cases: &[(&str, bool)] = &[
            ("/var/folders/xy/abc/T/proj", true),       // macOS tempdir
            ("/private/var/folders/xy/abc/proj", true), // macOS canonical
            ("/tmp/proj", true),                        // Linux tempdir
            ("/private/tmp/proj", true),                // macOS /tmp alias
            ("/run/user/1000/tmp/proj", true),          // Linux per-user runtime
            ("/tmpfoo/proj", false),                    // not a temp root (no false prefix match)
            ("/Users/brent/dev/smooai/smooth", false),  // real project
            ("/home/ci/work/repo", false),              // real project
        ];
        for (path, expected) in cases {
            assert_eq!(is_temp_path(std::path::Path::new(path)), *expected, "is_temp_path({path:?})");
        }

        // Whatever the OS reports as the system tempdir is always a temp root.
        let sys = std::env::temp_dir().join("some-phantom-project");
        assert!(is_temp_path(&sys), "system temp_dir child should be temp: {sys:?}");
    }

    #[test]
    fn cargo_bin_native_operative_prefers_install_root_th_92dac3() {
        // Pearl th-92dac3: `pnpm install:th`'s new
        // `cargo install --path crates/smooth-operative` step
        // drops the native binary at $CARGO_INSTALL_ROOT/bin
        // (or $CARGO_HOME/bin, or ~/.cargo/bin). The cwd-walk
        // lookup misses it when `th up` runs from outside the
        // smooth repo. This pure helper covers that path.
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let bin = bin_dir.join("smooth-operative");
        std::fs::write(&bin, b"x").unwrap();

        let install_root = tmp.path().to_str().unwrap();
        let found = cargo_bin_native_operative(Some(install_root), None, None);
        assert_eq!(found.as_deref(), Some(bin.as_path()));
    }

    #[test]
    fn cargo_bin_native_operative_falls_back_through_cargo_home_then_home_th_92dac3() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let bin = bin_dir.join("smooth-operative");
        std::fs::write(&bin, b"x").unwrap();

        // CARGO_INSTALL_ROOT unset → use CARGO_HOME
        let cargo_home = tmp.path().to_str().unwrap();
        let found = cargo_bin_native_operative(None, Some(cargo_home), None);
        assert_eq!(found.as_deref(), Some(bin.as_path()));

        // Both env vars unset → use home + ".cargo/bin"
        let tmp2 = tempfile::tempdir().expect("tempdir");
        let cargo_bin_dir = tmp2.path().join(".cargo").join("bin");
        std::fs::create_dir_all(&cargo_bin_dir).unwrap();
        let bin2 = cargo_bin_dir.join("smooth-operative");
        std::fs::write(&bin2, b"x").unwrap();

        let found = cargo_bin_native_operative(None, None, Some(tmp2.path()));
        assert_eq!(found.as_deref(), Some(bin2.as_path()));
    }

    #[test]
    fn cargo_bin_native_operative_none_when_binary_missing_th_92dac3() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join("bin")).unwrap();
        // bin dir exists but no smooth-operative binary inside it

        let install_root = tmp.path().to_str().unwrap();
        assert_eq!(cargo_bin_native_operative(Some(install_root), None, None), None);
    }

    #[test]
    fn cargo_bin_native_operative_none_when_all_inputs_none_th_92dac3() {
        assert_eq!(cargo_bin_native_operative(None, None, None), None);
    }

    #[test]
    fn resolve_direct_dispatch_narc_url_prefers_explicit_narc() {
        let got = resolve_direct_dispatch_narc_url(Some("http://narc.example/x"), Some("http://bs.example"));
        assert_eq!(got, "http://narc.example/x");
    }

    #[test]
    fn resolve_direct_dispatch_narc_url_falls_back_to_bigsmooth() {
        let got = resolve_direct_dispatch_narc_url(None, Some("http://bs.example"));
        assert_eq!(got, "http://bs.example");
    }

    #[test]
    fn resolve_direct_dispatch_narc_url_default_is_localhost() {
        let got = resolve_direct_dispatch_narc_url(None, None);
        assert_eq!(got, "http://127.0.0.1:4400");
    }

    #[test]
    fn resolve_direct_dispatch_narc_url_treats_empty_as_unset() {
        // Common shell footgun: `export SMOOTH_NARC_URL=` without a
        // value. The empty string should NOT win — fall through.
        let got = resolve_direct_dispatch_narc_url(Some(""), Some("http://bs.example"));
        assert_eq!(got, "http://bs.example");
        // Whitespace-only is also treated as unset.
        let got = resolve_direct_dispatch_narc_url(Some("   "), Some("http://bs.example"));
        assert_eq!(got, "http://bs.example");
        // Same precedence for the bigsmooth override.
        let got = resolve_direct_dispatch_narc_url(None, Some(""));
        assert_eq!(got, "http://127.0.0.1:4400");
    }

    #[test]
    fn find_native_operative_finds_debug_or_release_build() {
        // The direct-dispatch path needs a runner binary built for
        // the host triple. We don't assert which profile — CI or
        // dev boxes could have either — but at least one must
        // exist after a normal `cargo build -p smooai-smooth-operative`.
        //
        // This test runs inside cargo, which means the workspace
        // has been built; skip gracefully if the binary happens to
        // be missing (e.g. running on an alternate target dir).
        if let Some(p) = find_native_operative_binary() {
            assert!(p.is_file(), "returned path must point at a real file: {}", p.display());
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            assert_eq!(name, "smooth-operative", "must be the runner binary, got: {name}");
            // Must be under target/<profile>/, not the
            // aarch64-unknown-linux-musl cross-compile output
            // (that one is for sandboxed dispatch).
            let path_str = p.to_string_lossy();
            assert!(
                path_str.contains("/target/release/") || path_str.contains("/target/debug/"),
                "native path should be target/release or target/debug, got: {path_str}"
            );
            assert!(
                !path_str.contains("aarch64-unknown-linux-musl"),
                "native path must NOT be the cross-compiled runner, got: {path_str}"
            );
        }
    }

    #[test]
    fn project_cache_key_is_stable_and_distinguishes_paths() {
        let a = project_cache_key("/Users/me/dev/budgeting").unwrap();
        let b = project_cache_key("/Users/me/dev/budgeting").unwrap();
        assert_eq!(a, b, "same input → same key");
        assert!(a.starts_with("budgeting-"), "key leads with basename: {a}");

        // Sibling paths with the same basename get different suffixes.
        let a = project_cache_key("/home/alice/apps/web").unwrap();
        let b = project_cache_key("/home/bob/apps/web").unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("web-"));
        assert!(b.starts_with("web-"));

        // Weird chars collapsed so the key is filesystem-safe.
        let k = project_cache_key("/tmp/my project (copy)").unwrap();
        assert!(
            k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'),
            "unsafe char in {k}"
        );

        // Empty / whitespace → None.
        assert!(project_cache_key("").is_none());
        assert!(project_cache_key("   ").is_none());
    }

    #[test]
    fn max_sandbox_concurrency_env_override() {
        // Each sub-case uses a unique env var name via std::env isolation.
        // Set a valid numeric value.
        std::env::set_var("SMOOTH_SANDBOX_MAX_CONCURRENCY", "7");
        assert_eq!(max_sandbox_concurrency(), 7);

        // Zero is treated as unset → default.
        std::env::set_var("SMOOTH_SANDBOX_MAX_CONCURRENCY", "0");
        assert_eq!(max_sandbox_concurrency(), DEFAULT_SANDBOX_MAX_CONCURRENCY);

        // Garbage falls back to default.
        std::env::set_var("SMOOTH_SANDBOX_MAX_CONCURRENCY", "not-a-number");
        assert_eq!(max_sandbox_concurrency(), DEFAULT_SANDBOX_MAX_CONCURRENCY);

        // Unset falls back to default.
        std::env::remove_var("SMOOTH_SANDBOX_MAX_CONCURRENCY");
        assert_eq!(max_sandbox_concurrency(), DEFAULT_SANDBOX_MAX_CONCURRENCY);
    }

    #[test]
    fn test_health_response_serializes() {
        let resp = HealthResponse {
            ok: true,
            service: "test".into(),
            version: "0.1.0".into(),
            uptime: 42.0,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"uptime\":42.0"));
    }

    #[test]
    fn test_api_response_serializes() {
        let resp = ApiResponse {
            data: vec!["a", "b"],
            ok: true,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("[\"a\",\"b\"]"));
    }

    #[tokio::test]
    async fn test_router_builds() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };
        let state = AppState::new(pearl_store);
        let _router = build_router(state);
        // If we get here without panic, the router is valid
    }

    #[test]
    fn test_app_state_seeds_host_token() {
        // pearl th-87dfee: the host-tool bearer must live on AppState
        // (not a mutated process env var). A freshly built state with no
        // inherited SMOOTH_HOST_TOKEN gets a generated 32-char token, and
        // separate states get distinct tokens (per-process generation).
        let tmp = tempfile::tempdir().unwrap();
        let Ok(store_a) = smooth_pearls::PearlStore::init(&tmp.path().join("a")) else {
            return;
        };
        let Ok(store_b) = smooth_pearls::PearlStore::init(&tmp.path().join("b")) else {
            return;
        };
        let a = AppState::new(store_a);
        let b = AppState::new(store_b);
        assert_eq!(a.host_token.len(), 32, "uuid-simple token is 32 hex chars");
        assert!(a.host_token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.host_token, b.host_token, "each process gets a distinct token");
    }

    #[test]
    fn test_app_state_touch_updates_activity() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };
        let state = AppState::new(pearl_store);

        let before = *state.last_activity.lock().unwrap();
        std::thread::sleep(Duration::from_millis(10));
        state.touch();
        let after = *state.last_activity.lock().unwrap();
        assert!(after > before);
    }

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_long() {
        let result = truncate_str("this is a very long message that needs truncation", 20);
        assert!(result.len() <= 20);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_task_request_deserializes() {
        let json = r#"{"message":"Build X","model":"kimi-k2.5","budget":2.0}"#;
        let req: TaskRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.message, "Build X");
        assert_eq!(req.model.as_deref(), Some("kimi-k2.5"));
        assert_eq!(req.budget, Some(2.0));
        assert!(req.working_dir.is_none());
    }

    #[test]
    fn test_task_request_minimal() {
        let json = r#"{"message":"Do something"}"#;
        let req: TaskRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.message, "Do something");
        assert!(req.model.is_none());
        assert!(req.budget.is_none());
        assert!(req.working_dir.is_none());
    }

    // ── Delegation tests ──────────────────────────────────────

    #[test]
    fn test_delegate_request_deserializes() {
        let json = r#"{"parent_operator_id":"op-123","task":"Write unit tests","model":"kimi-k2.5"}"#;
        let req: DelegateRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.parent_operator_id, "op-123");
        assert_eq!(req.task, "Write unit tests");
        assert_eq!(req.model.as_deref(), Some("kimi-k2.5"));
    }

    #[test]
    fn test_delegate_request_minimal() {
        let json = r#"{"parent_operator_id":"op-1","task":"Do something"}"#;
        let req: DelegateRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.parent_operator_id, "op-1");
        assert_eq!(req.task, "Do something");
        assert!(req.model.is_none());
    }

    #[test]
    fn test_delegate_response_serializes() {
        let resp = DelegateResponse {
            delegation_id: "th-abc123".into(),
            status: "dispatched".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"delegation_id\":\"th-abc123\""));
        assert!(json.contains("\"status\":\"dispatched\""));
    }

    #[test]
    fn test_delegate_status_response_completed() {
        let resp = DelegateStatusResponse {
            delegation_id: "th-abc123".into(),
            status: "completed".into(),
            result: Some("All tests pass.".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"completed\""));
        assert!(json.contains("All tests pass."));
    }

    #[test]
    fn test_delegate_status_response_in_progress_no_result() {
        let resp = DelegateStatusResponse {
            delegation_id: "th-xyz789".into(),
            status: "in_progress".into(),
            result: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"in_progress\""));
        // result field should be absent (skip_serializing_if = None)
        assert!(!json.contains("\"result\""));
    }

    #[tokio::test]
    async fn test_delegate_endpoint_creates_pearl() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return; // Dolt binary not available, skip
        };
        let state = AppState::new(pearl_store);
        let app = build_router(state.clone());

        let body = serde_json::json!({
            "parent_operator_id": "op-test",
            "task": "Write unit tests for the auth module"
        });

        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/delegate")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["data"]["status"], "dispatched");
        let delegation_id = resp["data"]["delegation_id"].as_str().unwrap();
        assert!(delegation_id.starts_with("th-"), "pearl ID should start with th-");

        // Verify the pearl was created in the store
        let pearl = crate::pearls::get_pearl(&state.pearl_store, delegation_id)
            .unwrap()
            .expect("pearl should exist");
        assert!(pearl.title.contains("[delegated]"));
        assert_eq!(pearl.status, smooth_pearls::PearlStatus::Open);
    }

    #[tokio::test]
    async fn test_delegate_status_endpoint_returns_status() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };

        // Create a pearl directly to check status.
        let pearl = crate::pearls::create_pearl(&pearl_store, "test delegation", "test", "subtask", 1).unwrap();
        let pearl_id = pearl.id.clone();

        let state = AppState::new(pearl_store);
        let app = build_router(state.clone());

        // Check status — should be in_progress (Open maps to in_progress).
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(format!("/api/delegate/{pearl_id}/status"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["data"]["status"], "in_progress");
        assert_eq!(resp["data"]["delegation_id"], pearl_id);

        // Now close the pearl and check again.
        let _ = state.pearl_store.close(&[&pearl_id]);
        let app2 = build_router(state);
        let response2 = app2
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(format!("/api/delegate/{pearl_id}/status"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes2 = axum::body::to_bytes(response2.into_body(), usize::MAX).await.unwrap();
        let resp2: serde_json::Value = serde_json::from_slice(&body_bytes2).unwrap();
        assert_eq!(resp2["data"]["status"], "completed");
    }

    #[tokio::test]
    async fn test_delegate_status_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let Ok(pearl_store) = smooth_pearls::PearlStore::init(&tmp.path().join("dolt")) else {
            return;
        };
        let state = AppState::new(pearl_store);
        let app = build_router(state);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/delegate/th-nonexistent/status")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(resp["ok"], false);
        assert!(resp["data"]["error"].as_str().unwrap().contains("not found"));
    }

    // ===== pearl th-7b95ef: runner-stdout classification =====
    //
    // The dispatch loop used to forward any non-JSON line on the
    // runner's stdout as a `ServerEvent::TokenDelta`, which was then
    // persisted as `role: assistant` chat content. That made every
    // session JSON file open with multi-KB blobs of `[runner]
    // SMOOTH_POLICY_FILE env var not set` etc. — see
    // `~/.smooth/coding-sessions/08f084fc-…json` from the smoking
    // gun. These tests pin the new contract: non-JSON stdout is
    // classified `NonJson` and the caller must drop it.

    #[test]
    fn classify_runner_stdout_line_recognizes_valid_agent_event_json() {
        // Realistic AgentEvent the runner emits constantly.
        let line = r#"{"type":"TokenDelta","content":"hello"}"#;
        assert_eq!(classify_runner_stdout_line(line), RunnerStdoutLine::Json);
    }

    #[test]
    fn classify_runner_stdout_line_recognizes_json_with_surrounding_whitespace() {
        let line = "  {\"type\":\"Started\",\"agent_id\":\"a\"}  ";
        assert_eq!(classify_runner_stdout_line(line), RunnerStdoutLine::Json);
    }

    #[test]
    fn classify_runner_stdout_line_treats_empty_as_empty() {
        assert_eq!(classify_runner_stdout_line(""), RunnerStdoutLine::Empty);
        assert_eq!(classify_runner_stdout_line("   "), RunnerStdoutLine::Empty);
        assert_eq!(classify_runner_stdout_line("\t\n"), RunnerStdoutLine::Empty);
    }

    #[test]
    fn classify_runner_stdout_line_flags_bracket_runner_prefix_as_non_json() {
        // The exact regression that motivated pearl th-7b95ef. This
        // text used to be wrapped as TokenDelta and persisted as
        // assistant content. The new contract is: drop with a warn.
        let line = "[runner] SMOOTH_POLICY_FILE env var not set";
        assert_eq!(classify_runner_stdout_line(line), RunnerStdoutLine::NonJson);

        let line = "[runner] loaded policy: phase=execute, allowed_domains=[openrouter.ai], total_rules=5";
        assert_eq!(classify_runner_stdout_line(line), RunnerStdoutLine::NonJson);

        let line = "[runner] active role: fixer (slot=Coding, allow=*, deny=-)";
        assert_eq!(classify_runner_stdout_line(line), RunnerStdoutLine::NonJson);
    }

    #[test]
    fn classify_runner_stdout_line_flags_arbitrary_text_as_non_json() {
        // Stray `println!("debug: hit hot path")` style noise must NOT
        // be forwarded — it would land in the chat transcript.
        assert_eq!(classify_runner_stdout_line("debug: hit hot path"), RunnerStdoutLine::NonJson);
        // Partial JSON (common print-debugging mistake) is also dropped.
        assert_eq!(classify_runner_stdout_line(r#"{"type":"Token"#), RunnerStdoutLine::NonJson);
    }

    #[test]
    fn classify_runner_stdout_line_accepts_completed_event_with_metrics() {
        // Smoke-test the most stat-heavy event the runner emits so we
        // don't regress on serde-json strictness later.
        let line = r#"{"type":"Completed","agent_id":"op-1","iterations":3,"cost_usd":0.04,"prompt_tokens":1024,"completion_tokens":256,"cached_tokens":512}"#;
        assert_eq!(classify_runner_stdout_line(line), RunnerStdoutLine::Json);
    }
}
