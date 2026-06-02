use anyhow::Result;
use clap::Parser;
use engram_cli::app::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    engram_cli::run(cli).await
}
