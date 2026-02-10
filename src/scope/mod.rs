pub mod logical;
pub mod physical;

/// Calculates the combined proximity score between a memory's scopes and the current context.
///
/// This function combines physical and logical scope proximity:
/// - Physical score: based on file path matching (0.0 to 1.0)
/// - Logical bonus: based on dot-notation scope hierarchy (0.0 to 0.3)
/// - Total score is capped at 1.0
///
/// # Arguments
/// * `memory_physical` - Physical scope patterns from the memory (file paths/globs)
/// * `memory_logical` - Logical scope tags from the memory (dot-notation)
/// * `current_path` - Current file path (if any)
/// * `current_logical` - Current logical scope tags
///
/// # Returns
/// Combined proximity score from 0.0 to 1.0
pub fn scope_proximity(
    memory_physical: &[String],
    memory_logical: &[String],
    current_path: Option<&str>,
    current_logical: &[String],
) -> f64 {
    let physical_score = current_path
        .map(|p| physical::proximity(memory_physical, p))
        .unwrap_or(0.0);

    let logical_bonus = logical::proximity(memory_logical, current_logical);

    (physical_score + logical_bonus).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        );

        // Physical: 0.6 (parent module), Logical: 0.2 (parent match)
        // Total: 0.8
        assert_eq!(score, 0.8);
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
        );

        // Physical: 1.0 (exact), Logical: 0.3 (exact)
        // Total would be 1.3, but capped at 1.0
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_scope_proximity_no_current_path() {
        let memory_physical = vec!["src/api/**".to_string()];
        let memory_logical = vec!["auth".to_string()];
        let current_path = None;
        let current_logical = vec!["auth.oauth".to_string()];

        let score = scope_proximity(
            &memory_physical,
            &memory_logical,
            current_path,
            &current_logical,
        );

        // Physical: 0.0 (no current path), Logical: 0.2 (parent match)
        assert_eq!(score, 0.2);
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
        );

        assert_eq!(score, 0.0);
    }
}
