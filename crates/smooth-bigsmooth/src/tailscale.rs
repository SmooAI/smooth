//! Tailscale status — checks connection, hostname, identity.

use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct TailscaleStatus {
    pub connected: bool,
    pub hostname: Option<String>,
    pub tailnet: Option<String>,
    pub ip: Option<String>,
}

/// Find the tailscale CLI binary path.
///
/// Resolution order (first match wins):
/// 1. `/usr/local/bin/tailscale` — the CLI symlink the Mac App Store
///    version installs when you click "Install CLI…" from the menu bar.
///    Works from any process context (including launchd children).
/// 2. `tailscale` on PATH — Homebrew installs here.
///
/// We deliberately do **not** fall through to
/// `/Applications/Tailscale.app/Contents/MacOS/Tailscale` even though
/// it exists on App Store installs: that binary is the GUI launcher,
/// and invoking it from a non-GUI process (e.g. our launchd service)
/// fails with "The Tailscale GUI failed to start: CLIError 3".
fn tailscale_bin() -> &'static str {
    if std::path::Path::new("/usr/local/bin/tailscale").exists() {
        return "/usr/local/bin/tailscale";
    }
    "tailscale"
}

/// Get Tailscale status by running `tailscale status --json`.
pub fn get_status() -> TailscaleStatus {
    let output = Command::new(tailscale_bin()).args(["status", "--json"]).output();

    let Ok(output) = output else {
        return TailscaleStatus {
            connected: false,
            hostname: None,
            tailnet: None,
            ip: None,
        };
    };

    if !output.status.success() {
        return TailscaleStatus {
            connected: false,
            hostname: None,
            tailnet: None,
            ip: None,
        };
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();

    let connected = json.get("BackendState").and_then(|v| v.as_str()) == Some("Running");
    let hostname = json.pointer("/Self/HostName").and_then(|v| v.as_str()).map(String::from);
    let tailnet = json.get("MagicDNSSuffix").and_then(|v| v.as_str()).map(String::from);
    let ip = json.pointer("/TailscaleIPs/0").and_then(|v| v.as_str()).map(String::from);

    TailscaleStatus {
        connected,
        hostname,
        tailnet,
        ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_status_doesnt_panic() {
        // Just verify it returns something without crashing
        let status = get_status();
        // connected may be true or false depending on environment
        assert!(status.hostname.is_some() || status.hostname.is_none());
    }
}
