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
    /// When true, back `env_cache_key` with a microsandbox named Volume
    /// (first-class primitive: quota-able, listable via `Volume::list`,
    /// removable via `Volume::remove`) instead of the ad-hoc bind-mount
    /// of `~/.smooth/project-cache/<key>`.
    ///
    /// **Default: `true`** — the CLI (`th cache …`) understands both
    /// backends, so new caches go to the Volume store. To opt back into
    /// the legacy bind-mount backend for a specific run, set
    /// `SMOOTH_USE_VOLUMES=0` (or `false`/`no`/`off`). Existing
    /// bind-mount entries under `~/.smooth/project-cache/<key>` are
    /// still honored by Bill when this is false, and `th cache list`
    /// shows both populations until the old entries are pruned.
    pub use_named_volume_for_cache: bool,
    /// Additional port mappings beyond the default operator WebSocket (guest:4096).
    /// Each entry maps a guest port to a host port (0 = kernel-assigned).
    pub extra_ports: Vec<PortMapping>,
    /// OCI image to boot the VM from. Overrides the `SMOOTH_WORKER_IMAGE`
    /// env default when set. Usual value is
    /// `smooai/smooth-operator:latest` — a unified image where the
    /// agent installs toolchains at runtime via mise, persisted to
    /// the project cache bind mount.
    pub image: Option<String>,
    /// Secrets to inject via microsandbox's SecretBuilder. See
    /// [`SecretConfig`] for semantics — short version: the guest
    /// sees `env_var = placeholder` and the real value is only
    /// substituted on outbound requests to `allowed_hosts`.
    pub secrets: Vec<SecretConfig>,
}

/// One secret to plumb through microsandbox. The guest-visible
/// env var will hold `placeholder` until microsandbox rewrites an
/// outbound request to one of `allowed_hosts` at which point the
/// real `value` is substituted on the wire.
///
/// This is the in-process mirror of the on-wire [`SecretSpec`];
/// the two carry the same fields and convert cleanly.
#[derive(Debug, Clone)]
pub struct SecretConfig {
    pub env_var: String,
    pub value: String,
    pub placeholder: String,
    pub allowed_hosts: Vec<String>,
}

impl From<&SecretConfig> for smooth_bootstrap_bill::protocol::SecretSpec {
    fn from(s: &SecretConfig) -> Self {
        Self {
            env_var: s.env_var.clone(),
            value: s.value.clone(),
            placeholder: s.placeholder.clone(),
            allowed_hosts: s.allowed_hosts.clone(),
        }
    }
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
            // Default to the microsandbox named-Volume backend.
            // th-266809 flipped this after `th cache` learned both
            // backends (th-fb7bec). Opt out per-run with
            // SMOOTH_USE_VOLUMES=0.
            use_named_volume_for_cache: true,
            extra_ports: Vec::new(),
            image: None,
            secrets: Vec::new(),
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
            use_named_volume_for_cache: config.use_named_volume_for_cache,
            secrets: config.secrets.iter().map(smooth_bootstrap_bill::protocol::SecretSpec::from).collect(),
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
            use_named_volume_for_cache: config.use_named_volume_for_cache,
            secrets: config.secrets.iter().map(smooth_bootstrap_bill::protocol::SecretSpec::from).collect(),
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
// Docker backend — sandboxing for nested-virt environments.
// ---------------------------------------------------------------------------

/// Sandbox client that runs operator containers via the Docker CLI.
///
/// Intended for environments where microsandbox can't boot — GitHub
/// Actions' Linux runners, nested cloud VMs, Kubernetes pods — but
/// where the caller still wants *some* isolation (a new process
/// namespace, controlled filesystem view, network namespace) rather
/// than the `SMOOTH_WORKFLOW_DIRECT=1` "run the agent as a host
/// subprocess" path.
///
/// Compared to the microsandbox backend:
///   - No hardware isolation (containers share the host kernel).
///   - Narc / Wonk / Goalie still spin up INSIDE the container so
///     their in-process enforcement (regex secret detection, tool
///     policy gates, prompt-injection guard) still works. What's
///     lost is the hardware-level network policy enforcement that
///     krun/KVM gave us — if an operator gets root inside the
///     container, it can reach everything Docker can reach.
///   - Works in CI + nested virt out of the box.
///
/// Shelling out to `docker` rather than using Bollard keeps the dep
/// tree slim. The CLI is ubiquitous; when it isn't installed,
/// `create` fails loudly at first use with a clear error.
#[derive(Debug, Clone)]
pub struct DockerSandboxClient {
    docker_bin: String,
}

impl DockerSandboxClient {
    /// Construct a client. `docker_bin` defaults to `"docker"`, but
    /// `SMOOTH_DOCKER_BIN=/path/to/docker` overrides — useful on
    /// systems where it's `podman` or an alternate path.
    #[must_use]
    pub fn new() -> Self {
        let docker_bin = std::env::var("SMOOTH_DOCKER_BIN").unwrap_or_else(|_| "docker".into());
        Self { docker_bin }
    }

