//! Physical scope proximity based on file paths and glob patterns
//!
//! This module calculates proximity scores between memory scopes (file paths/globs)
//! and the current file path, using pattern matching to determine relevance.
//!
//! # Scoring
//!
//! - **1.0**: Exact file match
//! - **max(floor, base^depth)**: Depth-decayed score for directory/parent/root matches
//! - **0.0**: No match
//!
//! Default curve with base=0.82, floor=0.3: depth 1→0.82, 2→0.67, 3→0.55, 5→0.37, 7+→0.30
//!
//! # Glob Support
//!
//! Supports standard glob patterns:
//! - `*` matches any characters except `/`
//! - `**` matches any characters including `/` (recursive)
//! - `?` matches exactly one character
//!
//! # Key Functions
//!
//! - [`matches`]: Check if a path matches any pattern
//! - [`proximity`]: Calculate proximity score between scopes and current path

use globset::{Glob, GlobSetBuilder};

/// Checks if a file path matches any of the given patterns.
///
/// # Arguments
/// * `patterns` - Physical scope patterns (exact paths, globs, or "/")
/// * `path` - File path to check
///
/// # Returns
/// True if path matches any pattern, false otherwise
///
/// # Examples
///
/// ```ignore
/// use engramdb::scope::physical::matches;
///
/// assert!(matches(&["/".to_string()], "any/path/file.rs"));
/// assert!(matches(&["src/**/*.rs".to_string()], "src/api/handlers.rs"));
/// assert!(!matches(&["src/main.rs".to_string()], "src/lib.rs"));
/// ```
pub fn matches(patterns: &[String], path: &str) -> bool {
    // "/" matches everything
    if patterns.iter().any(|p| p == "/") {
        return true;
    }

    // A pattern matches when it is a parent directory prefix
    // (e.g. "src/cli/" matches "src/cli/commands/add.rs") OR shares the file's
    // directory. The same-directory case keeps the hard Filter-mode predicate
    // consistent with the proximity scorer, which rewards same-directory
    // siblings (finding #13); excluding them here meant memories that the
    // scorer would rank never surfaced in Filter mode.
    for pattern in patterns {
        if is_parent_directory(pattern, path) || is_same_directory(pattern, path) {
            return true;
        }
    }

    // Build a GlobSet from all patterns
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }

    if let Ok(globset) = builder.build() {
        globset.is_match(path)
    } else {
        false
    }
}

/// Calculates proximity score between memory scopes and current file path.
///
/// Returns the highest matching score from all memory scopes.
/// Scores use exponential depth decay: `max(floor, base^depth)`.
///
/// # Arguments
/// * `memory_scopes` - Physical scope patterns from the memory
/// * `current_path` - Current file path
/// * `base` - Exponential decay base (e.g. 0.82)
/// * `floor` - Minimum score regardless of depth (e.g. 0.3)
///
/// # Returns
/// Proximity score from 0.0 to 1.0
pub fn proximity(memory_scopes: &[String], current_path: &str, base: f64, floor: f64) -> f64 {
    let mut max_score = 0.0;

    for pattern in memory_scopes {
        let score = calculate_pattern_score(pattern, current_path, base, floor);
        if score > max_score {
            max_score = score;
        }
    }

    max_score
}

fn calculate_pattern_score(pattern: &str, current_path: &str, base: f64, floor: f64) -> f64 {
    if current_path.is_empty() {
        return 0.0;
    }

    // Exact match
    if pattern == current_path {
        return 1.0;
    }

    // Check if pattern is a glob
    if pattern.contains('*') {
        return calculate_glob_score(pattern, current_path, base, floor);
    }

    // Project root matches everything with depth-decayed score
    if pattern == "/" {
        let depth = directory_depth_from_root(current_path);
        return depth_decay_score(depth, base, floor);
    }

    // Check if pattern is a directory path (exact directory match)
    if is_same_directory(pattern, current_path) {
        // Same directory = depth 1 (file is one level inside the "directory")
        return depth_decay_score(1, base, floor);
    }

    // Check if pattern is a parent directory
    if is_parent_directory(pattern, current_path) {
        let depth = directory_depth_from_parent(pattern, current_path);
        return depth_decay_score(depth, base, floor);
    }

    0.0
}

/// Calculate the depth-decayed score: `max(floor, base^depth)`.
fn depth_decay_score(depth: usize, base: f64, floor: f64) -> f64 {
    if depth == 0 {
        return 1.0;
    }
    floor.max(base.powi(depth as i32))
}

