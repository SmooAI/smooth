mod audit;
mod proxy;
mod wonk;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "smooth-goalie", about = "In-VM HTTP forward proxy — delegates to Wonk")]
struct Args {
    /// Address to listen on
    #[arg(long, default_value = "127.0.0.1:8480")]
    listen: String,

    /// Wonk API endpoint
    #[arg(long, default_value = "http://127.0.0.1:8400")]
    wonk: String,

    /// Audit log file path (JSON-lines)
    #[arg(long, default_value = "/var/smooth/audit/goalie.jsonl")]
    audit_log: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("smooth_goalie=info".parse()?))
        .init();

    let args = Args::parse();

    tracing::info!(listen = %args.listen, wonk = %args.wonk, "Goalie starting");

    let audit_logger = audit::AuditLogger::new(&args.audit_log)?;
    let wonk_client = wonk::WonkClient::new(&args.wonk);

    proxy::run_proxy(&args.listen, wonk_client, audit_logger).await
}
