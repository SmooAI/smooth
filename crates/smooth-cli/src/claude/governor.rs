//! Shared, pool-aware rate-limit governor.
//!
//! The "temporarily limiting requests" throttle is **account-wide**, so
//! if N supervised Claude sessions each retry independently they thunder
//! the herd and make the throttle worse. The governor centralises the
//! backoff: a 429 on *any* session advances one shared backoff counter
//! and (optionally) trips a circuit breaker that holds the *whole* pool
//! off until a shared deadline.
//!
//! In the 1:1 topology there is one session and one governor — the pool
//! logic is inert but the same type is reused, so 1:N and mixed
//! topologies share one `Arc<RateLimitGovernor>` with no code change.
//!
//! The backoff math is pure (`backoff_ceiling`, `jittered`) and unit
//! tested without any clock or RNG; the stateful methods accept an
//! injectable jitter unit so they are deterministic in tests too.

// Backoff/jitter math is intentionally f64 to/from Duration millis; the
// precision loss and truncation are immaterial for sleep durations.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Exponential-backoff parameters. Defaults are tuned for the Claude Code
/// server throttle: a few seconds base, capped at five minutes.
#[derive(Debug, Clone, Copy)]
pub struct BackoffPolicy {
    /// Wait for the first retry (attempt 1), before jitter.
    pub base: Duration,
    /// Hard ceiling for any single wait, before jitter.
    pub max: Duration,
    /// Growth factor per consecutive failure.
    pub multiplier: f64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(5),
            max: Duration::from_secs(300),
            multiplier: 2.0,
        }
    }
}

/// The pre-jitter backoff ceiling for the `attempt`-th consecutive
/// failure (1-based). `attempt <= 0` is treated as 1. Saturates at
/// `policy.max`.
#[must_use]
pub fn backoff_ceiling(policy: &BackoffPolicy, attempt: u32) -> Duration {
    let attempt = attempt.max(1);
    let exp = f64::from(attempt - 1);
    let base_ms = policy.base.as_millis() as f64;
    let max_ms = policy.max.as_millis() as f64;
    let grown = base_ms * policy.multiplier.powf(exp);
    let capped = grown.min(max_ms).max(0.0);
    Duration::from_millis(capped as u64)
}

/// Apply **full jitter**: a uniformly random wait in `[0, ceiling]`.
/// `rand_unit` must be in `[0, 1)`; values outside are clamped. Full
/// jitter (rather than equal jitter) maximally decorrelates retries
/// across a pool, which is what we want for an account-wide limit.
#[must_use]
pub fn jittered(ceiling: Duration, rand_unit: f64) -> Duration {
    let unit = rand_unit.clamp(0.0, 1.0);
    let ms = ceiling.as_millis() as f64 * unit;
    Duration::from_millis(ms as u64)
}

/// Shared backoff state across one pool of supervised sessions.
pub struct RateLimitGovernor {
    policy: BackoffPolicy,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Consecutive rate-limit hits across the pool since the last success.
    consecutive: u32,
    /// While set and in the future, the whole pool must hold off.
    open_until: Option<Instant>,
}

impl RateLimitGovernor {
    /// A governor with the default backoff policy.
    #[must_use]
    pub fn new() -> Self {
        Self::with_policy(BackoffPolicy::default())
    }

