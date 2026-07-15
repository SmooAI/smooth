//! Best-effort tailnet exposure for the always-on daemon.
//!
//! When the `tailscale` CLI is present and the local node is up, this module
//! reverse-proxies the daemon's loopback listener onto the user's **tailnet**
//! via `tailscale serve`, so other devices the user owns reach the daemon at
//! `https://<machine>.<tailnet>.ts.net`.
//!
//! # Security — tailnet-private only
//!
//! This module uses `tailscale serve`, which exposes the target **only inside
//! the user's tailnet**. It does **not** and **must never** use
//! `tailscale funnel`, which would publish the daemon to the public internet.
//! The daemon is a single-trusted-operator personal instance; reachability is
//! deliberately confined to the operator's own devices.
//!
//! # Best-effort, never fatal
//!
//! Every tailscale interaction is best-effort: any failure (CLI missing, node
//! down, command error, malformed output) is logged and swallowed. The daemon
//! continues serving on loopback regardless. Set `SMOOTH_TAILSCALE_SERVE=0`
//! (or `false`) to opt out entirely.
//!
//! Verified against `tailscale 1.96.5`: background serve is
//! `tailscale serve --bg --https=<port> http://127.0.0.1:<local>`. The tailnet
//! port defaults to `443` but is overridable via `SMOOTH_TAILSCALE_HTTPS_PORT`
//! to coexist with another `serve` on a shared host. Teardown is
//! `tailscale serve reset` — but ONLY on the default `:443` (this version has no
//! per-port "off", and `reset` wipes ALL handlers); on a custom port the
//! background handler is left in place (coexistence-safe; see [`teardown_args`]).

use std::process::Command;

use serde::Deserialize;

/// Environment variable that, when set to `0`/`false` (case-insensitive),
/// disables tailnet exposure entirely.
const OPT_OUT_ENV: &str = "SMOOTH_TAILSCALE_SERVE";

/// Subset of `tailscale status --json` we care about.
#[derive(Debug, Deserialize)]
struct StatusJson {
    #[serde(rename = "BackendState")]
    backend_state: Option<String>,
    #[serde(rename = "Self")]
    self_node: Option<SelfNode>,
}

#[derive(Debug, Deserialize)]
struct SelfNode {
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
    #[serde(rename = "Online")]
    online: Option<bool>,
}

/// Whether the daemon's tailnet exposure has been disabled via the opt-out env
/// var. Pure helper so the env-var contract is unit-testable.
fn is_opted_out(value: Option<&str>) -> bool {
    matches!(value.map(str::trim).map(str::to_ascii_lowercase).as_deref(), Some("0" | "false"))
}

/// Argv (sans the `tailscale` program name) that exposes the loopback
/// `local_port` over HTTPS on the tailnet, in the background.
///
/// Verified against `tailscale 1.96.5`:
/// `tailscale serve --bg --https=<https_port> http://127.0.0.1:<local_port>`.
fn serve_args(local_port: u16, https_port: u16) -> Vec<String> {
    vec![
        "serve".to_string(),
        "--bg".to_string(),
        format!("--https={https_port}"),
        format!("http://127.0.0.1:{local_port}"),
    ]
}

