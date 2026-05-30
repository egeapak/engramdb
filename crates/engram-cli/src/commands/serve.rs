//! Start the MCP server.

use crate::output::OutputFormatter;
use anyhow::Result;
use engram_mcp::server;
use engramdb::types::EmbeddingBackend;
use std::path::Path;

/// Start the MCP server with the specified transport.
pub async fn run_serve(
    dir: &Path,
    transport: &str,
    port: Option<u16>,
    embedding_backend: Option<EmbeddingBackend>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let dir = dir.to_path_buf();

    match transport {
        "stdio" => {
            server::run_stdio(dir, embedding_backend).await?;
        }
        "sse" => {
            let port = port.unwrap_or(3100);
            formatter.print_message(&format!(
                "Starting EngramDB MCP server (SSE) on port {}...",
                port
            ));
            server::run_sse(dir, port, embedding_backend).await?;
        }
        other => {
            anyhow::bail!("Unknown transport: {}. Use 'stdio' or 'sse'.", other);
        }
    }

    Ok(())
}
