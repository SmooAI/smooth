//! Scheduled (proactive) tasks for the always-on agent.
//!
//! A [`Schedule`] re-enters a prompt into the daemon on a cadence — the
//! hermes-style "do this every morning / every N minutes" capability. The
//! always-on agent's schedules **survive restart** via [`SqliteScheduleStore`];
//! [`InMemoryScheduleStore`] is the dev/test backend. The tick loop that fires
//! due schedules into the operator lands in a following slice.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// How often a scheduled task fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleKind {
    /// Fire every `secs` seconds (minimum 1).
    EveryNSeconds { secs: u64 },
    /// Fire once per day at the given **UTC** `hour`:`minute`.
    DailyAt { hour: u8, minute: u8 },
}

impl ScheduleKind {
    /// The next fire time **strictly after** `after`.
    #[must_use]
    pub fn next_after(self, after: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::EveryNSeconds { secs } => after + Duration::seconds(i64::try_from(secs.max(1)).unwrap_or(i64::MAX)),
            Self::DailyAt { hour, minute } => {
                let h = u32::from(hour.min(23));
                let m = u32::from(minute.min(59));
                // Candidate today at h:m:00 UTC.
                let naive = after.date_naive().and_hms_opt(h, m, 0).unwrap_or_else(|| after.naive_utc());
                let candidate = Utc.from_utc_datetime(&naive);
                if candidate > after {
                    candidate
                } else {
                    candidate + Duration::days(1)
                }
            }
        }
    }
}

/// A scheduled task: a prompt fired on a [`ScheduleKind`] cadence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Stable id.
    pub id: String,
    /// The prompt re-entered into the daemon when this fires.
    pub prompt: String,
    /// The cadence.
    pub kind: ScheduleKind,
    /// Whether it's active (a disabled schedule never fires).
    pub enabled: bool,
    /// The next time this should fire.
    pub next_due: DateTime<Utc>,
    /// When it last fired, if ever.
    pub last_run: Option<DateTime<Utc>>,
}

impl Schedule {
    /// Create a new enabled schedule, first due at the next cadence point after
    /// `now`.
    #[must_use]
    pub fn new(id: impl Into<String>, prompt: impl Into<String>, kind: ScheduleKind, now: DateTime<Utc>) -> Self {
        Self {
            id: id.into(),
            prompt: prompt.into(),
            kind,
            enabled: true,
            next_due: kind.next_after(now),
            last_run: None,
        }
    }

    /// Whether this should fire at `now` (enabled and past its due time).
    #[must_use]
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        self.enabled && now >= self.next_due
    }

    /// Record a firing at `now` and advance `next_due` to the next cadence point.
    pub fn mark_fired(&mut self, now: DateTime<Utc>) {
        self.last_run = Some(now);
        self.next_due = self.kind.next_after(now);
    }
}

/// Durable storage for [`Schedule`]s.
#[async_trait]
pub trait ScheduleStore: Send + Sync {
    /// Persist a new (or replace an existing) schedule.
    ///
    /// # Errors
    /// Returns an error if the store cannot be written.
    async fn upsert(&self, schedule: Schedule) -> anyhow::Result<()>;

    /// All schedules, newest-`next_due` last.
    ///
    /// # Errors
    /// Returns an error if the store cannot be read.
    async fn list(&self) -> anyhow::Result<Vec<Schedule>>;

    /// Enabled schedules whose `next_due` is at or before `now`.
    ///
    /// # Errors
    /// Returns an error if the store cannot be read.
    async fn due(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Schedule>>;

    /// Remove a schedule (no-op if unknown).
    ///
    /// # Errors
    /// Returns an error if the store cannot be written.
    async fn delete(&self, id: &str) -> anyhow::Result<()>;
}

/// In-memory [`ScheduleStore`] — the dev/test backend (not durable).
#[derive(Debug, Default)]
pub struct InMemoryScheduleStore {
    inner: Mutex<HashMap<String, Schedule>>,
}

impl InMemoryScheduleStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Schedule>> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl ScheduleStore for InMemoryScheduleStore {
    async fn upsert(&self, schedule: Schedule) -> anyhow::Result<()> {
        self.lock().insert(schedule.id.clone(), schedule);
        Ok(())
    }

    async fn list(&self) -> anyhow::Result<Vec<Schedule>> {
        let mut out: Vec<Schedule> = self.lock().values().cloned().collect();
        out.sort_by_key(|s| s.next_due);
        Ok(out)
    }

    async fn due(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Schedule>> {
        let mut out: Vec<Schedule> = self.lock().values().filter(|s| s.is_due(now)).cloned().collect();
        out.sort_by_key(|s| s.next_due);
        Ok(out)
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.lock().remove(id);
        Ok(())
    }
}

/// Durable [`ScheduleStore`] backed by a local sqlite file — the always-on
/// agent's proactive schedules survive a daemon restart, with no external
/// service (mirrors the operator's local sqlite storage). Each schedule is one
/// JSON row keyed by id; the row set is small (one per scheduled task), so reads
/// load all rows and filter/sort in memory.
pub struct SqliteScheduleStore {
    db: Mutex<Connection>,
}

impl SqliteScheduleStore {
    /// Open (or create) the schedule store at `path`.
    ///
    /// # Errors
    /// Returns an error if the sqlite file can't be opened or the schema can't
    /// be created.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS schedules (id TEXT PRIMARY KEY, json TEXT NOT NULL);")?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Load every stored schedule (unordered).
    fn all(&self) -> Result<Vec<Schedule>> {
        let db = self.lock();
        let mut stmt = db.prepare("SELECT json FROM schedules")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str::<Schedule>(&row?)?);
        }
        Ok(out)
    }
}

