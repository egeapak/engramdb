//! Logical scope proximity based on hierarchical dot-notation tags
//!
//! This module calculates proximity bonuses between memory logical scopes and
//! current logical scopes using distance-aware hierarchical relationships in
//! dot-notation.
//!
//! # Scoring
//!
//! The bonus decays with hierarchical distance. Given two scopes and their
//! lowest common ancestor (LCA), `up_m` / `up_c` are the number of segments
//! each scope sits below the LCA.
//!
//! | Relationship                         | (up_m, up_c)         | Bonus |
//! |--------------------------------------|----------------------|-------|
//! | Exact match                          | (0, 0)               | 0.30  |
//! | Parent ↔ child                       | (0, 1) or (1, 0)     | 0.20  |
//! | Sibling                              | (1, 1)               | 0.15  |
//! | Grandparent ↔ grandchild             | (0, 2) or (2, 0)     | 0.10  |
//! | Cousin                               | (2, 2)               | 0.05  |
//! | Great-grandparent ↔ great-grandchild | (0, 3) or (3, 0)     | 0.05  |
//! | Anything else                        | —                    | 0.00  |
//!
//! # Hierarchy Rules
//!
//! - Scopes are hierarchical using dot notation: `auth.oauth.google`.
//! - Relationships are bidirectional — (memory, current) and (current, memory)
//!   score identically.
//! - Scopes that share no prefix segment score 0.
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
/// Proximity bonus from 0.0 to 0.30
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
    let Some((up_m, up_c)) = lca_distance(memory_scope, current_scope) else {
        return 0.0;
    };

    match (up_m, up_c) {
        (0, 0) => 0.30,
        (0, 1) | (1, 0) => 0.20,
        (1, 1) => 0.15,
        (0, 2) | (2, 0) => 0.10,
        (2, 2) => 0.05,
        (0, 3) | (3, 0) => 0.05,
        _ => 0.0,
    }
}

/// Returns `Some(n)` iff `ancestor` is a strict ancestor of `descendant`,
/// where `n` is the number of segments `descendant` sits below `ancestor`.
///
/// Returns `None` if `ancestor == descendant`, neither is an ancestor of the
/// other, or either scope is empty.
///
/// # Examples
/// - `ancestor_distance("a", "a.b")` → `Some(1)`
/// - `ancestor_distance("a", "a.b.c.d")` → `Some(3)`
/// - `ancestor_distance("a.b", "a.b")` → `None`
/// - `ancestor_distance("auth", "authentication")` → `None`
fn ancestor_distance(ancestor: &str, descendant: &str) -> Option<usize> {
    if ancestor.is_empty() || descendant.is_empty() {
        return None;
    }
    let asegs: Vec<&str> = ancestor.split('.').collect();
    let dsegs: Vec<&str> = descendant.split('.').collect();
    if asegs.len() >= dsegs.len() {
        return None;
    }
    if asegs.iter().zip(dsegs.iter()).all(|(a, d)| a == d) {
        Some(dsegs.len() - asegs.len())
    } else {
        None
    }
}

/// Returns `Some((up_a, up_b))` — the number of segments each scope sits above
/// their lowest common ancestor. Returns `None` when the scopes share no
/// prefix segment (no LCA) or either scope is empty.
///
/// # Examples
/// - `lca_distance("a.b.c", "a.b.d")` → `Some((1, 1))` (siblings)
/// - `lca_distance("a.b.c", "a.d.e")` → `Some((2, 2))` (cousins)
/// - `lca_distance("a", "a.b.c")` → `Some((0, 2))` (grandparent)
/// - `lca_distance("a.b.c", "a.b.c")` → `Some((0, 0))` (exact)
/// - `lca_distance("x.y", "a.b")` → `None`
fn lca_distance(a: &str, b: &str) -> Option<(usize, usize)> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    if a == b {
        return Some((0, 0));
    }
    if let Some(d) = ancestor_distance(a, b) {
        return Some((0, d));
    }
    if let Some(d) = ancestor_distance(b, a) {
        return Some((d, 0));
    }
    let asegs: Vec<&str> = a.split('.').collect();
    let bsegs: Vec<&str> = b.split('.').collect();
    let common = asegs
        .iter()
        .zip(bsegs.iter())
        .take_while(|(x, y)| x == y)
        .count();
    if common == 0 {
        return None;
    }
    Some((asegs.len() - common, bsegs.len() - common))
}

