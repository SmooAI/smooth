//! `smooth-daemon flow-host` is the shipped session host (th-e4aef9). The
//! protocol is tested exhaustively in smooth-flow's `tests/session_host.rs`
//! against its test-support binary; this proves the daemon's own subcommand
//! is wired to the same host: spawn it as the engine will, connect, drive
//! the child, read its exact exit code, release it.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test assertions")]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smooth_flow::session_host::client::{spawn_host, HostClient, HostCommand, HostEvent, OFFER, READY_TIMEOUT};
use smooth_flow::session_host::protocol::SpawnRequest;
use smooth_flow::session_host::record::{new_token, read_record, record_path};

#[test]
fn the_daemon_subcommand_hosts_a_session() {
    let dir = tempfile::Builder::new().prefix("fh").tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()));
    let req = SpawnRequest {
        token: new_token(),
        dir: dir.path().to_path_buf(),
        argv: vec!["sh".into(), "-c".into(), "printf 'hello from %s' \"$0\"; read x; exit 7".into()],
        cwd: dir.path().to_path_buf(),
        env,
        cols: 80,
        rows: 24,
        seq_start: 0,
        scrollback_rows: 1000,
        linger_secs: 60,
        owner: "daemon-test".into(),
        max_queue_bytes: None,
    };
    let id = "fs-d0d0d0d0";
    let cmd = HostCommand::daemon(env!("CARGO_BIN_EXE_smooth-daemon"));
    let spawned = spawn_host(&cmd, id, &req, READY_TIMEOUT * 4).unwrap();
    let rec = read_record(&record_path(dir.path(), id)).unwrap();
    assert_eq!(rec.pid, spawned.pid);

    let exits = Arc::new(Mutex::new(Vec::new()));
    let sink = exits.clone();
    let client = HostClient::connect(
        &spawned.socket,
        &req.token,
        OFFER,
        Arc::new(move |e| {
            if let HostEvent::Exit { code, .. } = e {
                sink.lock().unwrap().push(code);
            }
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.screen().unwrap().text.contains("hello from sh") {
        assert!(Instant::now() < deadline, "the child's output never reached the host's terminal");
        std::thread::sleep(Duration::from_millis(20));
    }
    client.input(b"\n").unwrap();
    while exits.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "no exit");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(exits.lock().unwrap()[0], Some(7));
    client.release().unwrap();
    while rec.host_alive() {
        assert!(Instant::now() < deadline, "the released host did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!record_path(dir.path(), id).exists());
}
