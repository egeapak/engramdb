//! Logical scope proximity based on hierarchical dot-notation tags
//!
//! This module calculates proximity bonuses between memory logical scopes and
//! current logical scopes using hierarchical relationships in dot-notation.
//!
//! # Scoring
//!
//! - **0.3**: Exact match (e.g., "auth.oauth" == "auth.oauth")
//! - **0.2**: Parent/child relationship (e.g., "auth" is parent of "auth.oauth")
//! - **0.15**: Sibling relationship (e.g., "auth.jwt" and "auth.oauth" share parent "auth")
//! - **0.0**: No relationship
//!
//! # Hierarchy Rules
//!
//! - Scopes are hierarchical using dot notation: "auth.oauth.google"
//! - Parent-child relationships are bidirectional (both directions score 0.2)
//! - Siblings must have the same immediate parent
//!
//! # Key Functions
//!
//! - [`proximity`]: Calculate highest proximity bonus between memory and current scopes

/// Calculates proximity bonus based on logical scope hierarchy using dot-notation.
///
/// Returns the highest bonus from any pair of (memory_scope, current_scope).
///
/// # Arguments
/// * `memory_scopes` - Logical scope tags from the memory
/// * `current_scopes` - Current logical scope tags
///
/// # Returns
/// Proximity bonus from 0.0 to 0.3
///
/// # Scoring
/// - **0.3**: Exact match
/// - **0.2**: Parent scope match
/// - **0.15**: Sibling scope match
/// - **0.0**: No match
pub fn proximity(memory_scopes: &[String], current_scopes: &[String]) -> f64 {
    let mut max_bonus = 0.0;

    for memory_scope in memory_scopes {
        for current_scope in current_scopes {
            let bonus = calculate_scope_bonus(memory_scope, current_scope);
            if bonus > max_bonus {
                max_bonus = bonus;
            }
        }
    }

    max_bonus
}

fn calculate_scope_bonus(memory_scope: &str, current_scope: &str) -> f64 {
    // Exact match
    if memory_scope == current_scope {
        return 0.3;
    }

    // Parent match: memory is parent of current
    if is_parent_scope(memory_scope, current_scope) {
        return 0.2;
    }

    // Parent match: current is parent of memory
    if is_parent_scope(current_scope, memory_scope) {
        return 0.2;
    }

    // Sibling match: both share a common parent
    if are_siblings(memory_scope, current_scope) {
        return 0.15;
    }

    0.0
}

/// Checks if scope A is a parent of scope B.
///
/// A is a parent of B if B starts with A followed by a dot.
///
/// # Examples
/// - "auth" is parent of "auth.oauth" (true)
/// - "auth.oauth" is parent of "auth.oauth.google" (true)
/// - "auth" is NOT parent of "authentication" (false)
fn is_parent_scope(parent: &str, child: &str) -> bool {
    if parent.is_empty() || child.is_empty() {
        return false;
    }

    if child.len() <= parent.len() {
        return false;
    }

    child.starts_with(parent) && child[parent.len()..].starts_with('.')
}

/// Checks if two scopes are siblings (share the same parent).
///
/// # Examples
/// - "auth.jwt" and "auth.oauth" are siblings (parent: "auth")
/// - "api.users" and "api.posts" are siblings (parent: "api")
/// - "auth" and "database" are NOT siblings (no common parent)
fn are_siblings(scope_a: &str, scope_b: &str) -> bool {
    let parent_a = extract_parent(scope_a);
    let parent_b = extract_parent(scope_b);

    if parent_a.is_none() || parent_b.is_none() {
        return false;
    }

    parent_a == parent_b && scope_a != scope_b
}

/// Extracts the parent scope from a dot-notation scope.
///
/// # Examples
/// - "auth.oauth.google" → Some("auth.oauth")
/// - "auth.oauth" → Some("auth")
/// - "auth" → None (no parent)
fn extract_parent(scope: &str) -> Option<String> {
    scope.rfind('.').map(|pos| scope[..pos].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proximity_exact_match() {
        let memory = vec!["auth.oauth".to_string()];
        let current = vec!["auth.oauth".to_string()];
        assert_eq!(proximity(&memory, &current), 0.3);
    }

    #[test]
    fn test_proximity_parent_match() {
        let memory = vec!["auth".to_string()];
        let current = vec!["auth.oauth".to_string()];
        assert_eq!(proximity(&memory, &current), 0.2);
    }

    #[test]
    fn test_proximity_child_match() {
        let memory = vec!["auth.oauth.google".to_string()];
        let current = vec!["auth.oauth".to_string()];
        assert_eq!(proximity(&memory, &current), 0.2);
    }

    #[test]
    fn test_proximity_sibling_match() {
        let memory = vec!["auth.jwt".to_string()];
        let current = vec!["auth.oauth".to_string()];
        assert_eq!(proximity(&memory, &current), 0.15);
    }

    #[test]
    fn test_proximity_no_match() {
        let memory = vec!["database.postgres".to_string()];
        let current = vec!["auth.oauth".to_string()];
        assert_eq!(proximity(&memory, &current), 0.0);
    }

    #[test]
    fn test_proximity_highest_bonus() {
        let memory = vec![
            "database.postgres".to_string(),
            "auth".to_string(),
            "auth.jwt".to_string(),
        ];
        let current = vec!["auth.oauth".to_string()];
        // Should pick parent match (0.2) over sibling match (0.15)
        assert_eq!(proximity(&memory, &current), 0.2);
    }

    #[test]
    fn test_proximity_multiple_current_scopes() {
        let memory = vec!["auth.oauth".to_string()];
        let current = vec!["database.postgres".to_string(), "auth.oauth".to_string()];
        assert_eq!(proximity(&memory, &current), 0.3);
    }

    #[test]
    fn test_is_parent_scope() {
        assert!(is_parent_scope("auth", "auth.oauth"));
        assert!(is_parent_scope("auth.oauth", "auth.oauth.google"));
        assert!(!is_parent_scope("auth.oauth", "auth"));
        assert!(!is_parent_scope("auth", "auth"));
        assert!(!is_parent_scope("auth", "authentication"));
    }

    #[test]
    fn test_are_siblings() {
        assert!(are_siblings("auth.jwt", "auth.oauth"));
        assert!(are_siblings("api.users", "api.posts"));
        assert!(!are_siblings("auth", "database"));
        assert!(!are_siblings("auth.oauth", "auth"));
        assert!(!are_siblings("auth.oauth", "auth.oauth"));
    }

    #[test]
    fn test_extract_parent() {
        assert_eq!(
            extract_parent("auth.oauth.google"),
            Some("auth.oauth".to_string())
        );
        assert_eq!(extract_parent("auth.oauth"), Some("auth".to_string()));
        assert_eq!(extract_parent("auth"), None);
        assert_eq!(extract_parent(""), None);
    }

    #[test]
    fn test_proximity_empty_scopes() {
        // Both empty scopes should return 0.0 proximity
        let memory: Vec<String> = vec![];
        let current: Vec<String> = vec![];
        assert_eq!(proximity(&memory, &current), 0.0);
    }

    #[test]
    fn test_proximity_deeply_nested() {
        // Deeply nested exact match should still return 0.3
        let memory = vec!["a.b.c.d.e".to_string()];
        let current = vec!["a.b.c.d.e".to_string()];
        assert_eq!(proximity(&memory, &current), 0.3);
    }
}
