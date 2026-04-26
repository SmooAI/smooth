//! Long-running `smooth-dolt serve` companion + Unix-socket client.
//!
//! Background: calling `PearlStore::open` from inside a Big Smooth tokio
//! handler reliably wedges the spawned `smooth-dolt sql` subprocess in
//! `pthread_cond_wait` and never returns. The same operation from a TTY
//! finishes in 50ms; the second open inside the same Rust process is
//! what's broken (see pearl `th-1a61a7` for the full investigation). This
//! module sidesteps the issue by replacing the spawn-per-call pattern
//! with a long-running `smooth-dolt serve <data-dir> --socket <path>`
//! subprocess that opens the Dolt database once and answers JSON-line
//! queries over a Unix socket.
//!
//! `SmoothDoltServer` owns the spawned process and exposes
//! [`SmoothDoltServer::client`] which returns a fresh `SmoothDoltClient`
//! connected to the socket. Clients can issue many requests sequentially
//! on a single connection. The server child is killed and the socket
//! file is removed on `Drop`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dolt::find_smooth_dolt_binary;

/// How long to wait for the spawned server to create its socket file
/// before giving up. Cold-start of the embedded Dolt engine on a
/// reasonably fast machine is sub-second; we give it a generous cap.
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(15);

/// Long-running `smooth-dolt serve` subprocess + the Unix socket it's
/// listening on. Created once per dolt data directory; clients open
/// fresh connections per call via [`SmoothDoltServer::client`].
#[derive(Debug)]
pub struct SmoothDoltServer {
    socket: PathBuf,
    /// Held to keep the child alive; killed in `Drop`.
    child: Option<Child>,
    /// Held to clean up the socket directory on drop.
    _socket_dir: tempfile::TempDir,
}

impl SmoothDoltServer {
    /// Spawn `smooth-dolt serve` for the given data dir. Blocks until the
    /// socket is ready (or `SERVER_START_TIMEOUT` elapses).
    ///
    /// # Errors
    /// Fails when the `smooth-dolt` binary can't be located, the spawn
    /// fails, or the server doesn't create its socket within the timeout.
    pub fn spawn(data_dir: &Path) -> Result<Self> {
        let bin = find_smooth_dolt_binary().context("smooth-dolt binary not found. Run: scripts/build-smooth-dolt.sh")?;

        let socket_dir = tempfile::Builder::new().prefix("smooth-dolt-").tempdir().context("create socket tempdir")?;
        let socket = socket_dir.path().join("dolt.sock");

        // ALL of stdin/stdout/stderr go to /dev/null. Inheriting any of
        // them from the parent process (especially launchd's
        // service.err redirection, which delivers a regular file as fd 2)
        // wedges the embedded Dolt engine — the Go runtime parks SQL
        // queries in `pthread_cond_wait` and never returns. Verified on
        // smoo-hub: same binary, shell-spawned with stderr → /dev/null
        // returns SQL in 67ms; Big-Smooth-spawned with stderr → service.err
        // hangs forever. We lose server-side log visibility — operators
        // who need it can run `smooth-dolt serve <dir> --socket <path>`
        // by hand.
        let mut cmd = Command::new(&bin);
        cmd.arg("serve")
            .arg(data_dir)
            .arg("--socket")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().with_context(|| format!("spawn {} serve {}", bin.display(), data_dir.display()))?;

        // Wait for the server to create its socket file.
        let deadline = Instant::now() + SERVER_START_TIMEOUT;
        while !socket.exists() {
            if Instant::now() >= deadline {
                // Ensure we don't leak the half-started child.
                let mut zombie = child;
                let _ = zombie.kill();
                let _ = zombie.wait();
                anyhow::bail!("smooth-dolt serve did not create socket within {:?}", SERVER_START_TIMEOUT);
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let server = Self {
            socket,
            child: Some(child),
            _socket_dir: socket_dir,
        };

        // Verify the server is actually responding before returning.
        let mut probe = server.client().context("connect to freshly-spawned smooth-dolt serve")?;
        probe.ping().context("ping freshly-spawned smooth-dolt serve")?;
        drop(probe);

        Ok(server)
    }

    /// Path to the Unix socket the server is listening on.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Open a fresh client connection. Many can coexist; the server
    /// handles each connection on its own goroutine.
    ///
    /// # Errors
    /// Fails when the socket can't be reached (server died, etc.).
    pub fn client(&self) -> Result<SmoothDoltClient> {
        SmoothDoltClient::connect(&self.socket)
    }
}

impl Drop for SmoothDoltServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // SIGTERM equivalent — the Go server handles it cleanly,
            // unlinks its socket file, and exits.
            let _ = child.kill();
            let _ = child.wait();
        }
        // tempfile::TempDir cleanup runs after this; if the server
        // forgot to remove its socket, that takes care of it.
    }
}

/// One open connection to a [`SmoothDoltServer`]. Each request gets a
/// monotonically-increasing correlation id so test/debug paths can
/// distinguish responses.
pub struct SmoothDoltClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: AtomicU64,
}

#[derive(Serialize)]
struct ServerRequest<'a> {
    id: String,
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stmt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmd: Option<&'a str>,
}

