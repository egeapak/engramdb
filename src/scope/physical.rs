use globset::{Glob, GlobSetBuilder};

/// Checks if a file path matches any of the given patterns.
/// Patterns can be exact paths, globs, or "/" for project root (matches everything).
pub fn matches(patterns: &[String], path: &str) -> bool {
    // "/" matches everything
    if patterns.iter().any(|p| p == "/") {
        return true;
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
/// Returns the highest matching score from all memory scopes.
///
/// Scoring:
/// - Exact file match: 1.0
/// - Same directory: 0.85
/// - Same module/parent: 0.6
/// - Project root "/": 0.4
/// - No match: 0.0
pub fn proximity(memory_scopes: &[String], current_path: &str) -> f64 {
    let mut max_score = 0.0;

    for pattern in memory_scopes {
        let score = calculate_pattern_score(pattern, current_path);
        if score > max_score {
            max_score = score;
        }
    }

    max_score
}

fn calculate_pattern_score(pattern: &str, current_path: &str) -> f64 {
    // Project root matches everything with base score
    if pattern == "/" {
        return 0.4;
    }

    // Exact match
    if pattern == current_path {
        return 1.0;
    }

    // Check if pattern is a glob
    if pattern.contains('*') {
        return calculate_glob_score(pattern, current_path);
    }

    // Check if pattern is a directory path (exact directory match)
    if is_same_directory(pattern, current_path) {
        return 0.85;
    }

    // Check if pattern is a parent directory
    if is_parent_directory(pattern, current_path) {
        return 0.6;
    }

    0.0
}

fn calculate_glob_score(pattern: &str, current_path: &str) -> f64 {
    // Try to match the glob
    let glob = match Glob::new(pattern) {
        Ok(g) => g,
        Err(_) => return 0.0,
    };

    let matcher = glob.compile_matcher();
    if !matcher.is_match(current_path) {
        return 0.0;
    }

    // Determine the score based on glob specificity
    // Pattern like "src/api/auth/*" covering the same directory → 0.85
    // Pattern like "src/api/**" covering parent module → 0.6

    // Extract the directory part before the glob pattern
    let pattern_dir = if let Some(pos) = pattern.find('*') {
        &pattern[..pos]
    } else {
        pattern
    };

    let pattern_dir = pattern_dir.trim_end_matches('/');
    let current_dir = extract_directory(current_path);

    // If the pattern is like "src/api/auth/*", it targets files in that specific directory
    if pattern.ends_with("/*") && pattern_dir == current_dir {
        return 0.85;
    }

    // If the pattern is like "src/api/**", it's a parent module pattern
    if pattern.contains("**") {
        if current_path.starts_with(pattern_dir) || pattern_dir.is_empty() {
            return 0.6;
        }
    }

    // For other glob patterns that match, default to parent score
    if current_path.starts_with(pattern_dir) || pattern_dir.is_empty() {
        return 0.6;
    }

    0.0
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

    if current_path.starts_with(pattern_normalized) {
        let remainder = &current_path[pattern_normalized.len()..];
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

    #[test]
    fn test_matches_root() {
        assert!(matches(&["/".to_string()], "any/path/file.rs"));
    }

    #[test]
    fn test_matches_exact() {
        assert!(matches(&["src/main.rs".to_string()], "src/main.rs"));
        assert!(!matches(&["src/main.rs".to_string()], "src/lib.rs"));
    }

    #[test]
    fn test_matches_glob() {
        assert!(matches(&["src/**/*.rs".to_string()], "src/api/handlers.rs"));
        assert!(matches(&["src/api/*".to_string()], "src/api/handlers.rs"));
    }

    #[test]
    fn test_proximity_exact() {
        let score = proximity(&["src/api/auth/handlers.rs".to_string()], "src/api/auth/handlers.rs");
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_proximity_same_directory_glob() {
        let score = proximity(&["src/api/auth/*".to_string()], "src/api/auth/handlers.rs");
        assert_eq!(score, 0.85);
    }

    #[test]
    fn test_proximity_parent_module() {
        let score = proximity(&["src/api/**".to_string()], "src/api/auth/handlers.rs");
        assert_eq!(score, 0.6);
    }

    #[test]
    fn test_proximity_root() {
        let score = proximity(&["/".to_string()], "src/api/auth/handlers.rs");
        assert_eq!(score, 0.4);
    }

    #[test]
    fn test_proximity_no_match() {
        let score = proximity(&["src/db/**".to_string()], "src/api/auth/handlers.rs");
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_proximity_highest_score() {
        let scopes = vec![
            "src/db/**".to_string(),
            "/".to_string(),
            "src/api/**".to_string(),
        ];
        let score = proximity(&scopes, "src/api/auth/handlers.rs");
        assert_eq!(score, 0.6); // Should pick the highest matching score
    }
}
