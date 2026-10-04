//! Adoption: a daemon that starts (after a crash, a restart or an update)
//! finds the hosts its predecessor left running and re-attaches to them.
//!
//! Everything needed is in the host records, so any daemon with the same
//! `owner` can adopt any of that owner's hosts. What happens to each record
//! (spec, "Adoption on daemon boot"):
//! - unreadable, or another owner's: left alone;
//! - its host is dead (pid gone, or a different process now has that pid):
//!   the record and socket are deleted, and the record is returned so the
//!   engine can settle the row from its `exit` (or as exit status unknown);
//! - its host speaks no protocol version we do: **left running** (`Held`),
//!   because killing a session we merely can't talk to loses the user's work;
//! - otherwise connected and handshaken: `Live`.

use std::os::unix::fs::FileTypeExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, killpg, Signal};
use nix::unistd::Pid;

use super::client::{ClientError, HostClient, OnEvent};
use super::record::{read_record, record_path, valid_id, HostRecord};

/// What one record turned out to be.
#[derive(Debug)]
pub enum Found {
    /// Connected; the session is live again.
    Live { record: HostRecord, client: HostClient },
    /// The host was dead; its record and socket are gone now. Settle the row
    /// from `record.exit`, or as exit status unknown.
    Stale { record: HostRecord },
    /// Alive but speaking no version we do. Left running; mark the row
    /// `held` with `reason`. [`kill_host`] can still end it.
    Held { record: HostRecord, reason: String },
    /// Alive but the connection failed (refused token, socket gone, a
    /// handshake timeout). Left running.
    Unreachable { record: HostRecord, reason: String },
    /// Another daemon's host (`owner` differs). Left alone.
    Foreign { record: HostRecord },
    /// Not a record we can read. Left alone.
    Unreadable { path: PathBuf, reason: String },
}

/// Scan `dir` and adopt every host `owner` created. `offer` is the protocol
/// range to offer (normally [`super::client::OFFER`]); `events` makes the
/// event sink for each host connected.
#[must_use]
pub fn adopt(dir: &Path, owner: &str, offer: (u32, u32), events: &dyn Fn(&HostRecord) -> OnEvent) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        // `.fs-….tmp` are a writer's temp files mid-rename.
        .filter(|p| p.extension().is_some_and(|x| x == "json") && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    paths.sort();
    paths.into_iter().map(|p| examine(dir, &p, owner, offer, events)).collect()
}

fn examine(dir: &Path, path: &Path, owner: &str, offer: (u32, u32), events: &dyn Fn(&HostRecord) -> OnEvent) -> Found {
    let record = match read_record(path) {
        Ok(r) => r,
        Err(e) => {
            return Found::Unreadable {
                path: path.to_path_buf(),
                reason: format!("{e:#}"),
            }
        }
    };
    // The file name must be the record's own: a record can't speak for (or
    // get us to delete) another id's files.
    if !valid_id(&record.id) || record_path(dir, &record.id) != path {
        return Found::Unreadable {
            path: path.to_path_buf(),
            reason: format!("record id {:?} does not match its file name", record.id),
        };
    }
    if record.owner != owner {
        return Found::Foreign { record };
    }
    if !record.host_alive() {
        remove_files(dir, &record);
        return Found::Stale { record };
    }
    if record.protocol < offer.0 || record.protocol > offer.1 {
        let reason = held_reason(record.protocol, offer);
        return Found::Held { record, reason };
    }
    match HostClient::connect(&record.socket, &record.token, offer, events(&record)) {
        Ok(client) => Found::Live { record, client },
        Err(ClientError::Version(_)) => {
            let reason = held_reason(record.protocol, offer);
            Found::Held { record, reason }
        }
        Err(e) => Found::Unreachable { record, reason: e.to_string() },
    }
}

fn held_reason(protocol: u32, offer: (u32, u32)) -> String {
    format!("host speaks protocol {protocol}; this daemon speaks {}..{}", offer.0, offer.1)
}

/// Delete a dead host's record and socket. The socket is only removed when
/// it is a socket named for this id, so a tampered record can't point us at
/// some other file.
fn remove_files(dir: &Path, record: &HostRecord) {
    let sock_name = format!("{}.sock", record.id);
    if record.socket.file_name().is_some_and(|n| n.to_string_lossy() == sock_name)
        && std::fs::symlink_metadata(&record.socket).is_ok_and(|m| m.file_type().is_socket())
    {
        let _ = std::fs::remove_file(&record.socket);
    }
    let _ = std::fs::remove_file(record_path(dir, &record.id));
}

