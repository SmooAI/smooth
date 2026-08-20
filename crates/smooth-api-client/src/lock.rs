//! Cross-process lock over a credentials file.
//!
//! Supabase rotates the refresh token on every successful exchange and
//! revokes the old one after a ~10s grace. Two `th` processes that both
//! find an expired session, both POST to Supabase, and both write the
//! file each end up holding a token the other invalidated: one `rename`
//! wins, the loser's token is the one now live server-side, and the
//! session is dead until `th auth login`. Serializing the whole
//! load → refresh → save sequence is what makes that impossible; the
//! waiter then re-reads and uses the winner's fresh token.
//!
//! # Why this lives in the *lowest* crate
//!
//! A lock only works if every writer takes the **same** one. Credentials
//! are written from two crates — this one ([`crate::CredentialsStore`],
//! via `SmoothApiClient::set_credentials`) and `smooth-cli` (which drives
//! the near-identical store from `smooai-client-shared`). Keeping the
//! implementation here, keyed on the credentials *path* rather than on
//! either store type, is what lets both of them contend for one sidecar.
//! It has to be path-keyed because the other store type lives in another
//! repo and can't grow a method.
//!
//! Same primitive and same sidecar reasoning as
//! `smooth_pearls::registry::auto_register_at`.

use std::path::Path;

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;

/// Held for the duration of a credential read-modify-write. Releases the
/// OS lock on drop.
pub struct CredentialLock(std::fs::File);

impl Drop for CredentialLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

/// Take the exclusive lock guarding the credentials file at `cred_path`.
///
/// Blocks until the current holder releases. A refresh is one HTTP
/// roundtrip, and the loser of the race wants the winner's token anyway,
/// so waiting is strictly better than racing.
///
/// **Not re-entrant.** Taking it twice in one thread deadlocks (`flock`
/// and `LockFileEx` both key on the open file description, not the
/// thread), which is why [`crate::CredentialsStore::save`] does *not*
/// take it for you — the read-modify-write callers must hold it across
/// their own load+save, and a lock inside `save` would deadlock against
/// them.
///
/// ponytail: blocking `lock_exclusive`, no timeout. If a wedged process
/// ever hangs `th`, swap for `try_lock_exclusive` + a bounded retry.
///
/// # Errors
/// The lock directory can't be created, or the sidecar can't be opened /
/// locked.
pub fn credential_lock(cred_path: &Path) -> Result<CredentialLock> {
    // Sidecar rather than the json itself: locking the json would race
    // its own create+truncate, and the json may not exist yet on a first
    // login. The sidecar always exists once we create it here.
    let lock_path = cred_path.with_extension("lock");
    if let Some(parent) = lock_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open credential lock {}", lock_path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("acquire credential lock {}", lock_path.display()))?;
    Ok(CredentialLock(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_serializes_concurrent_read_modify_write() {
        // The shape of the credential race: N threads each read a
        // counter, bump it, write it back. Without the lock the
        // interleaved reads lose writes; with it every increment lands.
        // Asserts on the outcome, not on timing.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("smooai.json");
        std::fs::write(&path, "0").expect("seed");

        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    let _guard = credential_lock(&path).expect("lock");
                    let n: u32 = std::fs::read_to_string(&path).expect("read").parse().expect("parse");
                    // Widen the window the lock has to cover.
                    std::thread::yield_now();
                    std::fs::write(&path, (n + 1).to_string()).expect("write");
                });
            }
        });

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "8",
            "a lost update means the lock did not serialize"
        );
    }

    #[test]
    fn lock_creates_the_sidecar_next_to_a_missing_credentials_file() {
        // First login: the json does not exist yet, so the lock must not
        // depend on it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("smooai-user.json");
        let guard = credential_lock(&path).expect("lock");
        assert!(dir.path().join("nested").join("smooai-user.lock").exists());
        drop(guard);
    }

    #[test]
    fn the_two_stores_take_different_locks() {
        // `th auth login --m2m` must not block on a user-session refresh.
        let dir = tempfile::tempdir().expect("tempdir");
        let user = dir.path().join("smooai-user.json");
        let m2m = dir.path().join("smooai.json");
        let _held = credential_lock(&user).expect("lock user");
        // Would deadlock if both stores hashed to one sidecar.
        let _other = credential_lock(&m2m).expect("lock m2m");
    }

    #[test]
    fn the_sidecar_is_not_the_credentials_file() {
        // If the lock file *were* the destination, every `save` would be
        // renaming over a file it holds open — which POSIX tolerates and
        // Windows does not.
        let dir = tempfile::tempdir().expect("tempdir");
        let creds = dir.path().join("smooai.json");
        let _guard = credential_lock(&creds).expect("lock");
        assert!(!creds.exists(), "locking must not create the credentials file itself");
    }
}
