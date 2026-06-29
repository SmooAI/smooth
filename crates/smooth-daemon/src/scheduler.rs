//! The scheduler tick loop — the always-on agent's proactivity (EPIC th-c89c2a,
//! th-2ff975).
//!
//! On a cadence, due [`Schedule`](crate::schedule::Schedule)s are fired into the
//! operator as fresh turns. The transport is abstracted behind [`TurnDriver`] so
//! the loop logic is testable without a live server; the production driver
//! ([`OperatorTurnDriver`], next slice) connects to the daemon's own operator as
//! a **WS client** and sends the canonical `send_message` — proactivity is "just
//! another client on the protocol," no operator-side change needed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::schedule::ScheduleStore;

/// Drives a scheduled prompt into the agent as a fresh turn.
///
/// One method, so a test can record calls and the production impl can be a WS
/// client. Implementations should be cheap to clone/share (`Arc`-friendly).
#[async_trait]
pub trait TurnDriver: Send + Sync {
    /// Fire `prompt` into the agent. Returns once the turn is accepted (the
    /// production driver may stream + drain the response); an `Err` means the
    /// firing failed and the schedule is left due for the next tick.
    ///
    /// # Errors
    /// Returns an error if the turn could not be dispatched.
    async fn drive(&self, prompt: &str) -> anyhow::Result<()>;
}

/// Run one scheduler pass at `now`: fire every due schedule, then advance +
/// persist it. A driver error leaves that schedule due (logged, retried next
/// tick) rather than dropping the firing. Returns how many fired successfully.
///
/// # Errors
/// Returns an error only if the store can't be read; per-schedule driver
/// failures are logged and skipped, not propagated.
pub async fn tick(store: &dyn ScheduleStore, driver: &dyn TurnDriver, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let due = store.due(now).await?;
    let mut fired = 0;
    for mut schedule in due {
        match driver.drive(&schedule.prompt).await {
            Ok(()) => {
                schedule.mark_fired(now);
                store.upsert(schedule).await?;
                fired += 1;
            }
            Err(e) => {
                tracing::warn!(id = %schedule.id, error = %e, "scheduled turn failed; leaving due for the next tick");
            }
        }
    }
    Ok(fired)
}

/// Spawn the scheduler loop on a background task: every `interval`, run [`tick`]
/// against `now`. Errors reading the store are logged, never fatal — the
/// always-on agent keeps ticking. Returns the task handle (drop = keep running).
#[must_use]
pub fn spawn_scheduler(store: Arc<dyn ScheduleStore>, driver: Arc<dyn TurnDriver>, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // The first `tick()` resolves immediately; skip it so a just-booted
        // daemon doesn't fire a backlog before it's fully up.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match tick(store.as_ref(), driver.as_ref(), Utc::now()).await {
                Ok(n) if n > 0 => tracing::info!(fired = n, "scheduler fired due tasks"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "scheduler tick failed to read the store"),
            }
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use std::sync::Mutex;

    use chrono::TimeZone;

    use super::*;
    use crate::schedule::{InMemoryScheduleStore, Schedule, ScheduleKind};

    /// A [`TurnDriver`] that records the prompts it was asked to drive.
    #[derive(Default)]
    struct RecordingDriver {
        prompts: Mutex<Vec<String>>,
        fail: bool,
    }

    #[async_trait]
    impl TurnDriver for RecordingDriver {
        async fn drive(&self, prompt: &str) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("driver down");
            }
            self.prompts.lock().unwrap().push(prompt.to_string());
            Ok(())
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        Utc.from_utc_datetime(&DateTime::parse_from_rfc3339(s).unwrap().naive_utc())
    }

    #[tokio::test]
    async fn tick_fires_due_and_advances() {
        let now = at("2026-06-23T12:00:00Z");
        let store = InMemoryScheduleStore::new();
        // One past-due, one future.
        let mut due = Schedule::new("a", "morning brief", ScheduleKind::EveryNSeconds { secs: 3600 }, now);
        due.next_due = at("2026-06-23T11:59:00Z");
        store.upsert(due).await.unwrap();
        store
            .upsert(Schedule::new("b", "nightly", ScheduleKind::EveryNSeconds { secs: 3600 }, now))
            .await
            .unwrap();

        let driver = RecordingDriver::default();
        let fired = tick(&store, &driver, now).await.unwrap();

        assert_eq!(fired, 1, "only the due one fires");
        assert_eq!(*driver.prompts.lock().unwrap(), vec!["morning brief".to_string()]);
        // Advanced past `now`, so it's no longer due this instant.
        assert!(store.due(now).await.unwrap().is_empty(), "fired schedule advanced");
    }

    #[tokio::test]
    async fn tick_leaves_schedule_due_when_driver_fails() {
        let now = at("2026-06-23T12:00:00Z");
        let store = InMemoryScheduleStore::new();
        let mut due = Schedule::new("a", "brief", ScheduleKind::EveryNSeconds { secs: 3600 }, now);
        due.next_due = at("2026-06-23T11:59:00Z");
        store.upsert(due).await.unwrap();

        let driver = RecordingDriver {
            fail: true,
            ..Default::default()
        };
        let fired = tick(&store, &driver, now).await.unwrap();

        assert_eq!(fired, 0, "the failed firing doesn't count");
        assert_eq!(store.due(now).await.unwrap().len(), 1, "still due — retried next tick");
    }
}
