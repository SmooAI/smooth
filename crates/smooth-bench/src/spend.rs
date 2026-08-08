//! Gateway-measured LLM spend (pearls th-adf614, th-11f9bb).
//!
//! The protocol reports `$0` for every turn. The pipe is not broken —
//! `runner.rs` → `TurnUsage` → `eventual_response.data.data.usage.costUsd`
//! is all correct — the *source* read is missing: llm.smoo.ai returns a
//! request's cost only in the **`x-litellm-response-cost` response
//! header**, and the engine's LLM client parses the JSON body, where
//! there is no cost field at all. th-11f9bb fixes that at the root, and
//! fixing it there is what makes cost work for `th code`'s status bar
//! and the daemon too.
//!
//! This module is how the bench reports a real number *now*, and it
//! keeps working for the polyglot engines, which never emit cost in
//! their protocol events at all.
//!
//! Two measurements, because one alone would lie:
//!
//! 1. **Total** — `GET /key/info` returns the key's cumulative spend
//!    (`/spend/logs` is admin-only). A before/after delta around one
//!    model's run is everything that key spent during it.
//! 2. **Harness** — the bench's own driver and judge calls hit the same
//!    key. Left in, a cheap agent graded by an expensive judge looks
//!    expensive, which inverts exactly the comparison this is for. We
//!    make those calls ourselves, so we read their cost off the header
//!    and subtract it.
//!
//! ⚠️ The key is shared. Anything else spending on it during a run —
//! your own `th code` session, the smoo-hub daemon — lands in the delta.
//! [`Measured::caveat`] says so in the output rather than letting a
//! confident wrong number stand.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

/// Harness spend accumulated this process, in micro-dollars.
///
/// ponytail: a process-global rather than threading a cost out of
/// `judge()` and the convo driver and back up through two call chains.
/// There is one gateway and one bench process; a global is the honest
/// shape of that, and it keeps the change to two call sites.
static HARNESS_MICRO_USD: AtomicU64 = AtomicU64::new(0);

/// Micro-dollars per dollar. Cost is accumulated as an integer so the
/// counter is lock-free; LiteLLM's per-request costs run to ~1e-5 USD,
/// which is 10 micro-dollars — coarse enough to hold, fine enough that
/// rounding is far below the cent the leaderboard prints.
const MICRO: f64 = 1_000_000.0;

/// Record the cost of one call the BENCH made (judge or convo driver),
/// read from the gateway's `x-litellm-response-cost` response header.
///
/// A response without the header contributes nothing — the harness cost
/// is then under-counted, which biases the agent's cost *upward*. That
/// direction is deliberate: it never flatters the model under test.
pub fn record_harness_response(headers: &reqwest::header::HeaderMap) {
    if let Some(usd) = response_cost(headers) {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "cost is small and non-negative; clamped below"
        )]
        let micro = (usd * MICRO).max(0.0).round() as u64;
        HARNESS_MICRO_USD.fetch_add(micro, Ordering::Relaxed);
    }
}

/// Per-request cost from `x-litellm-response-cost`, in USD.
#[must_use]
pub fn response_cost(headers: &reqwest::header::HeaderMap) -> Option<f64> {
    headers.get("x-litellm-response-cost")?.to_str().ok()?.trim().parse::<f64>().ok()
}

/// Total harness spend so far this process, in USD.
#[must_use]
pub fn harness_spent() -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "micro-dollar counts are far below 2^53")]
    let usd = HARNESS_MICRO_USD.load(Ordering::Relaxed) as f64 / MICRO;
    usd
}

/// The key's cumulative spend, in USD, from `GET /key/info`.
///
/// # Errors
/// Errors if the gateway is unreachable, rejects the key, or returns a
/// body without `info.spend`.
pub async fn key_spend(gateway_url: &str, key: &str) -> Result<f64> {
    // /key/info hangs off the gateway ROOT, not the /v1 chat base.
    let base = gateway_url.trim_end_matches('/').trim_end_matches("/v1");
    let url = format!("{base}/key/info");
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(key)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let text = resp.text().await.context("reading /key/info body")?;
    anyhow::ensure!(status.is_success(), "/key/info returned {status}: {}", crate::judge::truncate(&text, 200));
    let v: serde_json::Value = serde_json::from_str(&text).context("/key/info was not JSON")?;
    v.pointer("/info/spend")
        .and_then(serde_json::Value::as_f64)
        .context("/key/info has no numeric info.spend")
}

