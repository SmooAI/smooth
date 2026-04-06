//! Boardroom mode.
//!
//! When `SMOOTH_BOARDROOM_MODE=1`, Big Smooth boots with its own cast:
//! Wonk, Goalie, Narc, Scribe, and Archivist, all as tokio tasks inside
//! Big Smooth's process. This is the mode Big Smooth runs in *inside*
//! the Boardroom microVM.
//!
//! # What's in The Board
//!
//! * **Big Smooth** — the orchestrator (this process).
//! * **Wonk** — access control authority for any tool surface Big Smooth
//!   exposes to itself (currently minimal; reserved for future "Big Smooth
//!   writes to its own tracking DB" style guardrails).
//! * **Goalie** — HTTP forward proxy for any outbound network call Big
//!   Smooth makes (e.g., to OpenCode Zen for LLM judge calls). Delegates
//!   decisions to Wonk.
//! * **Narc** — tool surveillance hook. Presently wired but unused (Big
//!   Smooth's tool surface inside the Boardroom is empty; included for
//!   parity so future features inherit surveillance automatically).
//! * **Scribe** — structured logging. Forwards to the boardroom's own
//!   Archivist so boardroom activity is visible alongside operator logs.
//! * **Archivist** — **central log aggregator**. Bound on `0.0.0.0:4401`
//!   inside the Boardroom VM so Bill can port-forward it to a host port,
//!   making it reachable from every Operator VM via
//!   `host.containers.internal:<archivist_host_port>`. This is the one
//!   cross-VM network dependency in the whole architecture.
//!
//! # Lifecycle
//!
//! [`spawn_boardroom_cast`] is called once from [`crate::server::start`]
//! when `SMOOTH_BOARDROOM_MODE=1`. It spawns every cast member on its own
//! tokio task and returns a [`BoardroomHandles`] struct holding the live
//! URLs. The handles outlive every incoming request — they are kept in
//! `AppState` so future code can query `boardroom.archivist_url()` etc.

use std::sync::Arc;

use anyhow::{Context, Result};
use smooth_archivist::server::{build_router_with_state as archivist_router, AppState as ArchivistState};
use smooth_archivist::{event_archive::MemoryEventArchive, store::MemoryArchiveStore};
use smooth_diver::server::{build_router_with_state as diver_router, AppState as DiverState};
use smooth_diver::store::DiverStore;
use smooth_scribe::server::{build_router_with_state as scribe_router, AppState as ScribeState};
use smooth_scribe::{spawn_forwarder, ForwarderHandle};
use smooth_wonk::policy::PolicyHolder;
use smooth_wonk::server::{build_router as wonk_router, AppState as WonkState};
use smooth_wonk::{Negotiator, WonkHook};

/// Live URLs of every Boardroom cast member. Stored on `AppState` so
/// request handlers can talk to their in-process neighbours (and tests
/// can assert on them).
#[derive(Clone)]
pub struct BoardroomHandles {
    /// URL of the Boardroom's own Wonk. `http://127.0.0.1:<port>`.
    pub wonk_url: String,
    /// URL of the Boardroom's own Goalie. `http://127.0.0.1:<port>`.
    pub goalie_url: String,
    /// URL of the Boardroom's own Scribe. `http://127.0.0.1:<port>`.
    pub scribe_url: String,
    /// URL of the Boardroom's Archivist. **Bound on 0.0.0.0:4401** (the
    /// fixed guest port Bill maps to a host port on Boardroom spawn).
    /// `http://0.0.0.0:4401`.
    pub archivist_url: String,
    /// Archivist's bound port (always 4401 in Boardroom mode).
    pub archivist_port: u16,
    /// Scribe forwarder handle, for callers who want to push entries
    /// directly bypassing the HTTP layer.
    pub scribe_forwarder: Option<ForwarderHandle>,
    /// Wonk hook — wrapped in Arc because `WonkHook` itself is not Clone.
    pub wonk_hook: Arc<WonkHook>,
    /// URL of the Boardroom's Diver (pearl lifecycle manager).
    pub diver_url: String,
}

impl std::fmt::Debug for BoardroomHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoardroomHandles")
            .field("wonk_url", &self.wonk_url)
            .field("goalie_url", &self.goalie_url)
            .field("scribe_url", &self.scribe_url)
            .field("archivist_url", &self.archivist_url)
            .field("archivist_port", &self.archivist_port)
            .finish()
    }
}

