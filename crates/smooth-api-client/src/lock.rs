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

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;

/// How long [`credential_lock`] waits for another process before giving up.
///
/// A healthy holder keeps the lock for one refresh round-trip (well under a
/// second), and every HTTP call made under it is bounded by
/// [`LOCKED_SECTION_TIMEOUT`]. So a wait this long means the holder is wedged,
/// and waiting forever would hang every `th` and daemon on the machine behind
/// it. That is exactly what happened on 2026-09-24 (th-2d2c15): SmoothFlow's
/// daemon held the lock across a Supabase refresh with no HTTP timeout, and
/// `th auth login` sat in `flock()` indefinitely.
pub const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(45);

/// The most time any caller may spend doing work (network included) while it
/// holds the credential lock. Callers wrap their refresh in
/// `tokio::time::timeout(LOCKED_SECTION_TIMEOUT, …)` so a hung request releases
/// the lock instead of holding it for hours. Deliberately shorter than
/// [`DEFAULT_LOCK_WAIT`], so a waiter always outlasts a live-but-slow holder.
pub const LOCKED_SECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Held for the duration of a credential read-modify-write. Releases the
/// OS lock on drop.
pub struct CredentialLock(std::fs::File);

impl Drop for CredentialLock {
    fn drop(&mut self) {
        // Clear the holder note first so a reader never blames a process that
        // already let go.
        let _ = self.0.set_len(0);
        let _ = FileExt::unlock(&self.0);
    }
}

/// Take the exclusive lock guarding the credentials file at `cred_path`,
/// waiting up to [`DEFAULT_LOCK_WAIT`].
///
/// **Not re-entrant.** Taking it twice in one process deadlocks until the
/// timeout (`flock` and `LockFileEx` key on the open file description, not the
/// thread), which is why [`crate::CredentialsStore::save`] does *not* take it
/// for you — the read-modify-write callers must hold it across their own
/// load+save, and a lock inside `save` would deadlock against them.
///
/// This blocks the calling thread. From async code use
/// [`credential_lock_async`], which waits on a blocking-pool thread instead of
/// stalling a runtime worker.
///
/// # Errors
/// The lock directory can't be created, the sidecar can't be opened, or
/// another process kept the lock past the wait. The timeout error names the
/// holder (pid, program, since when) so the user can see what to restart.
pub fn credential_lock(cred_path: &Path) -> Result<CredentialLock> {
    credential_lock_within(cred_path, DEFAULT_LOCK_WAIT)
}

/// [`credential_lock`] with an explicit maximum wait.
///
/// # Errors
/// See [`credential_lock`].
pub fn credential_lock_within(cred_path: &Path, wait: Duration) -> Result<CredentialLock> {
    // Sidecar rather than the json itself: locking the json would race
    // its own create+truncate, and the json may not exist yet on a first
    // login. The sidecar always exists once we create it here.
    let lock_path = sidecar(cred_path);
    if let Some(parent) = lock_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open credential lock {}", lock_path.display()))?;

    let deadline = Instant::now() + wait;
    let mut pause = Duration::from_millis(25);
    loop {
        if FileExt::try_lock_exclusive(&file).with_context(|| format!("acquire credential lock {}", lock_path.display()))? {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "gave up waiting {}s for the Smoo credentials lock ({}): {}. That process is stuck mid-refresh; restart it (quit and reopen the app, or `kill` the pid) and try again.",
                wait.as_secs(),
                lock_path.display(),
                describe_holder(&lock_path)
            );
        }
        std::thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(Duration::from_millis(500));
    }

    // Record who holds it, for the next waiter's error message. Best-effort:
    // the lock itself is the flock, not this text.
    let note = holder_note();
    let _ = file
        .set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(note.as_bytes()));
    Ok(CredentialLock(file))
}

