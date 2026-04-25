//! Diag harness for pearl th-461ab9.
//!
//! Sends a `Spawn` request to a running `bootstrap-bill` and reports how
//! long it took. Used to bisect which `SandboxSpec` feature triggers the
//! known hang on macOS HVF.
//!
//! Usage:
//!   cargo run --release -p smooai-smooth-bootstrap-bill --example bisect_spawn -- \
//!     --bill 127.0.0.1:4444 --variant baseline --timeout 120
//!
//! Variants (cumulative):
//!   baseline — alpine + ["echo","hello"], NO mounts/secrets/network/ports
//!   mounts   — baseline + 4 bind-mounts (runner-bin + workspace + .smooth + policy)
//!   secrets  — baseline + 1 SecretSpec (LLM gateway placeholder)
//!   network  — baseline + allow_host_loopback=true (NetworkPolicy::allow_all)
//!   ports    — baseline + 10 port forwards (host_port=0)
//!   secrets-network — baseline + secrets + network (strong suspect combo)
//!   all      — full real spec the orchestrator builds

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;

use smooth_bootstrap_bill::client::BillClient;
use smooth_bootstrap_bill::protocol::{BindMountSpec, PortMapping, SandboxSpec, SecretSpec};

#[derive(Parser, Debug)]
struct Args {
    /// Bill TCP address (host:port).
    #[arg(long, default_value = "127.0.0.1:4444")]
    bill: String,

    /// Which feature variant to send.
    #[arg(long)]
    variant: String,

    /// Hard wall-clock timeout (seconds) for the Spawn call. After this
    /// we give up and report HUNG. The harness intentionally does NOT
    /// destroy the sandbox afterwards — keeping that to the operator
    /// to avoid mutating Bill's registry while another bisect run is
    /// in flight.
    #[arg(long, default_value_t = 90)]
    timeout: u64,

    /// Image to use. Default `alpine` for fast iteration.
    /// Use `ghcr.io/smooai/smooth-operator:latest` for the production image.
    #[arg(long, default_value = "alpine")]
    image: String,

    /// Optional override for the sandbox name. Default is auto-generated.
    #[arg(long)]
    name: Option<String>,

    /// Repeat the spawn N times sequentially. Each iteration uses a unique
    /// generated name. Use to surface state-accumulation hangs.
    #[arg(long, default_value_t = 1)]
    repeat: usize,

    /// Concurrent spawns per iteration. Each iteration fires `concurrent`
    /// Spawn requests in parallel and waits for all. Use to surface
    /// parallel-state-pressure hangs.
    #[arg(long, default_value_t = 1)]
    concurrent: usize,

    /// Skip the per-spawn destroy. Without it, every successful spawn is
    /// destroyed before the next iteration. With it, sandboxes accumulate
    /// in Bill's registry and microsandbox state.
    #[arg(long, default_value_t = false)]
    no_destroy: bool,

    /// Skip the per-spawn exec probe (faster iterations).
    #[arg(long, default_value_t = false)]
    no_exec: bool,
}

