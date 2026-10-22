//! Restore after an interruption (th-0937a1, SMOODEV-4113 phase 1).
//!
//! Quitting the SmoothFlow app kills its tmux server (`tmuxOwnedByApp`), and a
//! reboot kills everything, so a row can come back with no process behind it
//! for a reason that is not a crash. Telling the two apart is the point:
//!
//! - **interrupted** — the row was launched in an earlier boot, or its daemon
//!   recorded a clean quit ([`Engine::shutdown`](crate::Engine::shutdown))
//!   before its host went away. An agent whose conversation can be resumed is
//!   resumed, staggered, without spending the crash budget (rule 2).
//! - **crashed** — anything else, as before.
//!
//! Either way an agent is never relaunched with its original argv when that
//! argv carries the prompt: the row is left `dead` with attention
//! [`RESTORABLE`] instead, and resuming it is the user's call.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use crate::harness::{PromptAs, Registry, ResumeMode};
use crate::store::Session;

/// Attention on an interrupted agent row waiting for its staggered resume.
pub const RESTORING: &str = "restoring";
/// Attention on an agent row SmoothFlow will not relaunch by itself: it has
/// no harness session id to resume, and its original argv would send the
/// prompt again.
pub const RESTORABLE: &str = "restorable";
/// Gap between two interrupted agents' resumes, so a fleet coming back does
/// not start every harness at once.
pub const RESTORE_STAGGER: Duration = Duration::from_millis(300);
/// The `config` key prefix of a daemon's clean-quit marker; the suffix is the
/// daemon's supervision identity (its default socket), because two daemons
/// share one `flow.db`.
pub const CLEAN_QUIT_KEY: &str = "clean_quit";

/// The `config` key holding the clean-quit marker of the daemon supervising
/// as `owner`.
#[must_use]
pub fn clean_quit_key(owner: &str) -> String {
    format!("{CLEAN_QUIT_KEY}:{owner}")
}

/// Why a row's process is gone when it was not a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interruption {
    /// It was launched in an earlier boot of this machine.
    Reboot,
    /// Its daemon shut down cleanly, and its host went away after.
    Quit,
}

impl Interruption {
    /// How the row's attention says it.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Reboot => "interrupted by a restart of this machine",
            Self::Quit => "interrupted when SmoothFlow quit",
        }
    }
}

/// Classify a row whose process is gone.
///
/// `row_boot` is the boot it was launched in (`None` on rows from before the column), `current_boot` this
/// boot (`None` where it cannot be read), `clean_quit` whether its daemon
/// left a clean-quit marker. `None` means nothing says it was not a crash.
#[must_use]
pub fn interruption(row_boot: Option<&str>, current_boot: Option<&str>, clean_quit: bool) -> Option<Interruption> {
    match (row_boot, current_boot) {
        (Some(row), Some(now)) if row != now => Some(Interruption::Reboot),
        _ if clean_quit => Some(Interruption::Quit),
        _ => None,
    }
}

/// Whether resuming `session` continues its conversation.
///
/// Its manifest resumes by a harness session id it has, or continues the
/// latest conversation in its worktree. Otherwise a resume is a relaunch of the original argv.
#[must_use]
pub fn resume_continues(session: &Session, registry: &Registry) -> bool {
    registry.get(session.kind.as_str()).is_some_and(|m| match m.resume.mode {
        ResumeMode::ResumeSession => session.agent_session_id.is_some(),
        ResumeMode::ContinueLatest => true,
        ResumeMode::RelaunchCommand => false,
    })
}

/// Whether relaunching `session` would send its prompt a second time.
///
/// The relaunch is its original argv ([`resume_continues`] is false) and that argv
/// is where its manifest puts the prompt. A pasted prompt is pasted once, at
/// creation, so relaunching a `paste` harness starts it empty instead.
#[must_use]
pub fn relaunch_resends_prompt(session: &Session, registry: &Registry) -> bool {
    !resume_continues(session, registry) && registry.get(session.kind.as_str()).is_none_or(|m| m.launch.prompt_as != PromptAs::Paste)
}

/// This boot of this machine, or `None` where it cannot be read.
///
/// Read once per process: macOS's `kern.bootsessionuuid` (a fresh UUID per boot; unlike
/// `kern.boottime`, a wall-clock change does not move it), else
/// `kern.boottime`'s seconds; Linux's `/proc/sys/kernel/random/boot_id`.
#[must_use]
pub fn current_boot_id() -> Option<String> {
    static BOOT: OnceLock<Option<String>> = OnceLock::new();
    BOOT.get_or_init(read_boot_id).clone()
}

