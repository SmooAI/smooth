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
//!    window during rollout). A dead/stale addr (refused, or silent on two
//!    probes a second apart) fails the probe and we start.
//!
//! `SMOOTH_ALLOW_SECOND_DAEMON=1` skips both — the deliberate multi-instance
//! escape hatch for development.

use std::fs::{File, TryLockError};
use std::path::Path;
use std::time::Duration;

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

/// The one predicate for "this is a deliberate second instance": empty and
/// `0` count as unset, so the lock and the daemon.addr advertisement agree.
pub(crate) fn allow_second() -> bool {
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

/// What one `/health` probe says about the daemon at an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Liveness {
    /// It answered `/health` with a 2xx.
    Alive,
    /// Positive evidence nothing of ours is there: the connection was
    /// refused, or something else answered with a non-2xx.
    Dead,
    /// A timeout, a reset, a half-read response. A daemon that is merely slow
    /// (this machine runs dozens of agents; load averages of 40 are normal)
    /// looks exactly like this, so it is not evidence of death (th-4af55f).
    Inconclusive,
}

const FIRST_PROBE: Duration = Duration::from_millis(750);
const PROBE_GAP: Duration = Duration::from_secs(1);
const SECOND_PROBE: Duration = Duration::from_secs(3);

/// True when a daemon is serving `/health` at `addr`.
///
/// One probe decides when it is conclusive. An inconclusive one is asked
/// again after a pause, with a longer timeout, and only a second failure
/// counts as death. Before th-4af55f any error, including a 750 ms timeout,
/// meant "dead": a live daemon slow under load lost `flow.addr` to a second
/// one, and the single-instance check let a second daemon start beside a
/// slow pre-lock one.
pub(crate) async fn probe_health(addr: &str) -> bool {
    probe_liveness(addr, FIRST_PROBE, PROBE_GAP, SECOND_PROBE).await
}

/// [`probe_health`] with its timings as parameters, for tests.
async fn probe_liveness(addr: &str, first: Duration, gap: Duration, second: Duration) -> bool {
    match probe_once(addr, first).await {
        Liveness::Alive => true,
        Liveness::Dead => false,
        Liveness::Inconclusive => {
            tokio::time::sleep(gap).await;
            let again = probe_once(addr, second).await;
            tracing::debug!(addr, ?again, "health probe timed out once; asked again");
            again == Liveness::Alive
        }
    }
}

/// One `GET /health` at `addr` within `timeout`.
async fn probe_once(addr: &str, timeout: Duration) -> Liveness {
    let url = format!("http://{addr}/health");
    let Ok(client) = reqwest::Client::builder().timeout(timeout).build() else {
        return Liveness::Inconclusive;
    };
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Liveness::Alive,
        Ok(_) => Liveness::Dead,
        // A connect timeout is also `is_connect`, so test for it first.
        Err(e) if e.is_timeout() => Liveness::Inconclusive,
        Err(e) if e.is_connect() => Liveness::Dead,
        Err(_) => Liveness::Inconclusive,
    }
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

    /// A listener that answers each connection with `responses[i]` in turn,
    /// or accepts and says nothing when that entry is `None`.
    async fn scripted_server(responses: Vec<Option<&'static str>>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            #[allow(clippy::collection_is_never_read, reason = "owning the sockets is what keeps them open")]
            let mut held = Vec::new();
            for reply in responses {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                match reply {
                    Some(r) => {
                        let _ = sock.write_all(r.as_bytes()).await;
                    }
                    // Keep it open and silent: a daemon too busy to answer.
                    None => held.push(sock),
                }
            }
            std::future::pending::<()>().await;
        });
        addr
    }

    const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    const NOT_FOUND: &str = "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    const FAST: Duration = Duration::from_millis(200);
    /// Long enough for a refusal everywhere: Windows retries the SYN and
    /// reports a refused loopback connect only after ~2 s (Unix: at once).
    const REFUSAL: Duration = Duration::from_secs(8);

    #[tokio::test]
    async fn refused_is_dead_and_a_2xx_is_alive() {
        assert_eq!(probe_once("127.0.0.1:1", REFUSAL).await, Liveness::Dead);
        let addr = scripted_server(vec![Some(OK)]).await;
        assert_eq!(probe_once(&addr, FAST).await, Liveness::Alive);
    }

    #[tokio::test]
    async fn something_else_answering_is_dead() {
        let addr = scripted_server(vec![Some(NOT_FOUND)]).await;
        assert_eq!(probe_once(&addr, FAST).await, Liveness::Dead);
    }

    /// th-4af55f: a timeout is not evidence of death.
    #[tokio::test]
    async fn a_timeout_is_inconclusive() {
        let addr = scripted_server(vec![None]).await;
        assert_eq!(probe_once(&addr, FAST).await, Liveness::Inconclusive);
    }

    /// The bug: a daemon too slow for the first probe lost its claim.
    #[tokio::test]
    async fn a_daemon_slow_once_is_still_alive() {
        let addr = scripted_server(vec![None, Some(OK)]).await;
        assert!(probe_liveness(&addr, FAST, Duration::from_millis(50), FAST).await);
    }

    #[tokio::test]
    async fn two_timeouts_apart_are_dead() {
        let addr = scripted_server(vec![None, None]).await;
        assert!(!probe_liveness(&addr, FAST, Duration::from_millis(50), FAST).await);
    }

    #[tokio::test]
    async fn refused_is_dead_without_a_second_probe() {
        let started = std::time::Instant::now();
        assert!(!probe_liveness("127.0.0.1:1", REFUSAL, Duration::from_secs(60), REFUSAL).await);
        assert!(started.elapsed() < Duration::from_secs(30), "a refusal is conclusive: no pause, no retry");
    }
}