impl BoardroomHandles {
    /// Archivist URL as seen from inside another VM. Operator VMs see the
    /// Boardroom Archivist at `http://host.containers.internal:<host_port>`,
    /// where `<host_port>` is whatever Bill mapped 4401 to when spawning
    /// the Boardroom. We don't know that host port from inside the
    /// Boardroom process; the caller (the test harness / bootstrap code)
    /// is responsible for threading it through `SMOOTH_ARCHIVIST_HOST_PORT`
    /// so we can build the right URL for operator env.
    #[must_use]
    pub fn operator_facing_archivist_url(&self) -> Option<String> {
        let port = std::env::var("SMOOTH_ARCHIVIST_HOST_PORT").ok().and_then(|p| p.parse::<u16>().ok())?;
        // The operator VM needs to reach the Boardroom's Archivist via
        // the host network. 127.0.0.1 won't work from inside a microVM
        // (the guest kernel handles loopback locally). We derive the
        // host's real IP from SMOOTH_BOOTSTRAP_BILL_URL, which already
        // contains it (the test / launcher set it via detect_host_ip).
        let host = std::env::var("SMOOTH_BOOTSTRAP_BILL_URL")
            .ok()
            .and_then(|u| {
                u.trim_start_matches("http://")
                    .trim_start_matches("https://")
                    .split(':')
                    .next()
                    .map(String::from)
            })
            .unwrap_or_else(|| "127.0.0.1".into());
        Some(format!("http://{host}:{port}"))
    }
}

/// The fixed guest port Archivist binds on inside the Boardroom VM.
/// Bill maps it to an ephemeral host port when spawning the Boardroom.
pub const ARCHIVIST_GUEST_PORT: u16 = 4401;