/// Async [`credential_lock`]: waits on the blocking pool so a contended lock
/// never stalls a tokio worker (a blocked worker can starve the very task that
/// would release the lock).
///
/// # Errors
/// See [`credential_lock`]; also fails if the blocking task panics.
pub async fn credential_lock_async(cred_path: &Path) -> Result<CredentialLock> {
    let path = cred_path.to_path_buf();
    tokio::task::spawn_blocking(move || credential_lock(&path))
        .await
        .context("credential lock task panicked")?
}

/// Who holds the credentials lock right now, as a sentence ("held by
/// smooth-daemon (pid 8907) since …"), from the note the holder wrote.
#[must_use]
pub fn credential_lock_holder(cred_path: &Path) -> String {
    describe_holder(&sidecar(cred_path))
}

fn sidecar(cred_path: &Path) -> PathBuf {
    cred_path.with_extension("lock")
}

/// `pid=… exe=… since=…` for whoever takes the lock now.
fn holder_note() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".to_owned());
    format!("pid={} exe={} since={}\n", std::process::id(), exe, chrono::Utc::now().to_rfc3339())
}

/// Human description of the current holder, from the note it wrote.
fn describe_holder(lock_path: &Path) -> String {
    let mut text = String::new();
    let _ = std::fs::File::open(lock_path).and_then(|mut f| f.read_to_string(&mut text));
    let field = |key: &str| {
        text.split_whitespace()
            .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .map(str::to_owned)
    };
    match (field("pid"), field("exe"), field("since")) {
        (Some(pid), exe, since) => format!(
            "held by {} (pid {pid}) since {}",
            exe.unwrap_or_else(|| "an unknown program".to_owned()),
            since.unwrap_or_else(|| "an unknown time".to_owned())
        ),
        _ => "held by a process that did not record itself (an older th or daemon)".to_owned(),
    }
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
    fn a_wedged_holder_times_out_and_is_named_instead_of_hanging_forever() {
        // th-2d2c15: the holder never releases. The waiter must give up within
        // its bound and say who is holding it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("smooai-user.json");
        let _held = credential_lock(&path).expect("lock");
        let started = Instant::now();
        let err = std::thread::scope(|s| s.spawn(|| credential_lock_within(&path, Duration::from_millis(300)).err()).join().unwrap())
            .expect("a second lock must time out, not succeed");
        assert!(started.elapsed() < Duration::from_secs(5), "waited {:?}", started.elapsed());
        let msg = format!("{err:#}");
        assert!(msg.contains(&format!("pid {}", std::process::id())), "error must name the holder: {msg}");
        assert!(msg.contains("gave up waiting"), "{msg}");
    }

    #[test]
    fn a_released_lock_is_taken_by_a_waiter_within_its_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("smooai.json");
        let held = credential_lock(&path).expect("lock");
        std::thread::scope(|s| {
            let waiter = s.spawn(|| credential_lock_within(&path, Duration::from_secs(10)).map(|_| ()));
            std::thread::sleep(Duration::from_millis(150));
            drop(held);
            waiter.join().unwrap().expect("the waiter gets the lock once it is released");
        });
    }

    #[test]
    fn releasing_clears_the_holder_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("smooai.json");
        drop(credential_lock(&path).expect("lock"));
        assert_eq!(
            std::fs::read_to_string(sidecar(&path)).expect("read"),
            "",
            "a released lock must not blame anyone"
        );
    }

    #[test]
    fn an_old_style_holder_without_a_note_is_still_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join("x.lock");
        std::fs::write(&lock, "").expect("write");
        assert!(describe_holder(&lock).contains("did not record itself"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_async_lock_never_stalls_the_runtime_while_waiting() {
        // On a single-threaded runtime a blocking flock on the worker would
        // freeze every other task. The async variant must let this ticker run.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("smooai-user.json");
        let held = credential_lock(&path).expect("lock");
        let waiter = {
            let path = path.clone();
            tokio::spawn(async move { credential_lock_async(&path).await.map(|_| ()) })
        };
        let mut ticks = 0;
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            ticks += 1;
        }
        assert_eq!(ticks, 5, "the runtime kept running while the lock was contended");
        drop(held);
        waiter.await.unwrap().expect("acquired after release");
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
