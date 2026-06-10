//! Scope proximity scoring for EngramDB
//!
//! This module calculates how relevant a memory is to the current context based on
//! physical (file paths) and logical (dot-notation tags) scope matching.
//!
//! # Components
//!
//! - [`physical`]: Physical scope matching using file paths and glob patterns
//! - [`logical`]: Logical scope hierarchy matching using dot-notation
//! - [`scope_proximity`]: Combines both physical and logical scores
//!
//! # Scoring System
//!
//! Physical scores range from 0.0 to 1.0 based on file path similarity.
//! Logical scores provide a bonus of 0.0 to 0.3 based on hierarchical tag matching.
//! The combined score is capped at 1.0 to maintain consistent score ranges.
//!
//! The combiner distinguishes three context cases (see [`scope_proximity`]):
//!
//! 1. **Path present** (with or without logical context): physical score plus
//!    logical bonus, capped at 1.0. Physical is the primary signal; logical is
//!    a small additive bonus on top.
//! 2. **Logical-only** (no path, logical context present): logical match
//!    quality becomes the primary signal. The score is
//!    `logical_only_floor + logical_bonus` (capped at 1.0) when the memory has
//!    any logical relationship to the query, `logical_only_floor` alone when
//!    the memory declares no logical scopes (neutral — unscoped memories are
//!    not penalized for a context they never claimed), and 0.0 when the
//!    memory's declared logical scopes are unrelated to the query (mirrors how
//!    non-matching physical scopes score 0.0 under a path query).
//! 3. **No context**: 0.0 raw score. The scorer treats absent context as a
//!    neutral 1.0 multiplier instead (see `scoring::composite`).
//!
//! # Design Decisions
//!
//! - Physical scope takes precedence (higher weight) over logical scope
//! - The "/" pattern matches all paths with a base score of 0.4
//! - When a path is given, a missing physical match results in 0.0 physical
//!   score (logical bonus still applies)
//! - A logical-only query must not collapse to the bare bonus (max 0.3): that
//!   would put every result below the default 0.45 relevance threshold
//! - Multiple memory scopes return the highest matching score

pub mod logical;
pub mod physical;