fn build_spec(variant: &str, image: &str, name: &str) -> SandboxSpec {
    let mut spec = SandboxSpec {
        name: name.to_string(),
        image: image.to_string(),
        cpus: 1,
        memory_mb: 512,
        env: HashMap::new(),
        mounts: vec![],
        ports: vec![],
        timeout_seconds: 60,
        allow_host_loopback: false,
        env_cache_key: None,
        use_named_volume_for_cache: false,
        secrets: vec![],
    };

    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp"));
    let smooth_home = home.join(".smooth");

    let runner_bin_dir = smooth_home.join("operator-bin");
    let _ = std::fs::create_dir_all(&runner_bin_dir);
    let policy_dir = std::env::temp_dir().join("smooth-bisect-policy");
    let _ = std::fs::create_dir_all(&policy_dir);
    let workspace = std::env::temp_dir().join("smooth-bisect-workspace");
    let _ = std::fs::create_dir_all(&workspace);

    let mounts = vec![
        BindMountSpec {
            host_path: runner_bin_dir.to_string_lossy().to_string(),
            guest_path: "/opt/smooth/bin".into(),
            readonly: true,
        },
        BindMountSpec {
            host_path: workspace.to_string_lossy().to_string(),
            guest_path: "/workspace".into(),
            readonly: false,
        },
        BindMountSpec {
            host_path: smooth_home.to_string_lossy().to_string(),
            guest_path: "/root/.smooth".into(),
            readonly: false,
        },
        BindMountSpec {
            host_path: policy_dir.to_string_lossy().to_string(),
            guest_path: "/opt/smooth/policy".into(),
            readonly: true,
        },
    ];

    let secrets = vec![SecretSpec {
        env_var: "SMOOTH_API_KEY".into(),
        value: "fake-test-value-not-real".into(),
        placeholder: "SMOOTH_PLACEHOLDER_API_KEY_NOT_SUBSTITUTED".into(),
        allowed_hosts: vec!["llm.smoo.ai".into()],
    }];

    let ports: Vec<PortMapping> = [3000u16, 3001, 4000, 4200, 5000, 5173, 8000, 8080, 8888, 9090]
        .into_iter()
        .map(|p| PortMapping {
            host_port: 0,
            guest_port: p,
            bind_all: false,
        })
        .collect();

    match variant {
        "baseline" => {}
        "mounts" => {
            spec.mounts = mounts;
        }
        "secrets" => {
            spec.secrets = secrets;
        }
        "network" => {
            spec.allow_host_loopback = true;
        }
        "ports" => {
            spec.ports = ports;
        }
        "secrets-network" => {
            spec.secrets = secrets;
            spec.allow_host_loopback = true;
        }
        "mounts-network" => {
            spec.mounts = mounts;
            spec.allow_host_loopback = true;
        }
        "all" => {
            spec.mounts = mounts;
            spec.secrets = secrets;
            spec.allow_host_loopback = true;
            spec.ports = ports;
            spec.env.insert("SMOOTH_API_URL".into(), "https://llm.smoo.ai/v1".into());
            spec.env.insert("SMOOTH_WORKSPACE".into(), "/workspace".into());
            spec.env.insert("SMOOTH_HOME".into(), "/root/.smooth".into());
        }
        "all-volume-cache" => {
            spec.mounts = mounts;
            spec.secrets = secrets;
            spec.allow_host_loopback = true;
            spec.ports = ports;
            spec.env_cache_key = Some("bisect-named-vol-key".into());
            spec.use_named_volume_for_cache = true;
            spec.env.insert("SMOOTH_API_URL".into(), "https://llm.smoo.ai/v1".into());
        }
        "all-bind-cache" => {
            spec.mounts = mounts;
            spec.secrets = secrets;
            spec.allow_host_loopback = true;
            spec.ports = ports;
            spec.env_cache_key = Some("bisect-bind-key".into());
            spec.use_named_volume_for_cache = false;
            spec.env.insert("SMOOTH_API_URL".into(), "https://llm.smoo.ai/v1".into());
        }
        other => panic!("unknown variant: {other}"),
    }

    spec
}

fn unique_name(variant: &str, ordinal: usize) -> String {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let suffix = format!("{:012x}", (now_ns ^ (pid << 32) ^ (ordinal as u128)) & 0xFFFF_FFFF_FFFF);
    format!("bisect-{variant}-{suffix}")
}

#[derive(Debug)]
enum AttemptOutcome {
    Spawned {
        spawn_ms: u128,
        exec_code: Option<i32>,
        exec_ms: Option<u128>,
    },
    Error {
        spawn_ms: u128,
        err: String,
    },
    Hung,
}

