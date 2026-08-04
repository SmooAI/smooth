//! `POST /api/usage` + `GET /api/stats` — the Stats page's data.
//!
//! The engine computes per-turn token/cost (`TurnUsage`) and streams it to the
//! client on the `eventual_response` event, but never persists it — so there's
//! no spend history in the operator db. Rather than fork the pinned engine's
//! runner to write it, the client POSTs each turn's usage here and we append it
//! to `~/.smooth/usage.jsonl`. `GET /api/stats` then aggregates that log (spend,
//! tokens, by-model, by-day) alongside the durable activity counts the operator
//! db already has ([`SqliteStorageAdapter::activity_snapshot`]).
//!
//! "Spend" is the authoritative per-call cost the LiteLLM gateway reports
//! (`x-litellm-response-cost-*` headers → the engine's `gateway_cost_usd`, which
//! rides `eventual_response.usage.costUsd`) — not a local token×rate estimate.
//! Recording it here on every turn keeps a durable spend ledger the gateway
//! itself doesn't hand back to a single-tenant daemon.
//!
//! Ungated, like the sibling `/search` and `/api/session/cwd` routes: the daemon
//! binds loopback (+ an opt-in tailnet), and these are usage counts, not secrets.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::operator_storage::{ActivitySnapshot, SqliteStorageAdapter};

/// One recorded turn of LLM usage. `ts` is stamped server-side on POST; the
/// client sends everything else (the numbers it already has from
/// `eventual_response.usage`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageRecord {
    /// RFC 3339, server-stamped.
    ts: String,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    cost_usd: f64,
}

/// The POST body — a turn's usage without `ts` (we stamp it).
#[derive(Debug, Deserialize)]
struct RecordBody {
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    cost_usd: f64,
}

/// Shared state: where the usage log lives + the durable store for activity.
#[derive(Clone)]
struct StatsState {
    usage_path: PathBuf,
    storage: Arc<SqliteStorageAdapter>,
}

/// A spend rollup keyed by model or day.
#[derive(Debug, Clone, Default, Serialize)]
struct Bucket {
    usd: f64,
    prompt_tokens: u64,
    completion_tokens: u64,
    turns: u64,
}

impl Bucket {
    fn add(&mut self, r: &UsageRecord) {
        self.usd += r.cost_usd;
        self.prompt_tokens += r.prompt_tokens;
        self.completion_tokens += r.completion_tokens;
        self.turns += 1;
    }
}