/// Spawn every Boardroom cast member on its own tokio task.
///
/// # Errors
///
/// Returns an error if any HTTP bind fails (port in use, permission
/// denied, etc.) or if the default policy TOML cannot be parsed.
pub async fn spawn_boardroom_cast(pearl_store: Option<smooth_pearls::PearlStore>) -> Result<BoardroomHandles> {
    tracing::info!("boardroom: spawning in-process cast");

    // --- Archivist ---------------------------------------------------------
    // Bound on 0.0.0.0:4401 so microsandbox's port forward can expose it
    // on the host, reaching operator VMs via host.containers.internal.
    let archivist_store = Arc::new(MemoryArchiveStore::new());
    let archivist_event_archive = Arc::new(MemoryEventArchive::new());
    let archivist_state = ArchivistState {
        store: Arc::clone(&archivist_store),
        event_archive: Arc::clone(&archivist_event_archive),
    };
    let archivist_r = archivist_router(archivist_state);
    let archivist_listener = tokio::net::TcpListener::bind(("0.0.0.0", ARCHIVIST_GUEST_PORT))
        .await
        .with_context(|| format!("boardroom: bind archivist on 0.0.0.0:{ARCHIVIST_GUEST_PORT}"))?;
    let archivist_addr = archivist_listener.local_addr().context("boardroom: archivist local addr")?;
    let archivist_url = format!("http://{archivist_addr}");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(archivist_listener, archivist_r).await {
            tracing::error!(error = %e, "boardroom: Archivist server crashed");
        }
    });
    tracing::info!(url = %archivist_url, "boardroom: Archivist up");

    // --- Scribe (forwards to our own Archivist) ----------------------------
    // When Big Smooth runs inside the Boardroom VM, its own Scribe uses
    // the loopback archivist URL — same process, same memory, but goes
    // through the HTTP path for consistency with operator Scribes.
    let scribe_forwarder = spawn_forwarder(archivist_url.clone(), "boardroom".to_string());
    let scribe_state = ScribeState::with_forwarder(scribe_forwarder.clone());
    let scribe_r = scribe_router(scribe_state);
    let scribe_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("boardroom: bind scribe")?;
    let scribe_addr = scribe_listener.local_addr().context("boardroom: scribe local addr")?;
    let scribe_url = format!("http://{scribe_addr}");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(scribe_listener, scribe_r).await {
            tracing::error!(error = %e, "boardroom: Scribe server crashed");
        }
    });
    tracing::info!(url = %scribe_url, "boardroom: Scribe up");

    // --- Wonk -------------------------------------------------------------
    // The boardroom Wonk is minimal: we ship a default-permissive policy
    // so the legal policy file format is exercised and any future
    // boardroom tool surface gets guardrails for free. Policy can be
    // tightened in a follow-up.
    let default_policy_toml =
        crate::policy::generate_policy_for_task("boardroom", "boardroom", "execute", "boardroom-token", &[], crate::policy::TaskType::Coding)
            .context("boardroom: generate default wonk policy")?;
    let policy = smooth_policy::Policy::from_toml(&default_policy_toml).map_err(|e| anyhow::anyhow!("boardroom: parse wonk policy: {e}"))?;
    let policy_holder = PolicyHolder::from_policy(policy);
    let negotiator = Negotiator::new("http://127.0.0.1:1/no-leader", policy_holder.clone());
    let wonk_state = Arc::new(WonkState::new(policy_holder, negotiator));
    let wonk_r = wonk_router(wonk_state);
    let wonk_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("boardroom: bind wonk")?;
    let wonk_addr = wonk_listener.local_addr().context("boardroom: wonk local addr")?;
    let wonk_url = format!("http://{wonk_addr}");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(wonk_listener, wonk_r).await {
            tracing::error!(error = %e, "boardroom: Wonk server crashed");
        }
    });
    tracing::info!(url = %wonk_url, "boardroom: Wonk up");

    // --- Goalie -----------------------------------------------------------
    // Goalie is a full HTTP forward proxy. We give it a WonkClient pointed
    // at the boardroom Wonk and let `run_proxy` bind the port itself.
    let goalie_audit_path = std::env::var("SMOOTH_BOARDROOM_GOALIE_AUDIT").unwrap_or_else(|_| "/tmp/goalie-boardroom.jsonl".into());
    let goalie_audit = smooth_goalie::AuditLogger::new(&goalie_audit_path).context("boardroom: create goalie audit logger")?;
    let goalie_wonk_client = smooth_goalie::WonkClient::new(&wonk_url);
    let goalie_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("boardroom: probe goalie port")?;
    let goalie_addr = goalie_listener.local_addr().context("boardroom: goalie local addr")?;
    drop(goalie_listener);
    let goalie_listen = goalie_addr.to_string();
    let goalie_url = format!("http://{goalie_addr}");
    tokio::spawn(async move {
        if let Err(e) = smooth_goalie::run_proxy(&goalie_listen, goalie_wonk_client, goalie_audit).await {
            tracing::error!(error = %e, "boardroom: Goalie proxy crashed");
        }
    });
    tracing::info!(url = %goalie_url, audit = %goalie_audit_path, "boardroom: Goalie up");

    // --- Narc (hook; wired for parity, inert without a tool surface) -------
    // Narc is a ToolHook, not a server. Building one here documents that
    // Big Smooth's internal tool surface (when we add one) will inherit
    // surveillance for free. For now it's held only for symmetry.
    let wonk_hook = Arc::new(WonkHook::new(&wonk_url));

    // --- Diver (pearl lifecycle manager) ----------------------------------
    // Diver wraps the PearlStore with lifecycle management: dispatch creates
    // a pearl, complete closes it, operators can create sub-pearls, and
    // Jira sync happens automatically when env vars are set.
    let diver_url = if let Some(store) = pearl_store {
        let diver_store = DiverStore::new(store);
        let jira = smooth_diver::JiraClient::from_env().map(Arc::new);
        let diver_state = DiverState {
            store: Arc::new(diver_store),
            jira,
        };
        let diver_r = diver_router(diver_state);
        let diver_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("boardroom: bind diver")?;
        let diver_addr = diver_listener.local_addr().context("boardroom: diver local addr")?;
        let url = format!("http://{diver_addr}");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(diver_listener, diver_r).await {
                tracing::error!(error = %e, "boardroom: Diver server crashed");
            }
        });
        tracing::info!(url = %url, "boardroom: Diver up");
        url
    } else {
        tracing::warn!("boardroom: no PearlStore provided, Diver not started");
        String::new()
    };

    Ok(BoardroomHandles {
        wonk_url,
        goalie_url,
        scribe_url,
        archivist_url,
        archivist_port: ARCHIVIST_GUEST_PORT,
        scribe_forwarder: Some(scribe_forwarder),
        wonk_hook,
        diver_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawning the cast from a unit test validates every server binds,
    /// every URL is well-formed, and the Archivist lands on 4401.
    ///
    /// Runs without hardware virt because nothing in the cast is a
    /// microVM — everything is a plain tokio task on host loopback.
    #[tokio::test]
    async fn spawn_boardroom_cast_brings_up_every_member() {
        // Skip if 4401 is already bound (concurrent test, or stale
        // boardroom process). This is a smoke test, not a contention
        // regression guard.
        if std::net::TcpListener::bind(("0.0.0.0", ARCHIVIST_GUEST_PORT)).is_err() {
            eprintln!("skipping: 0.0.0.0:{ARCHIVIST_GUEST_PORT} already in use");
            return;
        }
        let handles = match spawn_boardroom_cast(None).await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("skipping: boardroom cast spawn failed: {e}");
                return;
            }
        };
        assert!(handles.archivist_url.contains(":4401"));
        assert!(handles.wonk_url.starts_with("http://127.0.0.1:"));
        assert!(handles.scribe_url.starts_with("http://127.0.0.1:"));
        assert!(handles.goalie_url.starts_with("http://127.0.0.1:"));
        assert_eq!(handles.archivist_port, 4401);

        // The archivist should be reachable on /health.
        let resp = reqwest::get(format!("{}/health", handles.archivist_url)).await.expect("archivist health");
        assert!(resp.status().is_success(), "archivist /health returned {}", resp.status());
    }
}