/// Calculates the combined proximity score between a memory's scopes and the current context.
///
/// This function combines physical and logical scope proximity:
/// - Physical score: based on file path matching with depth decay (0.0 to 1.0)
/// - Logical bonus: based on dot-notation scope hierarchy (0.0 to 0.3)
/// - Total score is capped at 1.0
///
/// When `current_path` is `None` but `current_logical` is non-empty
/// (logical-only context), the logical bonus is the *primary* signal rather
/// than an additive extra, scaled into a usable multiplier range:
/// `logical_only_floor + bonus` for related memories, `logical_only_floor`
/// for memories with no logical scopes, 0.0 for memories whose logical scopes
/// are unrelated to the query.
///
/// # Arguments
/// * `memory_physical` - Physical scope patterns from the memory (file paths/globs)
/// * `memory_logical` - Logical scope tags from the memory (dot-notation)
/// * `current_path` - Current file path (if any)
/// * `current_logical` - Current logical scope tags
/// * `depth_decay_base` - Exponential decay base for physical scope (e.g. 0.82)
/// * `depth_decay_floor` - Minimum physical scope score (e.g. 0.3)
/// * `logical_only_floor` - Neutral base score for logical-only context
///   (config `retrieval.scoring.scope_multiplier_floor`, default 0.5)
///
/// # Returns
/// Combined proximity score from 0.0 to 1.0 (for config-validated inputs)
pub fn scope_proximity(
    memory_physical: &[String],
    memory_logical: &[String],
    current_path: Option<&str>,
    current_logical: &[String],
    depth_decay_base: f64,
    depth_decay_floor: f64,
    logical_only_floor: f64,
) -> f64 {
    let logical_bonus = logical::proximity(memory_logical, current_logical);

    match current_path {
        // Path present: physical is the primary signal, logical adds a bonus.
        Some(path) => {
            let physical_score =
                physical::proximity(memory_physical, path, depth_decay_base, depth_decay_floor);
            (physical_score + logical_bonus).min(1.0)
        }
        // Logical-only context: logical match quality is the primary signal.
        None if !current_logical.is_empty() => {
            if memory_logical.is_empty() {
                // Unscoped memory: neutral floor, no bonus and no suppression.
                logical_only_floor
            } else if logical_bonus > 0.0 {
                // Related: floor plus the distance-decayed bonus, capped at 1.0.
                (logical_only_floor + logical_bonus).min(1.0)
            } else {
                // Declared logical scopes, none related: fully suppressed,
                // mirroring non-matching physical scopes under a path query.
                0.0
            }
        }
        // No scope context at all.
        None => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: f64 = 0.82;
    const FLOOR: f64 = 0.3;
    /// Mirrors the default `retrieval.scoring.scope_multiplier_floor`.
    const LOGICAL_FLOOR: f64 = 0.5;

    fn assert_approx(actual: f64, expected: f64, msg: &str) {
        assert!(
            (actual - expected).abs() < 0.01,
            "{}: expected {:.4}, got {:.4}",
            msg,
            expected,
            actual
        );
    }

    #[test]
    fn test_scope_proximity_combined() {
        let memory_physical = vec!["src/api/**".to_string()];
        let memory_logical = vec!["auth".to_string()];
        let current_path = Some("src/api/auth/handlers.rs");
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &memory_physical,
            &memory_logical,
            current_path,
            &current_logical,
            BASE,
            FLOOR,
            LOGICAL_FLOOR,
        );

        // Physical: 0.6724 (depth 2 from "src/api"), Logical: 0.2 (parent match)
        // Total: 0.8724
        assert_approx(score, 0.8724, "combined");
    }

    #[test]
    fn test_scope_proximity_capped_at_one() {
        let memory_physical = vec!["src/api/auth/handlers.rs".to_string()];
        let memory_logical = vec!["auth.oauth".to_string()];
        let current_path = Some("src/api/auth/handlers.rs");
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &memory_physical,
            &memory_logical,
            current_path,
            &current_logical,
            BASE,
            FLOOR,
            LOGICAL_FLOOR,
        );

        // Physical: 1.0 (exact), Logical: 0.3 (exact)
        // Total would be 1.3, but capped at 1.0
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_scope_proximity_no_current_path() {
        // Logical-only context: the bonus rides on the neutral floor instead
        // of standing alone (regression: a bare 0.2 multiplier dragged every
        // logical-only rank result below the 0.45 relevance threshold).
        let memory_physical = vec!["src/api/**".to_string()];
        let memory_logical = vec!["auth".to_string()];
        let current_path = None;
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &memory_physical,
            &memory_logical,
            current_path,
            &current_logical,
            BASE,
            FLOOR,
            LOGICAL_FLOOR,
        );

        // Logical: 0.2 (parent match) on top of the 0.5 floor → 0.7
        assert_eq!(score, 0.7);
    }

    #[test]
    fn test_scope_proximity_no_match() {
        let memory_physical = vec!["src/db/**".to_string()];
        let memory_logical = vec!["database".to_string()];
        let current_path = Some("src/api/auth/handlers.rs");
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &memory_physical,
            &memory_logical,
            current_path,
            &current_logical,
            BASE,
            FLOOR,
            LOGICAL_FLOOR,
        );

        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_scope_proximity_logical_only_exact_match() {
        // Exact logical match with no path: floor + max bonus.
        let memory_logical = vec!["auth.oauth".to_string()];
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &[],
            &memory_logical,
            None,
            &current_logical,
            BASE,
            FLOOR,
            LOGICAL_FLOOR,
        );

        // 0.5 floor + 0.3 exact bonus = 0.8
        assert_eq!(score, 0.8);
    }

    #[test]
    fn test_scope_proximity_logical_only_mismatch_suppressed() {
        // Memory declares logical scopes but none relate to the query:
        // fully suppressed, like a non-matching physical scope under a path.
        let memory_logical = vec!["database.postgres".to_string()];
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &[],
            &memory_logical,
            None,
            &current_logical,
            BASE,
            FLOOR,
            LOGICAL_FLOOR,
        );

        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_scope_proximity_logical_only_unscoped_memory_neutral_floor() {
        // Memory with no logical scopes under a logical-only query: neutral
        // floor, no bonus and no suppression. Physical scopes are irrelevant
        // here because there is no path to match them against.
        let memory_physical = vec!["src/api/**".to_string()];
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &memory_physical,
            &[],
            None,
            &current_logical,
            BASE,
            FLOOR,
            LOGICAL_FLOOR,
        );

        assert_eq!(score, LOGICAL_FLOOR);
    }

    #[test]
    fn test_scope_proximity_logical_only_ordering() {
        // Match quality must order: exact > parent > sibling > unscoped > unrelated.
        let current_logical = vec!["auth.oauth".to_string()];
        let score_for = |memory_logical: &[String]| {
            scope_proximity(
                &[],
                memory_logical,
                None,
                &current_logical,
                BASE,
                FLOOR,
                LOGICAL_FLOOR,
            )
        };

        let exact = score_for(&["auth.oauth".to_string()]);
        let parent = score_for(&["auth".to_string()]);
        let sibling = score_for(&["auth.jwt".to_string()]);
        let unscoped = score_for(&[]);
        let unrelated = score_for(&["database".to_string()]);

        assert!(exact > parent, "exact {exact} > parent {parent}");
        assert!(parent > sibling, "parent {parent} > sibling {sibling}");
        assert!(
            sibling > unscoped,
            "sibling {sibling} > unscoped {unscoped}"
        );
        assert!(
            unscoped > unrelated,
            "unscoped {unscoped} > unrelated {unrelated}"
        );
        assert_eq!(unrelated, 0.0);
    }

    #[test]
    fn test_scope_proximity_logical_only_capped_at_one() {
        // High floor + exact bonus would exceed 1.0 — must be capped.
        let memory_logical = vec!["auth.oauth".to_string()];
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &[],
            &memory_logical,
            None,
            &current_logical,
            BASE,
            FLOOR,
            0.9,
        );

        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_scope_proximity_no_context_at_all() {
        // Neither path nor logical context: raw score stays 0.0 (the scorer
        // substitutes a neutral 1.0 multiplier for absent context).
        let memory_physical = vec!["src/api/**".to_string()];
        let memory_logical = vec!["auth".to_string()];

        let score = scope_proximity(
            &memory_physical,
            &memory_logical,
            None,
            &[],
            BASE,
            FLOOR,
            LOGICAL_FLOOR,
        );

        assert_eq!(score, 0.0);
    }
}
