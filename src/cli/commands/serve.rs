//! Start the MCP server.

use crate::cli::output::OutputFormatter;
use crate::mcp::server;
use anyhow::Result;
use std::path::Path;

/// Start the MCP server with the specified transport.
pub async fn run_serve(
    dir: &Path,
    transport: &str,
    port: Option<u16>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let dir = dir.to_path_buf();

    let rt = tokio::runtime::Runtime::new()?;

    match transport {
        "stdio" => {
            rt.block_on(server::run_stdio(dir))?;
        }
        "sse" => {
            let port = port.unwrap_or(3100);
            formatter.print_message(&format!(
                "Starting EngramDB MCP server (SSE) on port {}...",
                port
            ));
            rt.block_on(server::run_sse(dir, port))?;
        }
        other => {
            anyhow::bail!("Unknown transport: {}. Use 'stdio' or 'sse'.", other);
        }
    }

    Ok(())
}
