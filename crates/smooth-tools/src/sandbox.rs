//! Kernel-enforced sandboxing for shell subprocesses (EPIC th-c89c2a Phase 3
//! Slice 2) — **opt-in, off by default** (pearl th-efbab1).
//!
//! # Default: pass-through (the agent runs as you)
//!
//! Big Smooth is a personal agent operating AS its user on the user's own
//! machine. With the sandbox on, the very things a personal agent is asked to
//! do broke: `ssh smoo-hub` timed out (direct outbound kernel-denied behind the
//! egress proxy) and `git fetch` failed with "Operation not permitted" on
//! `~/.ssh/known_hosts` (credential-store read-deny). So by default
//! ([`SandboxMode::PassThrough`]) a [`SandboxedCommand`] is a normal user
//! subprocess: no Seatbelt profile, the user's env / `HOME` / `SSH_AUTH_SOCK` /
//! `PATH` inherited. Safety in that posture is the two userspace layers — the
//! permission gate (deny-policy circuit-breakers) and Narc — which are
//! unaffected by this switch.
//!
//! # Opt-in: enforced (`SMOOTH_SANDBOX=1`)
//!
//! Set [`SANDBOX_ENV`] (`SMOOTH_SANDBOX`) to `1`/`true`/`yes`/`on` and `bash`
//! (and CLI-wrapper plugins) run inside an OS sandbox that keeps the
//! guarantees a fully-hijacked shell must not get past: no writes to
//! `.git/hooks` / `.git/config` (either would re-enter execution outside the
//! sandbox via a hook or `core.hooksPath`) and **no reads** of the operator's
//! credential stores (`~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.config/gcloud`,
//! `~/.kube`, `~/.docker`, `~/.gnupg`, `~/.netrc`) — including the daemon's
//! *own* secrets and state in `~/.smooth` (`providers.json`'s LLM key, the
//! `auth/` JWT, the `operator-token` WS bearer, `operator-storage.db`,
//! `schedules.db`), so a sandboxed tool can't exfil what drives it or schedule
//! itself a second turn.
//!
//! With a proxy configured ([`SandboxPolicy::with_proxy`]) an enforced sandbox
//! also becomes the **egress boundary**: `HTTP(S)_PROXY` point at the loopback
//! goalie proxy and direct outbound network is kernel-denied except to
//! loopback, so off-box traffic must pass the proxy's exact-host allowlist. In
//! pass-through mode the proxy variables are still set, but nothing stops a
//! tool that ignores them — the allowlist is **advisory**.
//!
//! **Single spawn point.** A [`SandboxedCommand`] is the *only* way `bash`
//! builds its subprocess — there is no constructor that yields a plain
//! `Command` around it — so the mode decided here (and the secret-env scrub)
//! applies to every shell the agent gets. Pass-through is a mode of this type,
//! not a bypass of it.
//!
//! Platform status of the enforced mode:
//! - **macOS**: Seatbelt via `sandbox-exec` with a generated profile. Enforced.
//! - **Linux**: NOT YET (bubblewrap + Landlock + seccomp is TODO, th-08e05a).
//! - **Windows**: NOT YET (AppContainer / Job Object + restricted token is TODO,
//!   th-08e05a). The shell there is `cmd /C`, run with the operator's own token.
//!
//! Requesting the sandbox on a non-macOS platform logs a loud warning and runs
//! the shell unsandboxed — see `docs/Architecture/Windows-Security-Posture.md`.

use std::path::PathBuf;

use tokio::process::Command;

/// The env var that opts into the kernel OS sandbox: `1`/`true`/`yes`/`on`
/// (case-insensitive) enables it; unset, empty, or `0`/`false`/`no`/`off`
/// leaves it off. Default **off** (pearl th-efbab1).
pub const SANDBOX_ENV: &str = "SMOOTH_SANDBOX";

/// Whether shell subprocesses get the kernel OS sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// **Default.** A normal user subprocess: no kernel profile, the user's
    /// env inherited (minus the daemon's own config). The agent can
    /// do whatever the user can — `ssh`, `git fetch`, read `~/.ssh/known_hosts`.
    #[default]
    PassThrough,
    /// Opt-in (`SMOOTH_SANDBOX=1`): the kernel sandbox (Seatbelt on macOS) with
    /// credential-store read/write denies, git re-entry denies, the full
    /// secret-env scrub, and — with a proxy — kernel-forced egress.
    Enforced,
}

