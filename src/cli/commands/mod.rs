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
pub mod gc;
pub mod get;
pub mod init;
pub mod list;
pub mod reindex;
pub mod retrieve;
pub mod review;
pub mod search;
pub mod serve;
pub mod stats;
pub mod update;

pub use add::{run_add, AddParams};
pub use challenge::{run_challenge, ChallengeParams};
pub use completions::run_completions;
pub use compress::run_compress;
pub use delete::run_delete;
pub use gc::run_gc;
pub use get::run_get;
pub use init::run_init;
pub use list::run_list;
pub use reindex::run_reindex;
pub use retrieve::{run_retrieve, RetrieveParams};
pub use review::run_review;
pub use search::{run_search, SearchParams};
pub use serve::run_serve;
pub use stats::run_stats;
pub use update::{run_update, UpdateParams};
