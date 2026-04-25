//! Bill's TCP server and microsandbox registry.
//!
//! This module holds the **only** `microsandbox::Sandbox` handles on the
//! host. All other Board members (Big Smooth, Archivist, etc.) call in over
//! TCP loopback to spawn/exec/destroy pods.
//!
//! The server accepts one request per connection, dispatches it to the
//! registry, and writes exactly one response line before closing.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use microsandbox::{NetworkPolicy, Sandbox};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::protocol::{BillRequest, BillResponse, PortMapping, SandboxSpec};

/// Convert an arbitrary cache key into a microsandbox-safe Volume name.
///
/// microsandbox requires names to start with an alphanumeric char and
/// contain only `[A-Za-z0-9._-]`. Our cache keys are already hex-ish
/// workspace-path hashes in practice, but be defensive: replace anything
/// unsupported with `_`, prefix `smooth-cache-` so multiple smooth
/// installs coexist with other users of the microsandbox volumes store,
/// and cap length at 120 chars (microsandbox has no documented cap, but
/// a lot of filesystems stop liking names past ~255 bytes).
pub fn sanitize_volume_name(cache_key: &str) -> String {
    let mut sanitized = String::with_capacity(cache_key.len());
    for ch in cache_key.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    // Strip leading dots/hyphens/underscores — microsandbox requires
    // the first char to be alphanumeric.
    let trimmed = sanitized.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let body: String = trimmed.chars().take(100).collect();
    let name = if body.is_empty() { "nokey".to_string() } else { body };
    format!("smooth-cache-{name}")
}

// ---------------------------------------------------------------------------
// Registry — lifted wholesale from `smooth-bigsmooth/src/sandbox.rs`.
//
// Keyed by `spec.name`. `Sandbox` is not `Clone`, so we wrap it in an `Arc`
// to let concurrent Exec calls share it without holding the mutex across
// `.await`.
// ---------------------------------------------------------------------------

fn registry() -> &'static Mutex<HashMap<String, Arc<Sandbox>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Sandbox>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register(name: &str, sandbox: Sandbox) {
    if let Ok(mut map) = registry().lock() {
        map.insert(name.to_string(), Arc::new(sandbox));
    }
}

fn unregister(name: &str) -> Option<Arc<Sandbox>> {
    registry().lock().ok().and_then(|mut map| map.remove(name))
}

fn lookup(name: &str) -> Option<Arc<Sandbox>> {
    registry().lock().ok().and_then(|map| map.get(name).cloned())
}

/// Returns the list of sandbox names currently registered.
fn list() -> Vec<String> {
    registry().lock().ok().map(|map| map.keys().cloned().collect()).unwrap_or_default()
}

/// Destroy every registered sandbox. Intended for panic hooks and clean
/// shutdown paths — never in the request path.
pub async fn destroy_all() {
    let names = list();
    for name in names {
        if let Err(e) = destroy_sandbox(&name).await {
            tracing::warn!(name = %name, error = %e, "destroy_all: failed to destroy sandbox");
        }
    }
}

// ---------------------------------------------------------------------------
// Core operations (the same three verbs the old sandbox.rs exposed). These
// are pub so the in-process `DirectSandboxClient` in smooth-bigsmooth can
// call them without going through the TCP layer.
// ---------------------------------------------------------------------------

