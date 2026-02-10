//! MCP (Model Context Protocol) server for EngramDB.
//!
//! Exposes EngramDB operations as MCP tools, resources, and prompts so that
//! coding agents can store, retrieve, challenge, and manage project memories
//! over the standard MCP protocol.

pub mod error;
pub mod server;

pub use server::EngramDbServer;
