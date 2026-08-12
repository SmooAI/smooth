//! Single-instance enforcement — exactly ONE Big Smooth per machine.
//!
//! Two daemons ran side by side on 2026-08-10 (the app bundle's on :8899, a
//! `th`-launched one on :4400): they shared `~/.smooth/operator-storage.db`,
//! fought over `daemon.addr`, and clients discovered the one WITHOUT the
//! macOS Calendar TCC grant (pearl th-c71e6f). Two layers, both cheap:
//!
//! 1. **Advisory file lock** on `~/.smooth/daemon.lock`, held for the process
//!    lifetime. The OS releases it when the process dies, so there is no
//!    stale-pid problem to solve.
//! 2. **Health probe** of the address advertised in `daemon.addr` — catches a
//!    live daemon from a build that predates the lock (the mixed-version
//!    window during rollout). A dead/stale addr fails the probe and we start.
//!
//! `SMOOTH_ALLOW_SECOND_DAEMON=1` skips both — the deliberate multi-instance
//! escape hatch for development.

use std::fs::{File, TryLockError};
use std::path::Path;

use anyhow::{Context, Result};

/// Held for the daemon's lifetime; dropping it (or dying) releases the lock.
#[derive(Debug)]
pub struct InstanceLock {
    // None when SMOOTH_ALLOW_SECOND_DAEMON bypassed the lock.
    _file: Option<File>,
}

/// Take the machine-wide lock under `~/.smooth`, then probe for a pre-lock-era
/// daemon. Call before binding anything.
pub async fn acquire_default() -> Result<InstanceLock> {
    if allow_second() {
        tracing::warn!("SMOOTH_ALLOW_SECOND_DAEMON set — skipping single-instance enforcement");
        return Ok(InstanceLock { _file: None });
    }
    let dir = dirs_next::home_dir()
        .map(|h| h.join(".smooth"))
        .context("no home dir for ~/.smooth/daemon.lock")?;
    let lock = acquire_lock(&dir)?;
    // Belt and suspenders: an older daemon build holds no lock, but it DOES
    // advertise itself. A live /health there means a real daemon, not us.
    if let Some(addr) = advertised_addr(&dir) {
        if probe_health(&addr).await {
            anyhow::bail!(
                "another Big Smooth daemon is already serving at http://{addr} (it predates the single-instance lock). \
                 Refusing to start a second one — they would fight over ~/.smooth/operator-storage.db and daemon.addr. \
                 Stop it first, or set SMOOTH_ALLOW_SECOND_DAEMON=1 to run both anyway."
            );
        }
    }
    Ok(lock)
}

fn allow_second() -> bool {
    std::env::var("SMOOTH_ALLOW_SECOND_DAEMON").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Lock `<dir>/daemon.lock` exclusively. Pure over its dir for tests.
fn acquire_lock(dir: &Path) -> Result<InstanceLock> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("daemon.lock");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { _file: Some(file) }),
        Err(TryLockError::WouldBlock) => {
            let hint = advertised_addr(dir).map_or_else(String::new, |a| format!(" (it advertises http://{a})"));
            anyhow::bail!(
                "another Big Smooth daemon is already running on this machine{hint}. \
                 Refusing to start a second one — they would fight over ~/.smooth/operator-storage.db and daemon.addr. \
                 Stop it first (`th down`, or quit the Big Smooth app), or set SMOOTH_ALLOW_SECOND_DAEMON=1 to run both anyway."
            )
        }
        Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("locking {}", path.display())),
    }
}

/// The `host:port` a running daemon advertised in `<dir>/daemon.addr`, if any.
fn advertised_addr(dir: &Path) -> Option<String> {
    let addr = std::fs::read_to_string(dir.join("daemon.addr")).ok()?;
    let addr = addr.trim();
    (!addr.is_empty()).then(|| addr.to_owned())
}

/// True when a daemon answers `/health` at `addr` quickly. Conservative on
/// error: unreachable/slow means "no live daemon", and we proceed to start.
async fn probe_health(addr: &str) -> bool {
    let url = format!("http://{addr}/health");
    let Ok(client) = reqwest::Client::builder().timeout(std::time::Duration::from_millis(750)).build() else {
        return false;
    };
    matches!(client.get(&url).send().await, Ok(r) if r.status().is_success())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn first_acquire_succeeds_and_creates_the_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let lock = acquire_lock(dir.path()).unwrap();
        assert!(dir.path().join("daemon.lock").is_file());
        drop(lock);
    }

    #[test]
    fn second_acquire_fails_while_the_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let _held = acquire_lock(dir.path()).unwrap();
        // A second open of the same path is a distinct file description, so
        // the OS reports the conflict even within one process.
        let err = acquire_lock(dir.path()).unwrap_err().to_string();
        assert!(err.contains("already running"), "unhelpful message: {err}");
    }

    #[test]
    fn conflict_message_carries_the_advertised_addr() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("daemon.addr"), "127.0.0.1:8899").unwrap();
        let _held = acquire_lock(dir.path()).unwrap();
        let err = acquire_lock(dir.path()).unwrap_err().to_string();
        assert!(err.contains("http://127.0.0.1:8899"), "addr hint missing: {err}");
    }

    #[test]
    fn lock_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        drop(acquire_lock(dir.path()).unwrap());
        acquire_lock(dir.path()).expect("lock must be reacquirable after drop");
    }

    #[test]
    fn advertised_addr_ignores_missing_and_blank_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(advertised_addr(dir.path()), None);
        std::fs::write(dir.path().join("daemon.addr"), "  \n").unwrap();
        assert_eq!(advertised_addr(dir.path()), None);
        std::fs::write(dir.path().join("daemon.addr"), "127.0.0.1:4400\n").unwrap();
        assert_eq!(advertised_addr(dir.path()).as_deref(), Some("127.0.0.1:4400"));
    }

    #[tokio::test]
    async fn probe_health_is_false_when_nothing_listens() {
        // Port 1 on loopback: connection refused, fast.
        assert!(!probe_health("127.0.0.1:1").await);
    }
}
