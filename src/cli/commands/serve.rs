//! MCP server entry point (stub for M6).

use crate::cli::output::OutputFormatter;
use anyhow::Result;
use std::path::Path;

/// Start the MCP server (stub).
pub fn run_serve(
    _dir: &Path,
    transport: &str,
    port: Option<u16>,
    formatter: &OutputFormatter,
) -> Result<()> {
    formatter.print_message("MCP server will be available in a future release.");
    formatter.print_message(&format!("  Transport: {}", transport));
    if let Some(port) = port {
        formatter.print_message(&format!("  Port: {}", port));
    }
    Ok(())
}