/// End a host without the protocol (one we can't speak to).
///
/// SIGTERM the child's process group and the host, SIGKILL whatever is left after
/// `grace`, then delete the record and socket. Nothing is signalled unless
/// the record's pid is still the host it describes.
///
/// Returns whether the host is gone.
pub fn kill_host(dir: &Path, record: &HostRecord, grace: Duration) -> bool {
    if !record.host_alive() {
        remove_files(dir, record);
        return true;
    }
    let to_pid = |p: u32| Pid::from_raw(i32::try_from(p).unwrap_or(i32::MAX));
    let (host, child) = (to_pid(record.pid), to_pid(record.child_pid));
    // While the host lives it hasn't reaped a running child, so the child's
    // pid (its group id) is still the child's.
    let signal_all = |sig: Signal| {
        // The child's group only while the host hasn't recorded an exit (the
        // freshest record wins): after that its pid may be reused.
        let exited = read_record(&record_path(dir, &record.id)).map_or_else(|_| record.exit.is_some(), |r| r.exit.is_some());
        if !exited {
            let _ = killpg(child, sig);
        }
        let _ = kill(host, sig);
    };
    signal_all(Signal::SIGTERM);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !record.host_alive() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if record.host_alive() {
        signal_all(Signal::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(2);
        while record.host_alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let gone = !record.host_alive();
    if gone {
        remove_files(dir, record);
    }
    gone
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use super::*;
    use crate::session_host::record::{write_record, HostRecord};
    use std::sync::Arc;

    fn record(id: &str, owner: &str, pid: u32, pid_start: Option<i64>) -> HostRecord {
        HostRecord {
            v: 1,
            protocol: 1,
            id: id.into(),
            host_version: "0.0.0".into(),
            pid,
            pid_start,
            child_pid: pid,
            socket: PathBuf::from("/nonexistent/x.sock"),
            token: "0".repeat(64),
            owner: owner.into(),
            cwd: PathBuf::from("/"),
            argv: vec!["true".into()],
            created_at: "2026-10-03T00:00:00Z".into(),
            exit: None,
        }
    }

    fn no_events(_: &HostRecord) -> OnEvent {
        Arc::new(|_| {})
    }

    /// A pid that is certainly dead: a child we already reaped.
    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    #[test]
    fn stale_records_are_removed_and_returned_with_their_exit() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = record("fs-0000000a", "me", dead_pid(), Some(1));
        let sock = dir.path().join("fs-0000000a.sock");
        let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        r.socket = sock.clone();
        r.exit = Some(crate::session_host::protocol::ExitInfo {
            code: Some(4),
            signal: None,
            at: "then".into(),
        });
        write_record(dir.path(), &r).unwrap();
        let found = adopt(dir.path(), "me", (1, 1), &no_events);
        assert!(
            matches!(&found[..], [Found::Stale { record }] if record.exit.as_ref().unwrap().code == Some(4)),
            "{found:?}"
        );
        assert!(!record_path(dir.path(), "fs-0000000a").exists());
        assert!(!sock.exists());
    }

    /// Our own process is alive, but a different start time means the pid
    /// was recycled: the host is dead.
    #[test]
    fn a_recycled_pid_is_a_dead_host() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let start = crate::proc::start_time(me).unwrap();
        write_record(dir.path(), &record("fs-0000000b", "me", me, Some(start - 10_000))).unwrap();
        let found = adopt(dir.path(), "me", (1, 1), &no_events);
        assert!(matches!(&found[..], [Found::Stale { .. }]), "{found:?}");
    }

    #[test]
    fn foreign_unreadable_and_mismatched_records_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        write_record(dir.path(), &record("fs-0000000c", "someone-else", dead_pid(), Some(1))).unwrap();
        std::fs::write(dir.path().join("fs-0000000d.json"), b"{ not a record").unwrap();
        // A record claiming another id than its file name.
        let r = record("fs-0000000f", "me", dead_pid(), Some(1));
        std::fs::write(dir.path().join("fs-0000000e.json"), serde_json::to_vec(&r).unwrap()).unwrap();
        // A writer's temp file is not a record.
        std::fs::write(dir.path().join(".fs-00000010.abc.tmp"), b"x").unwrap();
        let found = adopt(dir.path(), "me", (1, 1), &no_events);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(matches!(found[0], Found::Foreign { .. }));
        assert!(matches!(found[1], Found::Unreadable { .. }));
        assert!(matches!(found[2], Found::Unreadable { .. }));
        for n in ["fs-0000000c.json", "fs-0000000d.json", "fs-0000000e.json"] {
            assert!(dir.path().join(n).exists(), "{n} left in place");
        }
    }

    /// A dead host's record that points its socket at an unrelated file
    /// must not get that file deleted.
    #[test]
    fn a_tampered_socket_path_is_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("precious.txt");
        std::fs::write(&victim, b"keep").unwrap();
        let mut r = record("fs-00000011", "me", dead_pid(), Some(1));
        r.socket = victim.clone();
        write_record(dir.path(), &r).unwrap();
        let found = adopt(dir.path(), "me", (1, 1), &no_events);
        assert!(matches!(&found[..], [Found::Stale { .. }]));
        assert!(victim.exists());
    }

    #[test]
    fn a_live_host_on_an_unknown_protocol_is_held_not_killed() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let mut r = record("fs-00000012", "me", me, crate::proc::start_time(me));
        r.protocol = 9;
        write_record(dir.path(), &r).unwrap();
        let found = adopt(dir.path(), "me", (1, 1), &no_events);
        match &found[..] {
            [Found::Held { reason, .. }] => assert_eq!(reason, "host speaks protocol 9; this daemon speaks 1..1"),
            other => panic!("{other:?}"),
        }
        assert!(record_path(dir.path(), "fs-00000012").exists());
    }

    #[test]
    fn a_live_host_whose_socket_is_gone_is_unreachable_and_left() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        write_record(dir.path(), &record("fs-00000013", "me", me, crate::proc::start_time(me))).unwrap();
        let found = adopt(dir.path(), "me", (1, 1), &no_events);
        assert!(matches!(&found[..], [Found::Unreachable { .. }]), "{found:?}");
        assert!(record_path(dir.path(), "fs-00000013").exists());
    }

    #[test]
    fn a_missing_dir_adopts_nothing() {
        assert!(adopt(Path::new("/nonexistent/flow-hosts"), "me", (1, 1), &no_events).is_empty());
    }
}
