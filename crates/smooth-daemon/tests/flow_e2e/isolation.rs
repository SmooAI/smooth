//! Proof that the suite never touches the developer's Big Smooth, and that
//! the macOS lane's lighter host (`flow_e2e_server`) is the same engine.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::support::{e2e_server_bin, prereqs, real_daemon_addr, skip, state, Daemon, WAIT};

/// The real `~/.smooth/{daemon.addr,daemon.lock,operator-token,flow.db}`
/// — bytes + mtime of each that exists.
fn real_smooth_files() -> Vec<(String, Option<(Vec<u8>, std::time::SystemTime)>)> {
    let home = dirs_next::home_dir().unwrap();
    ["daemon.addr", "daemon.lock", "operator-token", "flow.db"]
        .into_iter()
        .map(|f| {
            let p = home.join(".smooth").join(f);
            let snap = std::fs::read(&p).ok().and_then(|b| std::fs::metadata(&p).ok()?.modified().ok().map(|m| (b, m)));
            (f.to_string(), snap)
        })
        .collect()
}

#[tokio::test]
async fn suite_never_writes_the_real_daemon_addr_or_token() {
    if !prereqs() {
        return;
    }
    let before = real_smooth_files();
    let real_addr = real_daemon_addr();
    {
        let d = Daemon::boot().await;
        // The rig's advertisement lives in ITS home, and is not the user's daemon.
        let test_addr = std::fs::read_to_string(d.home.join(".smooth/daemon.addr")).unwrap();
        assert_eq!(test_addr.trim(), d.addr);
        if let Some((bytes, _)) = &real_addr {
            assert_ne!(
                String::from_utf8_lossy(bytes).trim(),
                d.addr,
                "the rig must never land on the user's daemon port"
            );
        }
        assert!(d.home.join(".smooth/operator-token").is_file());
        assert!(d.home.join(".smooth/flow.db").is_file(), "flow.db is under the test HOME");
        // The rig's tmux server is private too: nothing on `smooth-flow`.
        let s = d.new_session("shell", None).await;
        let id = s["id"].as_str().unwrap().to_string();
        d.wait_state(&id, "idle", WAIT).await;
        assert!(d.socket.starts_with("flow-e2e-"));
        let on_default = Command::new("tmux")
            .args(["-L", "smooth-flow", "has-session", "-t", &id])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!on_default.success(), "the session must not exist on the default flow server");
        let on_private = Command::new("tmux").args(["-L", &d.socket, "has-session", "-t", &id]).status().unwrap();
        assert!(on_private.success());
        d.kill(&id, false).await;
    }
    // Drop killed the daemon and its tmux server; the real files are untouched.
    assert_eq!(real_smooth_files(), before, "a real ~/.smooth file changed during the test");
    assert_eq!(real_daemon_addr(), real_addr);
}

#[tokio::test]
async fn daemon_teardown_leaves_no_tmux_server_or_process() {
    if !prereqs() {
        return;
    }
    let (socket, pid) = {
        let d = Daemon::boot().await;
        let s = d.new_session("fake-agent", Some("/work bye")).await;
        let id = s["id"].as_str().unwrap().to_string();
        d.wait_until(&id, "idle", WAIT, |s| state(s) == "idle").await;
        (d.socket.clone(), s["pid"].as_u64().unwrap() as u32)
    };
    let gone = Command::new("tmux")
        .args(["-L", &socket, "list-sessions"])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(!gone.success(), "the private tmux server is killed with the rig");
    let start = Instant::now();
    while Daemon::pid_alive(pid) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!Daemon::pid_alive(pid), "the agent pane's process died with its tmux server");
}

/// The macOS XCUITest lane hosts `flow_e2e_server` (the daemon's router +
/// supervisor, nothing else). Same manifests, same fake-agent, same states —
/// so a green mac lane means the same engine the daemon ships.
#[tokio::test]
async fn flow_e2e_server_example_hosts_the_same_engine() {
    if !prereqs() {
        return;
    }
    let Some(server) = e2e_server_bin() else {
        skip("flow_e2e_server not built: cargo build -p smooai-smooth-daemon --example flow_e2e_server");
        return;
    };
    // Borrow the rig for its HOME (fixtures installed) and workspace, but
    // point the example at them instead of the daemon.
    let d = Daemon::boot().await;
    let socket = format!("{}-ex", d.socket);
    let mut path = d.home.join(".local/bin").into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let mut child = Command::new(&server)
        .args(["--addr", "127.0.0.1:0", "--workspace"])
        .arg(&d.ws)
        .arg("--db")
        .arg(d.home.join("example-flow.db"))
        .args(["--token", "ex-tok", "--tmux-socket", &socket])
        .env_clear()
        .env("PATH", path)
        .env("HOME", &d.home)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let addr_file = d.ws.join(".flow-e2e-addr");
    let start = Instant::now();
    let addr = loop {
        let a = std::fs::read_to_string(&addr_file).unwrap_or_default().trim().to_string();
        if !a.is_empty() {
            break a;
        }
        assert!(start.elapsed() < WAIT, "flow_e2e_server never advertised");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let http = reqwest::Client::new();
    let post = |path: String, body: serde_json::Value| {
        let http = http.clone();
        let addr = addr.clone();
        async move {
            http.post(format!("http://{addr}{path}"))
                .header("x-smooth-token", "ex-tok")
                .json(&body)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    // No `{daemon_url}` here — fake-agent falls back to ./.flow-e2e-addr,
    // exactly what the mac lane relies on.
    let v = post("/api/flow/sessions".into(), json!({"kind":"fake-agent","worktree":d.ws,"prompt":"/work ex"})).await;
    let id = v["session"]["id"].as_str().unwrap().to_string();
    let start = Instant::now();
    loop {
        let list = http
            .get(format!("http://{addr}/api/flow/sessions"))
            .header("x-smooth-token", "ex-tok")
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        let s = &list["sessions"][0];
        if s["id"] == id && s["state"] == "idle" && s["state_source"] == "hooks" {
            break;
        }
        assert!(start.elapsed() < WAIT, "never idle via hooks on the example server: {list}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    post(format!("/api/flow/sessions/{id}/kill"), json!({})).await;
    let _ = child.kill();
    let _ = child.wait();
    let _ = Command::new("tmux").args(["-L", &socket, "kill-server"]).output();
}
