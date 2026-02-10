/// Calculates proximity bonus based on logical scope hierarchy using dot-notation.
/// Returns the highest bonus from any pair of (memory_scope, current_scope).
///
/// Scoring:
/// - Exact match: 0.3
/// - Parent scope match: 0.2
/// - Sibling scope match: 0.15
/// - No match: 0.0
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
/// A is a parent of B if B starts with A + "."
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
/// For example, "auth.jwt" and "auth.oauth" are siblings with parent "auth".
fn are_siblings(scope_a: &str, scope_b: &str) -> bool {
    let parent_a = extract_parent(scope_a);
    let parent_b = extract_parent(scope_b);

    if parent_a.is_none() || parent_b.is_none() {
        return false;
    }

    parent_a == parent_b && scope_a != scope_b
}

/// Extracts the parent scope from a dot-notation scope.
/// For example, "auth.oauth.google" → Some("auth.oauth")
/// "auth" → None (no parent)
fn extract_parent(scope: &str) -> Option<String> {
    if let Some(pos) = scope.rfind('.') {
        Some(scope[..pos].to_string())
    } else {
        None
    }
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
        let current = vec![
            "database.postgres".to_string(),
            "auth.oauth".to_string(),
        ];
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
        assert_eq!(extract_parent("auth.oauth.google"), Some("auth.oauth".to_string()));
        assert_eq!(extract_parent("auth.oauth"), Some("auth".to_string()));
        assert_eq!(extract_parent("auth"), None);
        assert_eq!(extract_parent(""), None);
    }
}