/// Count directory depth from root "/" to the file.
/// `"main.rs"` → 1, `"src/main.rs"` → 2, `"src/a/b.rs"` → 3.
fn directory_depth_from_root(path: &str) -> usize {
    if path.is_empty() {
        0
    } else {
        path.matches('/').count() + 1
    }
}

/// Count directory depth from a parent pattern to the file.
/// `("src/", "src/main.rs")` → 1, `("src/", "src/a/b.rs")` → 2, `("src/", "src/a/b/c.rs")` → 3.
fn directory_depth_from_parent(pattern: &str, current_path: &str) -> usize {
    let pattern_normalized = pattern.trim_end_matches('/');
    let remainder = current_path
        .strip_prefix(pattern_normalized)
        .unwrap_or("")
        .trim_start_matches('/');
    if remainder.is_empty() {
        0
    } else {
        // A bare filename ("handlers.rs") is 1 level; each '/' adds another
        remainder.matches('/').count() + 1
    }
}

fn calculate_glob_score(pattern: &str, current_path: &str, base: f64, floor: f64) -> f64 {
    // Try to match the glob
    let glob = match Glob::new(pattern) {
        Ok(g) => g,
        Err(_) => return 0.0,
    };

    let matcher = glob.compile_matcher();
    if !matcher.is_match(current_path) {
        return 0.0;
    }

    // Extract the directory part before the glob pattern
    let pattern_dir = if let Some(pos) = pattern.find('*') {
        &pattern[..pos]
    } else {
        pattern
    };

    let pattern_dir = pattern_dir.trim_end_matches('/');

    // Calculate depth from the glob's base directory to the file
    let depth = if pattern_dir.is_empty() {
        // Glob like "**/*.rs" — treat as root-level
        directory_depth_from_root(current_path)
    } else if current_path.starts_with(pattern_dir) {
        directory_depth_from_parent(pattern_dir, current_path)
    } else {
        return 0.0;
    };

    depth_decay_score(depth, base, floor)
}

fn is_same_directory(pattern: &str, current_path: &str) -> bool {
    let pattern_dir = extract_directory(pattern);
    let current_dir = extract_directory(current_path);
    pattern_dir == current_dir && !pattern_dir.is_empty()
}

fn is_parent_directory(pattern: &str, current_path: &str) -> bool {
    // Pattern should be a prefix of the current path with a "/" separator
    let pattern_normalized = pattern.trim_end_matches('/');

    if pattern_normalized.is_empty() {
        return false;
    }

    if let Some(remainder) = current_path.strip_prefix(pattern_normalized) {
        // Check if there's a path separator after the pattern
        remainder.starts_with('/')
    } else {
        false
    }
}

