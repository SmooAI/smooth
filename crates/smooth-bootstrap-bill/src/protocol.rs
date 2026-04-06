//! Wire protocol for Bootstrap Bill.
//!
//! Line-delimited JSON over TCP. Each connection carries exactly one
//! request followed by exactly one response, then closes. Simple, debuggable,
//! no framing tricks.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One bind mount from a host path into a sandbox.
///
/// Same shape as `smooth_bigsmooth::sandbox::BindMount` (the legacy type)
/// but serializable so it can cross the wire to Bill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindMountSpec {
    /// Absolute path on the host. Bill validates it before calling microsandbox.
    pub host_path: String,
    /// Path inside the guest.
    pub guest_path: String,
    /// Whether the mount is read-only.
    pub readonly: bool,
}

/// Host → guest port mapping. `host_port = 0` asks Bill to pick a free
/// ephemeral port from the host kernel (the assigned port comes back in
/// [`BillResponse::Spawned::host_ports`]).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PortMapping {
    pub host_port: u16,
    pub guest_port: u16,
    /// When true, Bill runs a TCP proxy that re-publishes this port on
    /// `0.0.0.0` in addition to microsandbox's default `127.0.0.1`.
    /// This makes the port reachable from OTHER microVMs via the host's
    /// real interface IP (which the TCP proxy inside those VMs connects
    /// to). Without this, published ports are only accessible from the
    /// host loopback — useless for cross-VM traffic like Archivist ingest.
    #[serde(default)]
    pub bind_all: bool,
}

/// Full spec for a sandbox Bill should spawn.
///
/// This is the serialisable form of the old `SandboxConfig`; Bill rebuilds
/// a `microsandbox::Sandbox` from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxSpec {
    /// Name used as the registry key and the microsandbox name (must be
    /// unique per-host while the sandbox is alive). Typical format:
    /// `smooth-operator-<short-uuid>` or `smooth-boardroom-<short-uuid>`.
    pub name: String,
    /// OCI image reference (e.g. `alpine`, or a custom boardroom image tag).
    pub image: String,
    /// Guest CPU count. Clamped to `u8::MAX` on Bill's side.
    pub cpus: u32,
    /// Guest memory in MiB.
    pub memory_mb: u32,
    /// Environment variables passed on the kernel command line. **Must be
    /// printable ASCII only** (microsandbox panics otherwise); Bill
    /// double-checks this before spawning.
    pub env: HashMap<String, String>,
    /// Bind mounts to apply before boot.
    pub mounts: Vec<BindMountSpec>,
    /// Port forwards to set up.
    pub ports: Vec<PortMapping>,
    /// Optional caller-chosen timeout in seconds, for diagnostics only.
    /// Bill does not enforce this; callers track their own deadlines.
    pub timeout_seconds: u64,
    /// Opt the sandbox out of microsandbox's default `public_only` network
    /// policy (which denies loopback + RFC1918 outbound). When true, Bill
    /// applies `NetworkPolicy::allow_all()` to this sandbox, so the guest
    /// can TCP to `127.0.0.1:<port>` and reach host services like Bill
    /// itself or the Boardroom's Archivist.
    ///
    /// Under the hood, microsandbox's TCP proxy calls `TcpStream::connect`
    /// on the host with the guest's destination address verbatim. With the
    /// default policy that path is blocked for loopback/private
    /// destinations; with `allow_all()` it goes through.
    #[serde(default)]
    pub allow_host_loopback: bool,
    /// Optional host-side directory for pearl-scoped environment caching.
    /// Bill bind-mounts this directory at `/opt/smooth/cache` (RW) inside
    /// the VM. The runner sets CARGO_HOME, npm_config_cache, etc. to paths
    /// inside this mount so compiled dependencies, installed packages, and
    /// build artifacts persist across operator VM runs for the same pearl.
    ///
    /// First run is cold: tools install from scratch, deps compile.
    /// Subsequent runs are warm: cached deps, cached packages.
    ///
    /// Bill creates the directory if it doesn't exist. Pass `None` to
    /// skip caching (bare Alpine every time).
    #[serde(default)]
    pub env_cache_key: Option<String>,
}

/// A request Bill accepts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BillRequest {
    /// Liveness probe.
    Ping,
    /// Spawn a sandbox and register it under `spec.name`.
    Spawn {
        spec: SandboxSpec,
    },
    /// Run a command inside an already-spawned sandbox. The command is
    /// executed synchronously; Bill replies with the full stdout/stderr
    /// captured when the command exits.
    Exec {
        name: String,
        argv: Vec<String>,
    },
    /// Stop the sandbox and remove it from the registry. Idempotent.
    Destroy {
        name: String,
    },
    /// Report how many sandboxes Bill currently holds. Handy for panic-hook
    /// teardown assertions in tests.
    List,
}

/// A response Bill sends. Exactly one of these is written per request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BillResponse {
    Pong {
        version: String,
    },
    Spawned {
        /// Confirmed sandbox name (Bill may have normalised it).
        name: String,
        /// Resolved port mappings — for each requested `guest_port`, the
        /// final `host_port` (0 entries in the request are replaced with
        /// the kernel-assigned port).
        host_ports: Vec<PortMapping>,
        /// RFC3339 creation timestamp (host clock).
        created_at: String,
    },
    ExecResult {
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
    Destroyed,
    SandboxList {
        names: Vec<String>,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip_ping() {
        let req = BillRequest::Ping;
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("\"Ping\""), "unexpected json: {json}");
        let decoded: BillRequest = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(decoded, BillRequest::Ping));
    }

    #[test]
    fn request_roundtrip_spawn() {
        let spec = SandboxSpec {
            name: "smooth-test".into(),
            image: "alpine".into(),
            cpus: 2,
            memory_mb: 1024,
            env: [("FOO".into(), "bar".into())].into(),
            mounts: vec![BindMountSpec {
                host_path: "/tmp/work".into(),
                guest_path: "/workspace".into(),
                readonly: false,
            }],
            ports: vec![PortMapping { host_port: 0, guest_port: 4096, bind_all: false }],
            timeout_seconds: 1800,
            allow_host_loopback: false,
            env_cache_key: None,
        };
        let req = BillRequest::Spawn { spec };
        let json = serde_json::to_string(&req).expect("serialize");
        let decoded: BillRequest = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            BillRequest::Spawn { spec } => {
                assert_eq!(spec.name, "smooth-test");
                assert_eq!(spec.mounts.len(), 1);
                assert_eq!(spec.ports[0].guest_port, 4096);
            }
            other => panic!("unexpected decoded request: {other:?}"),
        }
    }

    #[test]
    fn response_roundtrip_spawned() {
        let resp = BillResponse::Spawned {
            name: "smooth-test".into(),
            host_ports: vec![PortMapping { host_port: 42424, guest_port: 4096, bind_all: false }],
            created_at: "2026-04-05T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let decoded: BillResponse = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            BillResponse::Spawned { host_ports, .. } => {
                assert_eq!(host_ports[0].host_port, 42424);
            }
            other => panic!("unexpected decoded response: {other:?}"),
        }
    }

    #[test]
    fn response_roundtrip_error() {
        let resp = BillResponse::Error { message: "boom".into() };
        let json = serde_json::to_string(&resp).expect("serialize");
        let decoded: BillResponse = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            BillResponse::Error { message } => assert_eq!(message, "boom"),
            other => panic!("unexpected decoded response: {other:?}"),
        }
    }
}