    /// A governor with a custom backoff policy.
    #[must_use]
    pub fn with_policy(policy: BackoffPolicy) -> Self {
        Self {
            policy,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Consecutive rate-limit count since the last success.
    #[must_use]
    pub fn consecutive(&self) -> u32 {
        self.inner.lock().map(|i| i.consecutive).unwrap_or(0)
    }

    /// Record a rate-limit hit and return how long this caller should
    /// wait before retrying. Advances the shared counter and trips the
    /// pool-wide circuit breaker for that duration. `rand_unit` injects
    /// the jitter (use [`record_rate_limit`](Self::record_rate_limit) in
    /// production, which draws its own).
    pub fn record_rate_limit_with(&self, rand_unit: f64) -> Duration {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.consecutive = inner.consecutive.saturating_add(1);
        let wait = jittered(backoff_ceiling(&self.policy, inner.consecutive), rand_unit);
        inner.open_until = Instant::now().checked_add(wait);
        wait
    }

    /// Record a rate-limit hit drawing real jitter. See
    /// [`record_rate_limit_with`](Self::record_rate_limit_with).
    pub fn record_rate_limit(&self) -> Duration {
        // `rand` is already a dependency of the crate; a cheap thread-rng
        // draw is plenty for jitter.
        let unit: f64 = rand::random::<f64>();
        self.record_rate_limit_with(unit)
    }

    /// Record a successful turn: reset the consecutive counter and clear
    /// the circuit breaker so the pool resumes at full speed.
    pub fn record_success(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.consecutive = 0;
        inner.open_until = None;
    }

    /// Remaining pool-wide hold-off at `now`, or `None` if the pool may
    /// proceed. Pure in `now` for testing.
    #[must_use]
    pub fn hold_off_at(&self, now: Instant) -> Option<Duration> {
        let inner = self.inner.lock().ok()?;
        match inner.open_until {
            Some(deadline) if deadline > now => Some(deadline - now),
            _ => None,
        }
    }

    /// Remaining pool-wide hold-off right now.
    #[must_use]
    pub fn hold_off(&self) -> Option<Duration> {
        self.hold_off_at(Instant::now())
    }
}

impl Default for RateLimitGovernor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> BackoffPolicy {
        BackoffPolicy {
            base: Duration::from_secs(4),
            max: Duration::from_secs(60),
            multiplier: 2.0,
        }
    }

    #[test]
    fn ceiling_grows_exponentially_then_caps() {
        let p = pol();
        assert_eq!(backoff_ceiling(&p, 1), Duration::from_secs(4));
        assert_eq!(backoff_ceiling(&p, 2), Duration::from_secs(8));
        assert_eq!(backoff_ceiling(&p, 3), Duration::from_secs(16));
        assert_eq!(backoff_ceiling(&p, 4), Duration::from_secs(32));
        // 64 > max(60) → capped.
        assert_eq!(backoff_ceiling(&p, 5), Duration::from_secs(60));
        assert_eq!(backoff_ceiling(&p, 50), Duration::from_secs(60));
    }

    #[test]
    fn ceiling_treats_zero_attempt_as_first() {
        assert_eq!(backoff_ceiling(&pol(), 0), Duration::from_secs(4));
    }

    #[test]
    fn full_jitter_spans_zero_to_ceiling() {
        let c = Duration::from_secs(10);
        assert_eq!(jittered(c, 0.0), Duration::ZERO);
        assert_eq!(jittered(c, 0.5), Duration::from_secs(5));
        // clamps >=1.0 to the ceiling.
        assert_eq!(jittered(c, 1.0), Duration::from_secs(10));
        assert_eq!(jittered(c, 9.9), Duration::from_secs(10));
        // clamps negatives to 0.
        assert_eq!(jittered(c, -1.0), Duration::ZERO);
    }

    #[test]
    fn governor_advances_and_resets() {
        let g = RateLimitGovernor::with_policy(pol());
        assert_eq!(g.consecutive(), 0);
        // Use max jitter (1.0) so the wait equals the ceiling and is
        // deterministic.
        let w1 = g.record_rate_limit_with(1.0);
        assert_eq!(w1, Duration::from_secs(4));
        assert_eq!(g.consecutive(), 1);
        let w2 = g.record_rate_limit_with(1.0);
        assert_eq!(w2, Duration::from_secs(8));
        assert_eq!(g.consecutive(), 2);
        g.record_success();
        assert_eq!(g.consecutive(), 0);
        assert!(g.hold_off().is_none(), "success clears the breaker");
    }

    #[test]
    fn circuit_breaker_holds_then_clears() {
        let g = RateLimitGovernor::with_policy(pol());
        let now = Instant::now();
        g.record_rate_limit_with(1.0); // opens for ~4s
        let remaining = g.hold_off_at(now).expect("breaker should be open");
        // The governor stamps `open_until` from its own (slightly later)
        // `Instant::now()`, so remaining is ~4s plus a sliver. Assert the
        // ballpark, not an exact bound.
        assert!(
            remaining > Duration::from_secs(3) && remaining < Duration::from_secs(5),
            "remaining={remaining:?}"
        );
        // Far in the future the breaker has elapsed.
        let later = now + Duration::from_secs(10);
        assert!(g.hold_off_at(later).is_none());
    }

    #[test]
    fn zero_jitter_means_no_hold() {
        let g = RateLimitGovernor::with_policy(pol());
        let wait = g.record_rate_limit_with(0.0);
        assert_eq!(wait, Duration::ZERO);
        // open_until set to now+0 → already elapsed.
        assert!(g.hold_off().is_none());
    }
}
