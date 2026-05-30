//! Project identity computation based on git remote or working directory.
//!
//! This module computes a stable, unique project identifier using:
//! 1. Git remote URL (if available) - preferred for consistency across clones
//! 2. Absolute directory path - fallback for non-git projects
//!
//! The algorithm:
//! - Normalize git remote URL (strip protocol, lowercase, remove .git)
//! - SHA-256 hash the normalized URL or path
//! - Take first 16 hex characters as project ID
//!
//! This ensures the same project has the same ID across different machines
//! (when using git) or stable IDs for local projects.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

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
    let abs_path = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let path_str = abs_path.to_string_lossy();
    hash_string(&path_str)
}

/// If `dir` is a linked git worktree, return the absolute path of the main
/// worktree's project root.
///
/// Detection rules:
/// - Main worktree: `dir/.git` is a directory → returns `None`.
/// - Non-git directory: `dir/.git` is missing → returns `None`.
/// - Linked worktree: `dir/.git` is a file containing `gitdir: <path>`.
///   Read `<path>/commondir`, resolve it relative to `<path>` to get the
///   main `.git` directory, and return the canonicalized parent of that
///   directory (the main worktree's project root).
/// - Malformed worktree files / unreadable commondir: returns `None`.
pub fn detect_worktree_main(dir: &Path) -> Option<PathBuf> {
    let git_entry = dir.join(".git");
    let metadata = fs::symlink_metadata(&git_entry).ok()?;

    // Main worktree has a .git directory; nothing to do.
    if metadata.file_type().is_dir() {
        return None;
    }

    // Linked worktrees have a .git file with `gitdir: <path>` pointing at
    // the per-worktree subdir inside the main repo's .git dir.
    let content = fs::read_to_string(&git_entry).ok()?;
    let gitdir_line = content.lines().next()?.trim();
    let gitdir_raw = gitdir_line.strip_prefix("gitdir:")?.trim();
    if gitdir_raw.is_empty() {
        return None;
    }

    let worktree_gitdir = {
        let p = PathBuf::from(gitdir_raw);
        if p.is_absolute() {
            p
        } else {
            // Relative paths are resolved from the worktree root (where `.git` lives).
            dir.join(p)
        }
    };

    // The per-worktree gitdir contains a `commondir` file whose contents
    // (a path, usually relative to the gitdir) point at the main `.git` dir.
    let commondir_file = worktree_gitdir.join("commondir");
    let common_raw = fs::read_to_string(&commondir_file).ok()?;
    let common_trimmed = common_raw.trim();
    if common_trimmed.is_empty() {
        return None;
    }

    let main_git_dir = {
        let p = PathBuf::from(common_trimmed);
        if p.is_absolute() {
            p
        } else {
            worktree_gitdir.join(p)
        }
    };

    // Canonicalize *before* taking the parent so that `..` components (common
    // in the `commondir` file, e.g. "../..") are normalized. Without this,
    // `.parent()` on a literal path like `.../worktrees/wt/../..` only strips
    // the final `..`, landing one directory too deep.
    let main_git_dir = main_git_dir.canonicalize().ok()?;
    let main_root = main_git_dir.parent()?.to_path_buf();
    Some(main_root)
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

    // ---- Worktree detection ----

    /// Build a fake main + linked-worktree layout mirroring git's structure:
    ///
    ///   <root>/main/.git/              (main .git dir)
    ///   <root>/main/.git/worktrees/wt/ (per-worktree gitdir)
    ///       gitdir       -> <root>/wt/.git
    ///       commondir    -> ../..
    ///   <root>/wt/.git                 (file: `gitdir: <abs path>`)
    ///
    /// Returns (main_path, worktree_path).
    fn make_fake_worktree(root: &Path) -> (PathBuf, PathBuf) {
        let main = root.join("main");
        let wt = root.join("wt");
        let wt_gitdir = main.join(".git").join("worktrees").join("wt");
        fs::create_dir_all(main.join(".git")).unwrap();
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(&wt_gitdir).unwrap();
        fs::write(wt_gitdir.join("commondir"), "../..").unwrap();
        fs::write(
            wt_gitdir.join("gitdir"),
            wt.join(".git").to_string_lossy().as_ref(),
        )
        .unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();
        (main, wt)
    }

    #[test]
    fn test_detect_worktree_main_returns_main_for_linked_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let result = detect_worktree_main(&wt).expect("should detect main");
        assert_eq!(result, main.canonicalize().unwrap());
    }

    #[test]
    fn test_detect_worktree_main_returns_none_for_main_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(main.join(".git")).unwrap();
        assert!(detect_worktree_main(&main).is_none());
    }

    #[test]
    fn test_detect_worktree_main_returns_none_for_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(detect_worktree_main(tmp.path()).is_none());
    }

    #[test]
    fn test_detect_worktree_main_returns_none_for_malformed_git_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("weird");
        fs::create_dir_all(&dir).unwrap();
        // .git is a file but doesn't start with "gitdir:"
        fs::write(dir.join(".git"), "not a real git worktree").unwrap();
        assert!(detect_worktree_main(&dir).is_none());
    }

    #[test]
    fn test_detect_worktree_main_returns_none_when_commondir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        // Point at a gitdir that has no commondir file.
        let gitdir = tmp.path().join("fake-gitdir");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        assert!(detect_worktree_main(&wt).is_none());
    }
}