async fn one_attempt(client: BillClient, spec: SandboxSpec, args: &Args, ordinal: usize) -> AttemptOutcome {
    let name = spec.name.clone();
    let timeout_dur = Duration::from_secs(args.timeout);
    let start = Instant::now();
    let result = tokio::time::timeout(timeout_dur, client.spawn(spec)).await;
    let spawn_ms = start.elapsed().as_millis();
    match result {
        Ok(Ok((nm, _ports, _created_at))) => {
            let (exec_code, exec_ms) = if args.no_exec {
                (None, None)
            } else {
                let exec_start = Instant::now();
                let probe = "echo ok";
                match tokio::time::timeout(Duration::from_secs(15), client.exec(&nm, &["/bin/sh".into(), "-c".into(), probe.into()])).await {
                    Ok(Ok((_, _, code))) => (Some(code), Some(exec_start.elapsed().as_millis())),
                    Ok(Err(_)) => (Some(-2), Some(exec_start.elapsed().as_millis())),
                    Err(_) => (Some(-3), Some(exec_start.elapsed().as_millis())),
                }
            };
            if !args.no_destroy {
                let _ = tokio::time::timeout(Duration::from_secs(20), client.destroy(&nm)).await;
            }
            eprintln!("[#{ordinal:03}] spawned name={nm} spawn_ms={spawn_ms} exec_code={exec_code:?} exec_ms={exec_ms:?}");
            AttemptOutcome::Spawned { spawn_ms, exec_code, exec_ms }
        }
        Ok(Err(e)) => {
            let err = format!("{e:#}");
            eprintln!("[#{ordinal:03}] error name={name} spawn_ms={spawn_ms} err={err}");
            AttemptOutcome::Error { spawn_ms, err }
        }
        Err(_) => {
            eprintln!("[#{ordinal:03}] HUNG name={name} elapsed_ms>{}", spawn_ms);
            AttemptOutcome::Hung
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let args = Args::parse();

    eprintln!("=== bisect_spawn ===");
    eprintln!("bill={}", args.bill);
    eprintln!("variant={}", args.variant);
    eprintln!("image={}", args.image);
    eprintln!(
        "repeat={} concurrent={} no_destroy={} no_exec={}",
        args.repeat, args.concurrent, args.no_destroy, args.no_exec
    );
    eprintln!("timeout={}s", args.timeout);

    let client = BillClient::new(format!("http://{}", args.bill));

    // Liveness probe.
    let pong_start = Instant::now();
    match client.ping().await {
        Ok(v) => eprintln!("ping={} ({}ms)", v, pong_start.elapsed().as_millis()),
        Err(e) => {
            eprintln!("FATAL: bill not reachable: {e:#}");
            std::process::exit(2);
        }
    }

    let mut spawned = 0usize;
    let mut errored = 0usize;
    let mut hung = 0usize;
    let mut spawn_ms_samples: Vec<u128> = Vec::with_capacity(args.repeat * args.concurrent);
    let stress_start = Instant::now();

    for iter in 0..args.repeat {
        // Build a batch of N concurrent attempts.
        let mut handles = Vec::with_capacity(args.concurrent);
        for c in 0..args.concurrent {
            let ordinal = iter * args.concurrent + c;
            let name = args.name.clone().unwrap_or_else(|| unique_name(&args.variant, ordinal));
            let spec = build_spec(&args.variant, &args.image, &name);
            let client = client.clone();
            let args_clone = Args {
                bill: args.bill.clone(),
                variant: args.variant.clone(),
                timeout: args.timeout,
                image: args.image.clone(),
                name: None,
                repeat: 1,
                concurrent: 1,
                no_destroy: args.no_destroy,
                no_exec: args.no_exec,
            };
            handles.push(tokio::spawn(async move { one_attempt(client, spec, &args_clone, ordinal).await }));
        }
        for h in handles {
            match h.await.expect("join") {
                AttemptOutcome::Spawned { spawn_ms, .. } => {
                    spawned += 1;
                    spawn_ms_samples.push(spawn_ms);
                }
                AttemptOutcome::Error { .. } => errored += 1,
                AttemptOutcome::Hung => {
                    hung += 1;
                    eprintln!("STOPPING: hung detected at iter={iter}");
                }
            }
        }
        if hung > 0 {
            break;
        }
    }

    let total_ms = stress_start.elapsed().as_millis();
    let n = spawn_ms_samples.len() as u128;
    let avg = if n > 0 { spawn_ms_samples.iter().sum::<u128>() / n } else { 0 };
    let max = spawn_ms_samples.iter().copied().max().unwrap_or(0);
    let min = spawn_ms_samples.iter().copied().min().unwrap_or(0);
    eprintln!("=== summary ===");
    eprintln!("spawned={spawned} errored={errored} hung={hung} total_ms={total_ms}");
    eprintln!("spawn_ms min={min} avg={avg} max={max} (n={n})");
    if hung > 0 {
        std::process::exit(124);
    }
    if errored > 0 {
        std::process::exit(3);
    }
    Ok(())
}