impl SandboxMode {
    /// Resolve the mode from [`SANDBOX_ENV`] in this process's environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var(SANDBOX_ENV).ok().as_deref())
    }

    /// Pure core of [`from_env`](Self::from_env): `None` (unset) and anything
    /// unrecognized resolve to the default, [`PassThrough`](Self::PassThrough).
    #[must_use]
    pub fn from_env_value(raw: Option<&str>) -> Self {
        raw.and_then(Self::parse).unwrap_or_default()
    }

    /// Parse a raw switch value. `None` means unrecognized (a typo) — callers
    /// that log startup posture use it to warn rather than silently guess.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(Self::Enforced),
            "" | "0" | "false" | "no" | "off" => Some(Self::PassThrough),
            _ => None,
        }
    }

    /// Whether the kernel sandbox was asked for (independent of whether this
    /// platform can actually enforce it — see [`SandboxPolicy::is_enforced`]).
    #[must_use]
    pub const fn is_requested(self) -> bool {
        matches!(self, Self::Enforced)
    }
}

/// What the sandbox confines a shell subprocess to.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// The workspace the shell runs in.
    pub workspace: PathBuf,
    /// The operator's home, whose credential dirs are read-denied (enforced
    /// mode only).
    pub home: Option<PathBuf>,
    /// When set (`host:port`), the shell's egress is pointed at this loopback
    /// proxy via `HTTP(S)_PROXY`. In [`SandboxMode::Enforced`] on macOS the
    /// kernel also **denies direct outbound network** except to loopback, so
    /// the proxy (the goalie egress allowlist) is the only path off-box; in
    /// pass-through mode the variables are advisory.
    pub proxy: Option<String>,
    /// Pass-through (default) or kernel-enforced. [`for_workspace`](Self::for_workspace)
    /// resolves it from [`SANDBOX_ENV`].
    pub mode: SandboxMode,
}

impl SandboxPolicy {
    /// Build a policy for `workspace`, resolving the operator's home for the
    /// credential-deny rules and the mode from [`SANDBOX_ENV`] (default off).
    ///
    /// Uses `dirs_next::home_dir()` rather than `$HOME` directly: `HOME` is not
    /// set on Windows (it's `%USERPROFILE%`), so an env read silently yielded
    /// `None` there and dropped every credential-deny rule on the floor.
    #[must_use]
    pub fn for_workspace(workspace: PathBuf) -> Self {
        Self {
            workspace,
            home: dirs_next::home_dir(),
            proxy: None,
            mode: SandboxMode::from_env(),
        }
    }

    /// Point the shell's egress at the loopback proxy at `addr` (`host:port`):
    /// sets `HTTP(S)_PROXY`, and — enforced mode only — denies direct outbound.
    #[must_use]
    pub fn with_proxy(mut self, addr: impl Into<String>) -> Self {
        self.proxy = Some(addr.into());
        self
    }

    /// Override the mode resolved from the environment (tests, embedders).
    #[must_use]
    pub const fn with_mode(mut self, mode: SandboxMode) -> Self {
        self.mode = mode;
        self
    }

    /// Whether this build has a kernel sandbox to apply at all (macOS only today).
    #[must_use]
    pub const fn platform_supported() -> bool {
        cfg!(target_os = "macos")
    }

    /// Whether shells built from THIS policy actually run in a kernel sandbox:
    /// the sandbox was requested and the platform can enforce it.
    #[must_use]
    pub const fn is_enforced(&self) -> bool {
        self.mode.is_requested() && Self::platform_supported()
    }
}

/// A shell command routed through the single sandbox-application point.
///
/// The wrapped [`Command`] can only be obtained via [`shell`](Self::shell), so
/// there is no path that BYPASSES this type — the mode and the env scrub decided
/// here apply to every shell. That is not the same as "always sandboxed", and
/// the difference matters (pearls th-db25d4 item 11, th-efbab1):
///
/// | | FS confinement | egress deny |
/// |---|---|---|
/// | pass-through (default, every OS) | **none** — runs as the user | **none** — proxy env vars only, if configured |
/// | enforced, macOS | `sandbox-exec` + SBPL | kernel `(deny network-outbound)` |
/// | enforced, Linux | **none** (bubblewrap + Landlock TODO, th-08e05a) | **none** — proxy env vars only |
/// | enforced, Windows | **none** (AppContainer/Job Object TODO, th-08e05a) | **none** — proxy env vars only |
///
/// [`SandboxPolicy::is_enforced`] is the runtime answer; branch on it rather
/// than assuming.
pub struct SandboxedCommand(Command);

