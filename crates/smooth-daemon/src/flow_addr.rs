//! `~/.smooth/flow.addr` — where harness hooks find the flow engine
//! (th-c103c1).
//!
//! **The problem.** `flow-hook.sh` discovered the daemon through
//! `~/.smooth/daemon.addr`. Since PR #546 the SmoothFlow app's child daemon
//! deliberately does NOT write that file — Big Smooth owns it (th-3e6b1b:
//! a second instance repointing `th`, the hooks and Big Smooth's own clients
//! at itself is exactly the bug that file fixed). So on a machine where
//! SmoothFlow is the ONLY daemon, hooks had nowhere to post at all, and
//! adopting a plain `claude` could never fire.
//!
//! **The answer.** A second file, owned by the flow engine rather than by the
//! daemon identity. Both daemons host a flow engine and both share one
//! `~/.smooth/flow.db`, so either one can service a hook correctly — what
//! matters is that SOME live flow engine is reachable. The claim rule is
//! therefore "first live one wins, a stale file is taken over":
//!
//! * no file, or a file naming an address that no longer answers → claim it;
//! * a file naming a LIVE daemon → leave it alone (that daemon is serving
//!   hooks; ours still sees the writes, because the store is shared);
//! * our own address → rewrite (a restart on the same port).
//!
//! Released on shutdown, and only when it is still ours — a daemon that lost
//! the claim must not delete the winner's file.
//!
//! The hook's discovery chain is `$SMOOTH_FLOW_ADDR` → `flow.addr` →
//! `daemon.addr`, so nothing changes on a machine that only runs Big Smooth.

use std::path::{Path, PathBuf};

/// The file, under `dir` (`~/.smooth`).
#[must_use]
pub fn path(dir: &Path) -> PathBuf {
    dir.join("flow.addr")
}

/// The address currently claimed, if any.
#[must_use]
pub fn claimed(dir: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path(dir)).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// The claim decision, pure over the facts: should `addr` take the file whose
/// current contents are `existing`, given whether that address is `alive`?
#[must_use]
pub fn should_claim(existing: Option<&str>, addr: &str, alive: bool) -> bool {
    match existing.map(str::trim).filter(|s| !s.is_empty()) {
        None => true,
        Some(cur) if cur == addr => true,
        Some(_) => !alive,
    }
}

/// Write `addr` into `<dir>/flow.addr`.
///
/// # Errors
/// When the file cannot be written.
pub fn write(dir: &Path, addr: &str) -> std::io::Result<PathBuf> {
    let p = path(dir);
    crate::secret_file::write_secret(&p, addr)?;
    Ok(p)
}

/// Remove the claim, but only when it is still ours. Returns whether a file
/// was removed. Best-effort: an unreadable or missing file is not an error.
pub fn release(dir: &Path, addr: &str) -> bool {
    if claimed(dir).as_deref() != Some(addr) {
        return false;
    }
    std::fs::remove_file(path(dir)).is_ok()
}

/// Claim the file for `addr` if [`should_claim`] says so, probing the current
/// occupant for liveness. Returns whether this daemon now owns it.
pub async fn claim(dir: &Path, addr: &str) -> bool {
    let existing = claimed(dir);
    let alive = match existing.as_deref() {
        Some(cur) if cur != addr => crate::single_instance::probe_health(cur).await,
        _ => false,
    };
    if !should_claim(existing.as_deref(), addr, alive) {
        tracing::info!(
            addr,
            holder = existing.as_deref().unwrap_or("-"),
            "flow.addr is held by a live flow daemon — harness hooks go there (the flow store is shared)"
        );
        return false;
    }
    match write(dir, addr) {
        Ok(p) => {
            tracing::info!(path = %p.display(), addr, "claimed flow.addr — harness hooks reach this flow engine");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not write flow.addr — hooks fall back to daemon.addr");
            false
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn an_empty_or_missing_file_is_free() {
        assert!(should_claim(None, "127.0.0.1:1", false));
        assert!(should_claim(Some(""), "127.0.0.1:1", false));
        assert!(should_claim(Some("   \n"), "127.0.0.1:1", false));
    }

    #[test]
    fn a_live_holder_keeps_the_claim() {
        assert!(!should_claim(Some("127.0.0.1:8788"), "127.0.0.1:4400", true));
    }

    #[test]
    fn a_dead_holder_is_taken_over() {
        assert!(should_claim(Some("127.0.0.1:8788"), "127.0.0.1:4400", false));
    }

    #[test]
    fn our_own_address_is_always_reclaimed() {
        // A restart on the same port: the probe may even say "alive" (our own
        // dying listener), and we still rewrite it.
        assert!(should_claim(Some("127.0.0.1:4400"), "127.0.0.1:4400", true));
        assert!(should_claim(Some(" 127.0.0.1:4400 "), "127.0.0.1:4400", false));
    }

    #[test]
    fn write_claim_and_release_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(claimed(dir.path()), None);
        write(dir.path(), "127.0.0.1:4400").unwrap();
        assert_eq!(claimed(dir.path()).as_deref(), Some("127.0.0.1:4400"));
        assert!(!release(dir.path(), "127.0.0.1:9999"), "someone else's claim is never deleted");
        assert!(path(dir.path()).is_file());
        assert!(release(dir.path(), "127.0.0.1:4400"));
        assert_eq!(claimed(dir.path()), None);
        assert!(!release(dir.path(), "127.0.0.1:4400"), "releasing twice is a no-op");
    }

    #[tokio::test]
    async fn claim_takes_a_stale_file_and_leaves_a_live_one() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing listens on this port, so the holder is stale.
        write(dir.path(), "127.0.0.1:1").unwrap();
        assert!(claim(dir.path(), "127.0.0.1:4400").await);
        assert_eq!(claimed(dir.path()).as_deref(), Some("127.0.0.1:4400"));
    }
}