/// Poll `/key/info` until the reported spend stops moving.
///
/// LiteLLM posts spend **asynchronously**: a request whose response
/// header already says `x-litellm-response-cost: 0.00117` does not show
/// up in `/key/info` for another second or two. Sampling immediately
/// after a run therefore undercounts it — and undercounts the SLOWEST,
/// most expensive model worst, because its last request is the most
/// recent. That failure mode is invisible and it flatters exactly the
/// wrong model, which is how a premium model first read as costing 1.2x
/// a cheap one here.
///
/// Two consecutive equal reads means the writer has caught up. On a busy
/// shared key it may never settle, so this is capped and returns the
/// last read either way — a slightly-off number beats hanging the suite.
pub async fn settled_key_spend(gateway_url: &str, key: &str) -> Result<f64> {
    const SETTLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1500);
    const MAX_POLLS: usize = 20; // ~30s ceiling

    let mut last = key_spend(gateway_url, key).await?;
    for _ in 0..MAX_POLLS {
        tokio::time::sleep(SETTLE_INTERVAL).await;
        let next = key_spend(gateway_url, key).await?;
        if (next - last).abs() < f64::EPSILON {
            return Ok(next);
        }
        last = next;
    }
    Ok(last)
}

/// Log why a spend sample failed and degrade to `None`.
fn report_err(what: &str, r: Result<f64>) -> Option<f64> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("cost: {what} failed, this model's cost will be unmeasured — {e:#}");
            None
        }
    }
}

/// How fast the key is spending on traffic that is NOT ours, in USD per
/// second, sampled over an idle window before the suite starts.
///
/// This exists because the first version of this feature confidently
/// reported gpt-5.5 as CHEAPER than deepseek-v4-flash. It is not. The
/// key is shared with the smoo-hub daemon and whatever else is running,
/// and on a short suite that background traffic is the same order as
/// the thing being measured. A number that ranks a premium model below
/// a budget one is worse than no number, because someone will act on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseFloor {
    pub usd_per_second: f64,
}

impl NoiseFloor {
    /// Expected background spend over a run of `duration_ms`.
    #[must_use]
    #[allow(clippy::cast_precision_loss, reason = "durations are milliseconds; f64 is exact well past that")]
    pub fn over(self, duration_ms: u64) -> f64 {
        self.usd_per_second * (duration_ms as f64 / 1000.0)
    }

    /// Sample the key's idle drift across `window`.
    ///
    /// Call this BEFORE the suite runs, while nothing of ours is in
    /// flight — anything we start would be counted as background.
    ///
    /// # Errors
    /// Propagates a `/key/info` failure.
    pub async fn sample(gateway_url: &str, key: &str, window: std::time::Duration) -> Result<Self> {
        let before = key_spend(gateway_url, key).await?;
        tokio::time::sleep(window).await;
        let after = key_spend(gateway_url, key).await?;
        let secs = window.as_secs_f64().max(1.0);
        Ok(Self {
            usd_per_second: ((after - before) / secs).max(0.0),
        })
    }
}

/// One model's measured spend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    /// Everything the key spent during the run, harness included.
    pub gateway_total: f64,
    /// The bench's own driver + judge calls during the run.
    pub harness: f64,
}

impl Measured {
    /// What the agent under test cost: total minus our own calls.
    ///
    /// Clamped at zero — a shared key can in principle produce a
    /// slightly negative figure from rounding or a late-posting request,
    /// and a negative cost would sort to the top of the leaderboard.
    #[must_use]
    pub fn agent(self) -> f64 {
        (self.gateway_total - self.harness).max(0.0)
    }

    /// Whether this figure is big enough to mean anything, given how
    /// much the shared key was spending on its own.
    ///
    /// The 2x margin is deliberately conservative: at parity with the
    /// noise you cannot tell two models apart at all, and the whole
    /// point of the column is telling them apart.
    #[must_use]
    pub fn is_resolvable(self, noise: Option<NoiseFloor>, duration_ms: u64) -> bool {
        // A cost of exactly zero is never a measurement. It means the
        // spend had not posted yet, or the sample missed it — publishing
        // "$0.0000" would read as "this model is free".
        if self.agent() <= 0.0 {
            return false;
        }
        noise.is_none_or(|n| {
            let expected = n.over(duration_ms);
            expected <= f64::EPSILON || self.agent() > 2.0 * expected
        })
    }

    /// The line printed under the leaderboard, so nobody reads these
    /// numbers as more precise than they are.
    #[must_use]
    pub fn caveat() -> &'static str {
        "cost is measured from the gateway key's spend delta, minus the bench's own driver/judge calls. \
The key is SHARED — anything else spending on it during the run (a th code session, the smoo-hub daemon) \
lands in these numbers. Run on a quiet key for a clean read."
    }
}