impl SandboxedCommand {
    /// Build a shell invocation of `command` under `policy` — `sh -c` on Unix,
    /// `cmd /C` on Windows — kernel-sandboxed when `policy.mode` is
    /// [`SandboxMode::Enforced`] and the platform supports it.
    ///
    /// The child env is scrubbed at this single spawn point. Pass-through
    /// strips only the daemon's own config (`SMOOTH_*`, the gateway key, WS bearer,
    /// …) — the user's own credentials (`SSH_AUTH_SOCK`, `GITHUB_TOKEN`,
    /// `AWS_*`) are inherited, because the agent acts as the user. Enforced
    /// mode strips every secret-named variable (`*_TOKEN`, `*_SECRET`,
    /// `*_API_KEY`, …) so a read-only-classified `env`/`printenv` can't dump
    /// them.
    #[must_use]
    pub fn shell(policy: &SandboxPolicy, command: &str) -> Self {
        let mut cmd = build(policy, command);
        match policy.mode {
            SandboxMode::Enforced => scrub_secret_env(&mut cmd),
            SandboxMode::PassThrough => scrub_daemon_env(&mut cmd),
        }
        if let Some(addr) = &policy.proxy {
            // Point HTTP(S) egress at the loopback proxy.
            //
            // How binding this is depends on the mode and platform (pearl
            // th-db25d4 item 11). Enforced on macOS, the kernel network-deny
            // in `macos_profile` makes it non-optional — direct off-box
            // connects fail, so a tool ignoring these vars cannot reach the
            // network at all. Everywhere else (pass-through, or enforced on
            // Linux/Windows) there is no kernel deny, so these variables are a
            // REQUEST: honoured by well-behaved HTTP clients, ignored by
            // anything that opens its own socket (`ssh`, raw TCP).
            let url = format!("http://{addr}");
            for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "ALL_PROXY", "all_proxy"] {
                cmd.env(key, &url);
            }
            // Never proxy loopback itself (the proxy, local dev servers).
            cmd.env("NO_PROXY", "localhost,127.0.0.1,::1");
            cmd.env("no_proxy", "localhost,127.0.0.1,::1");
        }
        Self(cmd)
    }

    /// Take the underlying command to configure stdio / cwd / spawn. The
    /// sandbox wrapping (if any) is already baked in.
    #[must_use]
    pub fn into_command(self) -> Command {
        self.0
    }
}

/// The one place the mode becomes a process shape.
fn build(policy: &SandboxPolicy, command: &str) -> Command {
    match policy.mode {
        SandboxMode::PassThrough => plain_shell(command),
        SandboxMode::Enforced => enforced_shell(policy, command),
    }
}