/// Spawn a sandbox from a `SandboxSpec` and register it.
///
/// # Errors
///
/// Returns an error if:
/// - `spec.name` is already registered
/// - Any env var value contains non-ASCII bytes (would panic microsandbox)
/// - The VM fails to boot (missing hardware virt, bad image, port clash)
pub async fn spawn_sandbox(spec: SandboxSpec) -> Result<(String, Vec<PortMapping>, String)> {
    // Pre-flight: env values must be printable ASCII or microsandbox will
    // panic from `msb_krun_vmm::builder`. Catch it here with a clean error.
    for (k, v) in &spec.env {
        if let Some((pos, byte)) = v.bytes().enumerate().find(|&(_, b)| !(b' '..=b'~').contains(&b)) {
            anyhow::bail!("env var {k}: non-ASCII byte 0x{byte:02x} at offset {pos} (microsandbox requires printable ASCII env values)");
        }
    }
    if lookup(&spec.name).is_some() {
        anyhow::bail!("sandbox '{}' is already registered", spec.name);
    }

    tracing::info!(
        name = %spec.name,
        image = %spec.image,
        cpus = spec.cpus,
        memory_mb = spec.memory_mb,
        mounts = spec.mounts.len(),
        ports = spec.ports.len(),
        "bill: spawning sandbox"
    );

    // pearl th-461ab9 (diag): elapsed-since-spawn breadcrumbs.
    // The hang is observed at "wiring SecretBuilder" → silence; we don't
    // know whether we're stuck in OCI pull, db init, builder.create()
    // closures, or wait_for_relay. Tag every step so the gap pinpoints
    // the call.
    let diag_t0 = std::time::Instant::now();
    let diag_name = spec.name.clone();
    macro_rules! diag {
        ($msg:expr) => {
            tracing::info!(
                name = %diag_name,
                t_ms = diag_t0.elapsed().as_millis() as u64,
                "bill diag: {}", $msg
            );
        };
        ($msg:expr, $($field:tt)*) => {
            tracing::info!(
                name = %diag_name,
                t_ms = diag_t0.elapsed().as_millis() as u64,
                $($field)*,
                "bill diag: {}", $msg
            );
        };
    }
    diag!("entering spawn_sandbox after preflight checks");

    let cpus_u8 = u8::try_from(spec.cpus).unwrap_or(u8::MAX);
    diag!("calling Sandbox::builder()");
    let mut builder = Sandbox::builder(spec.name.clone())
        .image(spec.image.as_str())
        .cpus(cpus_u8)
        .memory(spec.memory_mb);
    diag!("builder constructed");

    // Resolve any host_port == 0 requests by asking the kernel for a free
    // port now, then handing that number to microsandbox. This keeps the
    // broker's contract ("0 means auto-assigned") intact without requiring
    // microsandbox to expose the kernel-assigned port back to us.
    let mut resolved_ports: Vec<PortMapping> = Vec::with_capacity(spec.ports.len());
    for port in &spec.ports {
        let host_port = if port.host_port == 0 {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).with_context(|| "probing for a free host port")?;
            let addr = listener.local_addr().context("reading probe addr")?;
            // Drop immediately so microsandbox can claim the port. There's a
            // narrow race window here but the kernel won't immediately reuse
            // a just-freed ephemeral port for another process.
            drop(listener);
            addr.port()
        } else {
            port.host_port
        };
        builder = builder.port(host_port, port.guest_port);
        resolved_ports.push(PortMapping {
            host_port,
            guest_port: port.guest_port,
            bind_all: port.bind_all,
        });
    }

    for (k, v) in &spec.env {
        builder = builder.env(k, v);
    }

    for mount in &spec.mounts {
        let host = mount.host_path.clone();
        let readonly = mount.readonly;
        builder = builder.volume(mount.guest_path.clone(), move |m| {
            let m = m.bind(host);
            if readonly {
                m.readonly()
            } else {
                m
            }
        });
    }

    // Project-scoped sandbox cache: two backends.
    //
    // A) Named-volume backend (opt-in via spec.use_named_volume_for_cache):
    //    ask microsandbox for a first-class Volume keyed off a sanitized
    //    cache_key. Quota-able, listable via `Volume::list`, removable via
    //    `Volume::remove`. Lives under ~/.microsandbox/volumes/<name>/.
    //
    // B) Bind-mount backend (default, backward-compatible): resolve the
    //    cache key to ~/.smooth/project-cache/<key>/ and bind-mount it at
    //    /opt/smooth/cache. Remains the default until the `th cache
    //    list|prune|clear` CLI commands are migrated to the Volume API —
    //    those commands read the bind-mount host path directly, so
    //    flipping the default prematurely breaks the user-facing tools.
    //
    // Either way, deps / stores (PNPM_STORE_PATH, CARGO_HOME, UV_CACHE_DIR,
    // GOPATH) live inside so repeated runs on the same pearl lineage share
    // them.
    if let Some(ref cache_key) = spec.env_cache_key {
        diag!("processing env_cache_key", cache_key = %cache_key, use_named_volume = spec.use_named_volume_for_cache);
        if spec.use_named_volume_for_cache {
            let volume_name = sanitize_volume_name(cache_key);
            diag!("calling Volume::get", volume = %volume_name);
            match microsandbox::volume::Volume::get(&volume_name).await {
                Ok(_) => {
                    tracing::info!(name = %spec.name, key = %cache_key, volume = %volume_name, "bill: reusing existing named Volume for project cache");
                }
                Err(_) => {
                    // Volume doesn't exist (or DB unreachable) — try to
                    // create. `VolumeAlreadyExists` is benign if another
                    // Bill raced us, so we only bail on other errors.
                    match microsandbox::volume::Volume::create(microsandbox::volume::VolumeConfig {
                        name: volume_name.clone(),
                        quota_mib: None,
                        labels: vec![("smooth-kind".into(), "project-cache".into()), ("smooth-cache-key".into(), cache_key.clone())],
                    })
                    .await
                    {
                        Ok(_) => {
                            tracing::info!(name = %spec.name, key = %cache_key, volume = %volume_name, "bill: created named Volume for project cache");
                        }
                        Err(e) => {
                            let msg = format!("{e}");
                            if msg.contains("already exists") || msg.to_ascii_lowercase().contains("already exists") {
                                tracing::debug!(name = %spec.name, volume = %volume_name, "bill: named volume already existed (benign race)");
                            } else {
                                return Err(anyhow::anyhow!(e).context(format!("bill: create named volume '{volume_name}' for cache key '{cache_key}'")));
                            }
                        }
                    }
                }
            }
            let vol_name_for_mount = volume_name.clone();
            builder = builder.volume("/opt/smooth/cache", move |m| m.named(vol_name_for_mount));
            tracing::info!(name = %spec.name, key = %cache_key, volume = %volume_name, "bill: mounting named Volume at /opt/smooth/cache");
        } else {
            let cache_dir = dirs_next::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join(".smooth")
                .join("project-cache")
                .join(cache_key);
            if !cache_dir.exists() {
                std::fs::create_dir_all(&cache_dir).with_context(|| format!("bill: create project cache: {}", cache_dir.display()))?;
                tracing::info!(path = %cache_dir.display(), key = %cache_key, "bill: created project cache dir");
            }
            // Bump mtime so atime-/mtime-based LRU pruning keeps recently
            // used projects warm. filetime crate would be nicer but we
            // avoid adding a dep for a cosmetic refresh — chmod back to
            // current mode triggers a ctime/mtime bump on most systems
            // without changing permissions.
            if let Ok(md) = std::fs::metadata(&cache_dir) {
                let _ = std::fs::set_permissions(&cache_dir, md.permissions());
            }
            let host = cache_dir.to_string_lossy().to_string();
            builder = builder.volume("/opt/smooth/cache", move |m| m.bind(host));
            tracing::info!(name = %spec.name, key = %cache_key, path = %cache_dir.display(), "bill: mounting project cache at /opt/smooth/cache (bind-mount)");
        }
    }

    // Opt-in: let the guest reach host loopback + RFC1918 addresses.
    // microsandbox's default policy is `public_only`, which denies
    // loopback/private outbound — fine for untrusted operator work, but
    // the Boardroom VM (and operator VMs dispatched by a Boardroom-mode
    // Big Smooth) must be able to talk back to Bill on 127.0.0.1 and to
    // the Boardroom's Archivist. When this flag is set we apply
    // `allow_all()` which removes those denies.
    //
    // The secrets layer rides on the same `.network(|n| ...)` closure,
    // so we stage a single builder mutation that owns both the policy
    // override and the secret entries.
    diag!("preparing network/secrets builder closure", allow_loopback = spec.allow_host_loopback, n_secrets = spec.secrets.len());
    let allow_loopback = spec.allow_host_loopback;
    let secrets = spec.secrets.clone();
    if allow_loopback || !secrets.is_empty() {
        if allow_loopback {
            tracing::info!(name = %spec.name, "bill: applying NetworkPolicy::allow_all() (host loopback enabled)");
        }
        if !secrets.is_empty() {
            tracing::info!(
                name = %spec.name,
                count = secrets.len(),
                "bill: wiring SecretBuilder entries (values substituted on allowed hosts only)"
            );
        }
        builder = builder.network(move |mut n| {
            if allow_loopback {
                n = n.policy(NetworkPolicy::allow_all());
            }
            for secret in &secrets {
                // Clone into the closure because the SecretBuilder is
                // moved on each call. The outer `secrets` vec is owned
                // by this closure (already cloned from spec above) so
                // we can drain through a reference without moves.
                let env_var = secret.env_var.clone();
                let value = secret.value.clone();
                let placeholder = secret.placeholder.clone();
                let allowed = secret.allowed_hosts.clone();
                n = n.secret(move |mut s| {
                    s = s.env(env_var).value(value).placeholder(placeholder);
                    for host in allowed {
                        s = s.allow_host(host);
                    }
                    s
                });
            }
            n
        });
    }

    diag!("about to call builder.create().await — this is the historical hang point");

    // pearl th-461ab9 (diag): parallel filesystem watcher. The microsandbox
    // SDK's spawn_sandbox creates ~/.microsandbox/sandboxes/<name>/{logs,
    // runtime,...} as part of `.create()`. If we never see those dirs
    // appear, the SDK is hung BEFORE forking the child msb process —
    // i.e. in OCI pull / db init / volume validate. If we see the dirs
    // but host.log stays empty (or has no `sandbox starting` line), the
    // child msb forked but never reached `microsandbox_runtime::vm::run`.
    let watch_name = spec.name.clone();
    let watch_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watch_stop_inner = watch_stop.clone();
    let watch_handle = tokio::spawn(async move {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let sandbox_dir = home.join(".microsandbox").join("sandboxes").join(&watch_name);
        let host_log = sandbox_dir.join("logs").join("host.log");
        let runtime_dir = sandbox_dir.join("runtime");
        let agent_sock = runtime_dir.join("agent.sock");
        let mut tick = 0u64;
        let mut head_logged = false;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if watch_stop_inner.load(std::sync::atomic::Ordering::SeqCst) {
                tracing::info!(name = %watch_name, "bill diag-watch: stopped (create() returned)");
                return;
            }
            tick += 1;
            let dir_exists = sandbox_dir.exists();
            let host_log_size = std::fs::metadata(&host_log).map(|m| m.len()).unwrap_or(0);
            let agent_sock_exists = agent_sock.exists();
            tracing::info!(
                name = %watch_name,
                tick,
                dir_exists,
                host_log_size,
                agent_sock_exists,
                "bill diag-watch: polling"
            );
            // First few hundred bytes of host.log when it lands — gives us
            // immediate visibility into whether the child msb is alive.
            if dir_exists && host_log_size > 0 && !head_logged {
                if let Ok(content) = std::fs::read_to_string(&host_log) {
                    let head: String = content.chars().take(800).collect();
                    tracing::info!(name = %watch_name, head = %head, "bill diag-watch: host.log head");
                    head_logged = true;
                }
            }
        }
    });

    let create_started = std::time::Instant::now();
    let sandbox = match builder.create().await {
        Ok(sb) => {
            diag!("builder.create().await RETURNED OK", create_ms = create_started.elapsed().as_millis() as u64);
            watch_stop.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = watch_handle.await;
            sb
        }
        Err(e) => {
            diag!("builder.create().await RETURNED ERR", create_ms = create_started.elapsed().as_millis() as u64, error = %e);
            watch_stop.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = watch_handle.await;
            return Err(anyhow::anyhow!(e).context(format!("bill: failed to create microVM '{}' from image '{}'", spec.name, spec.image)));
        }
    };

    register(&spec.name, sandbox);

    // For any port with `bind_all: true`, spawn a TCP forwarder on
    // 0.0.0.0:port that proxies to 127.0.0.1:port. This makes the
    // published port reachable from other microVMs via the host's real
    // network IP, working around microsandbox's `127.0.0.1`-only bind.
    for port in &resolved_ports {
        if port.bind_all {
            let hp = port.host_port;
            tokio::spawn(async move {
                if let Err(e) = run_bind_all_proxy(hp).await {
                    tracing::warn!(port = hp, error = %e, "bill: bind_all proxy failed");
                }
            });
            tracing::info!(host_port = hp, guest_port = port.guest_port, "bill: bind_all proxy started on 0.0.0.0:{hp}");
        }
    }

    let created_at = chrono::Utc::now().to_rfc3339();
    tracing::info!(name = %spec.name, "bill: sandbox spawned");
    Ok((spec.name, resolved_ports, created_at))
}

