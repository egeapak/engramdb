use anyhow::Result;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> Result<ExitCode> {
    // A hook invocation clap cannot parse exits 0 here (see `parse_cli`).
    let cli = match engram_cli::parse_cli(std::env::args_os()) {
        Ok(cli) => cli,
        Err(code) => return Ok(code),
    };
    engram_cli::run(cli).await?;
    Ok(ExitCode::SUCCESS)
}
