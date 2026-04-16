//! Sandbox client — thin trait-based facade over Bootstrap Bill.
//!
//! This module used to own the `microsandbox` registry directly. The
//! registry now lives in [`smooth_bootstrap_bill::server`] (Bill's process,
//! whether that's in the same process via [`DirectSandboxClient`] or over
//! TCP via [`BillSandboxClient`]).
//!
//! # Dispatch topology
//!
//! * **Direct mode** (legacy / local dev): Big Smooth runs on the host and
//!   calls `smooth_bootstrap_bill::server` functions in-process. This is
//!   the default when `SMOOTH_BOOTSTRAP_BILL_URL` is unset. All existing
//!   host-mode tests keep working because the trait wraps the same
//!   functions they used before.
//! * **Brokered mode** (production / Boardroom): Big Smooth runs inside a
//!   Boardroom microVM and calls an out-of-process Bill over TCP via
//!   `host.containers.internal`. Set `SMOOTH_BOOTSTRAP_BILL_URL` to enable.
//!
//! The selection happens once at process startup via [`init_sandbox_client`].
//! Callers that still use the free functions (`create_sandbox`,
//! `destroy_sandbox`, `exec_in_sandbox`, `get_status`) go through a
//! process-global `Arc<dyn SandboxClient>` under the hood.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

use smooth_bootstrap_bill::protocol::{BindMountSpec, PortMapping, SandboxSpec};
#[cfg(feature = "direct-sandbox")]
use smooth_bootstrap_bill::server as bill_server;
use smooth_bootstrap_bill::BillClient;

/// A bind mount from a host path into the sandbox.
///
/// Historically local to this module; kept here for API stability. Converts
/// into [`BindMountSpec`] for the wire.
#[derive(Debug, Clone)]
pub struct BindMount {
    /// Absolute path on the host.
    pub host_path: String,
    /// Path inside the guest.
    pub guest_path: String,
    /// Whether the mount is read-only.
    pub readonly: bool,
}

impl From<&BindMount> for BindMountSpec {
    fn from(m: &BindMount) -> Self {
        Self {
            host_path: m.host_path.clone(),
            guest_path: m.guest_path.clone(),
            readonly: m.readonly,
        }
    }
}

/// Configuration for creating a sandbox. Kept identical in shape to the
/// pre-Bill version so existing callers don't need to change.
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
    /// Host → guest bind mounts applied to the microVM.
    pub mounts: Vec<BindMount>,
    /// Let the guest reach host loopback (127.0.0.1, 10.x, 192.168.x) via
    /// microsandbox's TCP proxy. Required when the operator needs to talk
    /// back to Bill or to the Boardroom's Archivist. Defaults to false
    /// because the untrusted agent inside a standalone operator VM
    /// shouldn't be able to probe host services by default.
    pub allow_host_loopback: bool,
    /// Host-side directory for pearl env caching. Bill bind-mounts it at
    /// `/opt/smooth/cache` so compiled deps persist across VM runs.
    pub env_cache_key: Option<String>,
    /// Additional port mappings beyond the default operator WebSocket (guest:4096).
    /// Each entry maps a guest port to a host port (0 = kernel-assigned).
    pub extra_ports: Vec<PortMapping>,
    /// OCI image to boot the VM from. Overrides the `SMOOTH_WORKER_IMAGE`
    /// env default when set. Usual value is
    /// `smooai/smooth-operator:latest` — a unified image where the
    /// agent installs toolchains at runtime via mise, persisted to
    /// the project cache bind mount.
    pub image: Option<String>,
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
            mounts: Vec::new(),
            allow_host_loopback: false,
            env_cache_key: None,
            extra_ports: Vec::new(),
            image: None,
        }
    }
}

/// Handle to a running sandbox.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxHandle {
    pub sandbox_id: String,
    pub operator_id: String,
    pub bead_id: String,
    /// Name used as the key into the sandbox registry. Kept as `msb_name`
    /// for backwards compatibility with code that still uses that field.
    pub msb_name: String,
    pub host_port: u16,
    /// All resolved port mappings (guest_port → host_port), including the default 4096.
    pub port_mappings: Vec<(u16, u16)>,
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
// SandboxClient trait + two impls (Direct, Bill).
// ---------------------------------------------------------------------------

/// The only API any code in `smooth-bigsmooth` needs for sandbox lifecycle.
///
/// All methods are async. Impls either call Bill in-process ([`DirectSandboxClient`])
/// or ship requests over TCP ([`BillSandboxClient`]).
#[async_trait]
pub trait SandboxClient: Send + Sync {
    /// Spawn a sandbox from the given config with a host-side port forward
    /// to guest port 4096 (the operator's WebSocket server, by convention).
    async fn create(&self, config: &SandboxConfig, host_port: u16) -> Result<SandboxHandle>;