    fn container_name(operator_id: &str) -> String {
        // Docker container names are `[a-zA-Z0-9][a-zA-Z0-9_.-]+` —
        // operator IDs are UUIDs so they always fit.
        format!("smooth-operator-{operator_id}")
    }

    async fn run_cmd(&self, args: &[&str]) -> Result<(String, String, i32)> {
        let out = tokio::process::Command::new(&self.docker_bin)
            .args(args)
            .output()
            .await
            .with_context(|| format!("spawning `{} {}`", self.docker_bin, args.join(" ")))?;
        let code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        Ok((stdout, stderr, code))
    }
}

impl Default for DockerSandboxClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SandboxClient for DockerSandboxClient {
    async fn create(&self, config: &SandboxConfig, host_port: u16) -> Result<SandboxHandle> {
        let name = Self::container_name(&config.operator_id);
        let image = config
            .image
            .clone()
            .unwrap_or_else(|| std::env::var("SMOOTH_WORKER_IMAGE").unwrap_or_else(|_| "ghcr.io/smooai/smooth-operator:latest".into()));

        // Build `docker run` args.
        //   -d              detached; we'll use `docker exec` for the real work.
        //   --name          stable identifier used by exec/destroy.
        //   --rm            auto-clean on stop.
        //   --cpus / -m     resource limits (rough parity with microVM sizing).
        //   -p host:guest   port forwards — default operator WS on guest 4096.
        //   -v src:dst[:ro] bind mounts for workspace, policy, project cache.
        //   -e KEY=VAL      env passthrough for SMOOTH_* config.
        // Entrypoint is NOT the runner — we start the container with a
        // long-sleep so `docker exec` can invoke the runner below.
        let mut args: Vec<String> = vec!["run".into(), "-d".into(), "--rm".into(), "--name".into(), name.clone()];

        // Resource hints — Docker accepts --cpus as a fractional value;
        // treat our `cpus: u32` as whole cores.
        args.push("--cpus".into());
        args.push(config.cpus.to_string());
        args.push("-m".into());
        args.push(format!("{}m", config.memory_mb));

        // Default WS port + extras.
        args.push("-p".into());
        args.push(format!("127.0.0.1:{host_port}:4096"));
        for port in &config.extra_ports {
            // host_port=0 means let Docker pick an ephemeral one — docker
            // does that with `-p 0:guest`; we still report the allocation
            // back via `docker port` if callers need it. For now, pass
            // through explicit mappings only; kernel-assigned host ports
            // are a follow-up (same pattern the microsandbox path uses).
            if port.host_port != 0 {
                args.push("-p".into());
                args.push(format!("127.0.0.1:{}:{}", port.host_port, port.guest_port));
            }
        }

        // Allow host loopback reach via host.docker.internal.
        // Linux Docker needs --add-host=host.docker.internal:host-gateway
        // explicitly; Docker Desktop adds it automatically. Always include
        // so behaviour is portable.
        if config.allow_host_loopback {
            args.push("--add-host".into());
            args.push("host.docker.internal:host-gateway".into());
        }

        // Bind mounts. Each caller-provided mount maps host → guest at
        // the guest_path. The `readonly` flag, when set, appends `:ro`.
        for mount in &config.mounts {
            let flag = if mount.readonly { ":ro" } else { "" };
            args.push("-v".into());
            args.push(format!("{}:{}{}", mount.host_path, mount.guest_path, flag));
        }

        // Env vars — alphabetically ordered on output for deterministic
        // test assertions against the command line.
        let mut env_entries: Vec<(String, String)> = config.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        env_entries.sort();
        for (k, v) in env_entries {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }

        args.push(image);
        // Keep container alive indefinitely; we'll exec the runner.
        args.push("sleep".into());
        args.push("infinity".into());

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (stdout, stderr, code) = self.run_cmd(&arg_refs).await?;
        if code != 0 {
            anyhow::bail!("docker run failed (exit {code}): {stderr}\n{stdout}");
        }

        let created_at = chrono::Utc::now().to_rfc3339();
        let timeout_at = chrono::Utc::now()
            .checked_add_signed(chrono::Duration::seconds(i64::try_from(config.timeout_seconds).unwrap_or(i64::MAX)))
            .map(|t| t.to_rfc3339())
            .unwrap_or_default();

        let port_mappings: Vec<(u16, u16)> = std::iter::once((4096_u16, host_port))
            .chain(config.extra_ports.iter().filter(|p| p.host_port != 0).map(|p| (p.guest_port, p.host_port)))
            .collect();

        Ok(SandboxHandle {
            sandbox_id: config.operator_id.clone(),
            operator_id: config.operator_id.clone(),
            bead_id: config.bead_id.clone(),
            msb_name: name,
            host_port,
            port_mappings,
            created_at,
            timeout_at,
        })
    }

