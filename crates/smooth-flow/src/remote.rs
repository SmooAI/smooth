//! Remote sessions over the system `ssh` (SMOODEV-4269).
//!
//! A remote session is an ordinary fleet row whose process is a local `ssh`:
//! the session host (or tmux) owns that `ssh`, and the program runs on the
//! other machine. The row's `argv` keeps what runs *there*, so resume works
//! unchanged; [`RemoteTarget::ssh_argv`] wraps it at launch, the one place
//! `ssh` is spelled. Nothing is installed on the remote host.
//!
//! The system binary is the point: `~/.ssh/config`, agents, `ProxyJump` and
//! `ControlMaster` all apply, and a password or 2FA prompt appears in the pane
//! as it would in any terminal. Credentials never reach the engine as data:
//! a host value carrying a password is refused, no agent forwarding is asked
//! for, and what you type at a prompt goes straight to the PTY.
//!
//! What survives what is in `docs/Architecture/SmoothFlow.md` § Remote
//! sessions. In short: the remote program lives as long as the connection,
//! not as long as the row.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// The binary every remote session runs, resolved on the pane's `PATH`.
pub const SSH_BIN: &str = "ssh";
/// `ssh`'s own failure status: it could not connect, or the connection died.
/// A remote program can also exit 255; nothing can tell the two apart.
pub const SSH_FAILED_EXIT: i32 = 255;
/// Probe a silent link this often, and give up after this many misses, so a
/// dead network ends the `ssh` in about 90 s and the row says so. Without
/// these a lost link looks like a quiet session for as long as TCP lets it.
const KEEPALIVE: [&str; 4] = ["-o", "ServerAliveInterval=30", "-o", "ServerAliveCountMax=3"];
/// What a remote pane's `TERM` is. The daemon's own could be a local-only
/// entry (`xterm-ghostty`) the far side has no terminfo for.
pub const REMOTE_TERM: &str = "xterm-256color";
/// DNS allows 253; leave room for `user@`.
const MAX_HOST_LEN: usize = 255;

/// Where a remote session runs. `host` is what you would type after `ssh`: an
/// alias from `~/.ssh/config`, a host name or address, optionally `user@`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTarget {
    pub host: String,
    /// The directory on the remote host; `None` is the remote login directory.
    /// `~` and `~/…` mean the remote home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Refuse anything but a plain destination.
///
/// `ssh` reads a leading `-` as an option (`-oProxyCommand=…` runs a command),
/// so that is the case that matters most; [`RemoteTarget::ssh_argv`] also puts
/// `--` before the host. The character set leaves no room for whitespace,
/// shell syntax, `=` or `/`. A `:` before the `@` is a `user:password` pair,
/// and a password must not end up in `flow.db` or in `ps`. The port comes from
/// `~/.ssh/config`.
///
/// # Errors
/// With the reason, when `host` is not a plain destination.
pub fn validate_host(host: &str) -> Result<()> {
    if host.is_empty() {
        bail!("an SSH host is required");
    }
    if host.len() > MAX_HOST_LEN {
        bail!("SSH host is longer than {MAX_HOST_LEN} characters");
    }
    // Before the character check, so an `-oProxyCommand=…` gets the real reason.
    let (user, name) = host.rsplit_once('@').map_or((None, host), |(u, n)| (Some(u), n));
    if host.starts_with('-') || name.starts_with('-') {
        bail!("SSH host may not start with `-`: it would be read as an ssh option");
    }
    if let Some(bad) = host
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | ':' | '%')))
    {
        bail!("SSH host may not contain {bad:?}: use `[user@]host`, or a Host alias from ~/.ssh/config");
    }
    if name.is_empty() {
        bail!("SSH host has no host name after the `@`");
    }
    if let Some(user) = user {
        if user.is_empty() || user.contains('@') {
            bail!("SSH host must look like `[user@]host`");
        }
        if user.contains(':') {
            bail!("SSH host may not carry a password: use an ssh agent, or answer the prompt in the session");
        }
    }
    Ok(())
}