/// Extracts the parent scope from a dot-notation scope.
///
/// # Examples
/// - "auth.oauth.google" → Some("auth.oauth")
/// - "auth.oauth" → Some("auth")
/// - "auth" → None (no parent)
#[allow(dead_code)]
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

    #[test]
    fn test_proximity_grandparent_bonus() {
        // memory "a" is grandparent of current "a.b.c" → 0.10
        let memory = vec!["a".to_string()];
        let current = vec!["a.b.c".to_string()];
        assert_eq!(proximity(&memory, &current), 0.10);
    }

    #[test]
    fn test_proximity_grandchild_bonus() {
        // memory "a.b.c" is grandchild of current "a" → 0.10
        let memory = vec!["a.b.c".to_string()];
        let current = vec!["a".to_string()];
        assert_eq!(proximity(&memory, &current), 0.10);
    }

    #[test]
    fn test_proximity_great_grandparent_bonus() {
        // memory "a" is great-grandparent of current "a.b.c.d" → 0.05
        let memory = vec!["a".to_string()];
        let current = vec!["a.b.c.d".to_string()];
        assert_eq!(proximity(&memory, &current), 0.05);
    }

    #[test]
    fn test_proximity_great_grandchild_regression_abcd_vs_a() {
        // Regression: memory "a.b.c.d" vs current "a" must be 0.05, not 0.20.
        // Previous boolean is_parent_scope returned the full parent bonus
        // regardless of distance. Distance-aware scoring must reflect that
        // "a" is 3 segments above "a.b.c.d", not 1.
        let memory = vec!["a.b.c.d".to_string()];
        let current = vec!["a".to_string()];
        assert_eq!(proximity(&memory, &current), 0.05);
    }

    #[test]
    fn test_proximity_cousin_bonus() {
        // memory "a.b.c" vs current "a.d.e" share grandparent "a" but not parent → 0.05
        let memory = vec!["a.b.c".to_string()];
        let current = vec!["a.d.e".to_string()];
        assert_eq!(proximity(&memory, &current), 0.05);
    }

    #[test]
    fn test_proximity_deep_no_match_beyond_great_grand() {
        // memory "a" vs current "a.b.c.d.e" — 4 segments below exceeds table → 0.0
        let memory = vec!["a".to_string()];
        let current = vec!["a.b.c.d.e".to_string()];
        assert_eq!(proximity(&memory, &current), 0.0);
    }

    #[test]
    fn test_proximity_no_common_prefix() {
        // memory "x.y" vs current "a.b" share nothing → 0.0
        let memory = vec!["x.y".to_string()];
        let current = vec!["a.b".to_string()];
        assert_eq!(proximity(&memory, &current), 0.0);
    }

    #[test]
    fn test_proximity_uneven_distance_no_match() {
        // memory "a.b" vs current "a.c.d" — LCA "a"; distances (1, 2) — not in table → 0.0
        let memory = vec!["a.b".to_string()];
        let current = vec!["a.c.d".to_string()];
        assert_eq!(proximity(&memory, &current), 0.0);
    }

    #[test]
    fn test_ancestor_distance_parent() {
        assert_eq!(ancestor_distance("a", "a.b"), Some(1));
    }

    #[test]
    fn test_ancestor_distance_grandparent() {
        assert_eq!(ancestor_distance("a", "a.b.c"), Some(2));
    }

    #[test]
    fn test_ancestor_distance_great_grandparent() {
        assert_eq!(ancestor_distance("a", "a.b.c.d"), Some(3));
    }

    #[test]
    fn test_ancestor_distance_strict_equal_returns_none() {
        assert_eq!(ancestor_distance("a.b", "a.b"), None);
    }

    #[test]
    fn test_ancestor_distance_not_ancestor() {
        assert_eq!(ancestor_distance("b", "a.b"), None);
        assert_eq!(ancestor_distance("a.b", "a"), None);
        assert_eq!(ancestor_distance("auth", "authentication"), None);
    }

    #[test]
    fn test_ancestor_distance_empty_scopes() {
        assert_eq!(ancestor_distance("", "a.b"), None);
        assert_eq!(ancestor_distance("a", ""), None);
    }

    #[test]
    fn test_lca_distance_siblings() {
        assert_eq!(lca_distance("a.b.c", "a.b.d"), Some((1, 1)));
    }

    #[test]
    fn test_lca_distance_cousins() {
        assert_eq!(lca_distance("a.b.c", "a.d.e"), Some((2, 2)));
    }

    #[test]
    fn test_lca_distance_ancestor() {
        assert_eq!(lca_distance("a", "a.b"), Some((0, 1)));
        assert_eq!(lca_distance("a.b", "a"), Some((1, 0)));
        assert_eq!(lca_distance("a", "a.b.c.d"), Some((0, 3)));
    }

    #[test]
    fn test_lca_distance_exact_match() {
        assert_eq!(lca_distance("a.b.c", "a.b.c"), Some((0, 0)));
    }

    #[test]
    fn test_lca_distance_no_common_prefix() {
        assert_eq!(lca_distance("x.y", "a.b"), None);
    }

    #[test]
    fn test_lca_distance_empty_scopes() {
        assert_eq!(lca_distance("", "a.b"), None);
        assert_eq!(lca_distance("a.b", ""), None);
    }

    #[test]
    fn test_proximity_picks_best_of_multiple_relationships() {
        // memory has: exact "a.b.c", cousin "a.d.e", and deep "a.x.y.z.q"
        // current is "a.b.c" — exact match wins (0.30)
        let memory = vec![
            "a.d.e".to_string(),
            "a.b.c".to_string(),
            "a.x.y.z.q".to_string(),
        ];
        let current = vec!["a.b.c".to_string()];
        assert_eq!(proximity(&memory, &current), 0.30);
    }
}
