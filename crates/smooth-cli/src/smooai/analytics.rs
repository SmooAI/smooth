//! `smoo analytics …` — scaffold stub; implementation lands in this PR
//! (MCP→CLI parity fan-out, EPIC pearls th-739bb1/th-b1f09c/th-a5d991).

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum Cmd {
    /// Placeholder — replaced by the implementing lane in this PR.
    #[command(hide = true)]
    Todo,
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Todo => anyhow::bail!("not implemented yet"),
    }
}
