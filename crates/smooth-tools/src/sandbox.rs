//! Kernel-enforced sandboxing for shell subprocesses (EPIC th-c89c2a Phase 3
//! Slice 2 — the enforcement boundary the permission engine only *expresses*).
//!
//! The security architecture's load-bearing layer: a reasoning agent can talk
//! its way past a userspace permission check, but it cannot talk its way past
//! the kernel. So `bash` is run inside an OS sandbox that confines filesystem
//! **writes** to the workspace (+ session temp) and **denies reads** of the
//! operator's credential stores (`~/.ssh`, `~/.aws`, …).
//!
//! **P0 — non-bypassable.** A [`SandboxedCommand`] is the *only* way `bash`
//! builds its subprocess. There is no constructor that yields a plain
//! `Command`, so no tool call / prompt can spawn an unsandboxed shell.
//!
//! Platform status:
//! - **macOS**: Seatbelt via `sandbox-exec` with a generated profile. Enforced.
//! - **Linux / other**: NOT YET (bubblewrap + Landlock + seccomp is TODO).
//!   Falls back to an unsandboxed shell with a loud warning — acceptable only
//!   for the single-trusted-user loopback daemon; tracked for hardening.

use std::path::PathBuf;

use tokio::process::Command;

/// What the sandbox confines a shell subprocess to.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// The only directory tree writes are permitted in (besides temp).
    pub workspace: PathBuf,
    /// The operator's home, whose credential dirs are read-denied.
    pub home: Option<PathBuf>,
}

impl SandboxPolicy {
    /// Build a policy confining writes to `workspace`, reading `HOME` from env
    /// for the credential-deny rules.
    #[must_use]
    pub fn for_workspace(workspace: PathBuf) -> Self {
        Self {
            workspace,
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }

    /// Whether this build actually enforces a kernel sandbox for shell commands.
    #[must_use]
    pub fn is_enforced() -> bool {
        cfg!(target_os = "macos")
    }
}

/// A shell command that is guaranteed to be built inside the OS sandbox.
///
/// The wrapped [`Command`] can only be obtained via [`shell`](Self::shell),
/// which always applies the sandbox — there is no unsandboxed path.
pub struct SandboxedCommand(Command);

impl SandboxedCommand {
    /// Build a sandboxed `sh -c <command>` under `policy`.
    #[must_use]
    pub fn shell(policy: &SandboxPolicy, command: &str) -> Self {
        Self(build(policy, command))
    }

