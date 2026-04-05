//! Sandbox management — microsandbox Rust SDK (no external `msb` CLI required).
//!
//! Each Smooth Operator runs in a hardware-isolated microVM via the
//! [`microsandbox`] crate. This module wraps the crate so the rest of Big Smooth
//! can remain agnostic about the VM backend.
//!
//! ### Lifecycle
//!
//! * [`create_sandbox`] builds a microVM from an OCI image, applies resource
//!   limits, port forwarding, environment, and workspace mount, then stores
//!   the live [`microsandbox::Sandbox`] in a process-wide registry keyed by
//!   `operator_id`.
//! * [`get_status`] / [`exec_in_sandbox`] look the handle up by
//!   `operator_id` and forward the call to the crate.
//! * [`destroy_sandbox`] removes the handle from the registry and calls
//!   `stop_and_wait` to cleanly shut the VM down.
//!
//! ### Why a registry
//!
//! The `microsandbox::Sandbox` struct is not `Serialize`, so it cannot live on
//! [`SandboxHandle`] (which is returned from HTTP routes and streamed over
//! WebSocket). Instead we keep the `Sandbox` in an in-process `HashMap` and
//! the `SandboxHandle` carries a stable string ID that callers use to reach
//! back into the registry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use microsandbox::Sandbox;
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

/// Handle to a running sandbox. Returned from [`create_sandbox`] and used as a
/// stable reference that crosses HTTP / WebSocket boundaries.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxHandle {
    pub sandbox_id: String,
    pub operator_id: String,
    pub bead_id: String,
    /// Name used as the key into the in-process sandbox registry. Kept as
    /// `msb_name` for backwards compatibility with code that still uses that
    /// field (it's just an opaque identifier now, no longer tied to the CLI).
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

// ---------------------------------------------------------------------------
// Registry: process-wide map from sandbox name → live microsandbox::Sandbox.
//
// `microsandbox::Sandbox` is not `Clone`, so we wrap it in an `Arc` to allow
// multiple callers to share it across `.await` points without holding the
// registry mutex.
// ---------------------------------------------------------------------------

fn registry() -> &'static Mutex<HashMap<String, Arc<Sandbox>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Sandbox>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Insert a live sandbox into the registry under `name`.
fn register(name: &str, sandbox: Sandbox) {
    if let Ok(mut map) = registry().lock() {
        map.insert(name.to_string(), Arc::new(sandbox));
    }
}

/// Remove a sandbox from the registry, returning it if present.
fn unregister(name: &str) -> Option<Arc<Sandbox>> {
    registry().lock().ok().and_then(|mut map| map.remove(name))
}

/// Clone the `Arc<Sandbox>` for `name`, if registered.
fn lookup(name: &str) -> Option<Arc<Sandbox>> {
    registry().lock().ok().and_then(|map| map.get(name).cloned())
}

/// Returns `true` if a sandbox is registered under `name`.
fn is_registered(name: &str) -> bool {
    registry().lock().ok().is_some_and(|map| map.contains_key(name))
}

// ---------------------------------------------------------------------------
// Public API — async-first, same types as the old CLI wrapper.
// ---------------------------------------------------------------------------

/// Check if the sandbox backend is available on this host.
///
/// With the embedded `microsandbox` crate there is no external CLI to check
/// for — the backend is always present at build time. This function always
/// returns `true` and exists for API compatibility with the previous `msb`
/// CLI wrapper. Runtime failures (missing KVM/HVF support) will surface
/// when a sandbox is actually created.
#[must_use]
pub fn is_available() -> bool {
    true
}

/// No-op: the crate does not need a daemon.
#[must_use]
pub fn is_server_running() -> bool {
    true
}

/// No-op: the crate does not need a daemon. Kept for API compatibility.
pub fn ensure_server() -> Result<()> {
    Ok(())
}