#[derive(Deserialize)]
struct ServerResponse {
    /// Echo of the request id. Currently unused on the client side
    /// (we read responses synchronously per request), but kept so
    /// debug logs and future pipelined transports can match up
    /// requests and responses.
    #[serde(default)]
    #[allow(dead_code)]
    id: String,
    ok: bool,
    #[serde(default)]
    error: String,
    #[serde(default)]
    data: Vec<Value>,
    #[serde(default)]
    out: String,
    #[serde(default)]
    rows_affected: i64,
}

impl SmoothDoltClient {
    /// Connect to a `smooth-dolt serve` Unix socket.
    ///
    /// # Errors
    /// Returns the underlying I/O error if the socket can't be opened.
    pub fn connect(socket: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket).with_context(|| format!("connect {}", socket.display()))?;
        let reader = BufReader::new(stream.try_clone().context("clone unix stream for reader")?);
        Ok(Self {
            stream,
            reader,
            next_id: AtomicU64::new(1),
        })
    }

    /// Liveness check.
    pub fn ping(&mut self) -> Result<()> {
        let _ = self.send(ServerRequest {
            id: self.next_id(),
            op: "ping",
            query: None,
            stmt: None,
            message: None,
            limit: None,
            cmd: None,
        })?;
        Ok(())
    }

    /// Run a SELECT and parse rows as JSON values.
    ///
    /// # Errors
    /// Returns the server's reported error (or a transport failure).
    pub fn sql(&mut self, query: &str) -> Result<Vec<Value>> {
        let resp = self.send(ServerRequest {
            id: self.next_id(),
            op: "sql",
            query: Some(query),
            stmt: None,
            message: None,
            limit: None,
            cmd: None,
        })?;
        Ok(resp.data)
    }

    /// Run a non-SELECT statement.
    ///
    /// # Errors
    /// Returns the server's reported error (or a transport failure).
    pub fn exec(&mut self, stmt: &str) -> Result<i64> {
        let resp = self.send(ServerRequest {
            id: self.next_id(),
            op: "exec",
            query: None,
            stmt: Some(stmt),
            message: None,
            limit: None,
            cmd: None,
        })?;
        Ok(resp.rows_affected)
    }

    /// `dolt add -A && dolt commit -m <message> --allow-empty`.
    pub fn commit(&mut self, message: &str) -> Result<String> {
        let resp = self.send(ServerRequest {
            id: self.next_id(),
            op: "commit",
            query: None,
            stmt: None,
            message: Some(message),
            limit: None,
            cmd: None,
        })?;
        Ok(resp.out)
    }

    /// `dolt log -n <limit>` returning pre-formatted lines.
    pub fn log(&mut self, limit: usize) -> Result<String> {
        let resp = self.send(ServerRequest {
            id: self.next_id(),
            op: "log",
            query: None,
            stmt: None,
            message: None,
            limit: Some(limit),
            cmd: None,
        })?;
        Ok(resp.out)
    }

    /// One of the `dolt` no-arg commands: `push`, `pull`, `gc`, `status`.
    pub fn dolt(&mut self, cmd: &str) -> Result<String> {
        let resp = self.send(ServerRequest {
            id: self.next_id(),
            op: "dolt",
            query: None,
            stmt: None,
            message: None,
            limit: None,
            cmd: Some(cmd),
        })?;
        Ok(resp.out)
    }

    fn next_id(&self) -> String {
        format!("c-{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn send(&mut self, req: ServerRequest<'_>) -> Result<ServerResponse> {
        let mut bytes = serde_json::to_vec(&req).context("encode request")?;
        bytes.push(b'\n');
        self.stream.write_all(&bytes).context("write request")?;
        self.stream.flush().context("flush request")?;

        let mut line = String::new();
        let n = self.reader.read_line(&mut line).context("read response")?;
        if n == 0 {
            anyhow::bail!("smooth-dolt server closed connection");
        }
        let resp: ServerResponse = serde_json::from_str(line.trim_end()).with_context(|| format!("parse response: {line}"))?;
        if !resp.ok {
            return Err(anyhow!("smooth-dolt: {}", resp.error));
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spin up a server against a freshly-init'd dolt store and exercise
    /// the basic ops. Skips silently if `smooth-dolt` isn't built locally.
    #[test]
    fn server_roundtrip() {
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(_) => return,
        };
        let dolt_dir = tmp.path().join("dolt");

        // Initialize via the existing one-shot path.
        if crate::dolt::SmoothDolt::new(&dolt_dir).and_then(|d| d.init()).is_err() {
            // No binary or init failed — skip.
            return;
        }
        let dolt = crate::dolt::SmoothDolt::new(&dolt_dir).unwrap();
        // Create a tiny table so we have something to query.
        dolt.exec("CREATE TABLE IF NOT EXISTS smoke (id INT, label TEXT)").unwrap();
        dolt.exec("INSERT INTO smoke VALUES (1, 'hello'), (2, 'world')").unwrap();
        // Drop the one-shot handle so it doesn't hold any global resources.
        drop(dolt);

        let server = match SmoothDoltServer::spawn(&dolt_dir) {
            Ok(s) => s,
            Err(_) => return, // serve binary missing or spawn failed in CI
        };

        let mut client = server.client().expect("client");
        client.ping().expect("ping");

        let rows = client.sql("SELECT id, label FROM smoke ORDER BY id").expect("sql");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["label"], "hello");
        assert_eq!(rows[1]["label"], "world");

        // Reuse the same connection for multiple queries.
        let count_rows = client.sql("SELECT COUNT(*) AS n FROM smoke").expect("count");
        assert_eq!(count_rows[0]["n"], 2);
    }
}
