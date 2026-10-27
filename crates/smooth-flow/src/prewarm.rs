//! Pre-warmed shells (SMOODEV-4127): ⌘N as fast as a terminal.
//!
//! A login shell takes ~0.4–0.7 s to reach its first prompt (rc files,
//! prompt themes, `git status` in the prompt), and nothing the engine does
//! can make that faster. So it is done early: the engine keeps a few
//! **spare** shells already sitting at a prompt, one per directory a new
//! shell is likely to open in, and `flow.new {kind: shell}` in that
//! directory *claims* one instead of launching — the row is created under
//! the id the spare was launched with, its tmux session is renamed to that
//! id, and the first attach draws a prompt that is already there.
//!
//! A spare is invisible: it has no row, so it is not in `flow.hello`,
//! `th flow ls` or on a phone. Its tmux session name starts with
//! [`SPARE_PREFIX`] so a daemon that died with spares up can reap them
//! on its next start. Which directories get one: the directory of every
//! shell just created (a second ⌘N there is the common case) and whatever a
//! client hints with `flow.prewarm` (SmoothFlow sends the focused pane's
//! directory). At most [`CAP`] at once, least recently wanted out first; a
//! spare older than [`MAX_AGE`] is replaced rather than handed out.
//!
//! `SMOOTH_FLOW_PREWARM=0` turns it off.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// tmux session-name prefix of a spare.
pub const SPARE_PREFIX: &str = "flowspare-";

/// Spares kept at once.
pub const CAP: usize = 3;

/// A spare older than this is stale (its prompt's git status, its env) and
/// is replaced instead of claimed.
pub const MAX_AGE: Duration = Duration::from_mins(30);

/// The env switch.
pub const ENV: &str = "SMOOTH_FLOW_PREWARM";

/// Whether pre-warming is on for this value of [`ENV`] (unset ⇒ on).
#[must_use]
pub fn enabled(var: Option<&str>) -> bool {
    !matches!(var.map(str::trim), Some("0" | "false" | "off" | "no"))
}

/// The tmux session name a spare with session id `id` runs under.
#[must_use]
pub fn spare_name(id: &str) -> String {
    format!("{SPARE_PREFIX}{id}")
}

/// Whether a tmux session name is a spare's.
#[must_use]
pub fn is_spare(name: &str) -> bool {
    name.starts_with(SPARE_PREFIX)
}

/// One shell waiting at a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spare {
    /// The session id it was launched for (its exit wrapper knows it).
    pub id: String,
    pub dir: PathBuf,
    pub socket: String,
    pub pid: u32,
    pub pid_start: Option<i64>,
    pub argv: Vec<String>,
    pub born: Instant,
}

impl Spare {
    /// The tmux session it runs under until claimed.
    #[must_use]
    pub fn tmux_name(&self) -> String {
        spare_name(&self.id)
    }
}

/// The spares, most recently wanted last, plus the directories being warmed.
#[derive(Debug, Default)]
pub struct Pool {
    spares: Vec<Spare>,
    warming: Vec<PathBuf>,
}

impl Pool {
    /// Take the spare for `dir` on `socket`, if there is one. A stale one is
    /// returned in the second slot for the caller to kill.
    pub fn take(&mut self, dir: &Path, socket: &str, now: Instant) -> (Option<Spare>, Option<Spare>) {
        let Some(i) = self.spares.iter().position(|s| s.dir == dir && s.socket == socket) else {
            return (None, None);
        };
        let s = self.spares.remove(i);
        if now.duration_since(s.born) > MAX_AGE {
            (None, Some(s))
        } else {
            (Some(s), None)
        }
    }

    /// Should a spare be started for `dir`? No when one is there or on its
    /// way. A spare already there counts as wanted again (it moves to the
    /// back of the eviction line).
    pub fn want(&mut self, dir: &Path, socket: &str) -> bool {
        if let Some(i) = self.spares.iter().position(|s| s.dir == dir && s.socket == socket) {
            let s = self.spares.remove(i);
            self.spares.push(s);
            return false;
        }
        if self.warming.iter().any(|d| d == dir) {
            return false;
        }
        self.warming.push(dir.to_path_buf());
        true
    }

    /// A warm-up for `dir` ended without a spare (launch failed).
    pub fn abandon(&mut self, dir: &Path) {
        self.warming.retain(|d| d != dir);
    }

