//! Command handler implementations.
//!
//! Each submodule implements a specific CLI command handler. Handlers receive
//! parsed arguments, interact with the storage layer, and format output using
//! the provided OutputFormatter.

pub mod add;
pub mod challenge;
pub mod completions;
pub mod compress;
pub mod delete;
pub mod doctor;
pub mod gc;
pub mod get;
pub mod hook;
pub mod init;
pub mod list;
pub mod migrate;
pub mod projects;
pub mod query;
pub mod reindex;
pub mod review;
pub mod rollback;
pub mod serve;
pub mod setup;
pub mod stats;
pub mod update;

pub use add::{run_add, AddParams};
pub use challenge::{run_challenge, ChallengeParams};
pub use completions::run_completions;
pub use compress::run_compress;
pub use delete::run_delete;
pub use doctor::run_doctor;
pub use gc::run_gc;
pub use get::run_get;
pub use hook::{run_hook_pre_tool_use, run_hook_session_start};
pub use init::run_init;
pub use list::run_list;
pub use migrate::run_migrate;
pub use projects::run_projects;
pub use query::{run_query, QueryParams};
pub use reindex::run_reindex;
pub use review::run_review;
pub use rollback::run_rollback;
pub use serve::run_serve;
pub use setup::run_setup;
pub use stats::run_stats;
pub use update::{run_update, UpdateParams};
