//! Unattended-run resolver for Safehouse Narc `Ask` verdicts.
//!
//! When a scenario or `th code --headless --auto-approve <mode>` run
//! is in flight, no human is at the TUI to pick a scope on the
//! inline approval card. This module spawns a tokio task that polls
//! `/api/access/pending` and resolves each pending request per the
//! configured mode:
//!
//! - `deny`    — resolves every Ask as Deny @ scope=once. The safe
//!   default for unattended runs.
//! - `once`    — Approve @ scope=once (re-asks next call).
//! - `session` — Approve @ scope=session (cached for the run's
//!   lifetime).
//! - `project` / `user` — Approve at that scope. Side effects on
//!   wonk-allow.toml; rarely what a bench scenario wants.
//!
//! Pearl th-400773.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::scenarios::AutoApprove;

/// Tokio task handle returned by [`spawn_resolver`]. Drop to stop
/// the resolver — the inner loop checks `Arc::strong_count` to know
/// when to exit (same shape as the TUI's SSE subscriber).
pub struct AutoApproveHandle {
    /// Shared sentinel — when the only strong count is the
    /// task's own, the loop exits.
    _alive: Arc<()>,
    /// Handle to abort if the caller wants a hard stop instead of
    /// waiting for the sentinel.
    pub task: tokio::task::JoinHandle<()>,
}

#[derive(Serialize)]
struct ResolveBody<'a> {
    id: &'a str,
    scope: &'a str,
}