/// Create and start a sandbox.
///
/// Builds a microVM using the [`microsandbox`] crate, stores the live handle
/// in the in-process registry, and returns a serializable [`SandboxHandle`].
///
/// # Errors
///
/// Returns an error if the VM fails to boot (missing KVM/HVF, OCI image not
/// found, port already in use, etc.).
pub async fn create_sandbox(config: &SandboxConfig, host_port: u16) -> Result<SandboxHandle> {
    let msb_name = format!("smooth-operator-{}", config.operator_id);
    let image = std::env::var("SMOOTH_WORKER_IMAGE").unwrap_or_else(|_| "alpine".into());

    tracing::info!(
        name = %msb_name,
        image = %image,
        host_port,
        cpus = config.cpus,
        memory_mb = config.memory_mb,
        "Creating microVM sandbox"
    );

    // Build the sandbox with our standard configuration. Port 4096 inside the
    // VM is the operator's WebSocket server; we map it to `host_port` so
    // Big Smooth can reach it.
    //
    // `cpus` is a `u8` in the crate; clamp to the max to avoid a panic on
    // overflow. `memory` takes `impl Into<Mebibytes>` and `u32` implements it.
    let cpus_u8 = u8::try_from(config.cpus).unwrap_or(u8::MAX);
    let mut builder = Sandbox::builder(msb_name.clone())
        .image(image.as_str())
        .cpus(cpus_u8)
        .memory(config.memory_mb)
        .port(host_port, 4096);

    // Inject environment variables (LLM API key, model, etc.).
    for (k, v) in &config.env {
        builder = builder.env(k, v);
    }

    let sandbox = builder
        .create()
        .await
        .with_context(|| format!("Failed to create microVM sandbox '{msb_name}' from image '{image}'"))?;

    register(&msb_name, sandbox);

    let now = chrono::Utc::now();
    let timeout_at = now + chrono::Duration::seconds(i64::try_from(config.timeout_seconds).unwrap_or(i64::MAX));

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

/// Destroy a sandbox: remove it from the registry and stop the microVM.
///
/// Idempotent — returns `Ok(())` if the sandbox is already gone.
///
/// # Errors
///
/// Returns an error if `stop_and_wait` on the underlying microVM fails AND
/// no other references to the Arc are held. If other references exist (e.g.,
/// a concurrent `exec_in_sandbox` call), the VM will still stop when the last
/// reference is dropped.
pub async fn destroy_sandbox(msb_name: &str) -> Result<()> {
    let Some(arc) = unregister(msb_name) else {
        tracing::debug!(name = %msb_name, "destroy_sandbox: no sandbox registered");
        return Ok(());
    };

    tracing::info!(name = %msb_name, "Destroying microVM sandbox");
    // Only call stop_and_wait if we hold the sole reference. Otherwise leave
    // cleanup to the Arc drop (the crate handles lifecycle internally).
    match Arc::try_unwrap(arc) {
        Ok(sandbox) => {
            sandbox
                .stop_and_wait()
                .await
                .with_context(|| format!("Failed to stop sandbox '{msb_name}'"))?;
        }
        Err(arc_shared) => {
            tracing::debug!(
                name = %msb_name,
                refs = Arc::strong_count(&arc_shared),
                "destroy_sandbox: other references exist; stop will happen on last drop"
            );
        }
    }
    Ok(())
}

/// Get the current status of a sandbox.
///
/// Returns a `SandboxStatus` with `running: false` if the sandbox is unknown
/// to the registry. Presence in the registry is the source of truth: Big
/// Smooth owns the lifecycle of every sandbox it creates and removes the
/// entry on `destroy_sandbox`.
pub async fn get_status(msb_name: &str) -> SandboxStatus {
    let running = is_registered(msb_name);

    SandboxStatus {
        running,
        // Health is currently equivalent to "running"; a real health endpoint
        // inside the operator would flip this independently.
        healthy: running,
        phase: "unknown".into(),
        uptime_ms: 0,
    }
}

/// Execute a command inside a running sandbox and collect its output.
///
/// Returns `(stdout, stderr, exit_code)`. Exit code is `-1` if the VM is
/// not registered or the exec call fails.
///
/// # Errors
///
/// Returns an error if the sandbox is not registered or the command fails
/// to launch (note: a non-zero exit code is reported via the returned tuple,
/// not as an error).
pub async fn exec_in_sandbox(msb_name: &str, command: &[&str]) -> Result<(String, String, i32)> {
    let Some((cmd, args)) = command.split_first() else {
        anyhow::bail!("exec_in_sandbox: command is empty");
    };

    // Clone the Arc out of the registry before awaiting so we do not hold the
    // mutex across an `.await`.
    let sandbox = lookup(msb_name).ok_or_else(|| anyhow::anyhow!("no sandbox registered under '{msb_name}'"))?;

    // `Sandbox::exec` wants the command as `impl Into<String>` and the args as
    // an iterator of `impl Into<String>`. Convert both to owned `String`s to
    // avoid lifetime / trait-resolution issues with `&&str`.
    let cmd_owned: String = (*cmd).to_string();
    let args_owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();

    let output = sandbox
        .exec(cmd_owned, args_owned)
        .await
        .with_context(|| format!("exec in sandbox '{msb_name}' failed"))?;

    let stdout = output.stdout().unwrap_or_default();
    let stderr = output.stderr().unwrap_or_default();
    let code = output.status().code;
    Ok((stdout, stderr, code))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let config = SandboxConfig::default();
        assert!(config.operator_id.starts_with("operator-"));
        assert_eq!(config.phase, "assess");
        assert_eq!(config.cpus, 2);
        assert_eq!(config.memory_mb, 4096);
        assert!(config.permissions.contains(&"beads:read".into()));
    }

    #[test]
    fn is_available_returns_true_with_embedded_backend() {
        // The embedded crate backend is always "available" — VM boot errors
        // surface at create_sandbox time, not availability time.
        assert!(is_available());
    }

    #[test]
    fn is_server_running_and_ensure_server_are_noops() {
        assert!(is_server_running());
        ensure_server().expect("ensure_server is a no-op");
    }

    #[tokio::test]
    async fn destroy_sandbox_is_idempotent_for_unknown_name() {
        // Destroying a non-registered sandbox must not error.
        destroy_sandbox("nonexistent-sandbox-xyz").await.expect("idempotent destroy");
    }

    #[tokio::test]
    async fn get_status_for_unknown_sandbox_reports_not_running() {
        let status = get_status("nonexistent-sandbox-xyz").await;
        assert!(!status.running);
        assert!(!status.healthy);
    }

    #[tokio::test]
    async fn exec_in_unknown_sandbox_errors() {
        let result = exec_in_sandbox("nonexistent-sandbox-xyz", &["echo", "hi"]).await;
        assert!(result.is_err(), "exec in unknown sandbox must error, got {result:?}");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no sandbox registered"), "unexpected error message: {err}");
    }

    #[tokio::test]
    async fn exec_with_empty_command_errors() {
        let result = exec_in_sandbox("any", &[]).await;
        assert!(result.is_err());
    }

    #[test]
    fn registry_roundtrip_requires_sandbox_type() {
        // We cannot construct a real Sandbox in a unit test (requires VM boot),
        // but we can at least verify the registry is initialized and empty on
        // first access.
        let map = registry().lock().expect("lock registry");
        // Just assert we can lock and read it; don't assert emptiness because
        // other tests may have populated it concurrently.
        let _ = map.len();
    }

    // ------------------------------------------------------------------
    // Smoke test: actually boot a microVM and run a command inside it.
    //
    // Marked `#[ignore]` because it depends on hardware virtualization
    // (KVM on Linux, HVF on Apple Silicon) and needs to pull the `alpine`
    // OCI image on first run — both of which make it unsuitable for
    // `cargo test` in CI. Run explicitly with:
    //
    //     cargo test -p smooth-bigsmooth -- --ignored sandbox_smoke
    //
    // The test boots a single Alpine VM, runs `echo hello from microvm`,
    // asserts the output, then cleans up.
    // ------------------------------------------------------------------
    #[tokio::test]
    #[ignore = "requires hardware virtualization and an OCI image pull"]
    async fn sandbox_smoke_boot_and_exec() {
        let config = SandboxConfig {
            operator_id: format!("smoke-{}", &Uuid::new_v4().to_string()[..8]),
            cpus: 1,
            memory_mb: 512,
            ..SandboxConfig::default()
        };

        // SMOKE_SANDBOX_PORT lets the operator pick a free port on CI if needed.
        let port = std::env::var("SMOOTH_SMOKE_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(24096);

        let handle = create_sandbox(&config, port).await.expect("create sandbox");
        assert!(handle.msb_name.starts_with("smooth-operator-"));

        let status = get_status(&handle.msb_name).await;
        assert!(status.running, "sandbox should be running after create");

        let (stdout, stderr, code) = exec_in_sandbox(&handle.msb_name, &["echo", "hello from microvm"])
            .await
            .expect("exec in sandbox");
        assert_eq!(code, 0, "echo should exit 0, stderr: {stderr}");
        assert!(stdout.contains("hello from microvm"), "unexpected stdout: {stdout:?}");

        destroy_sandbox(&handle.msb_name).await.expect("destroy sandbox");

        let status = get_status(&handle.msb_name).await;
        assert!(!status.running, "sandbox should be gone after destroy");
    }
}