/// Quote `s` as one word for a POSIX shell.
#[must_use]
pub fn posix_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `cd` to `dir`: `~` stays the remote home, everything else is literal.
fn cd_clause(dir: &str) -> String {
    let word = match dir {
        "~" => "\"$HOME\"".to_string(),
        d => match d.strip_prefix("~/") {
            Some("") => "\"$HOME\"".to_string(),
            Some(rest) => format!("\"$HOME\"/{}", posix_quote(rest)),
            // `cd -x` is an option and `cd -` the previous directory.
            None if d.starts_with('-') => posix_quote(&format!("./{d}")),
            None => posix_quote(d),
        },
    };
    format!("cd {word}")
}

impl RemoteTarget {
    /// A checked target: `host` trimmed and valid, an empty `cwd` dropped.
    ///
    /// # Errors
    /// When the host is refused (see [`validate_host`]) or the directory holds
    /// a control character.
    pub fn new(host: &str, cwd: Option<&str>) -> Result<Self> {
        let host = host.trim();
        validate_host(host)?;
        let cwd = cwd.map(str::trim).filter(|c| !c.is_empty());
        if cwd.is_some_and(|c| c.chars().any(char::is_control)) {
            bail!("remote directory may not contain control characters");
        }
        Ok(Self {
            host: host.to_string(),
            cwd: cwd.map(str::to_string),
        })
    }

    /// The command line that runs `argv` (empty: a login shell) on the host.
    ///
    /// `ssh` joins its command words with spaces and hands the result to the
    /// remote login shell, so this is exactly one word of shell text. Agents
    /// run through `$SHELL -lc`, because a non-login `ssh host cmd` has a bare
    /// `PATH` that rarely holds `~/.local/bin/claude`. The text assumes a
    /// POSIX-style login shell (sh, bash, zsh, fish); csh is not supported.
    fn remote_command(&self, argv: &[String]) -> Option<String> {
        let cd = self.cwd.as_deref().map(cd_clause);
        if argv.is_empty() {
            return cd.map(|cd| format!("{cd} && exec \"$SHELL\" -l"));
        }
        let words: Vec<String> = argv.iter().map(|a| posix_quote(a)).collect();
        let run = format!("exec {}", words.join(" "));
        let inner = cd.map_or_else(|| run.clone(), |cd| format!("{cd} && {run}"));
        Some(format!("exec \"$SHELL\" -lc {}", posix_quote(&inner)))
    }

    /// The local command line for this target. `argv` is what runs remotely.
    ///
    /// Re-checks the target, because a row outlives the code that wrote it.
    /// `--` ends `ssh`'s options, so nothing after it can become one.
    ///
    /// # Errors
    /// When the target is not valid.
    pub fn ssh_argv(&self, argv: &[String]) -> Result<Vec<String>> {
        let target = Self::new(&self.host, self.cwd.as_deref())?;
        let mut out: Vec<String> = vec![SSH_BIN.into(), "-t".into()];
        out.extend(KEEPALIVE.iter().map(ToString::to_string));
        out.push("--".into());
        out.push(target.host.clone());
        out.extend(target.remote_command(argv));
        Ok(out)
    }

    /// The row's `project`: every session on one host groups under it in a
    /// client's fleet, and its last path component reads as the host.
    #[must_use]
    pub fn project(&self) -> String {
        format!("ssh://{}", self.host)
    }

    /// How a row names this target: `box`, or `box:~/src/api`.
    #[must_use]
    pub fn label(&self) -> String {
        self.cwd.as_ref().map_or_else(|| self.host.clone(), |c| format!("{}:{c}", self.host))
    }

