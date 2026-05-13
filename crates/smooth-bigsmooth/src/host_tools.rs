//! Host-tool proxy — `/api/host/exec`.
//!
//! Lets a teammate (smooth-operator inside a sandbox) invoke a small
//! whitelist of host CLIs (`gh`, `git`, `kubectl`, `jq`, `curl`) without
//! shipping host credentials into the sandbox. The teammate calls Big
//! Smooth over its already-allowed `host.containers.internal:4400` route;
//! Big Smooth runs the requested tool on the host with the user's
//! existing auth (gh keyring, kubeconfig, ssh-agent, …) and returns
//! stdout/stderr/exit.
//!
//! Why proxy rather than env-passthrough or bind-mount?
//! - Bind-mounting `~/.config/gh` into a sandbox exposes the raw token to
//!   anything running inside; a hostile teammate could exfiltrate it.
//! - Forwarding `GH_TOKEN` has the same problem — once the sandbox has
//!   the token, the security model leaks.
//! - The proxy keeps secrets on the host. The sandbox only ever sees the
//!   command output. Wonk + Narc still see the call as a normal HTTP
//!   request to a known endpoint, so the audit trail stays clean.
//!
//! Hardening:
//! - **Allowlist** — only commands in `allowed_tools()` are accepted. The
//!   set is `gh`, `git`, `kubectl`, `jq`, `curl` by default; override with
//!   `SMOOTH_HOST_TOOLS=gh,kubectl,...`.
//! - **Bearer auth** — `Authorization: Bearer <SMOOTH_HOST_TOKEN>`. The
//!   token is generated per-process at startup and threaded into the
//!   sandbox's env so only legit teammates can call.
//! - **30 s timeout, 8 KiB output cap** — same shape as the in-VM bash
//!   tool's defaults.
//!
//! The teammate consumes this through a `host_tool(tool, args)` tool
//! registered in `smooth-operator-runner` (see `host_tool.rs`).

use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::server::AppState;

#[derive(Deserialize)]
pub struct HostExecBody {
    pub tool: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Serialize)]
pub struct HostExecResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub truncated: bool,
}

const DEFAULT_ALLOWLIST: &[&str] = &[
    "gh", "git", "kubectl", "jq", "curl",
    // Pearl th-c366ff continuation (2026-05-12): "ping" really means
    // ICMP ping, not "curl as a stand-in". The sandbox's smoltcp
    // proxy is TCP-only — there's no path for ICMP from inside the
    // guest. Letting the agent shell out to `ping` on the HOST via
    // host_tool gives a real reachability answer (and host has
    // Tailscale, so `ping smoo-hub` works there). `ping` is
    // reconnaissance-only, no host-state mutation, low risk.
    "ping", // DNS lookup helpers — same shape, no mutation, useful for
    // "what does <host> resolve to?" / "is DNS working?".
    "dig", "nslookup", "host",
];
const TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_CAP: usize = 8 * 1024;

fn allowed_tools() -> Vec<String> {
    if let Ok(spec) = std::env::var("SMOOTH_HOST_TOOLS") {
        return spec.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    DEFAULT_ALLOWLIST.iter().map(|s| (*s).to_string()).collect()
}

fn host_token() -> Option<String> {
    std::env::var("SMOOTH_HOST_TOKEN").ok()
}

/// `POST /api/host/exec` — run a whitelisted host CLI on behalf of a
/// teammate.
pub async fn host_exec_handler(
    State(_state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<HostExecBody>,
) -> Result<Json<HostExecResponse>, (StatusCode, String)> {
    // Auth check — reject anything that isn't a teammate calling from
    // the sandbox network policy with the per-process bearer.
    let expected = host_token().ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, "host-exec: SMOOTH_HOST_TOKEN not set on Big Smooth".into()))?;
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    let presented = auth.strip_prefix("Bearer ").unwrap_or("");
    if presented != expected {
        return Err((StatusCode::UNAUTHORIZED, "host-exec: bad bearer token".into()));
    }

    // Allowlist check — refuse anything outside the configured set.
    let allowed = allowed_tools();
    if !allowed.iter().any(|t| t == &body.tool) {
        return Err((StatusCode::FORBIDDEN, format!("host-exec: tool '{}' not in allowlist {:?}", body.tool, allowed)));
    }

    // Run the command. Inherits Big Smooth's env, including the user's
    // PATH and home dir, so authed CLIs work as the user expects.
    let mut cmd = Command::new(&body.tool);
    for a in &body.args {
        cmd.arg(a);
    }
    let result = tokio::time::timeout(TIMEOUT, cmd.output()).await;
    let out = match result {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("host-exec: spawn failed: {e}"))),
        Err(_) => return Err((StatusCode::REQUEST_TIMEOUT, format!("host-exec: '{}' timed out after 30s", body.tool))),
    };

    let mut stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let mut stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let mut truncated = false;
    if stdout.len() > OUTPUT_CAP {
        stdout.truncate(OUTPUT_CAP);
        truncated = true;
    }
    if stderr.len() > OUTPUT_CAP {
        stderr.truncate(OUTPUT_CAP);
        truncated = true;
    }

    Ok(Json(HostExecResponse {
        stdout,
        stderr,
        exit_code: out.status.code().unwrap_or(-1),
        truncated,
    }))
}

/// Generate a fresh bearer token. Called once at server startup; the
/// token is then threaded into every sandbox's env via dispatch.
pub fn generate_host_token() -> String {
    use uuid::Uuid;
    Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests that touch SMOOTH_HOST_TOOLS share global process env, so
    // running them in parallel is racy: one test's `remove_var` can
    // land between another's `set_var` and `allowed_tools()` call. A
    // single Mutex (poisoned-on-panic is fine — the next caller just
    // sees the panic and propagates) serialises them while still
    // allowing the rest of the suite to run in parallel.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn default_allowlist_contains_gh() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var("SMOOTH_HOST_TOOLS").ok();
        std::env::remove_var("SMOOTH_HOST_TOOLS");
        let a = allowed_tools();
        if let Some(v) = original {
            std::env::set_var("SMOOTH_HOST_TOOLS", v);
        }
        assert!(a.iter().any(|s| s == "gh"));
        assert!(a.iter().any(|s| s == "git"));
        assert!(a.iter().any(|s| s == "kubectl"));
    }

    #[test]
    fn env_override_replaces_allowlist() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var("SMOOTH_HOST_TOOLS").ok();
        std::env::set_var("SMOOTH_HOST_TOOLS", "gh,jq");
        let a = allowed_tools();
        if let Some(v) = original {
            std::env::set_var("SMOOTH_HOST_TOOLS", v);
        } else {
            std::env::remove_var("SMOOTH_HOST_TOOLS");
        }
        assert_eq!(a, vec!["gh".to_string(), "jq".to_string()]);
    }

    #[test]
    fn host_token_round_trip() {
        let t = generate_host_token();
        assert_eq!(t.len(), 32); // uuid simple = 32 hex chars
                                 // Should be deterministic in shape but unique per call.
        assert_ne!(generate_host_token(), t);
    }
}