/// The platform's own interpreter with no kernel profile: `sh -c` on Unix.
#[cfg(not(target_os = "windows"))]
fn plain_shell(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

/// Windows has no `sh` — the shell is `cmd /C`.
///
/// `cmd` is the one interpreter guaranteed present on every Windows host;
/// PowerShell startup is ~10x slower and its default execution policy can
/// refuse to run at all. Model-authored `bash` snippets that use POSIX syntax
/// will fail here — that is visible in the tool output (a `cmd` error), which
/// the agent can react to, unlike a silently missing binary.
#[cfg(target_os = "windows")]
fn plain_shell(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(command);
    cmd
}

#[cfg(target_os = "macos")]
fn enforced_shell(policy: &SandboxPolicy, command: &str) -> Command {
    let profile = macos_profile(policy);
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(profile).arg("sh").arg("-c").arg(command);
    cmd
}

/// The sandbox was explicitly requested but this platform has no kernel
/// sandbox yet: say so loudly on every spawn, then run the plain shell.
#[cfg(not(target_os = "macos"))]
fn enforced_shell(policy: &SandboxPolicy, command: &str) -> Command {
    let _ = policy;
    tracing::warn!(
        "{SANDBOX_ENV} is on but bash is running UNSANDBOXED: kernel sandbox not yet implemented on this platform \
         (Linux: bubblewrap+Landlock, Windows: AppContainer/Job Object — th-08e05a). See docs/Architecture/Windows-Security-Posture.md"
    );
    plain_shell(command)
}

/// Generate a Seatbelt (SBPL) profile: allow-by-default, but confine writes to
/// the workspace + temp and deny reads of credential stores + writes to
/// `.git/hooks` (which would re-enter execution outside the sandbox).
#[cfg(target_os = "macos")]
fn macos_profile(policy: &SandboxPolicy) -> String {
    // Personal-assistant posture (th-sandbox-personal): Big Smooth is a trusted
    // agent on the operator's own machine, so writes are allowed by default
    // (edit `~/.zshrc`, `~/.config`, dotfiles — a jailed assistant is useless).
    // Safety is behavioural: the deny-policy circuit-breakers + the Narc LLM
    // judge gate dangerous *actions*. The kernel keeps only the guarantees that
    // must hold even against a fully-hijacked shell: no exfil/overwrite of the
    // crown-jewel credential dirs (below), no planting a launch agent, and no
    // re-entering execution via `.git/hooks`/`.git/config`. SBPL is
    // last-match-wins, so these targeted denies override the opening allow.
    // Git re-entry is denied in EVERY repo, not just the workspace: now that
    // writes are allowed by default, a workspace-scoped deny would leave a hook
    // plantable in any other checkout on the machine. The profile therefore no
    // longer needs the workspace path at all.
    let mut p = String::from(
        "(version 1)\n\
         (allow default)\n\
         (deny file-write* (regex #\"/\\.git/hooks/\"))\n\
         (deny file-write* (regex #\"/\\.git/config$\"))\n",
    );
    // The daemon's OWN state counts as a crown jewel, not just the user's
    // credentials. `~/.smooth/operator-token` is the bearer for
    // `ws://127.0.0.1:8787/ws` — a sandboxed shell that reads it can open a
    // FRESH connection as the owner principal and drive the agent outside this
    // conversation's permission mode, and `schedules.db` turns that into
    // persistence (the same primitive as the already-denied LaunchAgents).
    // The two SQLite stores use `regex`, not `literal`, so the `-wal`/`-shm`
    // siblings are covered too — a literal would leave every recently written
    // row readable in the WAL.
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
             \x20  (subpath \"{h}/.smooth/auth\")\n\
             \x20  (literal \"{h}/.smooth/operator-token\")\n\
             \x20  (regex #\"/\\.smooth/operator-storage\\.db\")\n\
             \x20  (regex #\"/\\.smooth/schedules\\.db\"))\n"
        );
        // Crown-jewel WRITE deny: even with writes allowed by default, a
        // hijacked shell must not overwrite credentials or plant persistence.
        // Mirrors the read-deny set + `~/Library/LaunchAgents` (a login-agent
        // written here would later execute OUTSIDE the sandbox).
        let _ = write!(
            p,
            "(deny file-write*\n\
             \x20  (subpath \"{h}/.ssh\")\n\
             \x20  (subpath \"{h}/.aws\")\n\
             \x20  (subpath \"{h}/.config/gh\")\n\
             \x20  (subpath \"{h}/.config/gcloud\")\n\
             \x20  (subpath \"{h}/.kube\")\n\
             \x20  (subpath \"{h}/.docker\")\n\
             \x20  (subpath \"{h}/.gnupg\")\n\
             \x20  (literal \"{h}/.netrc\")\n\
             \x20  (subpath \"{h}/.smooth/auth\")\n\
             \x20  (literal \"{h}/.smooth/providers.json\")\n\
             \x20  (literal \"{h}/.smooth/operator-token\")\n\
             \x20  (regex #\"/\\.smooth/operator-storage\\.db\")\n\
             \x20  (regex #\"/\\.smooth/schedules\\.db\")\n\
             \x20  (subpath \"{h}/Library/LaunchAgents\"))\n"
        );
    }
    if policy.proxy.is_some() {
        // Egress boundary: deny direct outbound network, allowing only loopback
        // (the goalie proxy + local dev servers). Off-box traffic must go
        // through the proxy's exact-host allowlist; a tool ignoring HTTP_PROXY
        // just can't connect out. SBPL is last-match-wins, so these override the
        // opening `(allow default)`.
        p.push_str(
            "(deny network-outbound)\n\
             (allow network-outbound (remote ip \"localhost:*\"))\n\
             (allow network-outbound (remote unix-socket))\n",
        );
    }
    p
}

/// Enforced mode: remove every secret-bearing variable from the child's
/// inherited environment, so a tool can't read credentials out of its process
/// env. Platform-independent (it also applies where the FS sandbox is not yet
/// in place) and runs at the single [`SandboxedCommand::shell`] spawn point.
fn scrub_secret_env(cmd: &mut Command) {
    scrub_env_where(cmd, is_secret_env_name);
}

/// Pass-through mode: remove only the daemon's OWN configuration (`SMOOTH_*`
/// and the gateway key — its LLM credentials, WS bearer, egress/workspace
/// knobs), which is not the user's
/// env and would otherwise leak into transcripts and nested `th` calls. The
/// user's own credentials stay, because the agent acts as the user.
fn scrub_daemon_env(cmd: &mut Command) {
    scrub_env_where(cmd, is_daemon_env_name);
}

fn scrub_env_where(cmd: &mut Command, strip: fn(&str) -> bool) {
    for (name, _) in std::env::vars_os() {
        if let Some(name) = name.to_str() {
            if strip(name) {
                cmd.env_remove(name);
            }
        }
    }
}