    /// Why a row ended, when its `ssh` exited [`SSH_FAILED_EXIT`].
    #[must_use]
    pub fn lost_detail(&self) -> String {
        format!("SSH to {} failed or the connection dropped (exit {SSH_FAILED_EXIT})", self.host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn plain_destinations_are_accepted() {
        for ok in [
            "box",
            "dev-box",
            "box.tail1234.ts.net",
            "me@box",
            "my_user@10.0.0.2",
            "::1",
            "me@fe80::1%en0",
            "a-b",
        ] {
            assert!(validate_host(ok).is_ok(), "{ok} should be valid");
        }
    }

    /// The argument-injection case: a value `ssh` would read as an option.
    #[test]
    fn a_host_starting_with_a_dash_is_refused() {
        for bad in [
            "-oProxyCommand=touch /tmp/x",
            "-oProxyCommand=x",
            "-J",
            "-",
            "--",
            "me@-oProxyCommand=x",
            "me@-box",
        ] {
            let err = validate_host(bad).unwrap_err().to_string();
            assert!(err.contains("may not start with `-`"), "{bad}: {err}");
        }
        assert!(RemoteTarget::new("-oProxyCommand=x", None).is_err());
        assert!(RemoteTarget::new("  -oProxyCommand=x", None).is_err(), "trimming must not hide it");
    }

    #[test]
    fn hostile_and_malformed_hosts_are_refused() {
        for bad in [
            "",
            "box;id",
            "box id",
            "box\nid",
            "box\0",
            "$(id)",
            "`id`",
            "box|id",
            "box&",
            "box>x",
            "'box'",
            "\"box\"",
            "ssh://box",
            "box/x",
            "a=b",
            "bo\u{78}\u{301}x",
            "me@",
            "@box",
            "a@b@c",
        ] {
            assert!(validate_host(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(validate_host(&"a".repeat(MAX_HOST_LEN + 1)).is_err());
        assert!(validate_host(&"a".repeat(MAX_HOST_LEN)).is_ok());
    }

    #[test]
    fn a_password_in_the_host_is_refused_and_not_echoed() {
        let err = validate_host("me:hunter2@box").unwrap_err().to_string();
        assert!(err.contains("password"), "{err}");
        assert!(!err.contains("hunter2"), "the error must not repeat it: {err}");
    }

    #[test]
    fn a_row_with_a_bad_host_never_reaches_ssh() {
        let forged = RemoteTarget {
            host: "-oProxyCommand=x".into(),
            cwd: None,
        };
        assert!(forged.ssh_argv(&[]).is_err());
    }

    #[test]
    fn a_control_character_in_the_directory_is_refused() {
        assert!(RemoteTarget::new("box", Some("/a\nb")).is_err());
        assert!(RemoteTarget::new("box", Some("/a\u{1b}b")).is_err());
        assert_eq!(RemoteTarget::new("box", Some("  ")).unwrap().cwd, None);
    }

    #[test]
    fn the_host_always_follows_the_end_of_options() {
        let v = RemoteTarget::new("box", None).unwrap().ssh_argv(&[]).unwrap();
        assert_eq!(
            v,
            strs(&["ssh", "-t", "-o", "ServerAliveInterval=30", "-o", "ServerAliveCountMax=3", "--", "box"])
        );
    }

    /// Credential-bearing or forwarding flags are the user's config to choose.
    #[test]
    fn no_forwarding_or_batch_flags_are_added() {
        let v = RemoteTarget::new("box", Some("/x")).unwrap().ssh_argv(&strs(&["claude"])).unwrap();
        assert!(!v.iter().any(|a| a == "-A" || a == "-X" || a == "-Y" || a.contains("BatchMode")), "{v:?}");
    }

    #[test]
    fn a_shell_in_a_directory_cds_then_becomes_the_login_shell() {
        let v = RemoteTarget::new("box", Some("/srv/my app")).unwrap().ssh_argv(&[]).unwrap();
        assert_eq!(v.last().unwrap(), "cd '/srv/my app' && exec \"$SHELL\" -l");
    }

    #[test]
    fn the_remote_home_stays_the_remote_home() {
        let cmd = |d| RemoteTarget::new("box", Some(d)).unwrap().remote_command(&[]).unwrap();
        assert!(cmd("~").starts_with("cd \"$HOME\" &&"));
        assert!(cmd("~/").starts_with("cd \"$HOME\" &&"));
        assert!(cmd("~/my src").starts_with("cd \"$HOME\"/'my src' &&"));
        assert!(cmd("~other").starts_with("cd '~other' &&"), "only the user's own home expands");
        assert!(cmd("-x").starts_with("cd './-x' &&"), "never an option to cd");
        assert!(cmd("-").starts_with("cd './-' &&"), "never `cd -`");
    }

    #[test]
    fn quoting_survives_a_single_quote() {
        assert_eq!(posix_quote("it's"), r"'it'\''s'");
        assert_eq!(posix_quote(""), "''");
    }

    #[test]
    fn the_row_names_its_target() {
        let t = RemoteTarget::new(" me@box ", Some("~/api")).unwrap();
        assert_eq!(t.host, "me@box");
        assert_eq!(t.project(), "ssh://me@box");
        assert_eq!(t.label(), "me@box:~/api");
        assert_eq!(RemoteTarget::new("box", None).unwrap().label(), "box");
    }

    /// The whole path against a fake `ssh` that does what the real one does:
    /// joins its command words and hands them to the login shell. Every
    /// hostile argument must arrive as one literal word, and nothing in it
    /// may run. `-G`, which makes the real binary parse without connecting, is
    /// the companion check recorded in the docs.
    #[cfg(unix)]
    #[test]
    fn hostile_arguments_arrive_literally_through_a_fake_ssh() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("ssh");
        std::fs::write(
            &shim,
            "#!/bin/sh\nwhile [ \"$1\" != \"--\" ]; do shift; done\nshift\nprintf '%s\\n' \"$1\" > \"$OUT.host\"\nshift\nexec /bin/sh -c \"$*\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let probe = tmp.path().join("probe");
        std::fs::write(
            &probe,
            "#!/bin/sh\npwd > \"$OUT.pwd\"\nfor a in \"$@\"; do printf '[%s]\\n' \"$a\"; done > \"$OUT.args\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = tmp.path().join("it's a dir");
        std::fs::create_dir(&dir).unwrap();
        let pwned = tmp.path().join("pwned");
        let hostile = [
            format!("$(touch {})", pwned.display()),
            format!("`touch {}`", pwned.display()),
            format!("'; touch {}; '", pwned.display()),
            "two words".to_string(),
            "line\nbreak".to_string(),
            "--flag".to_string(),
            String::new(),
        ];
        let mut argv = vec![probe.to_string_lossy().into_owned()];
        argv.extend(hostile.iter().cloned());
        let target = RemoteTarget::new("me@box", Some(&dir.to_string_lossy())).unwrap();
        let cmd = target.ssh_argv(&argv).unwrap();
        assert_eq!(cmd[0], SSH_BIN);
        let out = tmp.path().join("out");
        let status = std::process::Command::new(&shim)
            .args(&cmd[1..])
            .env("OUT", &out)
            .env("SHELL", "/bin/sh")
            .env("HOME", tmp.path())
            .env("ENV", "/dev/null")
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let read = |ext: &str| std::fs::read_to_string(out.with_extension(ext)).unwrap();
        assert_eq!(read("host"), "me@box\n");
        let want: String = hostile.iter().map(|a| format!("[{a}]\n")).collect();
        assert_eq!(read("args"), want);
        let pwd = std::fs::canonicalize(read("pwd").trim()).unwrap();
        assert_eq!(pwd, std::fs::canonicalize(&dir).unwrap());
        assert!(!pwned.exists(), "a hostile argument was evaluated");
    }
}