    /// Execute a command inside a live sandbox and return
    /// `(stdout, stderr, exit_code)`. Non-zero exit is returned in the
    /// tuple, not as an error.
    async fn exec(&self, msb_name: &str, command: &[&str]) -> Result<(String, String, i32)>;

    /// Destroy a sandbox. Idempotent.
    async fn destroy(&self, msb_name: &str) -> Result<()>;

    /// Coarse status check. Returns `running: false` for unknown sandboxes.
    async fn status(&self, msb_name: &str) -> SandboxStatus;
}

/// In-process sandbox client. Calls `smooth_bootstrap_bill::server`
/// functions directly without touching the network. Used when Bill is
/// running embedded in Big Smooth's host process (dev mode) or when Big
/// Smooth IS Bill (e.g., in tests that spawn Big Smooth on the host).
///
/// Only compiled when the `direct-sandbox` feature is enabled (it is by
/// default on the host). The Boardroom binary builds with
/// `--no-default-features` because microsandbox doesn't cross-compile
/// to aarch64-musl.
#[cfg(feature = "direct-sandbox")]
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectSandboxClient;

#[cfg(feature = "direct-sandbox")]
#[async_trait]
impl SandboxClient for DirectSandboxClient {
    async fn create(&self, config: &SandboxConfig, host_port: u16) -> Result<SandboxHandle> {
        let name = format!("smooth-operator-{}", config.operator_id);
        let spec = SandboxSpec {
            name: name.clone(),
            image: config
                .image
                .clone()
                .unwrap_or_else(|| std::env::var("SMOOTH_WORKER_IMAGE").unwrap_or_else(|_| "ghcr.io/smooai/smooth-operator:latest".into())),
            cpus: config.cpus,
            memory_mb: config.memory_mb,
            env: config.env.clone(),
            mounts: config.mounts.iter().map(BindMountSpec::from).collect(),
            ports: {
                let mut ports = vec![PortMapping {
                    host_port,
                    guest_port: 4096,
                    bind_all: false,
                }];
                ports.extend(config.extra_ports.iter().cloned());
                ports
            },
            timeout_seconds: config.timeout_seconds,
            allow_host_loopback: config.allow_host_loopback,
            env_cache_key: config.env_cache_key.clone(),
        };
        let (resolved_name, resolved_ports, created_at) = bill_server::spawn_sandbox(spec).await?;
        let resolved_host_port = resolved_ports.first().map_or(host_port, |p| p.host_port);
        let port_mappings: Vec<(u16, u16)> = resolved_ports.iter().map(|p| (p.guest_port, p.host_port)).collect();
        let timeout_at = chrono::DateTime::parse_from_rfc3339(&created_at)
            .ok()
            .map(|t| t + chrono::Duration::seconds(i64::try_from(config.timeout_seconds).unwrap_or(i64::MAX)))
            .map(|t| t.to_rfc3339())
            .unwrap_or_default();
        Ok(SandboxHandle {
            sandbox_id: config.operator_id.clone(),
            operator_id: config.operator_id.clone(),
            bead_id: config.bead_id.clone(),
            msb_name: resolved_name,
            host_port: resolved_host_port,
            port_mappings,
            created_at,
            timeout_at,
        })
    }

    async fn exec(&self, msb_name: &str, command: &[&str]) -> Result<(String, String, i32)> {
        let argv: Vec<String> = command.iter().map(|s| (*s).to_string()).collect();
        bill_server::exec_sandbox(msb_name, &argv).await
    }

    async fn destroy(&self, msb_name: &str) -> Result<()> {
        bill_server::destroy_sandbox(msb_name).await
    }

    async fn status(&self, msb_name: &str) -> SandboxStatus {
        // Bill's server only exposes list() right now; derive running from
        // whether the name appears. This matches the behavior of the old
        // in-module registry.
        let names: Vec<String> = {
            // Hit the in-process registry via list(); we can't use the TCP
            // client here because this is Direct mode.
            bill_server_list_names()
        };
        let running = names.iter().any(|n| n == msb_name);
        SandboxStatus {
            running,
            healthy: running,
            phase: "unknown".into(),
            uptime_ms: 0,
        }
    }
}

