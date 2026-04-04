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
fn tailscale_bin() -> &'static str {
    // App Store version puts CLI inside the .app bundle
    if std::path::Path::new("/Applications/Tailscale.app/Contents/MacOS/Tailscale").exists() {
        return "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
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