    /// A warm-up finished: keep the spare, and return whatever it pushed out
    /// (over [`CAP`], oldest wanted first) for the caller to kill.
    pub fn insert(&mut self, spare: Spare) -> Vec<Spare> {
        self.warming.retain(|d| d != &spare.dir);
        // A racing warm-up for the same directory: keep the newer.
        let mut out: Vec<Spare> = Vec::new();
        if let Some(i) = self.spares.iter().position(|s| s.dir == spare.dir && s.socket == spare.socket) {
            out.push(self.spares.remove(i));
        }
        self.spares.push(spare);
        while self.spares.len() > CAP {
            out.push(self.spares.remove(0));
        }
        out
    }

    /// Every spare, for shutdown.
    pub fn drain(&mut self) -> Vec<Spare> {
        self.warming.clear();
        std::mem::take(&mut self.spares)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.spares.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spares.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;

    fn spare(id: &str, dir: &str, born: Instant) -> Spare {
        Spare {
            id: id.into(),
            dir: dir.into(),
            socket: "s".into(),
            pid: 1,
            pid_start: None,
            argv: vec!["zsh".into(), "-l".into()],
            born,
        }
    }

    #[test]
    fn env_switch() {
        assert!(enabled(None));
        assert!(enabled(Some("1")));
        assert!(enabled(Some("")));
        for off in ["0", "false", "off", "no", " 0 "] {
            assert!(!enabled(Some(off)), "{off}");
        }
    }

    #[test]
    fn names() {
        assert_eq!(spare_name("fs-1"), "flowspare-fs-1");
        assert!(is_spare("flowspare-fs-1"));
        assert!(!is_spare("fs-1"));
        assert_eq!(spare("fs-2", "/x", Instant::now()).tmux_name(), "flowspare-fs-2");
    }

    #[test]
    fn take_matches_directory_and_socket_once() {
        let now = Instant::now();
        let mut p = Pool::default();
        assert!(p.want(Path::new("/a"), "s"));
        assert!(!p.want(Path::new("/a"), "s"), "already warming");
        assert!(p.insert(spare("fs-1", "/a", now)).is_empty());
        assert_eq!(p.take(Path::new("/b"), "s", now), (None, None));
        assert_eq!(p.take(Path::new("/a"), "other", now), (None, None), "another socket's server");
        let (got, stale) = p.take(Path::new("/a"), "s", now);
        assert_eq!(got.unwrap().id, "fs-1");
        assert!(stale.is_none());
        assert_eq!(p.take(Path::new("/a"), "s", now), (None, None), "claimed once");
        assert!(p.is_empty());
    }

    #[test]
    fn a_stale_spare_is_handed_back_for_killing() {
        let born = Instant::now();
        let mut p = Pool::default();
        p.insert(spare("fs-1", "/a", born));
        let (got, stale) = p.take(Path::new("/a"), "s", born + MAX_AGE + Duration::from_secs(1));
        assert!(got.is_none());
        assert_eq!(stale.unwrap().id, "fs-1");
    }

    #[test]
    fn over_cap_the_least_recently_wanted_goes() {
        let now = Instant::now();
        let mut p = Pool::default();
        for (i, d) in ["/a", "/b", "/c"].iter().enumerate() {
            assert!(p.want(Path::new(d), "s"));
            assert!(p.insert(spare(&format!("fs-{i}"), d, now)).is_empty());
        }
        // Wanting /a again moves it to the back of the line.
        assert!(!p.want(Path::new("/a"), "s"));
        assert!(p.want(Path::new("/d"), "s"));
        let out = p.insert(spare("fs-3", "/d", now));
        assert_eq!(
            out.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["fs-1"],
            "/b was the least recently wanted"
        );
        assert_eq!(p.len(), CAP);
    }

    #[test]
    fn a_racing_warm_up_keeps_the_newer_spare() {
        let now = Instant::now();
        let mut p = Pool::default();
        p.insert(spare("fs-1", "/a", now));
        let out = p.insert(spare("fs-2", "/a", now));
        assert_eq!(out[0].id, "fs-1");
        assert_eq!(p.take(Path::new("/a"), "s", now).0.unwrap().id, "fs-2");
    }

    #[test]
    fn abandon_lets_the_directory_be_tried_again() {
        let mut p = Pool::default();
        assert!(p.want(Path::new("/a"), "s"));
        p.abandon(Path::new("/a"));
        assert!(p.want(Path::new("/a"), "s"));
    }

    #[test]
    fn drain_empties_everything() {
        let mut p = Pool::default();
        p.want(Path::new("/b"), "s");
        p.insert(spare("fs-1", "/a", Instant::now()));
        assert_eq!(p.drain().len(), 1);
        assert!(p.is_empty());
        assert!(p.want(Path::new("/b"), "s"), "warming cleared too");
    }
}