/// A by-model or by-day row (a [`Bucket`] with its key inlined) for the JSON.
#[derive(Debug, Clone, Serialize)]
struct KeyedBucket {
    key: String,
    #[serde(flatten)]
    bucket: Bucket,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Spend {
    total_usd: f64,
    prompt_tokens: u64,
    completion_tokens: u64,
    turns: u64,
    /// Highest-spend model first.
    by_model: Vec<KeyedBucket>,
    /// Oldest day first (`YYYY-MM-DD`).
    by_day: Vec<KeyedBucket>,
}

#[derive(Debug, Serialize)]
struct StatsReply {
    activity: ActivitySnapshot,
    spend: Spend,
}

/// `POST /api/usage` — append a turn's usage to the log. Best-effort: a write
/// failure is a 500, but the client fires this fire-and-forget so a lost record
/// only costs one turn of history, never a broken chat.
async fn record_usage(State(state): State<StatsState>, Json(body): Json<RecordBody>) -> Result<StatusCode, (StatusCode, String)> {
    let record = UsageRecord {
        ts: Utc::now().to_rfc3339(),
        conversation_id: body.conversation_id,
        model: body.model,
        prompt_tokens: body.prompt_tokens,
        completion_tokens: body.completion_tokens,
        cost_usd: body.cost_usd,
    };
    append_record(&state.usage_path, &record).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/stats` — activity counts + the aggregated spend log.
async fn get_stats(State(state): State<StatsState>) -> Result<Json<StatsReply>, (StatusCode, String)> {
    let activity = state.storage.activity_snapshot().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let spend = aggregate(&read_records(&state.usage_path));
    Ok(Json(StatsReply { activity, spend }))
}

/// Append one JSON line to the usage log, creating it (and its parent) if needed.
fn append_record(path: &PathBuf, record: &UsageRecord) -> anyhow::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(record)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Read every parseable record from the log. A missing file is empty (no usage
/// yet); a corrupt line is skipped, not fatal — one bad line shouldn't blank the
/// whole dashboard.
fn read_records(path: &PathBuf) -> Vec<UsageRecord> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Roll the records up into totals + by-model + by-day.
fn aggregate(records: &[UsageRecord]) -> Spend {
    let mut spend = Spend::default();
    let mut by_model: HashMap<String, Bucket> = HashMap::new();
    let mut by_day: HashMap<String, Bucket> = HashMap::new();
    for r in records {
        spend.total_usd += r.cost_usd;
        spend.prompt_tokens += r.prompt_tokens;
        spend.completion_tokens += r.completion_tokens;
        spend.turns += 1;
        let model = if r.model.is_empty() { "unknown".to_owned() } else { r.model.clone() };
        by_model.entry(model).or_default().add(r);
        // `ts` is RFC 3339; the date is the first 10 chars (`YYYY-MM-DD`).
        let day = r.ts.get(0..10).unwrap_or("").to_owned();
        by_day.entry(day).or_default().add(r);
    }
    spend.by_model = sorted_by_usd_desc(by_model);
    spend.by_day = sorted_by_key_asc(by_day);
    spend
}

fn sorted_by_usd_desc(map: HashMap<String, Bucket>) -> Vec<KeyedBucket> {
    let mut v: Vec<KeyedBucket> = map.into_iter().map(|(key, bucket)| KeyedBucket { key, bucket }).collect();
    v.sort_by(|a, b| b.bucket.usd.partial_cmp(&a.bucket.usd).unwrap_or(std::cmp::Ordering::Equal));
    v
}

fn sorted_by_key_asc(map: HashMap<String, Bucket>) -> Vec<KeyedBucket> {
    let mut v: Vec<KeyedBucket> = map.into_iter().map(|(key, bucket)| KeyedBucket { key, bucket }).collect();
    v.sort_by(|a, b| a.key.cmp(&b.key));
    v
}

/// The Stats router: `POST /api/usage` + `GET /api/stats`, backed by the usage
/// log path and the durable store.
pub fn stats_router(usage_path: PathBuf, storage: Arc<SqliteStorageAdapter>) -> Router {
    let state = StatsState { usage_path, storage };
    Router::new()
        .route("/api/usage", post(record_usage))
        .route("/api/stats", get(get_stats))
        .with_state(state)
}

/// `~/.smooth/usage.jsonl` (override with `SMOOTH_USAGE_LOG`).
#[must_use]
pub fn usage_log_path() -> PathBuf {
    if let Ok(p) = std::env::var("SMOOTH_USAGE_LOG") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_next::home_dir().map_or_else(|| PathBuf::from("usage.jsonl"), |h| h.join(".smooth").join("usage.jsonl"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    fn fixture() -> (tempfile::TempDir, Router) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(SqliteStorageAdapter::open(&tmp.path().join("op.db")).unwrap());
        let router = stats_router(tmp.path().join("usage.jsonl"), storage);
        (tmp, router)
    }

    async fn post_usage(router: &Router, body: &str) -> StatusCode {
        let req = Request::builder()
            .method("POST")
            .uri("/api/usage")
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        router.clone().oneshot(req).await.unwrap().status()
    }

    async fn get_stats_json(router: &Router) -> serde_json::Value {
        let req = Request::builder().method("GET").uri("/api/stats").body(Body::empty()).unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn empty_stats_are_zeroed_not_error() {
        let (_tmp, router) = fixture();
        let json = get_stats_json(&router).await;
        assert_eq!(json["spend"]["total_usd"], 0.0);
        assert_eq!(json["spend"]["turns"], 0);
        assert_eq!(json["activity"]["conversations"], 0);
        assert!(json["spend"]["by_model"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn records_aggregate_by_total_and_model() {
        let (_tmp, router) = fixture();
        assert_eq!(post_usage(&router, r#"{"model":"opus","prompt_tokens":100,"completion_tokens":50,"cost_usd":0.30}"#).await, StatusCode::NO_CONTENT);
        assert_eq!(post_usage(&router, r#"{"model":"opus","prompt_tokens":200,"completion_tokens":80,"cost_usd":0.70}"#).await, StatusCode::NO_CONTENT);
        assert_eq!(post_usage(&router, r#"{"model":"haiku","prompt_tokens":10,"completion_tokens":5,"cost_usd":0.01}"#).await, StatusCode::NO_CONTENT);

        let json = get_stats_json(&router).await;
        assert_eq!(json["spend"]["turns"], 3);
        assert!((json["spend"]["total_usd"].as_f64().unwrap() - 1.01).abs() < 1e-9);
        assert_eq!(json["spend"]["prompt_tokens"], 310);
        assert_eq!(json["spend"]["completion_tokens"], 135);
        // by_model sorted by usd desc → opus (1.00) before haiku (0.01).
        let by_model = json["spend"]["by_model"].as_array().unwrap();
        assert_eq!(by_model[0]["key"], "opus");
        assert!((by_model[0]["usd"].as_f64().unwrap() - 1.00).abs() < 1e-9);
        assert_eq!(by_model[0]["turns"], 2);
        assert_eq!(by_model[1]["key"], "haiku");
    }

    #[tokio::test]
    async fn missing_model_is_bucketed_as_unknown() {
        let (_tmp, router) = fixture();
        post_usage(&router, r#"{"prompt_tokens":1,"completion_tokens":1,"cost_usd":0.001}"#).await;
        let json = get_stats_json(&router).await;
        assert_eq!(json["spend"]["by_model"][0]["key"], "unknown");
    }

    #[tokio::test]
    async fn corrupt_lines_are_skipped() {
        let (tmp, router) = fixture();
        std::fs::write(tmp.path().join("usage.jsonl"), "not json\n{\"ts\":\"2026-08-04T00:00:00Z\",\"model\":\"m\",\"cost_usd\":0.5}\n").unwrap();
        let json = get_stats_json(&router).await;
        assert_eq!(json["spend"]["turns"], 1);
        assert!((json["spend"]["total_usd"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    }
}