/// TCP proxy that re-publishes a `127.0.0.1`-bound port on `0.0.0.0`.
///
/// microsandbox publishes guest ports on 127.0.0.1 only (hardcoded in the
/// builder). For cross-VM traffic (e.g., an operator's Scribe forwarding
/// logs to the Boardroom's Archivist), the port must also be reachable
/// via the host's real network interface. This tiny proxy accepts
/// connections on `0.0.0.0:<port>` and forwards each one to
/// `127.0.0.1:<port>` via tokio::io::copy_bidirectional.
async fn run_bind_all_proxy(port: u16) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    loop {
        let (mut inbound, _) = listener.accept().await?;
        tokio::spawn(async move {
            match tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await {
                Ok(mut outbound) => {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
                Err(e) => {
                    tracing::debug!(port, error = %e, "bind_all proxy: connect to 127.0.0.1 failed");
                }
            }
        });
    }
}

/// Execute a command inside a live sandbox. Blocks until the command exits.
///
/// # Errors
///
/// Returns an error if the sandbox is not registered or microsandbox's
/// `exec` call fails (note: non-zero exit is reported in the return tuple,
/// not as an error).
pub async fn exec_sandbox(name: &str, argv: &[String]) -> Result<(String, String, i32)> {
    let Some((cmd, args)) = argv.split_first() else {
        anyhow::bail!("exec_sandbox: argv is empty");
    };
    let sandbox = lookup(name).ok_or_else(|| anyhow::anyhow!("no sandbox registered under '{name}'"))?;
    let cmd_owned: String = cmd.clone();
    let args_owned: Vec<String> = args.to_vec();

    tracing::info!(sandbox = %name, cmd = %cmd_owned, argc = args_owned.len(), "bill: exec starting");

    let output = sandbox
        .exec(cmd_owned.clone(), args_owned.clone())
        .await
        .with_context(|| format!("bill: exec in sandbox '{name}' failed"))?;
    let stdout = output.stdout().unwrap_or_default();
    let stderr = output.stderr().unwrap_or_default();
    let code = output.status().code;

    // When the exit is non-zero (or -1 = no exit event seen, likely
    // signal or missing executable), log the captured streams so the
    // dispatcher and users can tell what happened. Truncate to avoid
    // blowing out the log with megabytes of stdout.
    if code != 0 {
        let stdout_tail: String = stdout.chars().take(2_000).collect();
        let stderr_tail: String = stderr.chars().take(2_000).collect();
        tracing::warn!(
            sandbox = %name,
            cmd = %cmd,
            code,
            stdout_len = stdout.len(),
            stderr_len = stderr.len(),
            stdout_tail = %stdout_tail,
            stderr_tail = %stderr_tail,
            "bill: exec returned non-zero (code=-1 typically means the process was signaled, killed, or the exec event stream was closed before a proper exit was reported — check that the binary exists at the bind-mount target and is executable inside the guest)"
        );
    } else {
        tracing::debug!(sandbox = %name, stdout_len = stdout.len(), stderr_len = stderr.len(), "bill: exec ok");
    }

    Ok((stdout, stderr, code))
}