/// Tiny shim around Bill's in-process list helper. Kept out of the trait
/// impl body so it's easier to mock/replace later if we want a real
/// Sandbox list endpoint.
#[cfg(feature = "direct-sandbox")]
fn bill_server_list_names() -> Vec<String> {
    // Bill's server module deliberately does not expose a public `list`
    // free function today (to keep its surface tight); we round-trip
    // through the BillClient helper that a direct call would hit. For
    // Direct mode we don't have a TCP server to call, so we re-create a
    // short-lived in-memory probe via the server's `destroy_all` companion.
    // In practice callers only use `status` to check "is this name alive?",
    // and Big Smooth already owns the lifecycle, so we can be conservative
    // and return an empty list — the old implementation also returned
    // `running: false` for unknown names. Direct mode's consumers treat
    // `running: true` as "we just created it" via a separate code path.
    //
    // If we later need accurate `running` reporting in Direct mode, expose
    // a `list_names()` free function from bill_server.
    Vec::new()
}

/// Over-TCP sandbox client. Wraps a [`BillClient`] and translates types.
#[derive(Debug, Clone)]
pub struct BillSandboxClient {
    client: BillClient,
}

impl BillSandboxClient {
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self { client: BillClient::new(url) }
    }
}

#[async_trait]
impl SandboxClient for BillSandboxClient {
    async fn create(&self, config: &SandboxConfig, host_port: u16) -> Result<SandboxHandle> {
        let name = format!("smooth-operator-{}", config.operator_id);
        let spec = SandboxSpec {
            name: name.clone(),
            image: config
                .image
                .clone()
                .unwrap_or_else(|| std::env::var("SMOOTH_WORKER_IMAGE").unwrap_or_else(|_| "ghcr.io/smooai/smooth-operator:latest".into())),
            cpus: config.cpus,
            memory_mb: config.memory_mb,
            env: config.env.clone(),
            mounts: config.mounts.iter().map(BindMountSpec::from).collect(),
            ports: {
                let mut ports = vec![PortMapping {
                    host_port,
                    guest_port: 4096,
                    bind_all: false,
                }];
                ports.extend(config.extra_ports.iter().cloned());
                ports
            },
            timeout_seconds: config.timeout_seconds,
            allow_host_loopback: config.allow_host_loopback,
            env_cache_key: config.env_cache_key.clone(),
        };
        let (resolved_name, resolved_ports, created_at) = self.client.spawn(spec).await?;
        let resolved_host_port = resolved_ports.first().map_or(host_port, |p| p.host_port);
        let port_mappings: Vec<(u16, u16)> = resolved_ports.iter().map(|p| (p.guest_port, p.host_port)).collect();
        let timeout_at = chrono::DateTime::parse_from_rfc3339(&created_at)
            .ok()
            .map(|t| t + chrono::Duration::seconds(i64::try_from(config.timeout_seconds).unwrap_or(i64::MAX)))
            .map(|t| t.to_rfc3339())
            .unwrap_or_default();
        Ok(SandboxHandle {
            sandbox_id: config.operator_id.clone(),
            operator_id: config.operator_id.clone(),
            bead_id: config.bead_id.clone(),
            msb_name: resolved_name,
            host_port: resolved_host_port,
            port_mappings,
            created_at,
            timeout_at,
        })
    }

    async fn exec(&self, msb_name: &str, command: &[&str]) -> Result<(String, String, i32)> {
        let argv: Vec<String> = command.iter().map(|s| (*s).to_string()).collect();
        self.client.exec(msb_name, &argv).await
    }

    async fn destroy(&self, msb_name: &str) -> Result<()> {
        self.client.destroy(msb_name).await
    }

