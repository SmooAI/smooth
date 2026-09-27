//! `GET /api/ci-queue/status` — the web Queue tab's data (SMOODEV-3371).
//!
//! The queue lives in `th` (`th ci-queue`, SMOODEV-3355) as kernel flocks
//! under `~/.smooth/ci-queue/` with no daemon of its own, and smooth-daemon
//! cannot link smooth-cli. So this route runs `th ci-queue status --json`
//! (the same snapshot `th ci-queue web` streams) and relays it, using the
//! relay types both hosts share (`smooth_web::queue`).
//!
//! Two things keep it cheap on a machine that is, by definition, busy when
//! anyone opens this tab:
//! - **One `th` at a time, at most once a second.** Concurrent requests share
//!   the in-flight read; a snapshot younger than [`FRESH`] is served as is.
//! - **Nothing runs unless someone is looking.** The daemon never polls on
//!   its own. The pressure history the sparklines draw is kept from the reads
//!   clients asked for.
//!
//! `?since_ms=N` returns only samples newer than N, so a polling client pays
//! for the ten-minute window once.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use smooth_web::queue::{is_snapshot, Samples, StatusResponse};
use tokio::sync::Mutex;

/// A snapshot this young is served without running `th` again.
const FRESH: Duration = Duration::from_millis(900);
/// A `th ci-queue status` that takes longer than this is abandoned.
const READ_TIMEOUT: Duration = Duration::from_secs(8);
/// Finished jobs to ask for: enough for a per-label p50.
const HISTORY: &str = "200";

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    #[serde(default)]
    pub since_ms: u64,
}

#[derive(Default)]
struct Cache {
    snapshot: Option<serde_json::Value>,
    read_at: Option<Instant>,
    error: Option<String>,
    samples: Samples,
}

#[derive(Clone)]
pub struct CiQueueState {
    /// The command that prints a snapshot. `None` = no `th` found.
    argv: Option<Vec<OsString>>,
    cache: Arc<Mutex<Cache>>,
}

impl CiQueueState {
    /// Read the queue through the `th` this daemon would shell out to anyway.
    #[must_use]
    pub fn from_th() -> Self {
        let argv = smooth_tools::th::resolve_th().map(|th| {
            vec![
                th.into_os_string(),
                "ci-queue".into(),
                "status".into(),
                "--json".into(),
                "--history".into(),
                HISTORY.into(),
            ]
        });
        Self::with_argv(argv)
    }

    /// Any command that prints a snapshot as JSON (tests use `sh -c`).
    #[must_use]
    pub fn with_argv(argv: Option<Vec<OsString>>) -> Self {
        Self {
            argv,
            cache: Arc::new(Mutex::new(Cache::default())),
        }
    }
}

/// The Queue router: `GET /api/ci-queue/status`.
pub fn ci_queue_router(state: CiQueueState) -> Router {
    Router::new().route("/api/ci-queue/status", get(status)).with_state(state)
}

async fn status(State(state): State<CiQueueState>, Query(q): Query<StatusQuery>) -> Json<StatusResponse> {
    // Holding the lock across the read is the point: a second request waits
    // for the first's `th` instead of starting its own.
    let mut cache = state.cache.lock().await;
    if cache.read_at.is_none_or(|t| t.elapsed() >= FRESH) {
        match read(state.argv.as_deref()).await {
            Ok(snap) => {
                cache.samples.record(&snap);
                cache.snapshot = Some(snap);
                cache.error = None;
            }
            // Keep the last good snapshot on screen, and say it is stale.
            Err(e) => cache.error = Some(e),
        }
        cache.read_at = Some(Instant::now());
    }
    Json(StatusResponse {
        snapshot: cache.snapshot.clone(),
        samples: cache.samples.since(q.since_ms),
        error: cache.error.clone(),
    })
}

async fn read(argv: Option<&[OsString]>) -> Result<serde_json::Value, String> {
    let Some((bin, rest)) = argv.and_then(<[OsString]>::split_first) else {
        return Err("No `th` binary found for this daemon (set SMOOTH_TH_BIN, or install th).".into());
    };
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(rest).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = match tokio::time::timeout(READ_TIMEOUT, cmd.output()).await {
        Err(_) => {
            return Err(format!(
                "`th ci-queue status` took over {}s; the machine may be swamped.",
                READ_TIMEOUT.as_secs()
            ))
        }
        Ok(Err(e)) => return Err(format!("Could not run th: {e}")),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("unrecognized subcommand") {
            return Err("This th has no `ci-queue` command yet. Update it (`pnpm install:th`, or the menu bar).".into());
        }
        return Err(format!("`th ci-queue status` failed: {}", stderr.lines().next().unwrap_or("no output")));
    }
    let snap: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| "`th ci-queue status --json` did not print JSON.".to_string())?;
    if !is_snapshot(&snap) {
        return Err("`th ci-queue status --json` printed an unexpected shape.".into());
    }
    Ok(snap)
}

