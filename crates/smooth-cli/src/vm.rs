//! `th vm` lifecycle for the long-lived single-VM sandbox.
//!
//! Pearl th-893801 Phase 2 iter-4g. Manages the container
//! built by `scripts/build-smooth-vm-image.sh`:
//!
//! * `th vm up` — start the container if it isn't running.
//!   Bind-mounts host Docker socket, host-stub UDS, the
//!   user's workspace; attaches a named volume for `/root`
//!   so mise / pearl / SSH state survives restarts.
//! * `th vm down` — stop the container but leave the volume.
//! * `th vm status` — report container + volume state.
//! * `th vm prune` — stop the container AND remove the
//!   volume. Confirmation-gated unless `--yes` is passed.
//! * `th vm shell` — exec an interactive shell inside.
//!
//! Idempotent on every action: `up` is a no-op if the
//! container is already running, `down` is a no-op if it's
//! stopped, `prune` is fine on a missing volume.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use tokio::process::Command;

/// Default container name. Single VM per host — `th vm` is
/// not currently multi-tenant; if the user wants two they
/// run two checkouts in different `SMOOTH_VM_NAME` env vars.
const DEFAULT_CONTAINER_NAME: &str = "smooth-vm";

/// Default volume name backing `/root`.
const DEFAULT_VOLUME_NAME: &str = "smooth-vm-root";

/// Default image tag.
const DEFAULT_IMAGE: &str = "ghcr.io/smooai/smooth-vm:latest";

