//! On-disk registry of supervised Claude sessions.
//!
//! Each running supervisor owns one JSON file under
//! `~/.smooth/claude/sessions/<id>.json`. A directory-of-files (rather
//! than one shared file) means concurrent supervisors never race on a
//! write — each owns its own file. `ls`/`attach` read the directory and
//! prune entries whose tmux session has died.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A supervised session as recorded on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    /// Short, human-friendly id (also the registry file stem).
    pub id: String,
    /// tmux session name.
    pub session: String,
    /// tmux socket (`-L`) this session lives on.
    pub socket: String,
    /// Working directory the session was launched in.
    pub cwd: String,
    /// Optional label/role for display.
    pub label: Option<String>,
    /// PID of the supervising `th` process.
    pub pid: u32,
    /// When the session was launched.
    pub started_at: DateTime<Utc>,
}

/// `~/.smooth/claude/sessions`.
#[must_use]
pub fn registry_dir() -> PathBuf {
    dirs_next::home_dir().unwrap_or_default().join(".smooth").join("claude").join("sessions")
}

fn entry_path(id: &str) -> PathBuf {
    registry_dir().join(format!("{id}.json"))
}

/// Persist `entry`, creating the registry directory if needed.
///
/// # Errors
/// On directory creation, serialization, or write failure.
pub fn write_entry(entry: &SessionEntry) -> Result<PathBuf> {
    let dir = registry_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating registry dir {}", dir.display()))?;
    let path = entry_path(&entry.id);
    let json = serde_json::to_string_pretty(entry).context("serializing session entry")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Remove a session's registry file. Missing file is not an error.
pub fn remove_entry(id: &str) {
    let _ = std::fs::remove_file(entry_path(id));
}

/// Read every registry entry (without liveness checking).
///
/// # Errors
/// Never — unreadable/corrupt files are skipped so one bad file can't
/// break `ls`.
#[must_use]
pub fn read_all() -> Vec<SessionEntry> {
    let dir = registry_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(entry) = serde_json::from_str::<SessionEntry>(&text) {
                out.push(entry);
            }
        }
    }
    out.sort_by_key(|e| e.started_at);
    out
}

/// True if a tmux session is still present on its socket.
#[must_use]
pub fn is_session_live(entry: &SessionEntry) -> bool {
    Command::new("tmux")
        .args(["-L", &entry.socket, "has-session", "-t", &entry.session])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Read all entries, removing the registry files of any whose tmux
/// session has died, and return only the live ones.
#[must_use]
pub fn read_live_and_prune() -> Vec<SessionEntry> {
    let mut live = Vec::new();
    for entry in read_all() {
        if is_session_live(&entry) {
            live.push(entry);
        } else {
            remove_entry(&entry.id);
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_string(),
            session: format!("sess-{id}"),
            socket: format!("sock-{id}"),
            cwd: "/tmp".to_string(),
            label: Some("fixer".to_string()),
            pid: 4242,
            started_at: "2026-06-29T12:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn registry_dir_is_under_smooth() {
        let d = registry_dir();
        assert!(d.ends_with("claude/sessions"), "unexpected dir: {}", d.display());
    }

    #[test]
    fn entry_roundtrips_through_serde() {
        let e = sample("abc123");
        let json = serde_json::to_string(&e).unwrap();
        let back: SessionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, e.id);
        assert_eq!(back.session, e.session);
        assert_eq!(back.socket, e.socket);
        assert_eq!(back.pid, e.pid);
        assert_eq!(back.started_at, e.started_at);
    }

    #[test]
    fn dead_session_is_not_live() {
        // A socket that doesn't exist → has-session fails → not live.
        let e = sample("definitely-not-running-xyz");
        assert!(!is_session_live(&e));
    }
}