// The tests stand `sh -c` in for `th`, and the queue itself is Unix-only.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    fn snap(now_ms: u64) -> serde_json::Value {
        serde_json::json!({ "schema": 1, "now_ms": now_ms, "running": [], "waiting": [], "holds": [], "history": [], "readings": { "load1": 3.0, "cores": 12, "disks": [] } })
    }

    fn sh(script: &str) -> CiQueueState {
        CiQueueState::with_argv(Some(vec!["sh".into(), "-c".into(), script.into()]))
    }

    async fn get(router: &Router, uri: &str) -> serde_json::Value {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn relays_the_snapshot_and_a_first_sample() {
        let router = ci_queue_router(sh(&format!("echo '{}'", snap(1_000))));
        let json = get(&router, "/api/ci-queue/status").await;
        assert_eq!(json["snapshot"]["now_ms"], 1_000);
        assert_eq!(json["samples"].as_array().unwrap().len(), 1);
        assert_eq!(json["samples"][0]["readings"]["cores"], 12);
        assert!(json.get("error").is_none());
    }

    #[tokio::test]
    async fn since_ms_returns_only_newer_samples() {
        let router = ci_queue_router(sh(&format!("echo '{}'", snap(1_000))));
        let json = get(&router, "/api/ci-queue/status?since_ms=1000").await;
        assert!(json["samples"].as_array().unwrap().is_empty());
        assert_eq!(json["snapshot"]["now_ms"], 1_000, "the snapshot itself always comes back");
    }

    #[tokio::test]
    async fn a_second_request_inside_the_fresh_window_does_not_run_th_again() {
        let tmp = tempfile::tempdir().unwrap();
        let count = tmp.path().join("count");
        let script = format!("echo x >> {}; echo '{}'", count.display(), snap(1_000));
        let router = ci_queue_router(sh(&script));
        get(&router, "/api/ci-queue/status").await;
        get(&router, "/api/ci-queue/status").await;
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
    }

    #[tokio::test]
    async fn a_th_without_ci_queue_says_to_update() {
        let router = ci_queue_router(sh("echo \"error: unrecognized subcommand 'ci-queue'\" >&2; exit 2"));
        let json = get(&router, "/api/ci-queue/status").await;
        assert!(json["snapshot"].is_null());
        assert!(json["error"].as_str().unwrap().contains("no `ci-queue` command"));
    }

    #[tokio::test]
    async fn non_json_and_wrong_shapes_are_errors_not_garbage() {
        let router = ci_queue_router(sh("echo 'th ci-queue is Unix-only'"));
        assert!(get(&router, "/api/ci-queue/status").await["error"]
            .as_str()
            .unwrap()
            .contains("did not print JSON"));
        let router = ci_queue_router(sh("echo '{\"hello\":1}'"));
        assert!(get(&router, "/api/ci-queue/status").await["error"]
            .as_str()
            .unwrap()
            .contains("unexpected shape"));
    }

    #[tokio::test]
    async fn no_th_is_an_error_that_says_how_to_fix_it() {
        let router = ci_queue_router(CiQueueState::with_argv(None));
        let json = get(&router, "/api/ci-queue/status").await;
        assert!(json["error"].as_str().unwrap().contains("SMOOTH_TH_BIN"));
    }

    #[tokio::test]
    async fn a_failed_read_keeps_the_last_good_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let flag = tmp.path().join("fail");
        let script = format!("if [ -e {f} ]; then echo boom >&2; exit 1; fi; echo '{s}'", f = flag.display(), s = snap(1_000));
        let state = sh(&script);
        let router = ci_queue_router(state.clone());
        get(&router, "/api/ci-queue/status").await;
        std::fs::write(&flag, "").unwrap();
        state.cache.lock().await.read_at = None; // expire the cache
        let json = get(&router, "/api/ci-queue/status").await;
        assert_eq!(json["snapshot"]["now_ms"], 1_000);
        assert!(json["error"].as_str().unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn an_os_without_a_queue_is_relayed_not_an_error() {
        let router = ci_queue_router(sh("echo '{\"schema\":1,\"unsupported\":\"Windows has no flock\"}'"));
        let json = get(&router, "/api/ci-queue/status").await;
        assert_eq!(json["snapshot"]["unsupported"], "Windows has no flock");
        assert!(json.get("error").is_none());
    }
}