/// Measure one model's run: sample the key's spend and the harness
/// counter before, run `f`, sample again.
///
/// Returns `(result, None)` when the gateway can't be reached for either
/// sample — a bench that aborts because it couldn't price itself would
/// be worse than one that reports the pass rate without a cost.
pub async fn measure<F, T>(gateway_url: &str, key: Option<&str>, f: F) -> (T, Option<Measured>)
where
    F: std::future::Future<Output = T>,
{
    let Some(key) = key else {
        return (f.await, None);
    };
    // `before` is settled too: a previous model's spend may still be
    // posting, and it would otherwise land in this model's delta.
    // A failure here is REPORTED, not swallowed — "cost: not measured"
    // with no reason is how you spend an afternoon guessing.
    let before = report_err("sampling spend before the run", settled_key_spend(gateway_url, key).await);
    let harness_before = harness_spent();
    let out = f.await;
    let after = report_err("sampling spend after the run", settled_key_spend(gateway_url, key).await);
    let measured = match (before, after) {
        (Some(b), Some(a)) => Some(Measured {
            gateway_total: (a - b).max(0.0),
            harness: (harness_spent() - harness_before).max(0.0),
        }),
        _ => None,
    };
    (out, measured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn headers(cost: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-litellm-response-cost", HeaderValue::from_str(cost).expect("valid"));
        h
    }

    #[test]
    fn reads_the_cost_header_including_scientific_notation() {
        // The gateway really does send `1.4700000000000002e-05`.
        assert_eq!(response_cost(&headers("1.4700000000000002e-05")), Some(1.4700000000000002e-05));
        assert_eq!(response_cost(&headers("0.0")), Some(0.0));
        assert_eq!(response_cost(&HeaderMap::new()), None, "a response without the header has no cost");
        assert_eq!(response_cost(&headers("not-a-number")), None);
    }

    #[test]
    fn agent_cost_excludes_the_harness() {
        // A cheap agent graded by an expensive judge must not read as
        // expensive — that inverts the whole comparison.
        let m = Measured {
            gateway_total: 1.00,
            harness: 0.90,
        };
        assert!((m.agent() - 0.10).abs() < 1e-9);
    }

    #[test]
    fn agent_cost_never_goes_negative() {
        // A shared key + a late-posting request can make harness exceed
        // the observed delta; a negative cost would sort FIRST.
        let m = Measured {
            gateway_total: 0.01,
            harness: 0.05,
        };
        assert!((m.agent() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn harness_accumulates_across_calls() {
        let start = harness_spent();
        record_harness_response(&headers("0.000100"));
        record_harness_response(&headers("0.000200"));
        // A header-less response contributes nothing rather than guessing.
        record_harness_response(&HeaderMap::new());
        let delta = harness_spent() - start;
        assert!((delta - 0.0003).abs() < 1e-9, "expected 0.0003, got {delta}");
    }

    #[test]
    fn noise_floor_scales_with_run_length() {
        let n = NoiseFloor { usd_per_second: 0.0001 };
        assert!((n.over(10_000) - 0.001).abs() < 1e-12, "10s at 1e-4/s is 1e-3");
        assert!((n.over(0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_cost_under_the_noise_floor_is_not_resolvable() {
        // The regression this guards: a 20s run on a key drifting
        // $0.0001/s carries ~$0.002 of other people's traffic, which is
        // exactly the size of the figures being compared. Reporting them
        // ranked gpt-5.5 below deepseek-v4-flash, which is nonsense.
        let noise = Some(NoiseFloor { usd_per_second: 0.0001 });
        let small = Measured {
            gateway_total: 0.0020,
            harness: 0.0,
        };
        assert!(!small.is_resolvable(noise, 20_000), "0.0020 vs ~0.0020 of noise is not a measurement");

        let big = Measured {
            gateway_total: 0.5,
            harness: 0.0,
        };
        assert!(big.is_resolvable(noise, 20_000), "0.5 is far above the floor");
    }

    #[test]
    fn a_zero_cost_is_never_published_as_a_measurement() {
        // Observed live: a short run whose spend had not posted yet
        // produced agent() == 0.0, and the scoreboard published
        // "cost_usd: 0.0" — which reads as "free".
        let nothing = Measured {
            gateway_total: 0.0,
            harness: 0.0,
        };
        assert!(!nothing.is_resolvable(Some(NoiseFloor { usd_per_second: 0.0 }), 10_000));
        assert!(!nothing.is_resolvable(None, 10_000));
    }

    #[test]
    fn a_quiet_key_makes_everything_resolvable() {
        let quiet = Some(NoiseFloor { usd_per_second: 0.0 });
        let tiny = Measured {
            gateway_total: 0.000_01,
            harness: 0.0,
        };
        assert!(tiny.is_resolvable(quiet, 20_000), "on a quiet key even a tiny delta is exact");
        // Unknown noise must not block reporting either.
        assert!(tiny.is_resolvable(None, 20_000));
    }

    #[tokio::test]
    async fn settled_key_spend_gives_up_rather_than_hanging() {
        // An unreachable gateway must error out, not spin for 30s.
        let r = settled_key_spend("http://127.0.0.1:1/v1", "sk-nope").await;
        assert!(r.is_err(), "an unreachable gateway is an error, not a settled zero");
    }

    #[tokio::test]
    async fn measure_without_a_key_still_runs_the_body() {
        // No credentials must degrade to "no cost", never to "no run".
        let (out, measured) = measure("https://example.invalid/v1", None, async { 42 }).await;
        assert_eq!(out, 42);
        assert!(measured.is_none());
    }

    #[tokio::test]
    async fn measure_returns_none_when_the_gateway_is_unreachable() {
        let (out, measured) = measure("http://127.0.0.1:1/v1", Some("sk-nope"), async { "ran" }).await;
        assert_eq!(out, "ran", "the suite must still run when it cannot price itself");
        assert!(measured.is_none());
    }
}