fn extract_directory(path: &str) -> &str {
    if let Some(pos) = path.rfind('/') {
        &path[..pos]
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: f64 = 0.82;
    const FLOOR: f64 = 0.3;

    // Helper to compare floats within tolerance
    fn assert_approx(actual: f64, expected: f64, msg: &str) {
        assert!(
            (actual - expected).abs() < 0.01,
            "{}: expected {:.4}, got {:.4}",
            msg,
            expected,
            actual
        );
    }

    // --- matches() tests (unchanged, no config needed) ---

    #[test]
    fn test_matches_root() {
        assert!(matches(&["/".to_string()], "any/path/file.rs"));
    }

    #[test]
    fn test_matches_exact() {
        assert!(matches(&["src/main.rs".to_string()], "src/main.rs"));
        // #13: matches() now agrees with the proximity scorer — a sibling in
        // the SAME directory matches (it was previously excluded, contradicting
        // the scorer which rewards it). A file in a DIFFERENT directory still
        // does not match.
        assert!(matches(&["src/main.rs".to_string()], "src/lib.rs"));
        assert!(!matches(&["src/main.rs".to_string()], "tests/lib.rs"));
    }

    #[test]
    fn test_matches_glob() {
        assert!(matches(&["src/**/*.rs".to_string()], "src/api/handlers.rs"));
        assert!(matches(&["src/api/*".to_string()], "src/api/handlers.rs"));
    }

    #[test]
    fn test_matches_parent_directory() {
        assert!(matches(
            &["src/cli/".to_string()],
            "src/cli/commands/add.rs"
        ));
        assert!(matches(&["src/".to_string()], "src/cli/commands/add.rs"));
        assert!(!matches(
            &["src/vector/".to_string()],
            "src/cli/commands/add.rs"
        ));
    }

    #[test]
    fn test_matches_parent_directory_without_trailing_slash() {
        assert!(matches(&["src/cli".to_string()], "src/cli/commands/add.rs"));
        assert!(matches(&["src".to_string()], "src/cli/commands/add.rs"));
    }

    #[test]
    fn test_matches_directory_prefix_not_substring() {
        assert!(!matches(&["src/cl".to_string()], "src/cli/add.rs"));
        assert!(!matches(&["src/cl/".to_string()], "src/cli/add.rs"));
        assert!(!matches(&["src/cli_old".to_string()], "src/cli/add.rs"));
    }

    #[test]
    fn test_matches_deeply_nested_parent() {
        assert!(matches(&["src/".to_string()], "src/a/b/c/d/e/deep.rs"));
    }

    #[test]
    fn test_matches_mixed_patterns_with_directory() {
        let patterns = vec!["tests/unit/".to_string(), "src/cli/".to_string()];
        assert!(matches(&patterns, "src/cli/commands/add.rs"));
    }

    #[test]
    fn test_matches_empty_patterns() {
        assert!(!matches(&[], "src/main.rs"));
        assert!(!matches(&[], "any/path/file.rs"));
    }

    // Finding #13: the hard Filter-mode predicate `matches()` must not exclude
    // a scope that the proximity scorer rewards. The scorer treats files in the
    // same directory as related (see `test_proximity_same_directory_non_glob`),
    // so `matches()` must agree (decision: loosen the filter to the scorer).
    #[test]
    fn matches_agrees_with_scorer_for_same_directory_sibling() {
        let scope = ["src/api/a.rs".to_string()];
        let path = "src/api/b.rs";
        // The scorer rewards the sibling...
        assert!(proximity(&scope, path, BASE, FLOOR) > 0.0);
        // ...so the hard filter must let it through (red before fix).
        assert!(
            matches(&scope, path),
            "filter must not exclude a same-directory sibling the scorer ranks"
        );
        // Consistency holds the other way too: a different directory neither
        // scores nor matches.
        let other = "tests/api/b.rs";
        assert_eq!(proximity(&scope, other, BASE, FLOOR), 0.0);
        assert!(!matches(&scope, other));
    }

    // Finding #12 (verified non-bug): a mid-segment glob measures proximity
    // depth by counting path separators, so the partial leading segment before
    // the wildcard does not distort the depth. This pins that the score is
    // correct (the reviewer's worry does not manifest in the numeric result).
    #[test]
    fn glob_midsegment_depth_is_correct() {
        // `src/a*/x.rs` matches `src/api/x.rs`; the file is 2 levels below the
        // glob's real base directory `src/`.
        let score = proximity(&["src/a*/x.rs".to_string()], "src/api/x.rs", BASE, FLOOR);
        assert_approx(score, depth_decay_score(2, BASE, FLOOR), "mid-segment glob");
    }

    // --- directory_depth helper tests ---

    #[test]
    fn test_depth_from_root_shallow() {
        // Root-level file is 1 level from root
        assert_eq!(directory_depth_from_root("main.rs"), 1);
    }

    #[test]
    fn test_depth_from_root_one_dir() {
        assert_eq!(directory_depth_from_root("src/main.rs"), 2);
    }

    #[test]
    fn test_depth_from_root_deep() {
        assert_eq!(directory_depth_from_root("src/a/b/c.rs"), 4);
    }

    #[test]
    fn test_depth_from_parent_one_level() {
        // "src/" → "src/main.rs": file is 1 level inside
        assert_eq!(directory_depth_from_parent("src/", "src/main.rs"), 1);
        assert_eq!(directory_depth_from_parent("src", "src/main.rs"), 1);
    }

    #[test]
    fn test_depth_from_parent_two_levels() {
        assert_eq!(directory_depth_from_parent("src/", "src/a/b.rs"), 2);
    }

    #[test]
    fn test_depth_from_parent_deep() {
        assert_eq!(directory_depth_from_parent("src/", "src/a/b/c/d.rs"), 4);
    }

    // --- depth_decay_score tests ---

    #[test]
    fn test_decay_score_exact() {
        assert_eq!(depth_decay_score(0, BASE, FLOOR), 1.0);
    }

    #[test]
    fn test_decay_score_depth_1() {
        assert_approx(depth_decay_score(1, BASE, FLOOR), 0.82, "depth 1");
    }

    #[test]
    fn test_decay_score_depth_2() {
        assert_approx(depth_decay_score(2, BASE, FLOOR), 0.6724, "depth 2");
    }

    #[test]
    fn test_decay_score_depth_5() {
        assert_approx(depth_decay_score(5, BASE, FLOOR), 0.3707, "depth 5");
    }

    #[test]
    fn test_decay_score_hits_floor() {
        // depth 7: 0.82^7 ≈ 0.2493 < 0.3, so floor wins
        assert_approx(depth_decay_score(7, BASE, FLOOR), FLOOR, "depth 7 floor");
        assert_approx(depth_decay_score(10, BASE, FLOOR), FLOOR, "depth 10 floor");
    }

    #[test]
    fn test_decay_score_configurable() {
        // Steep decay: base=0.5
        assert_approx(depth_decay_score(1, 0.5, 0.1), 0.5, "steep depth 1");
        assert_approx(depth_decay_score(3, 0.5, 0.1), 0.125, "steep depth 3");
        assert_approx(depth_decay_score(4, 0.5, 0.1), 0.1, "steep depth 4 floor");
    }

    // --- proximity() tests with depth decay ---

    #[test]
    fn test_proximity_exact() {
        let score = proximity(
            &["src/api/auth/handlers.rs".to_string()],
            "src/api/auth/handlers.rs",
            BASE,
            FLOOR,
        );
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_proximity_same_directory_glob() {
        // "src/api/auth/*" → "src/api/auth/handlers.rs": depth 1 from glob base "src/api/auth"
        let score = proximity(
            &["src/api/auth/*".to_string()],
            "src/api/auth/handlers.rs",
            BASE,
            FLOOR,
        );
        // depth 1: 0.82^1 = 0.82
        assert_approx(score, 0.82, "same dir glob");
    }

    #[test]
    fn test_proximity_parent_module() {
        // "src/api/**" → "src/api/auth/handlers.rs": depth 2 from "src/api"
        let score = proximity(
            &["src/api/**".to_string()],
            "src/api/auth/handlers.rs",
            BASE,
            FLOOR,
        );
        // depth 2: 0.82^2 = 0.6724
        assert_approx(score, 0.6724, "parent module glob");
    }

    #[test]
    fn test_proximity_root() {
        // "/" → "src/api/auth/handlers.rs": depth 4
        let score = proximity(&["/".to_string()], "src/api/auth/handlers.rs", BASE, FLOOR);
        // depth 4: 0.82^4 = 0.4521
        assert_approx(score, 0.4521, "root to depth 4");
    }

    #[test]
    fn test_proximity_root_deep_file() {
        // "/" → deeply nested file: depth 6
        let score = proximity(
            &["/".to_string()],
            "src/components/common/shared/footer/index.jsx",
            BASE,
            FLOOR,
        );
        // depth 6: 0.82^6 = 0.3040
        assert_approx(score, 0.3040, "root to depth 6");
    }

    #[test]
    fn test_proximity_no_match() {
        let score = proximity(
            &["src/db/**".to_string()],
            "src/api/auth/handlers.rs",
            BASE,
            FLOOR,
        );
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_proximity_highest_score() {
        let scopes = vec![
            "src/db/**".to_string(),
            "/".to_string(),
            "src/api/**".to_string(),
        ];
        let score = proximity(&scopes, "src/api/auth/handlers.rs", BASE, FLOOR);
        // "src/api/**" gives depth 2 = 0.6724, which beats "/" at depth 4
        assert_approx(score, 0.6724, "highest score picked");
    }

    #[test]
    fn test_proximity_empty_patterns() {
        let score = proximity(&[], "src/api/handlers.rs", BASE, FLOOR);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_proximity_same_directory_non_glob() {
        // "src/api/a.rs" and "src/api/b.rs" are same directory → depth 1
        let score = proximity(&["src/api/a.rs".to_string()], "src/api/b.rs", BASE, FLOOR);
        assert_approx(score, 0.82, "same dir non-glob");
    }

    #[test]
    fn test_proximity_parent_directory() {
        // "src/" → "src/cli/commands/add.rs": depth 3
        let score = proximity(
            &["src/".to_string()],
            "src/cli/commands/add.rs",
            BASE,
            FLOOR,
        );
        // depth 3: 0.82^3 = 0.5514
        assert_approx(score, 0.5514, "parent dir depth 3");
    }
}
