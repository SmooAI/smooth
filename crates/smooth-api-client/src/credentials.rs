//! Token store on disk: `~/.smooth/auth/smooai.json`.
//!
//! Stores the latest access + refresh tokens, expiry, and which
//! user/org the session is scoped to. Written by `th login`, read by
//! every other `th` command that needs to call the platform API.
//!
//! Permissions: file is created with mode 0600 so other host users
//! can't trivially scrape the access token. `smooth-credential-helper`
//! exists for the more paranoid "stash in OS keychain" path, but for
//! now we just write JSON — same trade-off `gh` makes by default with
//! `~/.config/gh/hosts.yml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One stored session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// Bearer JWT to send as `Authorization: Bearer <access_token>`.
    pub access_token: String,
    /// Refresh token. Optional because some auth flows (raw M2M
    /// client-credentials) don't issue one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// When `access_token` expires. Used by the SDK middleware to
    /// proactively refresh before the call fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Authenticated user (email or UUID — whichever the auth flow
    /// gave us). Display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Active organization id, if the user picked one. `th` commands
    /// that take `--org` override this; commands that don't fall back
    /// to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_org_id: Option<String>,
    /// M2M client id we authenticated with. Stored so the SDK can
    /// silently re-exchange a fresh access_token when the current
    /// one expires without re-prompting the user. None for flows
    /// where we didn't use client_credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// M2M client secret. Same trust level as `access_token` (a
    /// short-lived JWT minted with this secret would let an attacker
    /// do everything the secret itself does), so we store both in the
    /// same 0600 file. Used for auto-refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// When the credentials were stored. Display-only.
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
}

impl Credentials {
    /// `true` when `expires_at` is in the past (with a 60s safety
    /// margin, so we refresh before the server rejects us).
    #[must_use]
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(expiry) => Utc::now() >= expiry - chrono::Duration::seconds(60),
            None => false,
        }
    }
}

/// Reads + writes the credentials file. Path is
/// `~/.smooth/auth/smooai.json` by default; override with
/// `SMOOAI_AUTH_FILE=/abs/path/to/file.json` for tests.
#[derive(Debug, Clone)]
pub struct CredentialsStore {
    path: PathBuf,
}

impl CredentialsStore {
    /// Construct using the default path.
    ///
    /// # Errors
    /// Fails when `$HOME` is unset *and* `SMOOAI_AUTH_FILE` is not
    /// provided — pretty unusual on a real machine.
    pub fn default_path() -> Result<Self> {
        if let Ok(explicit) = std::env::var("SMOOAI_AUTH_FILE") {
            return Ok(Self { path: PathBuf::from(explicit) });
        }
        let home = dirs_next::home_dir().context("$HOME not set; pass SMOOAI_AUTH_FILE to override")?;
        Ok(Self {
            path: home.join(".smooth").join("auth").join("smooai.json"),
        })
    }

    /// Construct with an explicit path. Mainly for tests.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The absolute file path the store reads + writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the stored credentials. `Ok(None)` when the file is
    /// missing — that's the "logged out" state, not an error.
    ///
    /// # Errors
    /// Returns an error on IO failures (perms, EIO) or when the file
    /// exists but isn't valid JSON.
    pub fn load(&self) -> Result<Option<Credentials>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => {
                let creds: Credentials = serde_json::from_str(&text).with_context(|| format!("parse credentials at {}", self.path.display()))?;
                Ok(Some(creds))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read credentials at {}", self.path.display())),
        }
    }

    /// Persist `creds` atomically (write to a tempfile alongside,
    /// then rename) so a crash mid-write can't corrupt the store.
    /// The file is mode `0600` from the instant it exists.
    ///
    /// `NamedTempFile` rather than a hand-rolled `.json.tmp`, for two
    /// reasons that both bit us:
    /// - the name is **unique** and created `O_EXCL`, so two `th`
    ///   processes saving at once can't scribble over each other's
    ///   half-written temp file (th-5c0189);
    /// - it is created `0600`, closing the window where `fs::write`
    ///   (0644 under the usual umask) left the token world-readable
    ///   until a follow-up `chmod` — long enough for another local user
    ///   to open it and hold the fd across the chmod.
    ///
    /// # Errors
    /// Disk / permission failures bubble up unchanged.
    pub fn save(&self, creds: &Credentials) -> Result<()> {
        let parent = match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => {
                create_private_dir(dir)?;
                dir
            }
            // Bare relative filename — write the temp file beside it.
            _ => Path::new("."),
        };
        let json = serde_json::to_string_pretty(creds).context("serialize credentials")?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent).with_context(|| format!("create temp file in {}", parent.display()))?;
        std::io::Write::write_all(&mut tmp, json.as_bytes()).context("write credentials")?;
        tmp.as_file().sync_all().context("flush credentials to disk")?;
        tmp.persist(&self.path)
            .map_err(|e| e.error)
            .with_context(|| format!("replace {}", self.path.display()))?;
        Ok(())
    }

    /// Remove the credentials file. `Ok(())` when no file exists —
    /// `th logout` is idempotent.
    ///
    /// # Errors
    /// Disk failures other than "not found" bubble up.
    pub fn delete(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("delete credentials at {}", self.path.display())),
        }
    }
}

