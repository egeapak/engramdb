//! Project identity computation based on git remote or working directory

use sha2::{Sha256, Digest};
use std::path::Path;
use std::fs;

/// Compute a project ID from the project directory.
///
/// Algorithm:
/// 1. Try to read .git/config for remote "origin" URL
/// 2. Normalize URL: strip protocol, remove .git suffix, lowercase
/// 3. SHA-256 hash, take first 16 hex chars
/// 4. Fallback: SHA-256 of absolute cwd path, first 16 hex chars
pub fn compute_project_id(project_dir: &Path) -> String {
    // Try git remote first
    if let Some(remote_url) = get_git_remote(project_dir) {
        let normalized = normalize_git_remote(&remote_url);
        return hash_string(&normalized);
    }

    // Fallback to absolute path
    let abs_path = project_dir.canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let path_str = abs_path.to_string_lossy();
    hash_string(&path_str)
}

fn get_git_remote(project_dir: &Path) -> Option<String> {
    let git_config = project_dir.join(".git/config");
    if !git_config.exists() {
        return None;
    }

    let content = fs::read_to_string(git_config).ok()?;

    // Parse git config for [remote "origin"] url
    let mut in_origin = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[remote \"origin\"]" {
            in_origin = true;
            continue;
        }
        if in_origin {
            if trimmed.starts_with('[') {
                // End of origin section
                break;
            }
            if trimmed.starts_with("url") {
                if let Some(url) = trimmed.split('=').nth(1) {
                    return Some(url.trim().to_string());
                }
            }
        }
    }
    None
}

fn normalize_git_remote(url: &str) -> String {
    let mut normalized = url.to_string();

    // Strip protocol
    if let Some(idx) = normalized.find("://") {
        normalized = normalized[idx + 3..].to_string();
    }

    // Strip git@ prefix for SSH URLs
    if normalized.starts_with("git@") {
        normalized = normalized[4..].to_string();
        // Replace first : with / for SSH URLs
        if let Some(idx) = normalized.find(':') {
            normalized.replace_range(idx..=idx, "/");
        }
    }

    // Remove .git suffix
    if normalized.ends_with(".git") {
        normalized.truncate(normalized.len() - 4);
    }

    // Lowercase
    normalized.to_lowercase()
}

fn hash_string(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();

    // Take first 16 hex chars
    format!("{:x}", result)[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_git_remote() {
        assert_eq!(
            normalize_git_remote("https://github.com/user/repo.git"),
            "github.com/user/repo"
        );
        assert_eq!(
            normalize_git_remote("git@github.com:user/repo.git"),
            "github.com/user/repo"
        );
        assert_eq!(
            normalize_git_remote("ssh://git@github.com/user/repo.git"),
            "github.com/user/repo"
        );
    }

    #[test]
    fn test_normalize_without_git_suffix() {
        // URL without .git suffix should still work
        assert_eq!(
            normalize_git_remote("https://github.com/user/repo"),
            "github.com/user/repo"
        );
        assert_eq!(
            normalize_git_remote("git@github.com:user/repo"),
            "github.com/user/repo"
        );
    }

    #[test]
    fn test_hash_consistency() {
        let input = "github.com/user/repo";
        let hash1 = hash_string(input);
        let hash2 = hash_string(input);
        assert_eq!(hash1, hash2, "Same input should produce same hash");
    }

    #[test]
    fn test_hash_length_16() {
        let input = "test-input";
        let hash = hash_string(input);
        assert_eq!(hash.len(), 16, "Hash should be exactly 16 characters");
        // Verify it's valid hex
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