fn read_boot_id() -> Option<String> {
    if let Some(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(id);
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let sysctl = |name: &str| {
        let bin = if Path::new("/usr/sbin/sysctl").exists() {
            "/usr/sbin/sysctl"
        } else {
            "sysctl"
        };
        Command::new(bin)
            .args(["-n", name])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    sysctl("kern.bootsessionuuid").or_else(|| sysctl("kern.boottime").as_deref().and_then(parse_boottime))
}

/// `{ sec = 1792423769, usec = 956949 } Mon Oct 19 …` → `boottime:1792423769`.
#[must_use]
pub fn parse_boottime(s: &str) -> Option<String> {
    let rest = s.split("sec =").nth(1)?;
    let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| format!("boottime:{digits}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{FlowStore, NewSession, SessionKind};

    fn row(kind: SessionKind, agent: Option<&str>, argv: &[&str]) -> Session {
        FlowStore::open_in_memory()
            .unwrap()
            .create(NewSession {
                kind: Some(kind),
                agent_session_id: agent.map(str::to_string),
                argv: argv.iter().map(|a| (*a).to_string()).collect(),
                project: "/p".into(),
                worktree: "/p".into(),
                ..Default::default()
            })
            .unwrap()
    }

    #[test]
    fn a_new_boot_or_a_clean_quit_is_an_interruption_and_nothing_else_is() {
        assert_eq!(interruption(Some("a"), Some("b"), false), Some(Interruption::Reboot));
        assert_eq!(
            interruption(Some("a"), Some("b"), true),
            Some(Interruption::Reboot),
            "a reboot says more than a quit"
        );
        assert_eq!(interruption(Some("a"), Some("a"), true), Some(Interruption::Quit));
        assert_eq!(interruption(None, Some("a"), true), Some(Interruption::Quit), "a legacy row after a clean quit");
        assert_eq!(
            interruption(Some("a"), None, true),
            Some(Interruption::Quit),
            "an unreadable boot falls back to the marker"
        );
        assert_eq!(interruption(Some("a"), Some("a"), false), None, "same boot, no marker: a crash");
        assert_eq!(interruption(None, Some("a"), false), None, "a legacy row proves nothing");
        assert_eq!(interruption(Some("a"), None, false), None);
        assert_eq!(interruption(None, None, false), None);
        assert!(Interruption::Reboot.describe().contains("restart"));
        assert!(Interruption::Quit.describe().contains("quit"));
    }

    #[test]
    fn only_a_resume_that_continues_the_conversation_counts() {
        let reg = Registry::builtin();
        // claude / codex: resume by id — only with one.
        assert!(resume_continues(&row(SessionKind::Claude, Some("u"), &["claude"]), &reg));
        assert!(!resume_continues(&row(SessionKind::Codex, None, &["codex", "hi"]), &reg));
        assert!(resume_continues(&row(SessionKind::Codex, Some("t"), &["codex", "hi"]), &reg));
        // crush: continue-latest needs no id.
        let crush: crate::store::SessionKind = "crush".parse().unwrap();
        assert!(resume_continues(&row(crush, None, &["crush"]), &reg));
        // th code / cline relaunch their command.
        let th_code: crate::store::SessionKind = "th-code".parse().unwrap();
        assert!(!resume_continues(&row(th_code, Some("x"), &["th", "code"]), &reg));
        // A shell has no manifest.
        assert!(!resume_continues(&row(SessionKind::Shell, None, &["sh"]), &reg));
    }

    #[test]
    fn only_an_argv_prompt_is_resent_by_a_relaunch() {
        let reg = Registry::builtin();
        assert!(
            relaunch_resends_prompt(&row(SessionKind::Codex, None, &["codex", "hi"]), &reg),
            "codex without hooks"
        );
        assert!(
            !relaunch_resends_prompt(&row(SessionKind::Codex, Some("t"), &["codex", "hi"]), &reg),
            "resumed, not relaunched"
        );
        let cline: crate::store::SessionKind = "cline".parse().unwrap();
        assert!(relaunch_resends_prompt(&row(cline, None, &["cline", "--tui", "hi"]), &reg));
        let th_code: crate::store::SessionKind = "th-code".parse().unwrap();
        assert!(
            !relaunch_resends_prompt(&row(th_code, None, &["th", "code"]), &reg),
            "a pasted prompt is not in the argv"
        );
        let unknown: crate::store::SessionKind = "not-a-harness".parse().unwrap();
        assert!(
            relaunch_resends_prompt(&row(unknown, None, &["x", "hi"]), &reg),
            "no manifest: assume the worst"
        );
    }

    #[test]
    fn boot_ids_parse_and_are_stable_in_one_process() {
        assert_eq!(
            parse_boottime("{ sec = 1792423769, usec = 956949 } Mon Oct 19 11:29:29 2026").as_deref(),
            Some("boottime:1792423769")
        );
        assert_eq!(parse_boottime("garbage"), None);
        assert_eq!(parse_boottime("{ sec = , usec = 1 }"), None);
        assert_eq!(current_boot_id(), current_boot_id());
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(current_boot_id().is_some(), "this machine's boot is readable");
        }
        assert_eq!(clean_quit_key("smoothflow"), "clean_quit:smoothflow");
    }
}
