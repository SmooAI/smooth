//! The check-queue page's server side (SMOODEV-3371).
//!
//! Shared by its two hosts: `th ci-queue web`, which serves `queue.html` on
//! its own, and smooth-daemon's `/api/ci-queue/status`, behind Big Smooth's
//! Queue tab.
//!
//! Both relay the same `th ci-queue status --json` snapshot as
//! `{snapshot, samples, error?}`. The snapshot stays an opaque
//! `serde_json::Value` here on purpose: the queue's schema grows (schema 2
//! adds a budget and rusage), and a relay that parsed it into structs would
//! drop every field it was not rebuilt for. The page decides what it knows.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use serde::{Deserialize, Serialize};

use crate::{serve_web, WebAssets};

/// Keep one pressure sample per this interval.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(5);
/// How much pressure history to keep (the sparklines' width).
pub const WINDOW: Duration = Duration::from_secs(600);

/// One point on the sparklines: the machine's readings, and the admission
/// budget when the queue has one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sample {
    pub t_ms: u64,
    pub readings: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<serde_json::Value>,
}

/// What both hosts send the page.
#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    /// The snapshot, or null when none could be read.
    pub snapshot: Option<serde_json::Value>,
    pub samples: Vec<Sample>,
    /// Why the snapshot is missing or stale, in words the page shows as is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Is this a snapshot the page can show: a live queue (`running` is a list)
/// or an honest "no queue here" (`unsupported`, which Windows sends)?
#[must_use]
pub fn is_snapshot(v: &serde_json::Value) -> bool {
    v.get("running").is_some_and(serde_json::Value::is_array) || v.get("unsupported").is_some_and(serde_json::Value::is_string)
}

/// The ten-minute window of samples, one per [`SAMPLE_EVERY`] of the
/// snapshots' own clock.
#[derive(Debug, Default, Clone)]
pub struct Samples(VecDeque<Sample>);

impl Samples {
    /// Take a sample from `snap` if one is due. Returns it when it was kept.
    pub fn record(&mut self, snap: &serde_json::Value) -> Option<Sample> {
        let t_ms = snap.get("now_ms").and_then(serde_json::Value::as_u64)?;
        let readings = snap.get("readings")?.clone();
        let every = u64::try_from(SAMPLE_EVERY.as_millis()).unwrap_or(u64::MAX);
        if self.0.back().is_some_and(|last| t_ms < last.t_ms.saturating_add(every)) {
            return None;
        }
        let sample = Sample {
            t_ms,
            readings,
            budget: snap.get("budget").filter(|b| !b.is_null()).cloned(),
        };
        self.0.push_back(sample.clone());
        let window = u64::try_from(WINDOW.as_millis()).unwrap_or(u64::MAX);
        while self.0.front().is_some_and(|s| s.t_ms.saturating_add(window) < t_ms) {
            self.0.pop_front();
        }
        Some(sample)
    }

    /// Samples newer than `t_ms`, oldest first.
    #[must_use]
    pub fn since(&self, t_ms: u64) -> Vec<Sample> {
        self.0.iter().filter(|s| s.t_ms > t_ms).cloned().collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The standalone page: `queue.html` at `/` (and for any unknown path), plus
/// the bundle's assets. The caller merges its `/api/*` routes in front.
pub fn queue_router() -> Router {
    let index = WebAssets::get("queue.html").map(|f| Arc::new(String::from_utf8_lossy(&f.data).into_owned()));
    Router::new().fallback(serve_web).with_state(index)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn snap(now_ms: u64) -> serde_json::Value {
        serde_json::json!({ "schema": 1, "now_ms": now_ms, "running": [], "readings": { "cores": 12 } })
    }

    #[test]
    fn samples_are_spaced_and_windowed() {
        let mut s = Samples::default();
        assert!(s.record(&snap(0)).is_some());
        assert!(s.record(&snap(1_000)).is_none(), "too soon");
        assert!(s.record(&snap(5_000)).is_some());
        assert_eq!(s.since(0).iter().map(|x| x.t_ms).collect::<Vec<_>>(), vec![5_000]);
        assert_eq!(s.len(), 2);
        s.record(&snap(700_000));
        assert_eq!(s.len(), 1, "older than the window is dropped");
        assert!(s.record(&serde_json::json!({ "no": "clock" })).is_none());
    }

    #[test]
    fn a_sample_carries_the_budget_when_there_is_one() {
        let mut s = Samples::default();
        let mut v = snap(0);
        v["budget"] = serde_json::json!({ "scale": 0.5 });
        assert_eq!(s.record(&v).unwrap().budget.unwrap()["scale"], 0.5);
        let plain = Samples::default().record(&snap(0)).unwrap();
        assert!(plain.budget.is_none());
        assert!(!serde_json::to_string(&plain).unwrap().contains("budget"), "absent, not null");
    }

    #[test]
    fn snapshots_are_a_queue_or_an_honest_unsupported() {
        assert!(is_snapshot(&snap(0)));
        assert!(is_snapshot(&serde_json::json!({ "schema": 1, "unsupported": "Windows" })));
        assert!(!is_snapshot(&serde_json::json!({ "hello": 1 })));
        assert!(!is_snapshot(&serde_json::json!({ "running": "nope" })));
    }

    #[tokio::test]
    async fn the_page_router_serves_html_at_root_and_for_unknown_paths() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let app = queue_router();
        for path in ["/", "/anything"] {
            let res = app.clone().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "path {path}");
        }
    }
}
