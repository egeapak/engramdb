//! MCP (Model Context Protocol) server for EngramDB.
//!
//! Exposes EngramDB operations as MCP tools, resources, and prompts so that
//! coding agents can store, retrieve, challenge, and manage project memories
//! over the standard MCP protocol.

pub mod error;
pub mod server;

pub use server::EngramDbServer;

// Test isolation: link `engram-test-support` so its `#[ctor]` redirects
// `ENGRAMDB_DATA_DIR` / `ENGRAMDB_CONFIG_DIR` to per-process temp dirs before
// any test runs. The in-crate `server::tests::global_*` tests build real
// `MemoryStore`s against the shared global store, so without this they would
// race on the *real* global data dir under nextest's process-per-test model.
// The explicit `arm()` reference keeps the linker from dead-stripping the
// constructor out of this crate's test binary.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn arm_test_isolation() {
    engram_test_support::arm();
}