#[async_trait]
impl ScheduleStore for SqliteScheduleStore {
    async fn upsert(&self, schedule: Schedule) -> Result<()> {
        let json = serde_json::to_string(&schedule)?;
        self.lock().execute(
            "INSERT OR REPLACE INTO schedules (id, json) VALUES (?1, ?2)",
            rusqlite::params![schedule.id, json],
        )?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        let mut out = self.all()?;
        out.sort_by_key(|s| s.next_due);
        Ok(out)
    }

    async fn due(&self, now: DateTime<Utc>) -> Result<Vec<Schedule>> {
        let mut out: Vec<Schedule> = self.all()?.into_iter().filter(|s| s.is_due(now)).collect();
        out.sort_by_key(|s| s.next_due);
        Ok(out)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.lock().execute("DELETE FROM schedules WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn every_n_seconds_advances_by_n() {
        let k = ScheduleKind::EveryNSeconds { secs: 90 };
        assert_eq!(k.next_after(at("2026-06-23T12:00:00Z")), at("2026-06-23T12:01:30Z"));
        // Zero is clamped to 1s so the scheduler can't busy-loop.
        let z = ScheduleKind::EveryNSeconds { secs: 0 };
        assert_eq!(z.next_after(at("2026-06-23T12:00:00Z")), at("2026-06-23T12:00:01Z"));
    }

    #[test]
    fn daily_at_picks_today_then_tomorrow() {
        let k = ScheduleKind::DailyAt { hour: 9, minute: 30 };
        // Before today's 09:30 → today.
        assert_eq!(k.next_after(at("2026-06-23T08:00:00Z")), at("2026-06-23T09:30:00Z"));
        // Exactly at 09:30 (not strictly after) → tomorrow.
        assert_eq!(k.next_after(at("2026-06-23T09:30:00Z")), at("2026-06-24T09:30:00Z"));
        // After today's 09:30 → tomorrow.
        assert_eq!(k.next_after(at("2026-06-23T18:00:00Z")), at("2026-06-24T09:30:00Z"));
    }

    #[test]
    fn out_of_range_daily_components_are_clamped() {
        let k = ScheduleKind::DailyAt { hour: 99, minute: 99 };
        assert_eq!(k.next_after(at("2026-06-23T08:00:00Z")), at("2026-06-23T23:59:00Z"));
    }

    #[test]
    fn schedule_due_and_advance_lifecycle() {
        let now = at("2026-06-23T12:00:00Z");
        let mut s = Schedule::new("s1", "summarize my inbox", ScheduleKind::EveryNSeconds { secs: 60 }, now);
        assert_eq!(s.next_due, at("2026-06-23T12:01:00Z"));
        assert!(!s.is_due(now), "not due before next_due");
        assert!(s.is_due(at("2026-06-23T12:01:00Z")), "due at next_due");

        s.mark_fired(at("2026-06-23T12:01:05Z"));
        assert_eq!(s.last_run, Some(at("2026-06-23T12:01:05Z")));
        assert_eq!(s.next_due, at("2026-06-23T12:02:05Z"), "advanced from the fire time");

        // Disabled never fires.
        s.enabled = false;
        assert!(!s.is_due(at("2026-06-23T13:00:00Z")));
    }

    #[test]
    fn schedule_kind_serde_round_trips() {
        for k in [ScheduleKind::EveryNSeconds { secs: 300 }, ScheduleKind::DailyAt { hour: 7, minute: 0 }] {
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(serde_json::from_str::<ScheduleKind>(&json).unwrap(), k);
        }
    }

    #[tokio::test]
    async fn in_memory_store_upsert_list_due_delete() {
        let now = at("2026-06-23T12:00:00Z");
        let store = InMemoryScheduleStore::new();
        // Due now (next_due in the past) vs. not-yet-due.
        let mut past = Schedule::new("a", "morning brief", ScheduleKind::DailyAt { hour: 6, minute: 0 }, now);
        past.next_due = at("2026-06-23T11:59:00Z");
        let future = Schedule::new("b", "nightly", ScheduleKind::EveryNSeconds { secs: 3600 }, now);
        store.upsert(past.clone()).await.unwrap();
        store.upsert(future).await.unwrap();

        assert_eq!(store.list().await.unwrap().len(), 2);
        let due = store.due(now).await.unwrap();
        assert_eq!(due.len(), 1, "only the past-due one is due");
        assert_eq!(due[0].id, "a");

        // A disabled schedule, even if past-due, is not returned.
        past.enabled = false;
        store.upsert(past).await.unwrap();
        assert!(store.due(now).await.unwrap().is_empty());

        store.delete("a").await.unwrap();
        assert_eq!(store.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sqlite_store_persists_across_reopen() {
        let now = at("2026-06-23T12:00:00Z");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedules.db");

        {
            let store = SqliteScheduleStore::open(&path).unwrap();
            let mut past = Schedule::new("a", "morning brief", ScheduleKind::DailyAt { hour: 6, minute: 0 }, now);
            past.next_due = at("2026-06-23T11:59:00Z");
            store.upsert(past).await.unwrap();
            store
                .upsert(Schedule::new("b", "nightly", ScheduleKind::EveryNSeconds { secs: 3600 }, now))
                .await
                .unwrap();
        }

        // Reopen — the schedules survive the "restart".
        let store = SqliteScheduleStore::open(&path).unwrap();
        assert_eq!(store.list().await.unwrap().len(), 2, "both schedules persisted");
        let due = store.due(now).await.unwrap();
        assert_eq!(due.len(), 1, "only the past-due one is due");
        assert_eq!(due[0].id, "a");
        assert_eq!(due[0].prompt, "morning brief");

        store.delete("a").await.unwrap();
        assert_eq!(store.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sqlite_store_fresh_db_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteScheduleStore::open(&dir.path().join("s.db")).unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }
}
