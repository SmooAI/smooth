//! The scheduler tick — what makes the always-on agent *proactive*.
//!
//! A background loop ([`spawn_scheduler`]) wakes periodically, asks the
//! [`ScheduleStore`](crate::schedule::ScheduleStore) which schedules are due,
//! and fires each one's prompt into a per-schedule session via the same
//! coordinator + [`run_task`](crate::runner::run_task) path a live client uses —
//! then advances the schedule's `next_due`. Scheduled runs have no connected
//! client, so their events are drained (they still persist to the durable event
//! log + conversation history, recoverable via `/api/session`).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::runner::{self, RunDeps, TaskSpec};
use crate::schedule::Schedule;
use crate::server::{load_prior, resolve_workspace, AppState};
use crate::session::SessionStatus;
use crate::wire::ServerEvent;

/// How often the scheduler wakes to check for due schedules.
const TICK_INTERVAL: Duration = Duration::from_secs(30);

/// Spawn the always-on scheduler loop. Detached: it lives for the daemon
/// process. No-op shutdown handling — the process ending stops it.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK_INTERVAL);
        tick.tick().await; // consume the immediate first tick
        loop {
            tick.tick().await;
            scheduler_tick(&state, Utc::now()).await;
        }
    });
}

/// Run one scheduler tick: fire every due schedule and advance it. Separated
/// from the loop so the fire/advance logic is testable without waiting on a
/// real interval.
pub(crate) async fn scheduler_tick(state: &AppState, now: DateTime<Utc>) {
    let due = match state.schedules.due(now).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "scheduler: due query failed");
            return;
        }
    };
    for mut schedule in due {
        tracing::info!(id = %schedule.id, "scheduler: firing schedule");
        dispatch_schedule(state, &schedule).await;
        schedule.mark_fired(now);
        if let Err(e) = state.schedules.upsert(schedule).await {
            tracing::warn!(error = %e, "scheduler: failed to advance schedule");
        }
    }
}

/// Fire a schedule's prompt into its dedicated `schedule:{id}` session. The
/// session setup is awaited (deterministic); the agent run itself is handed to
/// the coordinator, which spawns it (and skips if that session is already
/// running). Events are drained — they persist to the event log anyway.
async fn dispatch_schedule(state: &AppState, schedule: &Schedule) {
    let session_id = format!("schedule:{}", schedule.id);
    // Ensure the session exists (titled so the sessions list reads well).
    let title = format!("⏰ {}", schedule.prompt.chars().take(40).collect::<String>());
    let _ = state.sessions.create(Some(session_id.clone()), Some(title)).await;
    let prior_messages = load_prior(state, &session_id).await;

    let task_id = uuid::Uuid::new_v4().to_string();
    let spec = TaskSpec {
        task_id: task_id.clone(),
        session_id: session_id.clone(),
        message: schedule.prompt.clone(),
        model: None,
        budget: None,
        prior_messages,
        workspace: resolve_workspace(None),
    };
    // No client → drain the event stream (the EventStore still records it).
    let (out, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let deps = RunDeps {
        out,
        events: Arc::clone(&state.events),
        messages: Arc::clone(&state.messages),
        approvals: Arc::clone(&state.approvals),
        mode: state.permission_mode.get(),
        egress_proxy: state.egress_proxy.clone(),
        memory: Arc::clone(&state.memory),
    };
    let run = async move { runner::run_task(spec, deps).await };
    if state.coordinator.try_start(session_id.clone(), task_id, run).is_ok() {
        let _ = state.sessions.set_status(&session_id, SessionStatus::Active).await;
    } else {
        tracing::info!(id = %schedule.id, "scheduler: session busy, skipping this fire");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use crate::schedule::ScheduleKind;

    #[tokio::test]
    async fn tick_fires_due_schedules_and_advances_them() {
        // Make any spawned run fail fast (no LLM) so the fire-and-forget dispatch
        // doesn't hang — we only assert on the schedule advancing.
        std::env::remove_var("SMOOTH_API_URL");
        std::env::remove_var("SMOOTH_API_KEY");
        std::env::set_var("SMOOTH_PROVIDERS_FILE", "/nonexistent/smooth-daemon/sched-test.json");

        let state = AppState::new();
        let now = DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z").unwrap().with_timezone(&Utc);

        // One due (past next_due), one not.
        let mut due = Schedule::new("a", "morning brief", ScheduleKind::EveryNSeconds { secs: 3600 }, now);
        due.next_due = DateTime::parse_from_rfc3339("2026-06-23T11:00:00Z").unwrap().with_timezone(&Utc);
        state.schedules.upsert(due).await.unwrap();
        state
            .schedules
            .upsert(Schedule::new("b", "later", ScheduleKind::EveryNSeconds { secs: 3600 }, now))
            .await
            .unwrap();

        scheduler_tick(&state, now).await;

        // The due schedule advanced (last_run set, next_due moved past `now`).
        let all = state.schedules.list().await.unwrap();
        let a = all.iter().find(|s| s.id == "a").unwrap();
        assert_eq!(a.last_run, Some(now), "due schedule recorded a firing");
        assert!(a.next_due > now, "next_due advanced past now: {}", a.next_due);
        // The not-yet-due one is untouched.
        let b = all.iter().find(|s| s.id == "b").unwrap();
        assert!(b.last_run.is_none(), "not-due schedule didn't fire");

        // A `schedule:a` session was created for the run.
        assert!(state.sessions.get("schedule:a").await.unwrap().is_some(), "per-schedule session created");
    }
}