/// The tailnet HTTPS port the daemon serves on. Default 443; override with
/// `SMOOTH_TAILSCALE_HTTPS_PORT` to **coexist** with another `tailscale serve`
/// already holding `:443` on a shared host (e.g. smoo-hub) — point the daemon at
/// e.g. `8443` and it adds its own handler without disturbing the existing one.
fn tailnet_https_port() -> u16 {
    std::env::var("SMOOTH_TAILSCALE_HTTPS_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|p| *p != 0)
        .unwrap_or(443)
}

/// Argv that tears down the serve config — but ONLY on the default `:443`, where
/// the daemon owns the whole serve config. tailscale 1.96.5 has no per-port
/// "off" (only `serve reset`, which wipes ALL handlers), so on a custom
/// `https_port` (coexistence mode) we return `None` and DON'T tear down: the
/// `--bg` handler persists harmlessly and re-applies idempotently on restart,
/// leaving any other service's `:443` serve config untouched.
fn teardown_args(https_port: u16) -> Option<Vec<String>> {
    (https_port == 443).then(|| vec!["serve".to_string(), "reset".to_string()])
}

/// Parse `tailscale status --json` and report whether the local node is up.
/// "Up" is `BackendState == "Running"`, falling back to `Self.Online == true`
/// when `BackendState` is absent. Malformed or missing-field input → `false`.
fn parse_node_up(status_json: &str) -> bool {
    let Ok(status) = serde_json::from_str::<StatusJson>(status_json) else {
        return false;
    };
    if status.backend_state.as_deref() == Some("Running") {
        return true;
    }
    status.self_node.and_then(|node| node.online).unwrap_or(false)
}

/// Derive the tailnet URL (`https://<Self.DNSName>`, trailing dot trimmed) from
/// `tailscale status --json`. Returns `None` if the field is missing/empty or
/// the JSON is malformed.
fn parse_tailnet_url(status_json: &str) -> Option<String> {
    let status = serde_json::from_str::<StatusJson>(status_json).ok()?;
    let dns = status.self_node?.dns_name?;
    let host = dns.trim_end_matches('.').trim();
    if host.is_empty() {
        return None;
    }
    Some(format!("https://{host}"))
}

/// Run `tailscale status --json` and return its stdout, or `None` if the binary
/// is not on `PATH` (spawn fails) or the command reports failure. A successful
/// spawn doubles as the "resolvable on PATH" check.
fn status_json() -> Option<String> {
    let output = Command::new("tailscale").args(["status", "--json"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// `true` when the `tailscale` CLI is resolvable on `PATH` **and** the local
/// node is up. Both conditions are checked by running `tailscale status --json`:
/// a spawn failure means the CLI is absent, and the parsed `BackendState`
/// reports node liveness.
#[must_use]
pub fn available() -> bool {
    status_json().is_some_and(|json| parse_node_up(&json))
}

/// Guard that keeps the daemon exposed on the tailnet for its lifetime and
/// tears the serve config down on drop (or explicit [`TailscaleServe::stop`]).
#[derive(Debug)]
pub struct TailscaleServe {
    /// The tailnet URL the daemon is reachable at, if it could be determined.
    url: Option<String>,
    /// Whether a serve config is currently active and still needs teardown.
    /// Cleared by the first teardown so `stop` + `Drop` don't double-run.
    active: bool,
    /// The tailnet HTTPS port served on — determines whether teardown is safe to
    /// `serve reset` (only on the default `:443`; see [`teardown_args`]).
    https_port: u16,
}

impl TailscaleServe {
    /// Best-effort: expose the loopback `local_port` on the tailnet over HTTPS.
    ///
    /// Returns `None` (logging at info/warn) when exposure is opted out via
    /// `SMOOTH_TAILSCALE_SERVE=0|false`, when [`available`] is `false`, or when
    /// the `tailscale serve` invocation fails. Never panics; never propagates
    /// errors to the caller.
    #[must_use]
    pub fn start(local_port: u16) -> Option<Self> {
        if is_opted_out(std::env::var(OPT_OUT_ENV).ok().as_deref()) {
            tracing::info!("{OPT_OUT_ENV} opt-out set; daemon stays loopback-only (no tailnet serve)");
            return None;
        }
        if !available() {
            tracing::info!("tailscale CLI unavailable or node not up; daemon stays loopback-only");
            return None;
        }

        let https_port = tailnet_https_port();
        let args = serve_args(local_port, https_port);
        match Command::new("tailscale").args(&args).output() {
            Ok(output) if output.status.success() => {
                let url = status_json().as_deref().and_then(parse_tailnet_url);
                tracing::info!(url = ?url, local_port, https_port, "daemon exposed on tailnet via tailscale serve");
                Some(Self { url, active: true, https_port })
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(stderr = %stderr.trim(), "tailscale serve failed; daemon stays loopback-only");
                None
            }
            Err(error) => {
                tracing::warn!(%error, "could not run tailscale serve; daemon stays loopback-only");
                None
            }
        }
    }

    /// The tailnet URL the daemon is reachable at (`https://<machine>.<tailnet>.ts.net`),
    /// if it could be determined at start.
    #[must_use]
    pub fn url(&self) -> Option<String> {
        self.url.clone()
    }

    /// Explicitly tear down the serve config now. Equivalent to dropping the
    /// guard, but lets callers reclaim the tailnet config deterministically.
    pub fn stop(mut self) {
        self.teardown();
    }

    /// Idempotent best-effort teardown of the serve config. Errors are logged
    /// and swallowed.
    fn teardown(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(args) = teardown_args(self.https_port) else {
            tracing::info!(
                https_port = self.https_port,
                "custom tailnet port — leaving the serve handler in place (coexistence-safe; no global `serve reset`)"
            );
            return;
        };
        match Command::new("tailscale").args(args).output() {
            Ok(output) if output.status.success() => tracing::info!("tailscale serve config torn down"),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(stderr = %stderr.trim(), "tailscale serve teardown failed; serve config may linger");
            }
            Err(error) => tracing::warn!(%error, "could not run tailscale serve teardown; serve config may linger"),
        }
    }
}

impl Drop for TailscaleServe {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed-down but field-faithful sample of `tailscale status --json` for a
    /// running node (matches `tailscale 1.96.5` output shape).
    const STATUS_RUNNING: &str = r#"{
        "Version": "1.96.5",
        "BackendState": "Running",
        "TailscaleIPs": ["100.75.89.50"],
        "Self": {
            "ID": "nr37pYAMDH11CNTRL",
            "HostName": "marvin",
            "DNSName": "marvin.tailc13b5a.ts.net.",
            "OS": "macOS",
            "Online": true
        }
    }"#;

    #[test]
    fn parse_node_up_running() {
        assert!(parse_node_up(STATUS_RUNNING));
    }

    #[test]
    fn parse_node_up_stopped() {
        let json = r#"{"BackendState": "Stopped", "Self": {"Online": false}}"#;
        assert!(!parse_node_up(json));
    }

    #[test]
    fn parse_node_up_needs_login() {
        let json = r#"{"BackendState": "NeedsLogin"}"#;
        assert!(!parse_node_up(json));
    }

    #[test]
    fn parse_node_up_falls_back_to_self_online() {
        // No BackendState, but Self.Online is true.
        let json = r#"{"Self": {"Online": true}}"#;
        assert!(parse_node_up(json));
    }

    #[test]
    fn parse_node_up_malformed_is_false() {
        assert!(!parse_node_up("not json at all"));
        assert!(!parse_node_up(""));
        assert!(!parse_node_up("{"));
    }

    #[test]
    fn parse_node_up_missing_fields_is_false() {
        assert!(!parse_node_up("{}"));
        assert!(!parse_node_up(r#"{"Self": {}}"#));
    }

    #[test]
    fn parse_tailnet_url_trims_trailing_dot() {
        assert_eq!(parse_tailnet_url(STATUS_RUNNING), Some("https://marvin.tailc13b5a.ts.net".to_string()));
    }

    #[test]
    fn parse_tailnet_url_without_trailing_dot() {
        let json = r#"{"Self": {"DNSName": "marvin.tailc13b5a.ts.net"}}"#;
        assert_eq!(parse_tailnet_url(json), Some("https://marvin.tailc13b5a.ts.net".to_string()));
    }

    #[test]
    fn parse_tailnet_url_missing_or_empty_is_none() {
        assert_eq!(parse_tailnet_url("{}"), None);
        assert_eq!(parse_tailnet_url(r#"{"Self": {}}"#), None);
        assert_eq!(parse_tailnet_url(r#"{"Self": {"DNSName": ""}}"#), None);
        assert_eq!(parse_tailnet_url(r#"{"Self": {"DNSName": "."}}"#), None);
    }

    #[test]
    fn parse_tailnet_url_malformed_is_none() {
        assert_eq!(parse_tailnet_url("garbage"), None);
    }

    #[test]
    fn opt_out_disables_on_zero_and_false() {
        assert!(is_opted_out(Some("0")));
        assert!(is_opted_out(Some("false")));
        assert!(is_opted_out(Some("False")));
        assert!(is_opted_out(Some("FALSE")));
        assert!(is_opted_out(Some("  false  ")));
    }

    #[test]
    fn opt_out_does_not_disable_otherwise() {
        assert!(!is_opted_out(None));
        assert!(!is_opted_out(Some("1")));
        assert!(!is_opted_out(Some("true")));
        assert!(!is_opted_out(Some("")));
        assert!(!is_opted_out(Some("yes")));
    }

    #[test]
    fn serve_args_default_443() {
        // Verified against `tailscale serve --help` (tailscale 1.96.5).
        assert_eq!(serve_args(8787, 443), vec!["serve", "--bg", "--https=443", "http://127.0.0.1:8787"]);
    }

    #[test]
    fn serve_args_uses_given_local_and_https_ports() {
        // Coexistence: a custom tailnet port (e.g. 8443) + a custom local port.
        assert_eq!(serve_args(8788, 8443), vec!["serve", "--bg", "--https=8443", "http://127.0.0.1:8788"]);
    }

    #[test]
    fn teardown_resets_only_on_default_443() {
        // Default 443 → reset (daemon owns the whole serve config).
        assert_eq!(teardown_args(443), Some(vec!["serve".to_string(), "reset".to_string()]));
        // Custom port → None: never `serve reset` (would wipe a coexisting :443).
        assert_eq!(teardown_args(8443), None);
    }

    #[test]
    fn tailnet_https_port_parses_env() {
        std::env::set_var("SMOOTH_TAILSCALE_HTTPS_PORT", "8443");
        assert_eq!(tailnet_https_port(), 8443);
        std::env::set_var("SMOOTH_TAILSCALE_HTTPS_PORT", "  not-a-port  ");
        assert_eq!(tailnet_https_port(), 443, "garbage falls back to 443");
        std::env::set_var("SMOOTH_TAILSCALE_HTTPS_PORT", "0");
        assert_eq!(tailnet_https_port(), 443, "0 falls back to 443");
        std::env::remove_var("SMOOTH_TAILSCALE_HTTPS_PORT");
        assert_eq!(tailnet_https_port(), 443, "unset → 443");
    }

    #[test]
    fn url_returns_stored_value() {
        let guard = TailscaleServe {
            url: Some("https://marvin.tailc13b5a.ts.net".to_string()),
            active: false,
            https_port: 443,
        };
        assert_eq!(guard.url(), Some("https://marvin.tailc13b5a.ts.net".to_string()));
        // active: false so drop is a no-op and never shells out to tailscale.
    }

    #[test]
    fn teardown_is_idempotent_when_inactive() {
        // active: false means teardown short-circuits without invoking tailscale.
        let mut guard = TailscaleServe {
            url: None,
            active: false,
            https_port: 443,
        };
        guard.teardown();
        assert!(!guard.active);
    }
}