/// Whether an environment variable is the daemon's own configuration: every
/// `SMOOTH_*` knob, plus the LLM gateway key the daemon reads under the
/// platform's `SMOOAI_` prefix.
fn is_daemon_env_name(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    u.starts_with("SMOOTH_") || u == "SMOOAI_GATEWAY_KEY"
}

/// Whether an environment variable name looks like it carries a secret. Matched
/// on the name only (case-insensitive) so values never need inspecting: anything
/// `SMOOTH_*` (the daemon's own config), plus the usual credential markers. Kept
/// deliberately broad — a stripped false positive only loses a non-secret var
/// from the agent's shell, while a miss would leak a real credential.
fn is_secret_env_name(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    is_daemon_env_name(&u)
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

        /// The opt-in sandbox, set explicitly so these tests don't depend on
        /// the `SMOOTH_SANDBOX` value of whoever runs them.
        fn enforced(workspace: PathBuf) -> SandboxPolicy {
            SandboxPolicy::for_workspace(workspace).with_mode(SandboxMode::Enforced)
        }

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
            let policy = enforced(dir.path().to_path_buf());
            let (code, _out) = run(&policy, "echo hi > inside.txt && cat inside.txt").await;
            assert_eq!(code, 0, "writing inside the workspace should succeed");
            assert!(dir.path().join("inside.txt").exists());
        }

        #[tokio::test]
        async fn home_writes_allowed_but_crown_jewels_denied() {
            let dir = tempfile::tempdir().unwrap();
            let policy = enforced(dir.path().to_path_buf());
            let home = std::env::var("HOME").unwrap();
            // Personal-assistant posture: a benign $HOME write (a dotfile-style
            // path) now SUCCEEDS — an assistant that can't touch ~/.zshrc is useless.
            let benign = format!("{home}/.smooth-sandbox-benign-test.txt");
            let _ = std::fs::remove_file(&benign);
            let (code, out) = run(&policy, &format!("echo ok > '{benign}'")).await;
            assert_eq!(code, 0, "benign $HOME write should be allowed: {out}");
            assert!(std::path::Path::new(&benign).exists(), "benign $HOME file should exist");
            let _ = std::fs::remove_file(&benign);
            // But the crown jewels stay KERNEL-denied even with writes allowed by
            // default — a hijacked shell still can't overwrite ~/.ssh.
            let jewel = format!("{home}/.ssh/smooth-sandbox-escape-test");
            let _ = std::fs::remove_file(&jewel);
            let (jcode, jout) = run(&policy, &format!("mkdir -p '{home}/.ssh' 2>/dev/null; echo escaped > '{jewel}'")).await;
            assert_ne!(jcode, 0, "writing under ~/.ssh must be denied: {jout}");
            assert!(!std::path::Path::new(&jewel).exists(), "crown-jewel write must not land");
        }

        #[tokio::test]
        async fn reading_ssh_keys_is_denied() {
            let dir = tempfile::tempdir().unwrap();
            let policy = enforced(dir.path().to_path_buf());
            // Whether or not ~/.ssh exists, the sandbox must refuse to read it.
            let (_code, out) = run(&policy, "cat ~/.ssh/id_rsa ~/.ssh/id_ed25519 2>&1; echo DONE").await;
            assert!(out.contains("DONE"));
            assert!(!out.contains("PRIVATE KEY"), "no private key material should leak: {out}");
        }

        #[tokio::test]
        async fn writing_git_hooks_or_config_is_denied() {
            let dir = tempfile::tempdir().unwrap();
            let policy = enforced(dir.path().to_path_buf());
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
        async fn git_hooks_denied_in_repos_outside_the_workspace_too() {
            // Regression for the personal-assistant posture: writes are allowed by
            // default, so a workspace-scoped hook deny would leave every OTHER
            // checkout on the machine plantable. The deny is a path regex, not a
            // workspace subpath — prove it holds in an unrelated repo.
            let dir = tempfile::tempdir().unwrap();
            let other = tempfile::tempdir().unwrap();
            let policy = enforced(dir.path().to_path_buf());
            let hook = other.path().join(".git/hooks/post-checkout");
            let cfg = other.path().join(".git/config");
            let (_c, out) = run(
                &policy,
                &format!(
                    "mkdir -p '{h}' 2>&1; echo evil > '{hook}' 2>&1; echo '[core]' > '{cfg}' 2>&1; echo DONE",
                    h = other.path().join(".git/hooks").display(),
                    hook = hook.display(),
                    cfg = cfg.display(),
                ),
            )
            .await;
            assert!(out.contains("DONE"));
            assert!(!hook.exists(), "hook in an unrelated repo must not be plantable: {out}");
            assert!(!cfg.exists(), "git config in an unrelated repo must not be writable: {out}");
        }

        #[tokio::test]
        async fn reading_cloud_and_registry_creds_is_denied() {
            let dir = tempfile::tempdir().unwrap();
            let policy = enforced(dir.path().to_path_buf());
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
            let policy = enforced(dir.path().to_path_buf());
            let (_c, out) = run(&policy, "env; echo DONE").await;
            std::env::remove_var("SMOOTH_API_KEY");
            std::env::remove_var("MY_SERVICE_TOKEN");

            assert!(out.contains("DONE"));
            assert!(!out.contains("LEAK_SENTINEL"), "scrubbed secrets must not appear in `env`: {out}");
            assert!(out.contains("PATH="), "non-secret env (PATH) should still be inherited: {out}");
        }

        #[tokio::test]
        async fn proxy_policy_injects_http_proxy_env() {
            let dir = tempfile::tempdir().unwrap();
            let policy = enforced(dir.path().to_path_buf()).with_proxy("127.0.0.1:3128");
            let (code, out) = run(&policy, "echo P=$HTTP_PROXY,$HTTPS_PROXY,$NO_PROXY").await;
            assert_eq!(code, 0, "command runs: {out}");
            assert!(
                out.contains("P=http://127.0.0.1:3128,http://127.0.0.1:3128,localhost"),
                "proxy env injected: {out}"
            );
        }

        #[tokio::test]
        async fn proxy_policy_profile_parses_and_runs() {
            // If the network-deny SBPL is malformed, sandbox-exec fails to parse
            // the profile and nothing runs. A clean exit proves the generated
            // profile (FS rules + network rules) is valid SBPL.
            let dir = tempfile::tempdir().unwrap();
            let policy = enforced(dir.path().to_path_buf()).with_proxy("127.0.0.1:3128");
            let (code, out) = run(&policy, "echo sandbox-ok").await;
            assert_eq!(code, 0, "proxy-policy profile must be valid SBPL and run: {out}");
            assert!(out.contains("sandbox-ok"), "{out}");
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
            let policy = enforced(dir.path().to_path_buf());
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

        /// The daemon's own runtime state — not just its credentials. The
        /// operator token is a bearer for the local WS endpoint, so a
        /// sandboxed shell that reads it can reconnect as the owner principal
        /// outside this conversation's permission mode; `schedules.db` is how
        /// that gets made persistent. Both must be kernel-denied like `~/.ssh`.
        ///
        /// Uses a FAKE home (`policy.home`), so the test never reads or
        /// clobbers the real operator's token.
        #[tokio::test]
        async fn reading_or_writing_operator_state_is_denied() {
            let fake_home = tempfile::tempdir().unwrap();
            let home = std::fs::canonicalize(fake_home.path()).unwrap();
            let smooth = home.join(".smooth");
            std::fs::create_dir_all(&smooth).unwrap();
            for (name, sentinel) in [
                ("operator-token", "SMOOTH_WS_TOKEN_SENTINEL_7b1e"),
                ("operator-storage.db", "SMOOTH_STORAGE_SENTINEL_7b1e"),
                ("schedules.db", "SMOOTH_SCHEDULE_SENTINEL_7b1e"),
                // SQLite keeps the newest rows in the WAL, so the deny has to
                // cover the sibling files too — hence `regex`, not `literal`.
                ("schedules.db-wal", "SMOOTH_WAL_SENTINEL_7b1e"),
            ] {
                std::fs::write(smooth.join(name), sentinel).unwrap();
            }

            let ws = tempfile::tempdir().unwrap();
            let mut policy = enforced(ws.path().to_path_buf());
            policy.home = Some(home.clone());
            let s = smooth.display();

            let (_c, out) = run(
                &policy,
                &format!("cat '{s}/operator-token' '{s}/operator-storage.db' '{s}/schedules.db' '{s}/schedules.db-wal' 2>&1; echo DONE"),
            )
            .await;
            assert!(out.contains("DONE"));
            assert!(
                !out.contains("SENTINEL_7b1e"),
                "operator token / storage / schedule store must not be readable in-sandbox: {out}"
            );

            // And not overwritable — planting a schedule is persistence, the
            // same primitive as the already-denied `~/Library/LaunchAgents`.
            let (_c2, out2) = run(
                &policy,
                &format!("echo pwned > '{s}/operator-token' 2>&1; echo pwned > '{s}/schedules.db' 2>&1; echo DONE"),
            )
            .await;
            assert!(out2.contains("DONE"));
            assert_eq!(
                std::fs::read_to_string(smooth.join("operator-token")).unwrap(),
                "SMOOTH_WS_TOKEN_SENTINEL_7b1e",
                "{out2}"
            );
            assert_eq!(
                std::fs::read_to_string(smooth.join("schedules.db")).unwrap(),
                "SMOOTH_SCHEDULE_SENTINEL_7b1e",
                "{out2}"
            );
        }

        /// The other half of the th-efbab1 regression: with the sandbox opted
        /// IN, the exact read that broke `git fetch` stays kernel-denied.
        #[tokio::test]
        async fn enforced_mode_still_denies_reading_ssh_under_a_fake_home() {
            let (_guard, fake_home, known_hosts) = fake_home_with_known_hosts();
            let ws = tempfile::tempdir().unwrap();
            let mut policy = enforced(ws.path().to_path_buf());
            policy.home = Some(fake_home.clone());
            let (_c, out) = run(&policy, &format!("cat '{}' 2>&1; echo DONE", known_hosts.display())).await;
            assert!(out.contains("DONE"));
            assert!(!out.contains(KNOWN_HOSTS_SENTINEL), "enforced sandbox must deny ~/.ssh reads: {out}");
        }

        #[tokio::test]
        async fn enforced_policy_reports_enforced_on_macos() {
            let dir = tempfile::tempdir().unwrap();
            assert!(enforced(dir.path().to_path_buf()).is_enforced());
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
    fn daemon_env_names_are_only_the_daemons_own() {
        for name in ["SMOOTH_API_KEY", "SMOOTH_DAEMON_TOKEN", "smooth_workspace", "SMOOAI_GATEWAY_KEY"] {
            assert!(is_daemon_env_name(name), "{name} is daemon config");
        }
        // The user's own credentials survive pass-through: the agent acts as them.
        for name in ["SSH_AUTH_SOCK", "GITHUB_TOKEN", "AWS_ACCESS_KEY_ID", "OPENAI_API_KEY", "PATH", "HOME"] {
            assert!(!is_daemon_env_name(name), "{name} is the user's, not the daemon's");
        }
    }

    /// Cross-platform: the home used for the credential-deny rules must come
    /// from `dirs_next`, not `$HOME` — `HOME` is unset on Windows, so an env
    /// read yielded `None` and dropped every deny rule (see `for_workspace`).
    #[test]
    fn policy_for_workspace_picks_up_home() {
        let p = SandboxPolicy::for_workspace(PathBuf::from("/ws"));
        assert_eq!(p.workspace, PathBuf::from("/ws"));
        assert_eq!(p.home, dirs_next::home_dir());
        assert!(p.home.is_some(), "a home directory must resolve on every supported platform");
    }

    /// The non-macOS shell must be the platform's own interpreter. Regression
    /// guard for the Windows break: `sh -c` spawned a binary that does not
    /// exist there, so every `bash` tool call failed to spawn.
    #[tokio::test]
    async fn non_macos_shell_uses_the_platform_interpreter() {
        use std::process::Stdio;
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path().to_path_buf()).with_mode(SandboxMode::PassThrough);
        // `echo hi` is valid in sh and cmd alike.
        let out = SandboxedCommand::shell(&policy, "echo hi")
            .into_command()
            .current_dir(dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .expect("the platform shell must exist and spawn");
        assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
    }

    #[cfg(unix)]
    const KNOWN_HOSTS_SENTINEL: &str = "smoo-hub ssh-ed25519 KNOWN_HOSTS_SENTINEL_efbab1";

    /// A temp HOME holding `.ssh/known_hosts` — the file `git fetch` failed on.
    /// Returns the guard, the canonical home (Seatbelt matches resolved paths)
    /// and the file.
    #[cfg(unix)]
    fn fake_home_with_known_hosts() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        let known_hosts = home.join(".ssh").join("known_hosts");
        std::fs::write(&known_hosts, KNOWN_HOSTS_SENTINEL).unwrap();
        (dir, home, known_hosts)
    }

    #[test]
    fn mode_is_off_by_default_and_opt_in() {
        assert_eq!(SandboxMode::default(), SandboxMode::PassThrough);
        assert_eq!(SandboxMode::from_env_value(None), SandboxMode::PassThrough, "unset = off");
        for on in ["1", "true", "TRUE", "yes", "on", " On "] {
            assert_eq!(SandboxMode::from_env_value(Some(on)), SandboxMode::Enforced, "{on:?} should enable");
            assert!(SandboxMode::from_env_value(Some(on)).is_requested());
        }
        for off in ["", "0", "false", "no", "off", "OFF"] {
            assert_eq!(SandboxMode::from_env_value(Some(off)), SandboxMode::PassThrough, "{off:?} should disable");
        }
        // A typo is not silently treated as "on" — it resolves to the default,
        // and `parse` reports it so startup can warn.
        assert_eq!(SandboxMode::parse("enabled"), None);
        assert_eq!(SandboxMode::from_env_value(Some("enabled")), SandboxMode::PassThrough);
    }

    #[test]
    fn is_enforced_needs_both_the_switch_and_platform_support() {
        let pass = SandboxPolicy::for_workspace(PathBuf::from("/ws")).with_mode(SandboxMode::PassThrough);
        assert!(!pass.is_enforced(), "pass-through is never enforced");
        let on = SandboxPolicy::for_workspace(PathBuf::from("/ws")).with_mode(SandboxMode::Enforced);
        assert_eq!(
            on.is_enforced(),
            cfg!(target_os = "macos"),
            "SMOOTH_SANDBOX=1 resolves to Seatbelt on macOS only"
        );
        assert_eq!(SandboxPolicy::platform_supported(), cfg!(target_os = "macos"));
    }

    /// The th-efbab1 regression: Brent asked Big Smooth to `ssh smoo-hub` and
    /// `git fetch`; git died with "Operation not permitted" on
    /// `~/.ssh/known_hosts`. With the default (pass-through) mode the shell
    /// runs as the user and can read it.
    #[cfg(unix)]
    #[tokio::test]
    async fn pass_through_reads_ssh_under_a_temp_home() {
        use std::process::Stdio;
        let (_guard, fake_home, _known_hosts) = fake_home_with_known_hosts();
        let ws = tempfile::tempdir().unwrap();
        let mut policy = SandboxPolicy::for_workspace(ws.path().to_path_buf()).with_mode(SandboxMode::PassThrough);
        policy.home = Some(fake_home.clone());
        let out = SandboxedCommand::shell(&policy, "cat \"$HOME/.ssh/known_hosts\"")
            .into_command()
            .env("HOME", &fake_home)
            .current_dir(ws.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "pass-through read must succeed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains(KNOWN_HOSTS_SENTINEL), "{stdout}");
    }

    /// Pass-through inherits the user's own credentials (the agent acts as the
    /// user — `gh`, `aws`, `ssh-agent` must work) but still strips the daemon's
    /// own `SMOOTH_*` config, and sets no proxy vars when no proxy is set.
    #[cfg(unix)]
    #[tokio::test]
    async fn pass_through_keeps_user_env_but_strips_daemon_config() {
        use std::process::Stdio;
        std::env::set_var("SMOOTH_PT_DAEMON_SENTINEL", "DAEMON_SENTINEL_efbab1");
        std::env::set_var("PT_USER_TOKEN_SENTINEL", "USER_SENTINEL_efbab1");
        let ws = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(ws.path().to_path_buf()).with_mode(SandboxMode::PassThrough);
        let out = SandboxedCommand::shell(&policy, "env")
            .into_command()
            .current_dir(ws.path())
            .stdout(Stdio::piped())
            .output()
            .await
            .unwrap();
        std::env::remove_var("SMOOTH_PT_DAEMON_SENTINEL");
        std::env::remove_var("PT_USER_TOKEN_SENTINEL");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("USER_SENTINEL_efbab1"), "user credentials are inherited: {stdout}");
        assert!(!stdout.contains("DAEMON_SENTINEL_efbab1"), "daemon SMOOTH_* config is stripped: {stdout}");
        // No proxy forced without one configured (the marker `shell` adds when
        // it sets the proxy vars is absent, unless the user's own env has it).
        if std::env::var("NO_PROXY").as_deref() != Ok("localhost,127.0.0.1,::1") {
            assert!(!stdout.contains("NO_PROXY=localhost,127.0.0.1,::1"), "no proxy forced: {stdout}");
        }
    }

    /// With an egress proxy configured but the sandbox off, the proxy vars are
    /// still set (advisory) — the allowlist is a request, not a boundary.
    #[cfg(unix)]
    #[tokio::test]
    async fn pass_through_with_proxy_sets_advisory_proxy_env() {
        use std::process::Stdio;
        let ws = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(ws.path().to_path_buf())
            .with_mode(SandboxMode::PassThrough)
            .with_proxy("127.0.0.1:3128");
        assert!(!policy.is_enforced());
        let out = SandboxedCommand::shell(&policy, "echo P=$HTTPS_PROXY")
            .into_command()
            .current_dir(ws.path())
            .stdout(Stdio::piped())
            .output()
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("P=http://127.0.0.1:3128"));
    }
}