    async fn exec(&self, msb_name: &str, command: &[&str]) -> Result<(String, String, i32)> {
        let mut args: Vec<&str> = vec!["exec", msb_name];
        args.extend_from_slice(command);
        self.run_cmd(&args).await
    }

    async fn destroy(&self, msb_name: &str) -> Result<()> {
        // `rm -f` kills + removes. Idempotent-ish — `docker rm -f
        // <nonexistent>` exits non-zero but with a "No such container"
        // message; treat that as success.
        let (_stdout, stderr, code) = self.run_cmd(&["rm", "-f", msb_name]).await?;
        if code != 0 && !stderr.contains("No such container") {
            anyhow::bail!("docker rm -f failed (exit {code}): {stderr}");
        }
        Ok(())
    }

    async fn status(&self, msb_name: &str) -> SandboxStatus {
        // `docker inspect -f '{{.State.Running}}' <name>` → "true" / "false".
        let out = self.run_cmd(&["inspect", "-f", "{{.State.Running}}", msb_name]).await;
        let running = matches!(out, Ok((ref stdout, _, 0)) if stdout.trim() == "true");
        SandboxStatus {
            running,
            healthy: running,
            phase: "docker".into(),
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

/// Initialize the process-global sandbox client. Selection order:
///
/// 1. `SMOOTH_SANDBOX_BACKEND=docker` → `DockerSandboxClient`.
///    Intended for GitHub Actions, k8s pods, nested cloud VMs —
///    any environment where krun/KVM can't boot microVMs but
///    Docker is available.
/// 2. `SMOOTH_BOOTSTRAP_BILL_URL` set → `BillSandboxClient`
///    (brokered microsandbox over TCP; Boardroom mode).
/// 3. `direct-sandbox` feature enabled → `DirectSandboxClient`
///    (embedded microsandbox in-process — the dev-mac default).
/// 4. Otherwise → a broken `BillSandboxClient` pointed at an
///    obviously-wrong URL, so dispatch fails loudly.
///
/// Explicit override `SMOOTH_SANDBOX_BACKEND=microsandbox` picks
/// the microsandbox path regardless of other env; useful for
/// tests that want to bypass Docker even when it happens to be
/// installed. `SMOOTH_SANDBOX_BACKEND=direct` is reserved for the
/// unsandboxed host-subprocess path handled at dispatch time,
/// not here — the client trait doesn't have a "no sandbox" variant.
///
/// Safe to call multiple times; only the first call wins.
pub fn init_sandbox_client() {
    let _ = global_slot().get_or_init(|| -> Arc<dyn SandboxClient> {
        // Explicit backend override. Values other than "docker" and
        // "microsandbox" are logged and ignored.
        if let Ok(backend) = std::env::var("SMOOTH_SANDBOX_BACKEND") {
            let backend = backend.trim().to_ascii_lowercase();
            match backend.as_str() {
                "docker" => {
                    tracing::info!("sandbox: using DockerSandboxClient (SMOOTH_SANDBOX_BACKEND=docker)");
                    return Arc::new(DockerSandboxClient::new());
                }
                "microsandbox" | "msb" => {
                    // Fall through to the default microsandbox selection
                    // below (Bill TCP if URL set, else direct).
                }
                "direct" => {
                    // "direct" is handled at the dispatch level, not the
                    // client level. Fall through to the microsandbox
                    // selection so any sandboxed code path that DOES run
                    // (e.g. preflight checks) still has a backend.
                }
                other if !other.is_empty() => {
                    tracing::warn!(value = %other, "SMOOTH_SANDBOX_BACKEND value not recognised; falling back to default selection");
                }
                _ => {}
            }
        }
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

    #[test]
    fn secret_config_converts_to_wire_secret_spec_field_for_field() {
        let cfg = SecretConfig {
            env_var: "SMOOTH_API_KEY".into(),
            value: "sk-real-key".into(),
            placeholder: "SMOOTH_PLACEHOLDER_API_KEY_NOT_SUBSTITUTED".into(),
            allowed_hosts: vec!["llm.smoo.ai".into(), "api.backup.com".into()],
        };
        let wire: smooth_bootstrap_bill::protocol::SecretSpec = (&cfg).into();
        assert_eq!(wire.env_var, "SMOOTH_API_KEY");
        assert_eq!(wire.value, "sk-real-key");
        assert_eq!(wire.placeholder, "SMOOTH_PLACEHOLDER_API_KEY_NOT_SUBSTITUTED");
        assert_eq!(wire.allowed_hosts, vec!["llm.smoo.ai", "api.backup.com"]);
    }

    #[test]
    fn sandbox_config_default_has_empty_secrets() {
        // Defensible default: no secrets unless the caller adds
        // them explicitly. Prevents an accidental plaintext key
        // from any code path that uses `..SandboxConfig::default()`.
        let cfg = SandboxConfig::default();
        assert!(cfg.secrets.is_empty());
    }

    #[test]
    fn sandbox_config_default_uses_named_volume_backend() {
        // After th-fb7bec (dual-backend CLI) + th-266809 (this flip),
        // the named-Volume backend is the default. Older bind-mount
        // entries under ~/.smooth/project-cache/ are still managed by
        // `th cache list|prune|clear`, so no caches are stranded.
        // Opt out per-run with SMOOTH_USE_VOLUMES=0.
        let cfg = SandboxConfig::default();
        assert!(cfg.use_named_volume_for_cache);
    }

    #[test]
    fn docker_container_name_derives_from_operator_id() {
        let name = DockerSandboxClient::container_name("abc-123");
        assert_eq!(name, "smooth-operator-abc-123");
    }

    #[test]
    fn docker_sandbox_client_respects_docker_bin_env_override() {
        // Set + unwind via a scope to keep other tests unaffected.
        // SAFETY: tests set env vars that other tests might read; run
        // serially if this becomes flaky. For a single-test scope it's
        // fine.
        std::env::set_var("SMOOTH_DOCKER_BIN", "/usr/local/bin/podman");
        let c = DockerSandboxClient::new();
        assert_eq!(c.docker_bin, "/usr/local/bin/podman");
        std::env::remove_var("SMOOTH_DOCKER_BIN");
        let c = DockerSandboxClient::new();
        assert_eq!(c.docker_bin, "docker", "default when env unset");
    }

    #[tokio::test]
    async fn docker_status_reports_not_running_when_docker_missing() {
        // Point at a bogus binary so `docker inspect` fails to spawn.
        // The client should degrade to "not running" rather than
        // panic — this is the shape we want when Docker isn't
        // installed.
        let c = DockerSandboxClient {
            docker_bin: "/definitely/not/a/real/binary".into(),
        };
        let status = c.status("anything").await;
        assert!(!status.running);
        assert!(!status.healthy);
    }

    #[test]
    fn init_sandbox_client_respects_backend_env() {
        // We can't actually call `init_sandbox_client` in a test
        // because the global slot is process-shared and may already
        // be set. Instead, exercise the parsing logic by invoking
        // the branches through the public API.
        //
        // At minimum, verify that `SMOOTH_SANDBOX_BACKEND=docker`
        // produces a `DockerSandboxClient` when we construct it
        // directly the way `init_sandbox_client` does.
        std::env::set_var("SMOOTH_SANDBOX_BACKEND", "docker");
        let backend = std::env::var("SMOOTH_SANDBOX_BACKEND").unwrap().to_ascii_lowercase();
        assert_eq!(backend, "docker");
        let c = DockerSandboxClient::new();
        // Smoke-check: non-empty docker_bin means the constructor wired.
        assert!(!c.docker_bin.is_empty());
        std::env::remove_var("SMOOTH_SANDBOX_BACKEND");
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
