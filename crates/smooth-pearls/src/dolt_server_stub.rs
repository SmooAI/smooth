//! Non-Unix stand-in for [`crate::dolt_server`].
//!
//! The real module speaks to a long-running `smooth-dolt serve` over a
//! **Unix domain socket** (`std::os::unix::net::UnixStream`), which does
//! not exist on Windows. Rather than sprinkle `#[cfg(unix)]` through
//! `dolt.rs` / `store.rs` / the CLI, this stub keeps the type surface
//! identical and makes server mode simply unavailable:
//!
//! - [`SmoothDoltServer::try_attach`] returns `None`, so
//!   [`crate::dolt::SmoothDolt::new`] falls through to per-call CLI mode
//!   — the same path Unix takes when no server is running.
//! - Everything else errors, and is unreachable in practice because a
//!   `SmoothDoltServer` can never be constructed here.
//!
//! ponytail: stub over cfg-sprinkling — one file, zero call-site churn.
//! Give Windows a real server transport (TCP loopback, needs matching
//! support in the `smooth-dolt` Go binary) if server mode is ever wanted
//! there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::Value;

/// Error text shared by every stub method.
const UNSUPPORTED: &str = "smooth-dolt server mode requires Unix domain sockets and is unavailable on this platform; pearls run in per-call CLI mode";

/// Uninhabitable stand-in for the Unix `SmoothDoltServer`.
///
/// Carries a `Never`-like private field so it cannot be constructed:
/// the only constructors ([`Self::spawn`], [`Self::try_attach`]) refuse.
#[derive(Debug)]
pub struct SmoothDoltServer {
    _private: std::convert::Infallible,
}

impl SmoothDoltServer {
    /// Always fails — server mode needs a Unix socket.
    ///
    /// # Errors
    /// Always.
    pub fn spawn(_data_dir: &Path) -> Result<Self> {
        bail!(UNSUPPORTED)
    }

    /// Always `None` — callers fall back to per-call CLI mode.
    #[must_use]
    pub fn try_attach(_data_dir: &Path) -> Option<Self> {
        None
    }

    /// Unreachable: no `SmoothDoltServer` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn with_client<F, T>(&self, _f: F) -> Result<T>
    where
        F: FnOnce(&mut SmoothDoltClient) -> Result<T>,
    {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltServer` value can exist here.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        PathBuf::new()
    }

    /// Unreachable: no `SmoothDoltServer` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn client(&self) -> Result<SmoothDoltClient> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltServer` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn is_healthy(&self) -> Result<()> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltServer` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn ensure_healthy(&self) -> Result<()> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltServer` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn force_respawn(&self) -> Result<()> {
        bail!(UNSUPPORTED)
    }
}

/// Uninhabitable stand-in for the Unix `SmoothDoltClient`.
#[derive(Debug)]
pub struct SmoothDoltClient {
    _private: std::convert::Infallible,
}

impl SmoothDoltClient {
    /// Always fails — server mode needs a Unix socket.
    ///
    /// # Errors
    /// Always.
    pub fn connect(_socket: &Path) -> Result<Self> {
        bail!(UNSUPPORTED)
    }

    /// Always fails — server mode needs a Unix socket.
    ///
    /// # Errors
    /// Always.
    pub fn connect_with_timeout(_socket: &Path, _timeout: Duration) -> Result<Self> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltClient` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn ping(&mut self) -> Result<()> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltClient` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn sql(&mut self, _query: &str) -> Result<Vec<Value>> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltClient` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn exec(&mut self, _stmt: &str) -> Result<i64> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltClient` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn commit(&mut self, _message: &str) -> Result<String> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltClient` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn log(&mut self, _limit: usize) -> Result<String> {
        bail!(UNSUPPORTED)
    }

    /// Unreachable: no `SmoothDoltClient` value can exist here.
    ///
    /// # Errors
    /// Always.
    pub fn dolt(&mut self, _cmd: &str) -> Result<String> {
        bail!(UNSUPPORTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one behaviour that actually matters: attach must decline so
    /// `SmoothDolt::new` falls back to CLI mode instead of erroring.
    #[test]
    fn try_attach_declines_so_callers_fall_back_to_cli_mode() {
        assert!(SmoothDoltServer::try_attach(Path::new(".")).is_none());
    }

    #[test]
    fn spawn_reports_the_platform_limitation() {
        let err = SmoothDoltServer::spawn(Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("Unix domain sockets"), "unexpected error: {err}");
    }

    #[test]
    fn client_connect_reports_the_platform_limitation() {
        assert!(SmoothDoltClient::connect(Path::new("x")).is_err());
    }
}