/// `create_dir_all` that asks for mode `0700` up front on unix, so a
/// secret-bearing directory is never even briefly group/world-traversable.
/// Existing directories keep their mode — the file inside is `0600`
/// regardless, and silently re-chmod'ing a path the caller pointed us at
/// via `SMOOAI_AUTH_FILE` is not ours to do.
fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).with_context(|| format!("mkdir {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (tempfile::TempDir, CredentialsStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CredentialsStore::at(dir.path().join("smooai.json"));
        (dir, store)
    }

    fn fixture() -> Credentials {
        Credentials {
            access_token: "tok".into(),
            refresh_token: Some("rtok".into()),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            user: Some("brent@smoo.ai".into()),
            active_org_id: Some("org_abc123".into()),
            client_id: Some("cid".into()),
            client_secret: Some("csecret".into()),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let (_dir, store) = tmp_store();
        let creds = fixture();
        store.save(&creds).expect("save");
        let loaded = store.load().expect("load").expect("present");
        assert_eq!(loaded.access_token, creds.access_token);
        assert_eq!(loaded.user, creds.user);
        assert_eq!(loaded.active_org_id, creds.active_org_id);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let (_dir, store) = tmp_store();
        assert!(store.load().expect("load").is_none());
    }

    #[test]
    fn delete_is_idempotent() {
        let (_dir, store) = tmp_store();
        store.delete().expect("first delete");
        store.delete().expect("second delete");
    }

    #[test]
    fn is_expired_handles_no_expiry() {
        let mut creds = fixture();
        creds.expires_at = None;
        assert!(!creds.is_expired());
    }

    #[test]
    fn is_expired_with_past_expiry_returns_true() {
        let mut creds = fixture();
        creds.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        assert!(creds.is_expired());
    }

    #[test]
    fn is_expired_within_safety_margin_returns_true() {
        // 30s in the future is "expired" because our margin is 60s.
        let mut creds = fixture();
        creds.expires_at = Some(Utc::now() + chrono::Duration::seconds(30));
        assert!(creds.is_expired());
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = tmp_store();
        store.save(&fixture()).expect("save");
        let mode = std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    /// th-5c0189: the old `save` wrote a **fixed** `smooai.json.tmp`, so
    /// two savers shared one temp file and one could rename a blend of
    /// both writers' bytes into place. Unique `O_EXCL` temp names make
    /// every save land whole or not at all.
    #[test]
    fn concurrent_saves_never_blend_and_leave_no_temp_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CredentialsStore::at(dir.path().join("smooai.json"));
        store.save(&fixture()).expect("seed");

        std::thread::scope(|scope| {
            for i in 0..8 {
                let store = store.clone();
                scope.spawn(move || {
                    let mut creds = fixture();
                    creds.access_token = format!("tok-{i}");
                    creds.refresh_token = Some(format!("rtok-{i}"));
                    store.save(&creds).expect("save");
                });
            }
        });

        // Whichever writer won, the file must be one writer's credentials
        // in full — a torn write would fail to parse, or pair one saver's
        // access token with another's refresh token.
        let loaded = store.load().expect("load").expect("present");
        let suffix = loaded.access_token.strip_prefix("tok-").expect("intact access token");
        assert_eq!(
            loaded.refresh_token.as_deref(),
            Some(format!("rtok-{suffix}").as_str()),
            "torn write: mismatched token pair"
        );

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name())
            .filter(|name| name != "smooai.json")
            .collect();
        assert!(leftovers.is_empty(), "temp files leaked for the next writer to collide with: {leftovers:?}");
    }

    /// The parent of a credentials file is asked for 0700 at creation, so
    /// it is never even briefly group/world-traversable.
    #[cfg(unix)]
    #[test]
    fn save_creates_its_parent_directory_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CredentialsStore::at(dir.path().join("auth").join("smooai.json"));
        store.save(&fixture()).expect("save");
        let mode = std::fs::metadata(dir.path().join("auth")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected 0700, got {mode:o}");
    }
}
