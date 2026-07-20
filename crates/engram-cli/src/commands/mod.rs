//! Command handler implementations.
//!
//! Each submodule implements a specific CLI command handler. Handlers receive
//! parsed arguments, interact with the storage layer, and format output using
//! the provided OutputFormatter.

pub mod add;
pub mod challenge;
pub mod completions;
pub mod compress;
pub mod config;
pub mod daemon;
pub mod delete;
pub mod doctor;
pub mod gc;
pub mod get;
pub mod groups;
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
pub mod task;
pub mod update;
pub mod verify;

pub use add::{run_add, AddParams};
pub use challenge::{run_challenge, ChallengeParams};
pub use completions::run_completions;
pub use compress::run_compress;
pub use config::run_config;
pub use daemon::run_daemon_cmd;
pub use delete::run_delete;
pub use doctor::run_doctor;
pub use gc::run_gc;
pub use get::run_get;
pub use groups::run_groups;
pub use hook::{
    run_hook_post_tool_use, run_hook_pre_compact, run_hook_pre_tool_use, run_hook_session_end,
    run_hook_session_start, run_hook_user_prompt_submit,
};
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
pub use task::{run_task_complete, run_task_current};
pub use update::{run_update, UpdateParams};
pub use verify::run_verify;
