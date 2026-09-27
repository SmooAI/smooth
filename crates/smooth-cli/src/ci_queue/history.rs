//! Finished jobs, in `<queue dir>/history.db` — SQLite, WAL, rolling.
//!
//! This is what admission estimates from: a label's recent peak memory and
//! CPU time. It is written by every `th` that finishes a job, concurrently, so
//! it rides SQLite's own locking (WAL + a busy timeout) like `~/.smooth/mail.db`
//! and `pearls.db` — not the queue mutex, which must stay millisecond-short.
//!
//! A history row is advisory. Every failure to read or write it is reported
//! and swallowed: a broken history must degrade estimates to the class
//! defaults, never block a commit.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::pressure::Readings;
use super::queue::Class;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct HistoryEntry {
    pub label: String,
    pub class: Option<Class>,
    pub cwd: PathBuf,
    /// The repo root the job ran in (the nearest ancestor with a `.git`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    /// sha256 (first 16 hex) of the job's argv.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd_hash: Option<String>,
    pub ticket: u64,
    pub queued_at_ms: u64,
    #[serde(default)]
    pub finished_at_ms: u64,
    pub wait_ms: u64,
    pub run_ms: u64,
    /// `exit`, `signal`, `timeout`, `spawn-failed`, or `wait-timeout`.
    pub outcome: String,
    /// The code `th ci-queue run` exited with.
    pub exit: i32,
    /// Peak of the SUM of RSS across the job's process group (sampled): what
    /// the job actually needed from the machine at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_group_rss_kb: Option<u64>,
    /// The largest single process (`ru_maxrss` and sampling). Parallel builds
    /// sit far below `peak_group_rss_kb` here — that gap is why the sum is
    /// what admission uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_single_rss_kb: Option<u64>,
    /// User + system CPU of the job and its reaped descendants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,
    /// What admission estimated for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est_rss_kb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub est_millicores: Option<u64>,
    /// Machine pressure when it was admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<Readings>,
}

/// One past run of a label, as the estimator sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub peak_group_rss_kb: u64,
    pub cpu_ms: u64,
    pub run_ms: u64,
}

pub struct History {
    path: PathBuf,
    keep: usize,
}

impl History {
    pub fn new(dir: &Path, keep: usize) -> Self {
        Self {
            path: dir.join("history.db"),
            keep: keep.max(1),
        }
    }

