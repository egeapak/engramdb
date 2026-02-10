use clap::Parser;
use anyhow::Result;
use engramdb::cli::app::Cli;

fn main() -> Result<()> {
    let cli = Cli::parse();
    engramdb::cli::run(cli)
}