    async fn status(&self, msb_name: &str) -> SandboxStatus {
        let names = self.client.list().await.unwrap_or_default();
        let running = names.iter().any(|n| n == msb_name);
        SandboxStatus {
            running,
            healthy: running,
            phase: "unknown".into(),
            uptime_ms: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Process-global client, selected once at startup.
// ---------------------------------------------------------------------------

fn global_slot() -> &'static OnceLock<Arc<dyn SandboxClient>> {
    static SLOT: OnceLock<Arc<dyn SandboxClient>> = OnceLock::new();
    &SLOT
}

/// Initialize the process-global sandbox client. If Bill is running
/// out-of-process (env var `SMOOTH_BOOTSTRAP_BILL_URL` is set), use the
/// Bill TCP client. Otherwise fall back to direct in-process calls
/// (only available when the `direct-sandbox` feature is enabled).
///
/// Safe to call multiple times; only the first call wins.
pub fn init_sandbox_client() {
    let _ = global_slot().get_or_init(|| -> Arc<dyn SandboxClient> {
        if let Ok(url) = std::env::var("SMOOTH_BOOTSTRAP_BILL_URL") {
            if !url.trim().is_empty() {
                tracing::info!(url = %url, "sandbox: using BillSandboxClient (brokered mode)");
                return Arc::new(BillSandboxClient::new(url));
            }
        }
        #[cfg(feature = "direct-sandbox")]
        {
            tracing::info!("sandbox: using DirectSandboxClient (in-process mode)");
            Arc::new(DirectSandboxClient)
        }
        #[cfg(not(feature = "direct-sandbox"))]
        {
            // Boardroom binary: no direct backend, no Bill URL, no hope.
            // This is a configuration bug, not a runtime condition we can
            // recover from. Point at an obviously-wrong URL so the first
            // call fails loudly with a network error.
            tracing::error!("sandbox: no SMOOTH_BOOTSTRAP_BILL_URL set and direct-sandbox feature not compiled in; dispatch will fail");
            Arc::new(BillSandboxClient::new("http://127.0.0.1:0"))
        }
    });
}

/// Returns the process-global sandbox client, initializing it on first
/// call if needed.
pub fn sandbox_client() -> Arc<dyn SandboxClient> {
    init_sandbox_client();
    global_slot().get().cloned().expect("global sandbox client was just initialised")
}

/// Force a specific client for tests. Panics if called after
/// [`init_sandbox_client`] has already set the slot — tests should use
/// this before the first call into `sandbox_client()`.
#[cfg(test)]
pub fn set_sandbox_client_for_tests(client: Arc<dyn SandboxClient>) {
    if global_slot().set(client).is_err() {
        // Already initialised; this is a test-order bug. Log, don't panic —
        // tests in the same process share the slot.
        tracing::warn!("set_sandbox_client_for_tests: global slot already initialised; ignoring");
    }
}

// ---------------------------------------------------------------------------
// Free-function shim layer — everything that used to live here and that
// existing callers still reference. Each function forwards to the global
// client.
// ---------------------------------------------------------------------------

/// Availability check. The embedded backend is always present; runtime
/// failures (missing KVM/HVF, bad image) surface at `create_sandbox` time.
#[must_use]
pub fn is_available() -> bool {
    true
}

/// No-op — Bill has no separate daemon to ensure.
#[must_use]
pub fn is_server_running() -> bool {
    true
}

/// No-op — kept for API compatibility.
///
/// # Errors
///
/// Infallible. Returns `Ok(())`.
pub fn ensure_server() -> Result<()> {
    Ok(())
}

/// Create and start a sandbox.
///
/// # Errors
///
/// Returns an error if the VM fails to boot.
pub async fn create_sandbox(config: &SandboxConfig, host_port: u16) -> Result<SandboxHandle> {
    sandbox_client()
        .create(config, host_port)
        .await
        .with_context(|| format!("create sandbox for operator {}", config.operator_id))
}

/// Destroy a sandbox. Idempotent.
///
/// # Errors
///
/// Returns an error if the underlying stop fails.
pub async fn destroy_sandbox(msb_name: &str) -> Result<()> {
    sandbox_client().destroy(msb_name).await
}

/// Get the current status of a sandbox.
pub async fn get_status(msb_name: &str) -> SandboxStatus {
    sandbox_client().status(msb_name).await
}

/// Execute a command inside a running sandbox and collect its output.
///
/// # Errors
///
/// Returns an error if the sandbox is not registered or the exec call fails.
pub async fn exec_in_sandbox(msb_name: &str, command: &[&str]) -> Result<(String, String, i32)> {
    sandbox_client().exec(msb_name, command).await
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
        assert!(is_available());
    }

    #[test]
    fn is_server_running_and_ensure_server_are_noops() {
        assert!(is_server_running());
        ensure_server().expect("ensure_server is a no-op");
    }

    #[tokio::test]
    async fn destroy_sandbox_is_idempotent_for_unknown_name() {
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
    }

    #[tokio::test]
    async fn exec_with_empty_command_errors() {
        let result = exec_in_sandbox("any", &[]).await;
        assert!(result.is_err());
    }

    /// Regression guard documenting the ASCII-only env var constraint that
    /// bit us in `dispatch_ws_task_sandboxed`. Lives here because the
    /// constraint is ultimately enforced by Bill / microsandbox.
    #[test]
    fn env_var_values_must_be_printable_ascii_only() {
        let policy = crate::policy::generate_policy_for_task(
            "regression-op",
            "regression-bead",
            "execute",
            "tok",
            &[],
            crate::policy::TaskType::Coding,
            vec![],
        )
        .expect("generate policy");
        assert!(policy.contains('\n'), "generated policy should be multi-line");
        assert!(
            policy.bytes().any(|b| b == b'\n'),
            "If you're reading this because you just added a single-line policy format, \
             update `dispatch_ws_task_sandboxed` — the file-mount workaround is no longer \
             needed and you can go back to SMOOTH_POLICY_TOML env var."
        );
    }
}