    /// Take the underlying command to configure stdio / cwd / spawn. The
    /// sandbox wrapping is already baked in.
    #[must_use]
    pub fn into_command(self) -> Command {
        self.0
    }
}

#[cfg(target_os = "macos")]
fn build(policy: &SandboxPolicy, command: &str) -> Command {
    let profile = macos_profile(policy);
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(profile).arg("sh").arg("-c").arg(command);
    cmd
}

/// Generate a Seatbelt (SBPL) profile: allow-by-default, but confine writes to
/// the workspace + temp and deny reads of credential stores + writes to
/// `.git/hooks` (which would re-enter execution outside the sandbox).
#[cfg(target_os = "macos")]
fn macos_profile(policy: &SandboxPolicy) -> String {
    // Canonicalize so the profile paths match what the kernel enforces (macOS
    // symlinks /tmp → /private/tmp, /var/folders → /private/var/folders).
    let ws_path = std::fs::canonicalize(&policy.workspace).unwrap_or_else(|_| policy.workspace.clone());
    let ws = ws_path.display();
    let mut p = format!(
        "(version 1)\n\
         (allow default)\n\
         (deny file-write*)\n\
         (allow file-write*\n\
         \x20  (subpath \"{ws}\")\n\
         \x20  (subpath \"/tmp\")\n\
         \x20  (subpath \"/private/tmp\")\n\
         \x20  (subpath \"/private/var/folders\")\n\
         \x20  (literal \"/dev/null\")\n\
         \x20  (literal \"/dev/stdout\")\n\
         \x20  (literal \"/dev/stderr\")\n\
         \x20  (literal \"/dev/dtracehelper\")\n\
         \x20  (regex #\"^/dev/tty\")\n\
         \x20  (regex #\"^/dev/fd/\"))\n\
         (deny file-write* (subpath \"{ws}/.git/hooks\"))\n"
    );
    if let Some(home) = &policy.home {
        use std::fmt::Write as _;
        let home_path = std::fs::canonicalize(home).unwrap_or_else(|_| home.clone());
        let h = home_path.display();
        let _ = write!(
            p,
            "(deny file-read*\n\
             \x20  (subpath \"{h}/.ssh\")\n\
             \x20  (subpath \"{h}/.aws\")\n\
             \x20  (subpath \"{h}/.config/gh\")\n\
             \x20  (subpath \"{h}/.gnupg\"))\n"
        );
    }
    p
}

#[cfg(not(target_os = "macos"))]
fn build(policy: &SandboxPolicy, command: &str) -> Command {
    let _ = policy;
    tracing::warn!("bash is running UNSANDBOXED: kernel sandbox not yet implemented on this platform (Linux: bubblewrap+Landlock is TODO, th-08e05a)");
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;

        async fn run(policy: &SandboxPolicy, cmd: &str) -> (i32, String) {
            use std::process::Stdio;
            let out = SandboxedCommand::shell(policy, cmd)
                .into_command()
                .current_dir(&policy.workspace)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .unwrap();
            let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            (out.status.code().unwrap_or(-1), combined)
        }

        #[tokio::test]
        async fn write_inside_workspace_is_allowed() {
            let dir = tempfile::tempdir().unwrap();
            let policy = SandboxPolicy::for_workspace(dir.path().to_path_buf());
            let (code, _out) = run(&policy, "echo hi > inside.txt && cat inside.txt").await;
            assert_eq!(code, 0, "writing inside the workspace should succeed");
            assert!(dir.path().join("inside.txt").exists());
        }

        #[tokio::test]
        async fn write_outside_workspace_is_denied() {
            let dir = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap(); // a different tree
            let policy = SandboxPolicy::for_workspace(dir.path().to_path_buf());
            // Try to write into a sibling temp dir that is NOT the workspace and
            // NOT under /tmp's allowed prefixes for THIS workspace... actually
            // tempdirs are under /var/folders (allowed). Use $HOME instead.
            let target = format!("{}/smooth-sandbox-escape-test.txt", std::env::var("HOME").unwrap());
            let _ = std::fs::remove_file(&target);
            let (code, out) = run(&policy, &format!("echo escaped > '{target}'")).await;
            assert_ne!(code, 0, "writing to $HOME should be denied: {out}");
            assert!(!std::path::Path::new(&target).exists(), "escape file must not exist");
            let _ = (outside,);
        }

        #[tokio::test]
        async fn reading_ssh_keys_is_denied() {
            let dir = tempfile::tempdir().unwrap();
            let policy = SandboxPolicy::for_workspace(dir.path().to_path_buf());
            // Whether or not ~/.ssh exists, the sandbox must refuse to read it.
            let (_code, out) = run(&policy, "cat ~/.ssh/id_rsa ~/.ssh/id_ed25519 2>&1; echo DONE").await;
            assert!(out.contains("DONE"));
            assert!(!out.contains("PRIVATE KEY"), "no private key material should leak: {out}");
        }
    }

    #[test]
    fn policy_for_workspace_picks_up_home() {
        let p = SandboxPolicy::for_workspace(PathBuf::from("/ws"));
        assert_eq!(p.workspace, PathBuf::from("/ws"));
        // HOME is set in basically every test env.
        assert_eq!(p.home, std::env::var_os("HOME").map(PathBuf::from));
    }
}
