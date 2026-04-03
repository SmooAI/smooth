mod negotiate;
mod policy;
mod server;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "smooth-wonk", about = "In-VM access control authority — the single source of truth for policy")]
struct Args {
    /// Policy TOML file path
    #[arg(long, default_value = "/etc/smooth/policy.toml")]
    policy: String,

    /// Address to listen on
    #[arg(long, default_value = "127.0.0.1:8400")]
    listen: String,

    /// Big Smooth leader URL (for access negotiation)
    #[arg(long, env = "SMOOTH_LEADER_URL")]
    leader_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("smooth_wonk=info".parse()?))
        .init();

    let args = Args::parse();

    tracing::info!(policy = %args.policy, listen = %args.listen, "Wonk starting");

    // Load policy and start hot-reload watcher
    let policy_holder = policy::PolicyHolder::load_and_watch(&args.policy)?;

    // Determine leader URL from policy or CLI arg
    let leader_url = args.leader_url.unwrap_or_else(|| {
        let p = policy_holder.load();
        p.auth.leader_url.clone()
    });

    let negotiator = negotiate::Negotiator::new(&leader_url, policy_holder.clone());

    server::run_server(&args.listen, policy_holder, negotiator).await
}