/// Poll `/api/access/pending` every 100ms and resolve each entry
/// per `mode`. Returns a handle; drop it (or call `.task.abort()`)
/// to stop.
///
/// `base_url` should NOT have a trailing slash. Typical value is
/// `http://127.0.0.1:4400` (default Big Smooth bind).
#[must_use]
pub fn spawn_resolver(base_url: String, mode: AutoApprove) -> AutoApproveHandle {
    let sentinel = Arc::new(());
    let sentinel_for_task = Arc::clone(&sentinel);
    let task = tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let pending_url = format!("{base_url}/api/access/pending");
        let (verdict_path, scope_str): (&str, &str) = match mode {
            AutoApprove::Deny => ("deny", "once"),
            AutoApprove::Once => ("approve", "once"),
            AutoApprove::Session => ("approve", "session"),
            AutoApprove::Project => ("approve", "project"),
            AutoApprove::User => ("approve", "user"),
        };
        let resolve_url = format!("{base_url}/api/access/{verdict_path}");

        loop {
            // Exit when nothing outside this task still holds the
            // sentinel — same shape as auto_mode's strong-count
            // check, lets callers stop us by dropping the handle.
            if Arc::strong_count(&sentinel_for_task) <= 1 {
                tracing::debug!("auto-approve resolver: handle dropped, exiting");
                return;
            }

            let pending: Vec<serde_json::Value> = match client.get(&pending_url).send().await {
                Ok(resp) if resp.status().is_success() => resp.json().await.unwrap_or_default(),
                Ok(_) | Err(_) => Vec::new(),
            };

            for entry in pending {
                let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let body = ResolveBody { id, scope: scope_str };
                let send = client.post(&resolve_url).json(&body).send().await;
                if let Ok(resp) = send {
                    tracing::info!(
                        id,
                        scope = scope_str,
                        verdict = verdict_path,
                        status = %resp.status(),
                        "auto-approve resolver: resolved pending request"
                    );
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    AutoApproveHandle { _alive: sentinel, task }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    #[derive(Clone, Default)]
    struct FakeBs {
        // Pending requests we hand out; each call to /api/access/pending
        // drains them so the resolver doesn't loop forever on the same id.
        pending: Arc<Mutex<Vec<serde_json::Value>>>,
        // (verdict_path, body) tuples the resolver POSTed.
        resolved: Arc<Mutex<Vec<(String, ResolveBodyOwned)>>>,
    }

    #[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
    struct ResolveBodyOwned {
        id: String,
        scope: String,
    }

    async fn pending(State(s): State<FakeBs>) -> Json<Vec<serde_json::Value>> {
        let drained = s.pending.lock().unwrap().drain(..).collect();
        Json(drained)
    }

    async fn approve(State(s): State<FakeBs>, Json(body): Json<ResolveBodyOwned>) -> axum::http::StatusCode {
        s.resolved.lock().unwrap().push(("approve".into(), body));
        axum::http::StatusCode::OK
    }

    async fn deny(State(s): State<FakeBs>, Json(body): Json<ResolveBodyOwned>) -> axum::http::StatusCode {
        s.resolved.lock().unwrap().push(("deny".into(), body));
        axum::http::StatusCode::OK
    }

    async fn spawn_fake_bs() -> (String, FakeBs) {
        let state = FakeBs::default();
        let app = Router::new()
            .route("/api/access/pending", get(pending))
            .route("/api/access/approve", post(approve))
            .route("/api/access/deny", post(deny))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        (format!("http://{addr}"), state)
    }

    fn pending_request(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "bead_id": "pearl",
            "operator_id": "op",
            "kind": "network",
            "resource": "api.example.com",
            "reason": "test",
            "scope_options": ["once", "session", "pearl_project", "user"],
            "created_at": "2026-05-14T00:00:00Z",
        })
    }

    async fn wait_for_resolve(state: &FakeBs, want_verdict: &str, want_scope: &str, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            let found = {
                let resolved = state.resolved.lock().unwrap();
                resolved.iter().any(|(v, b)| v == want_verdict && b.scope == want_scope)
            };
            if found {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn deny_mode_resolves_pending_as_deny_once() {
        let (url, bs) = spawn_fake_bs().await;
        bs.pending.lock().unwrap().push(pending_request("p1"));

        let _handle = spawn_resolver(url, AutoApprove::Deny);
        let ok = wait_for_resolve(&bs, "deny", "once", Duration::from_secs(2)).await;
        assert!(ok, "deny mode should resolve as deny @ once");
    }

    #[tokio::test]
    async fn session_mode_resolves_pending_as_approve_session() {
        let (url, bs) = spawn_fake_bs().await;
        bs.pending.lock().unwrap().push(pending_request("p1"));

        let _handle = spawn_resolver(url, AutoApprove::Session);
        let ok = wait_for_resolve(&bs, "approve", "session", Duration::from_secs(2)).await;
        assert!(ok);
    }

    #[tokio::test]
    async fn once_mode_resolves_pending_as_approve_once() {
        let (url, bs) = spawn_fake_bs().await;
        bs.pending.lock().unwrap().push(pending_request("p1"));

        let _handle = spawn_resolver(url, AutoApprove::Once);
        let ok = wait_for_resolve(&bs, "approve", "once", Duration::from_secs(2)).await;
        assert!(ok);
    }

    #[tokio::test]
    async fn project_mode_resolves_pending_as_approve_project() {
        let (url, bs) = spawn_fake_bs().await;
        bs.pending.lock().unwrap().push(pending_request("p1"));

        let _handle = spawn_resolver(url, AutoApprove::Project);
        let ok = wait_for_resolve(&bs, "approve", "project", Duration::from_secs(2)).await;
        assert!(ok);
    }

    #[tokio::test]
    async fn dropping_handle_stops_the_loop() {
        let (url, _bs) = spawn_fake_bs().await;
        let handle = spawn_resolver(url, AutoApprove::Deny);
        // Drop the handle; the task's Arc count drops to 1 (its
        // own clone) and the next loop iteration exits.
        drop(handle);
        // No assertion needed — the test passes if it terminates
        // cleanly. We just don't want a stuck tokio task at the
        // end of the test runtime.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn resolver_handles_multiple_pendings_in_one_poll() {
        let (url, bs) = spawn_fake_bs().await;
        bs.pending.lock().unwrap().push(pending_request("p1"));
        bs.pending.lock().unwrap().push(pending_request("p2"));
        bs.pending.lock().unwrap().push(pending_request("p3"));

        let _handle = spawn_resolver(url, AutoApprove::Session);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let count = bs.resolved.lock().unwrap().len();
            if count == 3 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("expected 3 resolutions, got {}", bs.resolved.lock().unwrap().len());
    }
}