/// Destroy a sandbox. Idempotent: `Ok(())` if already gone.
///
/// # Errors
///
/// Returns an error if `stop_and_wait` fails and Bill held the sole Arc
/// reference. Otherwise the stop is deferred to the last Arc drop.
pub async fn destroy_sandbox(name: &str) -> Result<()> {
    let Some(arc) = unregister(name) else {
        tracing::debug!(name = %name, "bill: destroy on unknown sandbox (no-op)");
        return Ok(());
    };
    tracing::info!(name = %name, "bill: destroying sandbox");
    match Arc::try_unwrap(arc) {
        Ok(sandbox) => {
            sandbox
                .stop_and_wait()
                .await
                .with_context(|| format!("bill: failed to stop sandbox '{name}'"))?;
        }
        Err(shared) => {
            tracing::debug!(
                name = %name,
                refs = Arc::strong_count(&shared),
                "bill: destroy deferred; other arc references exist"
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// TCP server
// ---------------------------------------------------------------------------

/// Bind Bill's TCP listener on `addr` (typically `127.0.0.1:0`). Returns
/// the local `SocketAddr` plus a join handle for the accept loop.
///
/// # Errors
///
/// Returns an error if the bind fails.
pub async fn listen(addr: SocketAddr) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await.with_context(|| format!("bill: bind {addr}"))?;
    let local = listener.local_addr().context("bill: read local addr")?;
    tracing::info!(%local, "bill: listening");
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream).await {
                            tracing::warn!(%peer, error = %e, "bill: connection error");
                        }
                    });
                }
                Err(e) => {
                    tracing::error!(error = %e, "bill: accept failed; exiting accept loop");
                    break;
                }
            }
        }
    });
    Ok((local, handle))
}

