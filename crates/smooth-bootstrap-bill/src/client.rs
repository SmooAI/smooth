//! `BillClient` — TCP client used by Big Smooth and other Board members.
//!
//! The client opens a fresh TCP connection per request, writes the
//! line-delimited JSON request, and reads exactly one response line.

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::protocol::{BillRequest, BillResponse, PortMapping, SandboxSpec};

/// Client handle. Clone-friendly (cheap — holds only a URL).
#[derive(Debug, Clone)]
pub struct BillClient {
    /// Base URL of Bill, e.g. `http://127.0.0.1:42424` or
    /// `http://host.containers.internal:42424` from inside a VM. Only the
    /// host + port are used; the scheme is accepted to make config strings
    /// consistent with the rest of the Smooth URL world.
    url: String,
}

impl BillClient {
    /// Build a client pointed at Bill's URL.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    /// The configured URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Extract host + port from `self.url`, supporting `http://host:port`,
    /// `host:port`, or bare `host` (defaults to port 4444 — Bill's canonical
    /// port when unspecified).
    fn authority(&self) -> Result<String> {
        let s = self.url.trim();
        let s = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")).unwrap_or(s);
        let s = s.trim_end_matches('/');
        if s.contains(':') {
            Ok(s.to_string())
        } else {
            Ok(format!("{s}:4444"))
        }
    }

    async fn send(&self, request: &BillRequest) -> Result<BillResponse> {
        let addr = self.authority()?;
        let stream = TcpStream::connect(&addr).await.with_context(|| format!("connect to Bill at {addr}"))?;
        let (read_half, mut write_half) = stream.into_split();
        let mut json = serde_json::to_vec(request).context("serialize request")?;
        json.push(b'\n');
        write_half.write_all(&json).await.context("write request")?;
        write_half.flush().await.context("flush request")?;
        // Half-close the write side so Bill knows no more bytes are coming.
        write_half.shutdown().await.ok();
        drop(write_half);
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        reader.read_line(&mut line).await.context("read response")?;
        let response: BillResponse = serde_json::from_str(line.trim()).with_context(|| format!("parse response: {line:?}"))?;
        Ok(response)
    }

    /// Liveness probe. Returns Bill's version string on success.
    ///
    /// # Errors
    ///
    /// Network failures or a non-`Pong` response surface as an error.
    pub async fn ping(&self) -> Result<String> {
        match self.send(&BillRequest::Ping).await? {
            BillResponse::Pong { version } => Ok(version),
            BillResponse::Error { message } => anyhow::bail!("bill error: {message}"),
            other => anyhow::bail!("unexpected response to Ping: {other:?}"),
        }
    }

    /// Spawn a sandbox. Returns the resolved name, resolved port mappings
    /// (with any `host_port: 0` requests replaced by the kernel-assigned
    /// port), and the RFC3339 creation timestamp.
    ///
    /// # Errors
    ///
    /// Any `BillResponse::Error` is propagated with its message.
    pub async fn spawn(&self, spec: SandboxSpec) -> Result<(String, Vec<PortMapping>, String)> {
        match self.send(&BillRequest::Spawn { spec }).await? {
            BillResponse::Spawned { name, host_ports, created_at } => Ok((name, host_ports, created_at)),
            BillResponse::Error { message } => anyhow::bail!("bill error: {message}"),
            other => anyhow::bail!("unexpected response to Spawn: {other:?}"),
        }
    }

    /// Execute a command inside a running sandbox.
    ///
    /// # Errors
    ///
    /// Bill-side errors propagate. A non-zero exit code is **not** an error
    /// — it is returned in the tuple alongside stdout/stderr.
    pub async fn exec(&self, name: &str, argv: &[String]) -> Result<(String, String, i32)> {
        let req = BillRequest::Exec {
            name: name.to_string(),
            argv: argv.to_vec(),
        };
        match self.send(&req).await? {
            BillResponse::ExecResult { stdout, stderr, exit_code } => Ok((stdout, stderr, exit_code)),
            BillResponse::Error { message } => anyhow::bail!("bill error: {message}"),
            other => anyhow::bail!("unexpected response to Exec: {other:?}"),
        }
    }

    /// Destroy a sandbox. Idempotent.
    ///
    /// # Errors
    ///
    /// Bill-side errors propagate.
    pub async fn destroy(&self, name: &str) -> Result<()> {
        match self.send(&BillRequest::Destroy { name: name.to_string() }).await? {
            BillResponse::Destroyed => Ok(()),
            BillResponse::Error { message } => anyhow::bail!("bill error: {message}"),
            other => anyhow::bail!("unexpected response to Destroy: {other:?}"),
        }
    }

    /// List every sandbox Bill currently holds.
    ///
    /// # Errors
    ///
    /// Bill-side errors propagate.
    pub async fn list(&self) -> Result<Vec<String>> {
        match self.send(&BillRequest::List).await? {
            BillResponse::SandboxList { names } => Ok(names),
            BillResponse::Error { message } => anyhow::bail!("bill error: {message}"),
            other => anyhow::bail!("unexpected response to List: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_adds_default_port_for_bare_host() {
        let c = BillClient::new("localhost");
        assert_eq!(c.authority().unwrap(), "localhost:4444");
    }

    #[test]
    fn authority_strips_http_scheme() {
        let c = BillClient::new("http://127.0.0.1:4242");
        assert_eq!(c.authority().unwrap(), "127.0.0.1:4242");
    }

    #[test]
    fn authority_strips_trailing_slash() {
        let c = BillClient::new("http://127.0.0.1:4242/");
        assert_eq!(c.authority().unwrap(), "127.0.0.1:4242");
    }
}
