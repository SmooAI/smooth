//! Tailscale status — checks connection, hostname, identity.
//!
//! Re-homed into smooth-cli from the deleted `smooth-bigsmooth` crate (the
//! :4400 nuke, EPIC th-c89c2a). It's a generic `tailscale status --json`
//! wrapper with no microVM coupling, so it lives here for `th tailscale`.

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
/// 2. `tailscale` on PATH — Homebrew installs here.
fn tailscale_bin() -> &'static str {
    if std::path::Path::new("/usr/local/bin/tailscale").exists() {
        return "/usr/local/bin/tailscale";
    }
    "tailscale"
}

/// Get Tailscale status by running `tailscale status --json`.
#[must_use]
pub fn get_status() -> TailscaleStatus {
    let output = Command::new(tailscale_bin()).args(["status", "--json"]).output();

    let Ok(output) = output else {
        return TailscaleStatus { connected: false, hostname: None, tailnet: None, ip: None };
    };

    if !output.status.success() {
        return TailscaleStatus { connected: false, hostname: None, tailnet: None, ip: None };
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();

    let connected = json.get("BackendState").and_then(|v| v.as_str()) == Some("Running");
    let hostname = json.pointer("/Self/HostName").and_then(|v| v.as_str()).map(String::from);
    let tailnet = json.get("MagicDNSSuffix").and_then(|v| v.as_str()).map(String::from);
    let ip = json.pointer("/TailscaleIPs/0").and_then(|v| v.as_str()).map(String::from);

    TailscaleStatus { connected, hostname, tailnet, ip }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_struct_round_trips() {
        let s = TailscaleStatus { connected: true, hostname: Some("h".into()), tailnet: Some("t".into()), ip: Some("1.2.3.4".into()) };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: TailscaleStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.connected, s.connected);
        assert_eq!(back.ip, s.ip);
    }
}
