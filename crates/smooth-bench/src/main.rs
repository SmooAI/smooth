//! `smooth-bench` — internal benchmark harness binary.
//!
//! Not shipped in the `th` CLI. Run via:
//!
//!     cargo run -p smooai-smooth-bench -- aider-polyglot --task grade-school
//!
//! or the top-level wrapper `scripts/bench.sh`.

use anyhow::Result;
use clap::{Parser, Subcommand};
use smooth_bench::{print_summary, run_aider_polyglot, BenchOpts, PolyglotLang};

#[derive(Parser)]
#[command(name = "smooth-bench", version, about = "Smooth benchmark harness (internal)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a single Aider Polyglot task.
    AiderPolyglot {
        /// Task name (e.g. `grade-school`, `leap`, `forth`).
        #[arg(long)]
        task: String,
        /// Language subset. Default: python.
        #[arg(long, default_value = "python")]
        lang: String,
        /// Budget limit in USD for the LLM calls. Default: $0.50.
        #[arg(long, default_value_t = 0.50)]
        budget: f64,
        /// Override the routing (passed through to Big Smooth).
        #[arg(long)]
        model: Option<String>,
        /// Big Smooth URL. Defaults to http://localhost:4400.
        #[arg(long, default_value = "http://localhost:4400")]
        url: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Commands::AiderPolyglot {
            task,
            lang,
            budget,
            model,
            url,
        } => {
            let lang_enum =
                PolyglotLang::from_name(&lang).ok_or_else(|| anyhow::anyhow!("unknown language: {lang} (try python, rust, go, javascript, java, cpp)"))?;
            let opts = BenchOpts {
                big_smooth_url: url,
                budget_usd: Some(budget),
                model,
            };
            println!("Running aider-polyglot/{}/{task} …", lang_enum.dataset_dir());
            let result = run_aider_polyglot(lang_enum, &task, &opts).await?;
            print_summary(&result);
            if result.solved {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
    }
}
