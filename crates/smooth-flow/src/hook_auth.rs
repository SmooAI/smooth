//! Who is allowed to speak on `POST /api/flow/hooks` (th-91d032).
//!
//! Every other flow route is gated on the daemon's local token. The hooks
//! endpoint cannot be, because its callers are shell scripts that
//! third-party CLIs run, and handing the daemon token to every agent's
//! environment would give each agent the power to spawn shells, approve
//! prompts, and drive every other session. Hooks are instead authenticated
//! **per launch**:
//!
//! * When the engine launches a session's process it mints a 256-bit
//!   **hook token**, writes it to a `0600` file in a `0700` directory next to
//!   `flow.db`, and puts only the file's *path* in the pane environment
//!   ([`TOKEN_FILE_ENV`]). The secret never appears in argv (which other users
//!   can read on Linux) or in the environment (which agents print into
//!   transcripts).
//! * `flow.db` stores only the token's SHA-256, so both daemons that share
//!   the file can verify it. A token is bound to exactly one session row and
//!   one launch: a relaunch rotates it, and a kill or close revokes it.
//! * A hook presents the token in [`TOKEN_HEADER`]. The engine resolves the
//!   session **from the token**, not from the body's `session_id`, and
//!   rejects a body whose `session_id` names a different harness session.
//!   So a hook can only ever speak for its own session.
//!
//! **Unauthenticated hooks** (no token) come from harnesses SmoothFlow did not
//! spawn. They may create or update **adopted** rows (th-c103c1), and nothing
//! else. They never reach an engine-spawned row, and they never open an
//! approvable permission request. An adopted session's identity is only a
//! claim, so a `PermissionRequest` from one shows as "needs you" with no
//! request id, and the harness asks in its own terminal. They are also
//! refused when the request carries browser or proxy headers
//! ([`HookCaller::direct`]), so they are never accepted from a web page or
//! over `tailscale serve`.
//!
//! What this does NOT defend against: code running as the same OS user. It
//! can read the token file, or the daemon's own `operator-token`, and the
//! daemon's local token already grants more than any hook does. The
//! boundary this draws is between sessions, and between this machine's user
//! and everything else: other sessions' agents, other users, browsers,
//! tailnet peers, and the kernel-sandboxed tool subprocesses (which cannot
//! read `~/.smooth`).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rand::RngCore as _;
use sha2::{Digest, Sha256};

/// The header a hook presents its token in.
pub const TOKEN_HEADER: &str = "x-smooth-flow-hook-token";
/// The pane environment variable that names the token file.
pub const TOKEN_FILE_ENV: &str = "SMOOTH_FLOW_HOOK_TOKEN_FILE";
/// Directory (beside `flow.db`) holding one token file per live launch.
pub const TOKEN_DIR: &str = "flow-hook-tokens";

/// Headers that mean "not a hook script talking to loopback directly".
///
/// A browser sends `Origin` / `Sec-Fetch-Site`; a reverse proxy such as
/// `tailscale serve` adds `Forwarded`, `X-Forwarded-*` or `Tailscale-*`.
pub const INDIRECT_HEADERS: &[&str] = &[
    "origin",
    "sec-fetch-site",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "tailscale-user-login",
    "tailscale-user-name",
];

/// What the transport knows about a hook's caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookCaller {
    /// The presented hook token, if any.
    pub token: Option<String>,
    /// True when the request carried none of [`INDIRECT_HEADERS`].
    pub direct: bool,
}

impl HookCaller {
    /// Build from header lookups. `get(name)` returns the header's value.
    #[must_use]
    pub fn from_headers<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Self {
        let token = get(TOKEN_HEADER).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
        let direct = INDIRECT_HEADERS.iter().all(|h| get(h).is_none());
        Self { token, direct }
    }

    /// A caller with a token (tests and in-process reporters).
    #[must_use]
    pub fn with_token(token: &str) -> Self {
        Self {
            token: Some(token.to_string()),
            direct: true,
        }
    }

    /// A direct caller without a token: an adopted harness.
    #[must_use]
    pub const fn anonymous() -> Self {
        Self { token: None, direct: true }
    }
}

/// A fresh token: 32 bytes of OS randomness, hex.
#[must_use]
pub fn mint() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    to_hex(&b)
}

/// The SHA-256 of `token`, hex. This is what `flow.db` stores and looks up.
#[must_use]
pub fn hash(token: &str) -> String {
    to_hex(&Sha256::digest(token.as_bytes()))
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The token directory for a `flow.db` at `db_path`.
#[must_use]
pub fn token_dir(db_path: &Path) -> PathBuf {
    db_path.parent().unwrap_or_else(|| Path::new(".")).join(TOKEN_DIR)
}

/// Where session `id`'s token file lives.
#[must_use]
pub fn token_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.token"))
}

/// Write `token` for session `id`: `0700` directory, `0600` file, replaced
/// atomically (write a temp file, then rename) so a hook never reads half a
/// token. Returns the file's path.
///
/// # Errors
/// When the directory or file cannot be written.
pub fn write_token(dir: &Path, id: &str, token: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    restrict(dir, 0o700)?;
    let path = token_path(dir, id);
    let tmp = dir.join(format!(".{id}.{}.tmp", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        restrict(&tmp, 0o600)?;
        f.write_all(token.as_bytes()).context("write hook token")?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("install {}", path.display()))?;
    Ok(path)
}

/// Remove session `id`'s token file (best effort; a missing file is fine).
pub fn remove_token(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(token_path(dir, id));
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).with_context(|| format!("chmod {}", path.display()))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
const fn restrict(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    #[test]
    fn mint_is_256_bits_of_hex_and_never_repeats() {
        let a = mint();
        let b = mint();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn hash_is_stable_sha256_and_not_the_token() {
        assert_eq!(hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let t = mint();
        assert_eq!(hash(&t), hash(&t));
        assert_ne!(hash(&t), t);
    }

    #[test]
    fn caller_reads_the_token_and_flags_indirect_requests() {
        let none = HookCaller::from_headers(|_| None);
        assert_eq!(none, HookCaller::anonymous());
        let tok = HookCaller::from_headers(|h| (h == TOKEN_HEADER).then_some("  abc  "));
        assert_eq!(tok.token.as_deref(), Some("abc"), "trimmed");
        assert!(tok.direct);
        let blank = HookCaller::from_headers(|h| (h == TOKEN_HEADER).then_some("   "));
        assert_eq!(blank.token, None, "a blank header is no token");
        for h in INDIRECT_HEADERS {
            let c = HookCaller::from_headers(|name| (name == *h).then_some("x"));
            assert!(!c.direct, "{h} marks the request indirect");
        }
    }

    #[test]
    #[cfg(unix)]
    fn token_file_is_private_and_replaced_atomically() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let dir = token_dir(&tmp.path().join("flow.db"));
        assert_eq!(dir, tmp.path().join(TOKEN_DIR));
        let p = write_token(&dir, "fs-1", "first").unwrap();
        assert_eq!(p, token_path(&dir, "fs-1"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "first");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        // A loose directory is tightened on the next write.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_token(&dir, "fs-1", "second").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "second");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp file left behind");
        remove_token(&dir, "fs-1");
        assert!(!p.exists());
        remove_token(&dir, "fs-1");
    }
}
