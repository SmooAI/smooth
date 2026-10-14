//! `th chrome host` — the native-messaging host Chrome launches for the extension.
//!
//! Bridges the extension's stdio port to a Unix socket (`<dir>/socks/<pid>.sock`, 0600 in a
//! 0700 dir) that `th chrome …` and the MCP tools connect to. No TCP, so neither web pages nor
//! other users can reach it. One host per Chrome profile that has the extension.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::native::{read_frame, write_frame};

const CALL_TIMEOUT: Duration = Duration::from_secs(90);

type Pending = Arc<Mutex<HashMap<u64, Sender<Value>>>>;

struct Shared {
    to_chrome: Mutex<Box<dyn Write + Send>>,
    pending: Pending,
    hello: Mutex<Value>,
    next_id: AtomicU64,
    socket: PathBuf,
}

/// Create the socket dir (0700) and bind `<dir>/<pid>.sock` (0600).
///
/// # Errors
/// Filesystem or bind failure.
pub fn bind(dir: &Path) -> Result<(UnixListener, PathBuf)> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join(format!("{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, path))
}

/// Run the host until Chrome closes stdin.
///
/// # Errors
/// When the socket can't be bound.
pub fn run() -> Result<()> {
    let (listener, socket) = bind(&super::sock_dir()?)?;
    let shared = serve(listener, socket.clone(), Box::new(std::io::stdout()));
    let mut stdin = std::io::stdin().lock();
    pump_from_chrome(&shared, &mut stdin);
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// Start accepting socket clients on a background thread; returns the state the stdin pump feeds.
fn serve(listener: UnixListener, socket: PathBuf, to_chrome: Box<dyn Write + Send>) -> Arc<Shared> {
    let shared = Arc::new(Shared {
        to_chrome: Mutex::new(to_chrome),
        pending: Arc::default(),
        hello: Mutex::new(Value::Null),
        next_id: AtomicU64::new(1),
        socket,
    });
    let s = shared.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let s = s.clone();
            std::thread::spawn(move || handle_client(&s, stream));
        }
    });
    shared
}

/// Route extension messages: `hello` updates host info, `{id, …}` answers a pending call.
fn pump_from_chrome(shared: &Shared, from_chrome: &mut impl std::io::Read) {
    while let Ok(Some(msg)) = read_frame(from_chrome) {
        if msg.get("type").and_then(Value::as_str) == Some("hello") {
            *lock(&shared.hello) = msg;
            continue;
        }
        let Some(id) = msg.get("id").and_then(Value::as_u64) else { continue };
        let tx = lock(&shared.pending).remove(&id);
        if let Some(tx) = tx {
            let _ = tx.send(msg);
        }
    }
    for (_, tx) in lock(&shared.pending).drain() {
        let _ = tx.send(json!({"error": {"message": "Chrome closed the connection"}}));
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One JSON request per line in, one JSON reply per line out.
fn handle_client(shared: &Shared, stream: UnixStream) {
    let Ok(mut out) = stream.try_clone() else { return };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => call(shared, &req),
            Err(e) => json!({"error": {"message": format!("bad request: {e}")}}),
        };
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            return;
        }
    }
}

fn call(shared: &Shared, req: &Value) -> Value {
    let method = req.get("method").and_then(Value::as_str).unwrap_or_default();
    if method == "host.info" {
        let hello = lock(&shared.hello).clone();
        return json!({"result": {"pid": std::process::id(), "socket": shared.socket, "extension": hello}});
    }
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    lock(&shared.pending).insert(id, tx);
    let msg = json!({"id": id, "method": method, "params": req.get("params").cloned().unwrap_or(Value::Null)});
    let sent = write_frame(&mut **lock(&shared.to_chrome), &msg);
    if let Err(e) = sent {
        lock(&shared.pending).remove(&id);
        return json!({"error": {"message": format!("send to Chrome failed: {e}")}});
    }
    rx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| {
        lock(&shared.pending).remove(&id);
        json!({"error": {"message": format!("Chrome didn't answer {method} within {}s", CALL_TIMEOUT.as_secs())}})
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;
    use crate::chrome::client::Client;
    use std::os::unix::net::UnixStream as Sock;

    /// A fake extension on the other end of a socketpair: answers every request by echoing it.
    fn fake_chrome(mut ext: Sock) {
        std::thread::spawn(move || {
            let mut w = ext.try_clone().unwrap();
            write_frame(&mut w, &json!({"type": "hello", "version": "9.9.9", "browsing": false})).unwrap();
            while let Ok(Some(req)) = read_frame(&mut ext) {
                let reply = if req["method"] == "boom" {
                    json!({"id": req["id"], "error": {"message": "kaboom"}})
                } else {
                    json!({"id": req["id"], "result": {"echo": req["method"], "params": req["params"]}})
                };
                write_frame(&mut w, &reply).unwrap();
            }
        });
    }

    #[test]
    fn bridges_socket_clients_to_chrome_and_back() {
        let dir = tempfile::Builder::new().prefix("sc").tempdir_in("/tmp").unwrap();
        let (listener, socket) = bind(dir.path()).unwrap();
        assert_eq!(std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o700);

        let (host_side, ext_side) = Sock::pair().unwrap();
        fake_chrome(ext_side);
        let pump = serve(listener, socket.clone(), Box::new(host_side.try_clone().unwrap()));
        std::thread::spawn(move || pump_from_chrome(&pump, &mut host_side.try_clone().unwrap()));

        let mut c = Client::at(&socket).unwrap();
        let mut info = c.call("host.info", Value::Null).unwrap();
        for _ in 0..50 {
            if !info["extension"].is_null() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
            info = c.call("host.info", Value::Null).unwrap();
        }
        assert_eq!(info["extension"]["version"], "9.9.9");
        let r = c.call("bookmarks.getTree", json!({"x": 1})).unwrap();
        assert_eq!(r["echo"], "bookmarks.getTree");
        assert_eq!(r["params"]["x"], 1);
        assert_eq!(c.call("boom", Value::Null).unwrap_err().to_string(), "kaboom");

        #[allow(clippy::needless_collect, reason = "spawn every client before joining any")]
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let socket = socket.clone();
                std::thread::spawn(move || Client::at(&socket).unwrap().call("m", json!(i)).unwrap()["params"].clone())
            })
            .collect();
        let mut got: Vec<i64> = threads.into_iter().map(|t| t.join().unwrap().as_i64().unwrap()).collect();
        got.sort_unstable();
        assert_eq!(got, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn closed_chrome_fails_pending_calls() {
        let shared = Shared {
            to_chrome: Mutex::new(Box::new(Vec::new())),
            pending: Arc::default(),
            hello: Mutex::new(Value::Null),
            next_id: AtomicU64::new(1),
            socket: PathBuf::new(),
        };
        let (tx, rx) = mpsc::channel();
        lock(&shared.pending).insert(7, tx);
        pump_from_chrome(&shared, &mut std::io::empty());
        assert!(rx.recv().unwrap()["error"]["message"].as_str().unwrap().contains("closed"));
    }
}