/// Subcommands under `th vm`.
#[derive(Subcommand, Debug, Clone)]
pub enum VmCommands {
    /// Start the long-lived sandbox VM. Idempotent — if the
    /// container is already running, prints status and exits 0.
    Up {
        /// Image tag (override
        /// `SMOOTH_VM_IMAGE`). Default
        /// `ghcr.io/smooai/smooth-vm:latest`.
        #[arg(long)]
        image: Option<String>,
        /// Workspace dir to bind-mount as `/workspace`.
        /// Defaults to the current working directory.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Skip the Docker socket bind-mount. Use when the
        /// agent doesn't need to drive Docker on the host.
        #[arg(long)]
        no_docker: bool,
    },
    /// Stop the container without touching the persistent
    /// volume.
    Down,
    /// Show container + volume state.
    Status,
    /// Stop the container AND remove the persistent volume.
    /// `--yes` skips the confirmation prompt.
    Prune {
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Exec an interactive shell in the running VM (`th vm up`
    /// it first if not running).
    Shell {
        /// Shell to exec. Defaults to `bash`.
        #[arg(long, default_value = "bash")]
        shell: String,
    },
}

/// Top-level dispatcher invoked from `main.rs`.
pub async fn run(cmd: VmCommands) -> Result<()> {
    match cmd {
        VmCommands::Up { image, workspace, no_docker } => up(image, workspace, no_docker).await,
        VmCommands::Down => down().await,
        VmCommands::Status => status().await,
        VmCommands::Prune { yes } => prune(yes).await,
        VmCommands::Shell { shell } => shell_into(shell).await,
    }
}

// ── env helpers ─────────────────────────────────────────────

fn container_name() -> String {
    std::env::var("SMOOTH_VM_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CONTAINER_NAME.into())
}

fn volume_name() -> String {
    std::env::var("SMOOTH_VM_VOLUME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_VOLUME_NAME.into())
}

fn image_for(explicit: Option<String>) -> String {
    explicit
        .or_else(|| std::env::var("SMOOTH_VM_IMAGE").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| DEFAULT_IMAGE.into())
}

fn host_stub_socket_dir() -> PathBuf {
    std::env::var("SMOOTH_HOST_STUB_SOCKET_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_next::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".smooth/host-stub"))
}

// ── docker helpers ──────────────────────────────────────────

/// Run a docker command, returning its captured output.
async fn run_docker(args: &[&str]) -> Result<DockerOutput> {
    let output = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("failed to spawn docker {args:?}"))?;
    Ok(DockerOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

struct DockerOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Inspect a container; returns Some(state) when it exists,
/// None when `docker inspect` fails (i.e. no such container).
pub async fn container_state(name: &str) -> Result<Option<ContainerState>> {
    let out = run_docker(&["inspect", "--format", "{{.State.Status}}|{{.Image}}|{{.State.StartedAt}}", name]).await?;
    if !out.success {
        return Ok(None);
    }
    let line = out.stdout.trim();
    let mut parts = line.splitn(3, '|');
    let status = parts.next().unwrap_or("").to_string();
    let image = parts.next().unwrap_or("").to_string();
    let started_at = parts.next().unwrap_or("").to_string();
    Ok(Some(ContainerState { status, image, started_at }))
}

/// State summary returned by `container_state`.
#[derive(Debug, Clone)]
pub struct ContainerState {
    pub status: String,
    pub image: String,
    pub started_at: String,
}

impl ContainerState {
    /// True when the container's reported status is "running".
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
}

/// Inspect a volume; returns true when it exists.
async fn volume_exists(name: &str) -> Result<bool> {
    Ok(run_docker(&["volume", "inspect", name]).await?.success)
}

// ── subcommand impls ────────────────────────────────────────

async fn up(image: Option<String>, workspace: Option<PathBuf>, no_docker: bool) -> Result<()> {
    let name = container_name();
    let volume = volume_name();
    let image = image_for(image);
    let workspace = match workspace {
        Some(p) => p,
        None => std::env::current_dir().context("resolve current dir for workspace")?,
    };

    if let Some(state) = container_state(&name).await? {
        if state.is_running() {
            println!("smooth-vm '{name}' is already running ({})", state.image);
            return Ok(());
        }
        // Stopped container with this name — remove it before
        // a fresh `docker run`. The volume survives.
        let out = run_docker(&["rm", &name]).await?;
        if !out.success {
            bail!("failed to remove stopped container '{name}': {}", out.stderr.trim());
        }
    }

    // Make the host-stub socket dir if it doesn't exist; the
    // smooth-host-stub binary creates the socket inside.
    let host_stub_dir = host_stub_socket_dir();
    std::fs::create_dir_all(&host_stub_dir).with_context(|| format!("create host-stub dir at {}", host_stub_dir.display()))?;

    let workspace_str = workspace.canonicalize().unwrap_or(workspace).to_string_lossy().into_owned();
    let host_stub_str = host_stub_dir.to_string_lossy().into_owned();

    let mut args: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        name.clone(),
        "--restart".into(),
        "unless-stopped".into(),
        "-v".into(),
        format!("{volume}:/root"),
        "-v".into(),
        format!("{workspace_str}:/workspace"),
        "-v".into(),
        format!("{host_stub_str}:/run/smooth"),
        "-e".into(),
        "SMOOTH_SINGLE_PROCESS=1".into(),
        "-e".into(),
        "SMOOTH_HOST_STUB_SOCKET=/run/smooth/host.sock".into(),
    ];

    if !no_docker {
        match smooth_host_stub::docker_socket::detect() {
            Ok(detected) => {
                args.push("-v".into());
                args.push(format!("{}:/var/run/docker.sock", detected.path.display()));
                println!("smooth-vm: bind-mounting Docker socket from {}", detected.runtime.as_str());
            }
            Err(e) => {
                println!("smooth-vm: skipping Docker socket bind-mount ({e})");
            }
        }
    }

    args.push(image.clone());

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_docker(&arg_refs).await?;
    if !out.success {
        bail!("docker run failed: {}", out.stderr.trim());
    }
    println!("smooth-vm: started '{name}' (image: {image})");
    println!("  workspace: {}", workspace_str);
    println!("  volume:    {} (persists across `th vm down`)", volume);
    println!("  host-stub: {} (host stub UDS lands here)", host_stub_str);
    Ok(())
}

async fn down() -> Result<()> {
    let name = container_name();
    let Some(state) = container_state(&name).await? else {
        println!("smooth-vm '{name}' is not present (no-op)");
        return Ok(());
    };
    if !state.is_running() {
        println!("smooth-vm '{name}' is not running (status: {})", state.status);
        return Ok(());
    }
    let out = run_docker(&["stop", &name]).await?;
    if !out.success {
        bail!("docker stop failed: {}", out.stderr.trim());
    }
    println!("smooth-vm: stopped '{name}' (volume '{}' retained)", volume_name());
    Ok(())
}

async fn status() -> Result<()> {
    let name = container_name();
    let volume = volume_name();
    match container_state(&name).await? {
        Some(state) => {
            println!("smooth-vm '{name}':");
            println!("  status:    {}", state.status);
            println!("  image:     {}", state.image);
            println!("  started:   {}", state.started_at);
        }
        None => {
            println!("smooth-vm '{name}': not present (run `th vm up` to start)");
        }
    }
    if volume_exists(&volume).await? {
        println!("  volume:    {volume} (present)");
    } else {
        println!("  volume:    {volume} (none — fresh state on next `th vm up`)");
    }
    Ok(())
}

async fn prune(yes: bool) -> Result<()> {
    let name = container_name();
    let volume = volume_name();
    if !yes {
        eprintln!("This will remove container '{name}' AND volume '{volume}'.");
        eprintln!("All in-VM state (mise toolchains, pearl DB, SSH config, gh/aws/gcloud auth) will be lost.");
        eprintln!("Re-run with --yes to confirm.");
        return Err(anyhow!("aborted by user (no --yes)"));
    }
    if let Some(state) = container_state(&name).await? {
        if state.is_running() {
            let _ = run_docker(&["stop", &name]).await?;
        }
        let _ = run_docker(&["rm", "-f", &name]).await?;
        println!("smooth-vm: removed container '{name}'");
    }
    if volume_exists(&volume).await? {
        let out = run_docker(&["volume", "rm", &volume]).await?;
        if !out.success {
            bail!("docker volume rm failed: {}", out.stderr.trim());
        }
        println!("smooth-vm: removed volume '{volume}'");
    }
    Ok(())
}

async fn shell_into(shell: String) -> Result<()> {
    let name = container_name();
    let state = match container_state(&name).await? {
        Some(s) => s,
        None => {
            // Auto-up so `th vm shell` works on a fresh box.
            println!("smooth-vm: container missing — running `th vm up` first");
            up(None, None, false).await?;
            container_state(&name).await?.context("container should exist after up")?
        }
    };
    if !state.is_running() {
        bail!(
            "smooth-vm '{name}' exists but is not running (status: {}); run `th vm up` or `th vm prune` first",
            state.status
        );
    }
    // Exec replaces this process with `docker exec -it`. Using
    // a synchronous Command here so stdin/stdout pass through
    // to the user's terminal.
    let status = std::process::Command::new("docker")
        .args(["exec", "-it", &name, &shell])
        .status()
        .with_context(|| format!("failed to spawn docker exec -it {name} {shell}"))?;
    if !status.success() {
        bail!("shell exited with status {status}");
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env-touching tests share a process-global env; serialize them
    // so parallel test runs don't race on SMOOTH_VM_* vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn container_name_honors_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SMOOTH_VM_NAME", "alt-name");
        assert_eq!(container_name(), "alt-name");
        std::env::remove_var("SMOOTH_VM_NAME");
        assert_eq!(container_name(), DEFAULT_CONTAINER_NAME);
    }

    #[test]
    fn volume_name_honors_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SMOOTH_VM_VOLUME", "alt-volume");
        assert_eq!(volume_name(), "alt-volume");
        std::env::remove_var("SMOOTH_VM_VOLUME");
        assert_eq!(volume_name(), DEFAULT_VOLUME_NAME);
    }

    #[test]
    fn image_for_explicit_beats_env_beats_default() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SMOOTH_VM_IMAGE", "env-image");
        assert_eq!(image_for(Some("explicit".into())), "explicit");
        assert_eq!(image_for(None), "env-image");
        std::env::remove_var("SMOOTH_VM_IMAGE");
        assert_eq!(image_for(None), DEFAULT_IMAGE);
    }

    #[test]
    fn empty_env_value_falls_back_to_default() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SMOOTH_VM_NAME", "   ");
        assert_eq!(container_name(), DEFAULT_CONTAINER_NAME);
        std::env::remove_var("SMOOTH_VM_NAME");
    }

    #[test]
    fn container_state_is_running_only_for_running_status() {
        let running = ContainerState {
            status: "running".into(),
            image: "img".into(),
            started_at: "ts".into(),
        };
        let stopped = ContainerState {
            status: "exited".into(),
            image: "img".into(),
            started_at: "ts".into(),
        };
        assert!(running.is_running());
        assert!(!stopped.is_running());
    }
}