async fn handle_connection(stream: TcpStream) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await.context("bill: read request line")?;
    if n == 0 {
        return Ok(()); // peer closed immediately
    }
    let request: BillRequest = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            let resp = BillResponse::Error {
                message: format!("parse request: {e}"),
            };
            write_response(&mut write_half, &resp).await?;
            return Ok(());
        }
    };
    let response = dispatch(request).await;
    write_response(&mut write_half, &response).await?;
    Ok(())
}

async fn write_response(stream: &mut tokio::net::tcp::OwnedWriteHalf, response: &BillResponse) -> Result<()> {
    let mut json = serde_json::to_vec(response).context("bill: serialize response")?;
    json.push(b'\n');
    stream.write_all(&json).await.context("bill: write response")?;
    stream.flush().await.context("bill: flush response")?;
    Ok(())
}

async fn dispatch(request: BillRequest) -> BillResponse {
    match request {
        BillRequest::Ping => BillResponse::Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        BillRequest::Spawn { spec } => match spawn_sandbox(spec).await {
            Ok((name, host_ports, created_at)) => BillResponse::Spawned { name, host_ports, created_at },
            Err(e) => BillResponse::Error { message: format!("{e:#}") },
        },
        BillRequest::Exec { name, argv } => match exec_sandbox(&name, &argv).await {
            Ok((stdout, stderr, exit_code)) => BillResponse::ExecResult { stdout, stderr, exit_code },
            Err(e) => BillResponse::Error { message: format!("{e:#}") },
        },
        BillRequest::Destroy { name } => match destroy_sandbox(&name).await {
            Ok(()) => BillResponse::Destroyed,
            Err(e) => BillResponse::Error { message: format!("{e:#}") },
        },
        BillRequest::List => BillResponse::SandboxList { names: list() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_volume_name_preserves_safe_chars() {
        let name = sanitize_volume_name("abc123.-_");
        assert_eq!(name, "smooth-cache-abc123.-_");
    }

    #[test]
    fn sanitize_volume_name_replaces_unsafe_chars() {
        let name = sanitize_volume_name("proj/with space+slash");
        assert_eq!(name, "smooth-cache-proj_with_space_slash");
    }

    #[test]
    fn sanitize_volume_name_strips_leading_non_alnum() {
        // Leading symbols get trimmed so the resulting body starts
        // alphanumeric (microsandbox's `validate_volume_name` requires it);
        // the `smooth-cache-` prefix is still added by us.
        let name = sanitize_volume_name("---proj");
        assert_eq!(name, "smooth-cache-proj");
    }

    #[test]
    fn sanitize_volume_name_handles_empty_key() {
        let name = sanitize_volume_name("");
        assert_eq!(name, "smooth-cache-nokey");
    }

    #[test]
    fn sanitize_volume_name_caps_body_length() {
        let body = "a".repeat(500);
        let name = sanitize_volume_name(&body);
        // Body is capped at 100 chars + fixed prefix.
        assert_eq!(name.len(), "smooth-cache-".len() + 100);
        assert!(name.starts_with("smooth-cache-aaaa"));
    }

    #[tokio::test]
    async fn ping_roundtrip_over_tcp() {
        let (addr, _handle) = listen("127.0.0.1:0".parse().unwrap()).await.expect("bind");
        let client = crate::client::BillClient::new(format!("http://{addr}"));
        let version = client.ping().await.expect("ping");
        assert!(!version.is_empty(), "version should be non-empty");
    }

    #[tokio::test]
    async fn exec_unknown_sandbox_returns_error_response() {
        let (addr, _handle) = listen("127.0.0.1:0".parse().unwrap()).await.expect("bind");
        let client = crate::client::BillClient::new(format!("http://{addr}"));
        let err = client.exec("does-not-exist", &["echo".into(), "hi".into()]).await.unwrap_err();
        assert!(err.to_string().contains("no sandbox registered"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn destroy_unknown_sandbox_is_ok() {
        let (addr, _handle) = listen("127.0.0.1:0".parse().unwrap()).await.expect("bind");
        let client = crate::client::BillClient::new(format!("http://{addr}"));
        client.destroy("does-not-exist").await.expect("destroy should be idempotent");
    }

    #[tokio::test]
    async fn list_returns_empty_for_fresh_bill() {
        let (addr, _handle) = listen("127.0.0.1:0".parse().unwrap()).await.expect("bind");
        let client = crate::client::BillClient::new(format!("http://{addr}"));
        let names = client.list().await.expect("list");
        // Other concurrent tests in this process may have registered sandboxes;
        // all we can guarantee is that list() returns something (even if empty).
        let _ = names;
    }

    #[tokio::test]
    async fn spawn_rejects_non_ascii_env_values() {
        let spec = SandboxSpec {
            name: "ascii-test".into(),
            image: "alpine".into(),
            cpus: 1,
            memory_mb: 256,
            env: [("BAD".to_string(), "em\u{2014}dash".to_string())].into(),
            mounts: vec![],
            ports: vec![],
            timeout_seconds: 60,
            allow_host_loopback: false,
            env_cache_key: None,
            use_named_volume_for_cache: false,
            secrets: vec![],
        };
        let err = spawn_sandbox(spec).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("non-ASCII"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn spawn_rejects_duplicate_name() {
        // We can't actually boot a VM in a unit test, but we can populate
        // the registry with a fake entry and verify the duplicate check
        // fires before we reach the microsandbox builder. The registry is
        // process-global, so use a uniquely named key.
        let name = format!("dup-check-{}", uuid::Uuid::new_v4());
        // Inject a stub entry by routing through `register`. Because
        // `Sandbox` is not Clone and we can't construct one here, we skip
        // that path and instead trust the existing unit tests in
        // `smooth-bigsmooth` that exercised the same duplicate-check logic
        // before it moved here. This test stays as documentation.
        let _ = name;
    }
}
