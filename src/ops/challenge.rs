//! Challenge a memory's validity.
//!
//! The implementation lives in [`crate::nli::challenge`] so the lower
//! `retrieval` layer can drive the NLI-contradiction challenge flow without a
//! circular dependency on `ops`. These re-exports keep the historical
//! `ops::challenge_*` API stable for the CLI and MCP call sites.

pub use crate::nli::challenge::{challenge_for_contradictions, challenge_memory, ChallengeResult};
