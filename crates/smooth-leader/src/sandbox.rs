//! Sandbox management — microsandbox (msb) CLI wrapper.
//!
//! Each Smooth Operator runs in a hardware-isolated microVM via microsandbox.
//! This module wraps the `msb` CLI to create, destroy, and communicate with sandboxes.

use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use uuid::Uuid;

/// Configuration for creating a sandbox.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub operator_id: String,
    pub bead_id: String,
    pub workspace_path: String,
    pub permissions: Vec<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub phase: String,
    pub env: HashMap<String, String>,
    pub cpus: u32,
    pub memory_mb: u32,
    pub timeout_seconds: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            operator_id: format!("operator-{}", &Uuid::new_v4().to_string()[..8]),
            bead_id: String::new(),
            workspace_path: "/workspace".into(),
            permissions: vec!["beads:read".into(), "beads:write".into(), "fs:read".into(), "fs:write".into()],
            system_prompt: None,
            model: None,
            phase: "assess".into(),
            env: HashMap::new(),
            cpus: 2,
            memory_mb: 4096,
            timeout_seconds: 30 * 60,
        }
    }
}

/// Handle to a running sandbox.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxHandle {
    pub sandbox_id: String,
    pub operator_id: String,
    pub bead_id: String,
    pub msb_name: String,
    pub host_port: u16,
    pub created_at: String,
    pub timeout_at: String,
}

/// Status of a sandbox.
#[derive(Debug, Serialize)]
pub struct SandboxStatus {
    pub running: bool,
    pub healthy: bool,
    pub phase: String,
    pub uptime_ms: u64,
}

/// Run an msb command.
fn msb(args: &[&str]) -> Result<String> {
    let output = Command::new("msb")
        .args(args)
        .output()
        .context("Failed to run msb. Is microsandbox installed?")?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("msb command failed: {stderr}");
    }
}

/// Check if msb is available.
pub fn is_available() -> bool {
    Command::new("msb").arg("--version").output().map_or(false, |o| o.status.success())
}

/// Check if msb server is running.
pub fn is_server_running() -> bool {
    msb(&["server", "status"]).is_ok()
}

/// Start the msb server if not running.
pub fn ensure_server() -> Result<()> {
    if is_server_running() {
        return Ok(());
    }
    tracing::info!("Starting microsandbox server...");
    msb(&["server", "start", "--dev"]).context("Failed to start microsandbox server")?;
    // Give it a moment
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}

/// Create and start a sandbox.
pub fn create_sandbox(config: &SandboxConfig, host_port: u16) -> Result<SandboxHandle> {
    let msb_name = format!("smooth-operator-{}", config.operator_id);
    let image = std::env::var("SMOOTH_WORKER_IMAGE").unwrap_or_else(|_| "smooth-operator:latest".into());
    let port_map = format!("{host_port}:4096");
    let workspace_mount = format!("{}:/workspace", config.workspace_path);
    let memory_str = config.memory_mb.to_string();
    let cpus_str = config.cpus.to_string();

    let mut args: Vec<&str> = vec![
        "run",
        "--name",
        &msb_name,
        "--image",
        &image,
        "--memory",
        &memory_str,
        "--cpus",
        &cpus_str,
        "--port",
        &port_map,
        "--mount",
        &workspace_mount,
    ];

    // Add environment variables
    let env_strings: Vec<String> = config.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    for env in &env_strings {
        args.push("--env");
        args.push(env);
    }

    msb(&args)?;

    let now = chrono::Utc::now();
    let timeout_at = now + chrono::Duration::seconds(config.timeout_seconds as i64);

    Ok(SandboxHandle {
        sandbox_id: config.operator_id.clone(),
        operator_id: config.operator_id.clone(),
        bead_id: config.bead_id.clone(),
        msb_name,
        host_port,
        created_at: now.to_rfc3339(),
        timeout_at: timeout_at.to_rfc3339(),
    })
}

/// Destroy a sandbox.
pub fn destroy_sandbox(msb_name: &str) -> Result<()> {
    let _ = msb(&["stop", msb_name]);
    let _ = msb(&["rm", msb_name]);
    Ok(())
}

/// Get sandbox status.
pub fn get_status(msb_name: &str) -> SandboxStatus {
    let running = msb(&["status", msb_name]).map_or(false, |out| out.to_lowercase().contains("running"));

    let healthy = if running {
        // TODO: check health endpoint
        true
    } else {
        false
    };

    SandboxStatus {
        running,
        healthy,
        phase: "unknown".into(),
        uptime_ms: 0,
    }
}

/// Execute a command inside a sandbox.
pub fn exec_in_sandbox(msb_name: &str, command: &[&str]) -> Result<(String, String, i32)> {
    let mut args = vec!["exec", msb_name, "--"];
    args.extend_from_slice(command);

    let output = Command::new("msb").args(&args).output().context("Failed to exec in sandbox")?;

    Ok((
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SandboxConfig::default();
        assert!(config.operator_id.starts_with("operator-"));
        assert_eq!(config.phase, "assess");
        assert_eq!(config.cpus, 2);
        assert_eq!(config.memory_mb, 4096);
    }

    #[test]
    fn test_is_available() {
        // Just verify it doesn't panic
        let _ = is_available();
    }
}
