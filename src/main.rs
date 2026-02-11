use anyhow::Result;
use clap::Parser;
use engramdb::cli::app::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    engramdb::cli::run(cli).await
}