    fn open(&self) -> Result<Connection> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let c = Connection::open(&self.path).with_context(|| format!("opening {}", self.path.display()))?;
        c.busy_timeout(Duration::from_secs(5))?;
        c.pragma_update(None, "journal_mode", "WAL")?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY,
                label TEXT NOT NULL,
                class TEXT,
                cwd TEXT NOT NULL,
                repo TEXT,
                cmd_hash TEXT,
                ticket INTEGER NOT NULL,
                queued_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER NOT NULL,
                wait_ms INTEGER NOT NULL,
                run_ms INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                exit INTEGER NOT NULL,
                peak_group_rss_kb INTEGER,
                max_single_rss_kb INTEGER,
                cpu_ms INTEGER,
                est_rss_kb INTEGER,
                est_millicores INTEGER,
                pressure TEXT
            );
            CREATE INDEX IF NOT EXISTS jobs_by_label ON jobs(label, id);",
        )?;
        Ok(c)
    }

    pub fn record(&self, e: &HistoryEntry) -> Result<()> {
        let c = self.open()?;
        let pressure = e.pressure.as_ref().map(serde_json::to_string).transpose()?;
        c.execute(
            "INSERT INTO jobs (label, class, cwd, repo, cmd_hash, ticket, queued_at_ms, finished_at_ms, wait_ms, run_ms,
                               outcome, exit, peak_group_rss_kb, max_single_rss_kb, cpu_ms, est_rss_kb, est_millicores, pressure)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                e.label,
                e.class.map(Class::name),
                e.cwd.to_string_lossy(),
                e.repo.as_ref().map(|r| r.to_string_lossy().into_owned()),
                e.cmd_hash,
                to_i64(e.ticket),
                to_i64(e.queued_at_ms),
                to_i64(e.finished_at_ms),
                to_i64(e.wait_ms),
                to_i64(e.run_ms),
                e.outcome,
                e.exit,
                e.peak_group_rss_kb.map(to_i64),
                e.max_single_rss_kb.map(to_i64),
                e.cpu_ms.map(to_i64),
                e.est_rss_kb.map(to_i64),
                e.est_millicores.map(to_i64),
                pressure,
            ],
        )?;
        // Rolling: keep the newest `keep` rows.
        let max_id: Option<i64> = c.query_row("SELECT max(id) FROM jobs", [], |r| r.get(0)).optional()?.flatten();
        if let Some(max_id) = max_id {
            c.execute("DELETE FROM jobs WHERE id <= ?1", params![max_id - to_i64(self.keep as u64)])?;
        }
        Ok(())
    }

    /// The newest `n` jobs, oldest first.
    pub fn recent(&self, n: usize) -> Result<Vec<HistoryEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let c = self.open()?;
        let mut st = c.prepare(
            "SELECT label, class, cwd, repo, cmd_hash, ticket, queued_at_ms, finished_at_ms, wait_ms, run_ms, outcome, exit,
                    peak_group_rss_kb, max_single_rss_kb, cpu_ms, est_rss_kb, est_millicores, pressure
             FROM jobs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![to_i64(n as u64)], |r| {
            let class: Option<String> = r.get(1)?;
            let pressure: Option<String> = r.get(17)?;
            Ok(HistoryEntry {
                label: r.get(0)?,
                class: class.as_deref().and_then(Class::from_name),
                cwd: PathBuf::from(r.get::<_, String>(2)?),
                repo: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
                cmd_hash: r.get(4)?,
                ticket: to_u64(r.get(5)?),
                queued_at_ms: to_u64(r.get(6)?),
                finished_at_ms: to_u64(r.get(7)?),
                wait_ms: to_u64(r.get(8)?),
                run_ms: to_u64(r.get(9)?),
                outcome: r.get(10)?,
                exit: r.get(11)?,
                peak_group_rss_kb: r.get::<_, Option<i64>>(12)?.map(to_u64),
                max_single_rss_kb: r.get::<_, Option<i64>>(13)?.map(to_u64),
                cpu_ms: r.get::<_, Option<i64>>(14)?.map(to_u64),
                est_rss_kb: r.get::<_, Option<i64>>(15)?.map(to_u64),
                est_millicores: r.get::<_, Option<i64>>(16)?.map(to_u64),
                pressure: pressure.and_then(|p| serde_json::from_str(&p).ok()),
            })
        })?;
        let mut out: Vec<HistoryEntry> = rows.collect::<std::result::Result<_, _>>()?;
        out.reverse();
        Ok(out)
    }

    /// A label's last `window` runs that finished and were measured, newest
    /// first. Only runs that actually ran to an exit count: a timeout or a
    /// kill says nothing reliable about what the job needs.
    pub fn samples(&self, label: &str, window: usize) -> Result<Vec<Sample>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let c = self.open()?;
        let mut st = c.prepare(
            "SELECT peak_group_rss_kb, cpu_ms, run_ms FROM jobs
             WHERE label = ?1 AND outcome = 'exit' AND peak_group_rss_kb IS NOT NULL AND cpu_ms IS NOT NULL
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![label, to_i64(window as u64)], |r| {
            Ok(Sample {
                peak_group_rss_kb: to_u64(r.get(0)?),
                cpu_ms: to_u64(r.get(1)?),
                run_ms: to_u64(r.get(2)?),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// One-time import of the pre-SQLite `history.jsonl`, then set it aside
    /// (renamed, not deleted).
    pub fn import_jsonl(&self, jsonl: &Path) -> Result<usize> {
        let Ok(text) = std::fs::read_to_string(jsonl) else {
            return Ok(0);
        };
        let mut n = 0;
        for line in text.lines() {
            if let Ok(e) = serde_json::from_str::<HistoryEntry>(line) {
                self.record(&e)?;
                n += 1;
            }
        }
        std::fs::rename(jsonl, jsonl.with_extension("jsonl.imported"))?;
        Ok(n)
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn entry(label: &str, peak: Option<u64>, outcome: &str) -> HistoryEntry {
        HistoryEntry {
            label: label.into(),
            class: Some(Class::Heavy),
            cwd: "/work".into(),
            outcome: outcome.into(),
            run_ms: 1000,
            peak_group_rss_kb: peak,
            max_single_rss_kb: peak.map(|p| p / 2),
            cpu_ms: peak.map(|_| 2000),
            ..HistoryEntry::default()
        }
    }

    #[test]
    fn records_and_reads_back_every_field() {
        let tmp = tempfile::tempdir().unwrap();
        let h = History::new(tmp.path(), 100);
        let e = HistoryEntry {
            repo: Some("/work".into()),
            cmd_hash: Some("abc".into()),
            est_rss_kb: Some(7),
            est_millicores: Some(1500),
            pressure: Some(Readings {
                load1: Some(3.5),
                cores: 12,
                ..Readings::default()
            }),
            ..entry("typecheck", Some(4_000_000), "exit")
        };
        h.record(&e).unwrap();
        assert_eq!(h.recent(10).unwrap(), vec![e]);
    }

    #[test]
    fn keeps_only_the_newest_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let h = History::new(tmp.path(), 3);
        for i in 0..10 {
            h.record(&entry(&format!("j{i}"), Some(1), "exit")).unwrap();
        }
        let labels: Vec<String> = h.recent(100).unwrap().into_iter().map(|e| e.label).collect();
        assert_eq!(labels, ["j7", "j8", "j9"]);
    }

    #[test]
    fn samples_are_the_labels_measured_exits_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let h = History::new(tmp.path(), 100);
        h.record(&entry("clippy", Some(100), "exit")).unwrap();
        h.record(&entry("clippy", Some(200), "timeout")).unwrap(); // not a measurement
        h.record(&entry("clippy", None, "exit")).unwrap(); // unmeasured
        h.record(&entry("other", Some(999), "exit")).unwrap();
        h.record(&entry("clippy", Some(300), "exit")).unwrap();
        let s: Vec<u64> = h.samples("clippy", 10).unwrap().iter().map(|s| s.peak_group_rss_kb).collect();
        assert_eq!(s, [300, 100]);
        assert_eq!(h.samples("clippy", 1).unwrap().len(), 1);
    }

    #[test]
    fn no_database_reads_as_no_history() {
        let tmp = tempfile::tempdir().unwrap();
        let h = History::new(&tmp.path().join("never-created"), 10);
        assert!(h.recent(5).unwrap().is_empty());
        assert!(h.samples("x", 5).unwrap().is_empty());
    }

    #[test]
    fn imports_the_old_jsonl_once() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl = tmp.path().join("history.jsonl");
        std::fs::write(
            &jsonl,
            r#"{"label":"a","class":"heavy","cwd":"/w","ticket":1,"queued_at_ms":5,"wait_ms":0,"run_ms":9,"outcome":"exit","exit":0}
not json
"#,
        )
        .unwrap();
        let h = History::new(tmp.path(), 10);
        assert_eq!(h.import_jsonl(&jsonl).unwrap(), 1);
        assert!(!jsonl.exists());
        assert_eq!(h.recent(5).unwrap()[0].label, "a");
        assert_eq!(h.import_jsonl(&jsonl).unwrap(), 0, "a second import finds nothing");
    }
}
