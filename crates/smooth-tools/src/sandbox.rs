//! Kernel-enforced sandboxing for shell subprocesses (EPIC th-c89c2a Phase 3
//! Slice 2 — the enforcement boundary the permission engine only *expresses*).
//!
//! The security architecture's load-bearing layer: a reasoning agent can talk
//! its way past a userspace permission check, but it cannot talk its way past
//! the kernel. So `bash` is run inside an OS sandbox that confines filesystem
//! **writes** to the workspace (+ session temp) — additionally denying writes
//! to `.git/hooks` and `.git/config` (either would re-enter execution outside
//! the sandbox via a hook or `core.hooksPath`) — and **denies reads** of the
//! operator's credential stores (`~/.ssh`, `~/.aws`, `~/.config/gh`,
//! `~/.config/gcloud`, `~/.kube`, `~/.docker`, `~/.gnupg`, `~/.netrc`) —
//! including the daemon's *own* secrets in `~/.smooth` (`providers.json`'s
//! LLM key, the `auth/` JWT), so a sandboxed tool can't exfil what drives it.
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
    ///
    /// As well as the kernel FS confinement, the child env is **scrubbed** of
    /// secret-named variables (the daemon's own `SMOOTH_API_KEY` /
    /// `SMOOTH_DAEMON_TOKEN`, provider `*_API_KEY`s, `*_TOKEN`/`*_SECRET`/…), so
    /// a read-only-classified `env`/`printenv` can't dump what drives the agent.
    /// Applied here at the single spawn point, so there is no unscrubbed path.
    #[must_use]
    pub fn shell(policy: &SandboxPolicy, command: &str) -> Self {
        let mut cmd = build(policy, command);
        scrub_secret_env(&mut cmd);
        Self(cmd)
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
         (deny file-write* (subpath \"{ws}/.git/hooks\"))\n\
         (deny file-write* (literal \"{ws}/.git/config\"))\n"
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
             \x20  (subpath \"{h}/.config/gcloud\")\n\
             \x20  (subpath \"{h}/.kube\")\n\
             \x20  (subpath \"{h}/.docker\")\n\
             \x20  (subpath \"{h}/.gnupg\")\n\
             \x20  (literal \"{h}/.netrc\")\n\
             \x20  (literal \"{h}/.smooth/providers.json\")\n\
             \x20  (subpath \"{h}/.smooth/auth\"))\n"
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

/// Remove secret-bearing variables from the child's inherited environment, so a
/// tool can't read the daemon's own credentials out of its process env. This is
/// platform-independent (it also matters on Linux, where the FS sandbox is not
/// yet in place) and runs at the single [`SandboxedCommand::shell`] spawn point.
fn scrub_secret_env(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if let Some(name) = name.to_str() {
            if is_secret_env_name(name) {
                cmd.env_remove(name);
            }
        }
    }
}

/// Whether an environment variable name looks like it carries a secret. Matched
/// on the name only (case-insensitive) so values never need inspecting: anything
/// `SMOOTH_*` (the daemon's own config), plus the usual credential markers. Kept
/// deliberately broad — a stripped false positive only loses a non-secret var
/// from the agent's shell, while a miss would leak a real credential.
fn is_secret_env_name(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    u.starts_with("SMOOTH_")
        || u.contains("SECRET")
        || u.contains("TOKEN")
        || u.contains("PASSWORD")
        || u.contains("PASSWD")
        || u.contains("CREDENTIAL")
        || u.contains("API_KEY")
        || u.contains("APIKEY")
        || u.contains("ACCESS_KEY")
        || u.ends_with("_KEY")
        || u.ends_with("_PAT")
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

        #[tokio::test]
        async fn writing_git_hooks_or_config_is_denied() {
            let dir = tempfile::tempdir().unwrap();
            let policy = SandboxPolicy::for_workspace(dir.path().to_path_buf());
            // A postinstall script trying to plant a hook or repoint core.hooksPath
            // must fail — both would later execute OUTSIDE the sandbox.
            let (_c, out) = run(
                &policy,
                "mkdir -p .git/hooks 2>&1; echo evil > .git/hooks/post-checkout 2>&1; \
                 mkdir -p .git 2>&1; echo '[core]' > .git/config 2>&1; echo DONE",
            )
            .await;
            assert!(out.contains("DONE"));
            assert!(!dir.path().join(".git/hooks/post-checkout").exists(), "planted hook must not exist: {out}");
            assert!(!dir.path().join(".git/config").exists(), "git config must not be writable: {out}");
        }

        #[tokio::test]
        async fn reading_cloud_and_registry_creds_is_denied() {
            let dir = tempfile::tempdir().unwrap();
            let policy = SandboxPolicy::for_workspace(dir.path().to_path_buf());
            // The exfil targets beyond ~/.ssh: cloud + registry + netrc creds.
            let (_c, out) = run(
                &policy,
                "cat ~/.aws/credentials ~/.config/gcloud/credentials.db ~/.kube/config \
                 ~/.docker/config.json ~/.netrc 2>&1; echo DONE",
            )
            .await;
            assert!(out.contains("DONE"));
            // The invariant: no credential material leaks (denied reads on the
            // dirs that exist; absent ones simply have nothing to read).
            assert!(!out.contains("aws_secret_access_key"), "no AWS secret should leak: {out}");
            assert!(!out.contains("BEGIN PRIVATE KEY"), "no key material should leak: {out}");
        }

        #[tokio::test]
        async fn env_does_not_leak_daemon_secrets_but_keeps_path() {
            // A read-only `env` must not dump the daemon's own secrets: the child
            // env is scrubbed of secret-named vars at the spawn point. Plant one
            // on this process, then prove the sandboxed shell can't see it — while
            // a benign var (PATH) survives so the shell still works.
            std::env::set_var("SMOOTH_API_KEY", "LEAK_SENTINEL_a91c");
            std::env::set_var("MY_SERVICE_TOKEN", "LEAK_SENTINEL_b22d");
            let dir = tempfile::tempdir().unwrap();
            let policy = SandboxPolicy::for_workspace(dir.path().to_path_buf());
            let (_c, out) = run(&policy, "env; echo DONE").await;
            std::env::remove_var("SMOOTH_API_KEY");
            std::env::remove_var("MY_SERVICE_TOKEN");

            assert!(out.contains("DONE"));
            assert!(!out.contains("LEAK_SENTINEL"), "scrubbed secrets must not appear in `env`: {out}");
            assert!(out.contains("PATH="), "non-secret env (PATH) should still be inherited: {out}");
        }

        #[tokio::test]
        async fn reading_the_daemons_own_smooth_credentials_is_denied() {
            // The lethal case: the agent's own LLM key + auth JWT live in
            // ~/.smooth. A sandboxed tool reading them would exfil exactly what
            // drives the daemon. Plant a sentinel we fully own under the denied
            // `~/.smooth/auth` subpath and prove the sandbox can't read it.
            let home = std::env::var("HOME").unwrap();
            let auth_dir = std::path::Path::new(&home).join(".smooth").join("auth");
            let created = !auth_dir.exists();
            std::fs::create_dir_all(&auth_dir).unwrap();
            let sentinel = auth_dir.join("smooth-sandbox-sentinel.json");
            std::fs::write(&sentinel, "SMOOTH_SECRET_SENTINEL_4f3a").unwrap();

            let dir = tempfile::tempdir().unwrap();
            let policy = SandboxPolicy::for_workspace(dir.path().to_path_buf());
            let (_c, out) = run(&policy, "cat ~/.smooth/auth/smooth-sandbox-sentinel.json 2>&1; echo DONE").await;

            // Clean up our sentinel (and the dir only if we created it).
            let _ = std::fs::remove_file(&sentinel);
            if created {
                let _ = std::fs::remove_dir(&auth_dir);
            }

            assert!(out.contains("DONE"));
            assert!(
                !out.contains("SMOOTH_SECRET_SENTINEL_4f3a"),
                "the daemon's own creds under ~/.smooth/auth must not be readable in-sandbox: {out}"
            );
        }
    }

    #[test]
    fn secret_env_names_are_detected_broadly() {
        for name in [
            "SMOOTH_API_KEY",
            "SMOOTH_DAEMON_TOKEN",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ACCESS_KEY_ID",
            "GITHUB_TOKEN",
            "DB_PASSWORD",
            "STRIPE_SECRET",
            "GH_PAT",
        ] {
            assert!(is_secret_env_name(name), "{name} should be treated as secret");
        }
        for name in ["PATH", "HOME", "USER", "SHELL", "TERM", "LANG", "PWD", "TMPDIR"] {
            assert!(!is_secret_env_name(name), "{name} should NOT be treated as secret");
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
