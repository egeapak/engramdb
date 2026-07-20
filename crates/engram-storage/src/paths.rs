//! Path resolution utilities for EngramDB storage locations.
//!
//! This module provides functions to resolve all EngramDB storage paths:
//! - Project-local paths (.engramdb/, .engramdb/memories/)
//! - Global config dir (platform-specific via `dirs::config_dir()`):
//!   - macOS: `~/Library/Application Support/engramdb/`
//!   - Linux: `$XDG_CONFIG_HOME/engramdb/` (default `~/.config/engramdb/`)
//! - Global data dir (platform-specific via `dirs::data_dir()`):
//!   - macOS: `~/Library/Application Support/engramdb/`
//!   - Linux: `$XDG_DATA_HOME/engramdb/` (default `~/.local/share/engramdb/`)
//! - Personal project paths (`<global_data_dir>/projects/{id}/personal/`)
//! - LanceDB vector storage paths (`<global_data_dir>/projects/{id}/lancedb/`)
//! - Registry path
//!
//! Functions that depend on platform directories return `Result<PathBuf>` so
//! callers can handle the (rare) case where the platform directory
//! cannot be determined.

use super::error::{Result, StorageError};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

/// Returns the project-specific EngramDB directory (.engramdb/)
pub fn project_dir(dir: &Path) -> PathBuf {
    dir.join(".engramdb")
}

/// Make a file path relative to the project directory if possible.
///
/// Physical scopes are stored repo-relative, so callers that accept a
/// user/agent-supplied file path (MCP `query`, CLI `--path`, Claude Code
/// hooks) must relativize absolute paths before prefix/glob matching.
///
/// Behavior:
/// - Already-relative paths are lexically normalized (leading `./` stripped,
///   `.` segments dropped, duplicate `/` collapsed) — scope matching is
///   purely textual, so the natural spelling `./src/api/auth.rs` would
///   otherwise silently match nothing (`strip_prefix("src")` fails and
///   globset's literal `.` segment never matches `src/**`).
/// - Absolute paths under `project_dir` are returned repo-relative.
/// - Absolute paths NOT under `project_dir` are returned unchanged (they
///   legitimately match no repo-relative scope).
///
/// Both paths are canonicalized (best-effort: a nonexistent path falls back
/// to its literal form) before stripping so that symlinked roots — e.g.
/// `/tmp` on macOS, or `--dir .` — still strip correctly.
pub fn relativize_path(file_path: &str, project_dir: &Path) -> String {
    if Path::new(file_path).is_relative() {
        return normalize_relative(file_path);
    }
    let canonical_dir = project_dir
        .canonicalize()
        .unwrap_or(project_dir.to_path_buf());
    let canonical_file = Path::new(file_path)
        .canonicalize()
        .unwrap_or(Path::new(file_path).to_path_buf());
    canonical_file
        .strip_prefix(&canonical_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| file_path.to_string())
}

/// Lexical cleanup for a relative query path: drop `.` segments and empty
/// segments (duplicate `/`). `..` segments are kept as-is — resolving them
/// lexically would be wrong in the presence of symlinks, and a parent-
/// escaping path legitimately matches no repo-relative scope.
fn normalize_relative(file_path: &str) -> String {
    if !file_path.contains("./") && !file_path.contains("//") {
        return file_path.to_string();
    }
    let cleaned: Vec<&str> = file_path
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .collect();
    cleaned.join("/")
}

/// Returns the shared memories directory in the project
pub fn memories_dir(dir: &Path) -> PathBuf {
    project_dir(dir).join("memories")
}

/// Returns the global configuration directory (platform-specific).
///
/// - macOS: `~/Library/Application Support/engramdb/`
/// - Linux: `$XDG_CONFIG_HOME/engramdb/` (default `~/.config/engramdb/`)
///
/// Used only for the global registry and future global settings.
/// Respects `ENGRAMDB_CONFIG_DIR` env var for testing isolation.
pub fn global_config_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("ENGRAMDB_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }
    dirs::config_dir()
        .ok_or_else(|| StorageError::Validation("Could not determine config directory".to_string()))
        .map(|p| p.join("engramdb"))
}

/// Returns the global data directory (platform-specific).
///
/// - macOS: `~/Library/Application Support/engramdb/`
/// - Linux: `$XDG_DATA_HOME/engramdb/` (default `~/.local/share/engramdb/`)
///
/// Used for per-project personal memories and LanceDB indices.
pub fn global_data_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("ENGRAMDB_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    dirs::data_dir()
        .ok_or_else(|| StorageError::Validation("Could not determine data directory".to_string()))
        .map(|p| p.join("engramdb"))
}

/// Returns the model cache directory.
///
/// Resolution order:
/// 1. `ENGRAMDB_MODEL_CACHE_DIR` if set (used verbatim — lets tests point at a
///    throwaway dir so model-presence assertions don't depend on whatever the
///    developer happens to have cached, mirroring `ENGRAMDB_DATA_DIR`).
/// 2. Platform cache dir otherwise:
///    - macOS: `~/Library/Caches/engramdb/models/`
///    - Linux: `$XDG_CACHE_HOME/engramdb/models/` (default `~/.cache/engramdb/models/`)
///
/// Used for embedding models, reranker models, and NLI models.
pub fn model_cache_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("ENGRAMDB_MODEL_CACHE_DIR") {
        return Ok(PathBuf::from(path));
    }
    dirs::cache_dir()
        .ok_or_else(|| StorageError::Validation("Could not determine cache directory".to_string()))
        .map(|p| p.join("engramdb").join("models"))
}

/// Whether EngramDB is in offline mode (`ENGRAMDB_OFFLINE` set to a truthy
/// value: `1`, `true`, `yes`, or `on`, case-insensitive).
///
/// In offline mode the model loaders refuse to hit the network: a model that
/// isn't already in [`model_cache_dir`] fails fast instead of being downloaded.
/// Combined with `ENGRAMDB_MODEL_CACHE_DIR` this makes model *presence*
/// deterministic in tests regardless of what the developer has cached.
pub fn offline_enabled() -> bool {
    matches!(
        std::env::var("ENGRAMDB_OFFLINE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Whether a HuggingFace Hub repo is already present in [`model_cache_dir`].
///
/// Checks the hf-hub cache layout (`<cache>/models--<org>--<repo>/snapshots/`),
/// which is the same layout both `fastembed` and raw `hf-hub` write. Used by the
/// model loaders to decide, in [`offline_enabled`] mode, whether a load can be
/// served from cache or would require a (forbidden) download.
pub fn hf_repo_cached(repo_id: &str) -> bool {
    let Ok(cache_dir) = model_cache_dir() else {
        return false;
    };
    let repo_dir = cache_dir.join(format!("models--{}", repo_id.replace('/', "--")));
    // A fully-pulled repo has at least one snapshot revision with files in it.
    match std::fs::read_dir(repo_dir.join("snapshots")) {
        Ok(mut entries) => entries.any(|e| e.is_ok()),
        Err(_) => false,
    }
}

/// Returns the personal project directory for a given project ID
pub fn personal_dir(project_id: &str) -> Result<PathBuf> {
    Ok(global_data_dir()?
        .join("projects")
        .join(project_id)
        .join("personal"))
}

/// Returns the personal memories directory for a given project ID
pub fn personal_memories_dir(project_id: &str) -> Result<PathBuf> {
    Ok(personal_dir(project_id)?.join("memories"))
}

/// Returns the global LanceDB directory for a given project ID.
pub fn lancedb_dir(project_id: &str) -> Result<PathBuf> {
    Ok(global_data_dir()?
        .join("projects")
        .join(project_id)
        .join("lancedb"))
}

/// Well-known project ID for the global memory store.
///
/// This is 16 characters (matching the project ID format) but starts with
/// underscores so it can never collide with a real SHA-256-derived hex ID.
pub const GLOBAL_PROJECT_ID: &str = "__global_store__";

/// Returns the root directory for the global memory store.
///
/// Layout mirrors a normal project:
///   `<global_data_dir>/global/.engramdb/memories/`
///   `<global_data_dir>/global/.engramdb/manifest.toml`
///   `<global_data_dir>/global/.engramdb/config.toml`
pub fn global_store_dir() -> Result<PathBuf> {
    Ok(global_data_dir()?.join("global"))
}

/// Returns the LanceDB directory for the global memory store.
pub fn global_lancedb_dir() -> Result<PathBuf> {
    Ok(global_data_dir()?.join("global").join("lancedb"))
}

/// Returns the root directory for a named group memory store.
///
/// A *group store* is the generalization of the global store: an ordinary
/// machine-local `MemoryStore` shared by a set of subscribed projects. Each
/// group lives under `<global_data_dir>/groups/<group_id>/` and mirrors a
/// normal project layout (`.engramdb/memories/`, `manifest.toml`, …) so every
/// `MemoryStore` method works unchanged.
pub fn group_store_dir(group_id: &str) -> Result<PathBuf> {
    Ok(global_data_dir()?.join("groups").join(group_id))
}

/// Returns the LanceDB directory for a named group memory store.
pub fn group_lancedb_dir(group_id: &str) -> Result<PathBuf> {
    Ok(global_data_dir()?
        .join("groups")
        .join(group_id)
        .join("lancedb"))
}

/// Compute a stable group ID from a human-readable group name.
///
/// The ID is 16 characters (matching the project ID width) but carries a
/// `__g_` prefix so it can *never* collide with a real 16-hex project ID (hex
/// digits never start with `_`) nor with [`GLOBAL_PROJECT_ID`] (`__global…`).
/// [`is_group_id`] recognizes it by that prefix.
///
/// The name is trimmed and lowercased before hashing, so `"Backend Family"`,
/// `" backend family "`, and `"backend family"` all resolve to the same group
/// — group identity is name-based and case/whitespace-insensitive, like a
/// slug. Twelve hex chars (6 bytes of SHA-256) follow the 4-char prefix for a
/// total width of 16.
pub fn compute_group_id(name: &str) -> String {
    let normalized = name.trim().to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let result = hasher.finalize();
    // Format the leading bytes explicitly rather than via `{:x}`: sha2 0.11's
    // `finalize` returns a `hybrid_array::Array` that no longer implements
    // `LowerHex` (see `project_id::hash_string`). Six bytes → twelve hex chars.
    let hex: String = result.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("__g_{hex}")
}

/// Whether `id` is a group store ID (as produced by [`compute_group_id`]).
pub fn is_group_id(id: &str) -> bool {
    id.starts_with("__g_")
}

/// Returns the global registry path (`<global_config_dir>/registry.json`).
///
/// Respects `ENGRAMDB_REGISTRY_PATH` env var for testing isolation.
pub fn registry_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("ENGRAMDB_REGISTRY_PATH") {
        return Ok(PathBuf::from(path));
    }
    Ok(global_config_dir()?.join("registry.json"))
}

/// Returns the memory file path for a given memory ID
/// Note: This function tries to find the memory in both shared and personal directories
/// by checking for files that start with the given ID prefix.
pub async fn memory_path(dir: &Path, id: &str) -> Option<PathBuf> {
    // Try shared memories first
    let shared_dir = memories_dir(dir);
    if let Some(path) = find_memory_in_dir(&shared_dir, id).await {
        return Some(path);
    }

    // Try personal memories
    // We need to compute project_id to find personal dir
    // For simplicity, we'll use the compute_project_id from project_id module
    use crate::project_id::compute_project_id;
    let project_id = compute_project_id(dir);
    if let Ok(personal_dir) = personal_memories_dir(&project_id) {
        return find_memory_in_dir(&personal_dir, id).await;
    }
    None
}

/// Helper function to find a memory file by ID prefix in a directory.
///
/// Handles both old (`<uuid>.md`) and new (`<slug>_<uuid>.md`) filename formats.
///
/// Matching strategy:
/// 1. Extract the UUID part from each file stem and check for exact or prefix match.
/// 2. If exactly one match is found, return it.
/// 3. If multiple matches are found, return `None` (ambiguous).
/// 4. If no matches are found, return `None`.
pub async fn find_memory_in_dir(dir: &Path, id: &str) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }

    let Ok(mut entries) = async_fs::read_dir(dir).await else {
        return None;
    };

    let mut prefix_matches: Vec<PathBuf> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if super::memory_file::stem_matches_id_prefix(stem, id) {
                let id_part = super::memory_file::extract_id_from_stem(stem);
                if id_part == id {
                    // Exact match — return immediately, no ambiguity.
                    return Some(path);
                }
                prefix_matches.push(path);
            }
        }
    }

    // Only return a prefix match if it's unambiguous (exactly one match).
    if prefix_matches.len() == 1 {
        return prefix_matches.into_iter().next();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_dir() {
        let path = Path::new("/tmp/my_project");
        let result = project_dir(path);
        assert_eq!(result, PathBuf::from("/tmp/my_project/.engramdb"));
    }

    // Scope matching is purely textual, so the natural relative spellings
    // (`./src/...`, doubled slashes) must be normalized or they silently
    // match no repo-relative scope at all.
    #[test]
    fn test_relativize_path_normalizes_relative_spellings() {
        let project = Path::new("/nonexistent/project");
        assert_eq!(
            relativize_path("./src/api/auth.rs", project),
            "src/api/auth.rs"
        );
        assert_eq!(
            relativize_path("src//api/auth.rs", project),
            "src/api/auth.rs"
        );
        assert_eq!(
            relativize_path("src/./api/auth.rs", project),
            "src/api/auth.rs"
        );
        // Plain relative paths pass through untouched.
        assert_eq!(
            relativize_path("src/api/auth.rs", project),
            "src/api/auth.rs"
        );
        // A dot inside a segment name is not a `.` segment.
        assert_eq!(relativize_path("x./y", project), "x./y");
        // `..` is preserved (matches no repo-relative scope, by design).
        assert_eq!(relativize_path("../other/f.rs", project), "../other/f.rs");
    }

    #[test]
    fn test_memories_dir() {
        let path = Path::new("/tmp/my_project");
        let result = memories_dir(path);
        assert_eq!(result, PathBuf::from("/tmp/my_project/.engramdb/memories"));
    }

    // Suffix assertions use component-wise `Path::ends_with` (like
    // `test_model_cache_dir_default_when_unset` below), NOT a stringified
    // comparison against a forward-slash literal — Windows renders these
    // paths with backslashes, so the string form fails there.
    #[test]
    fn test_personal_dir() {
        let result = personal_dir("abc123").unwrap();
        assert!(result.ends_with("projects/abc123/personal"), "{result:?}");
    }

    #[test]
    fn test_personal_memories_dir() {
        let result = personal_memories_dir("abc123").unwrap();
        assert!(result.ends_with("personal/memories"), "{result:?}");
    }

    #[test]
    fn test_lancedb_dir() {
        let result = lancedb_dir("abc123").unwrap();
        assert!(result.ends_with("projects/abc123/lancedb"), "{result:?}");
    }

    // Group store paths mirror the global-store layout under `groups/<id>/`.
    // Suffix assertions are component-wise (`Path::ends_with`) so they hold on
    // Windows, exactly like `test_lancedb_dir` above.
    #[test]
    fn test_group_store_dir() {
        let result = group_store_dir("__g_abc123def456").unwrap();
        assert!(result.ends_with("groups/__g_abc123def456"), "{result:?}");
    }

    #[test]
    fn test_group_lancedb_dir() {
        let result = group_lancedb_dir("__g_abc123def456").unwrap();
        assert!(
            result.ends_with("groups/__g_abc123def456/lancedb"),
            "{result:?}"
        );
    }

    #[test]
    fn test_compute_group_id_shape() {
        let id = compute_group_id("Backend Family");
        assert_eq!(id.len(), 16, "group id must be 16 chars: {id}");
        assert!(id.starts_with("__g_"), "group id must be prefixed: {id}");
        // The 12 chars after the prefix are lowercase hex.
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_compute_group_id_stable_and_normalized() {
        // Stable across calls, and case/whitespace-insensitive (name-based
        // identity, like a slug).
        let base = compute_group_id("Backend Family");
        assert_eq!(base, compute_group_id("Backend Family"));
        assert_eq!(base, compute_group_id("  backend family  "));
        assert_eq!(base, compute_group_id("BACKEND FAMILY"));
        // A different name must produce a different id.
        assert_ne!(base, compute_group_id("Frontend Family"));
    }

    #[test]
    fn test_is_group_id() {
        assert!(is_group_id(&compute_group_id("anything")));
        assert!(is_group_id("__g_0123456789ab"));
        assert!(!is_group_id("abc123def4567890")); // real 16-hex project id
        assert!(!is_group_id(GLOBAL_PROJECT_ID)); // the global store is not a group
    }

    #[test]
    fn test_global_data_dir() {
        let result = global_data_dir().unwrap();
        assert!(result.exists() || !result.to_string_lossy().is_empty());
    }

    #[test]
    fn test_model_cache_dir_env_override() {
        // `ENGRAMDB_MODEL_CACHE_DIR` wins over the platform cache dir and is used
        // verbatim. Safe to mutate the process env here: nextest runs each test
        // in its own process, and the var is unset elsewhere.
        std::env::set_var("ENGRAMDB_MODEL_CACHE_DIR", "/tmp/engramdb-override");
        assert_eq!(
            model_cache_dir().unwrap(),
            PathBuf::from("/tmp/engramdb-override")
        );
        std::env::remove_var("ENGRAMDB_MODEL_CACHE_DIR");
    }

    #[test]
    fn test_model_cache_dir_default_when_unset() {
        std::env::remove_var("ENGRAMDB_MODEL_CACHE_DIR");
        let result = model_cache_dir().unwrap();
        assert!(result.ends_with("engramdb/models"));
    }

    #[test]
    fn test_offline_enabled_truthy_and_falsy() {
        for truthy in ["1", "true", "TRUE", "Yes", "on", " on "] {
            std::env::set_var("ENGRAMDB_OFFLINE", truthy);
            assert!(offline_enabled(), "{truthy:?} should be offline");
        }
        for falsy in ["0", "false", "no", "off", ""] {
            std::env::set_var("ENGRAMDB_OFFLINE", falsy);
            assert!(!offline_enabled(), "{falsy:?} should not be offline");
        }
        std::env::remove_var("ENGRAMDB_OFFLINE");
        assert!(!offline_enabled(), "unset should not be offline");
    }

    #[test]
    fn test_hf_repo_cached_detects_snapshot_layout() {
        use tempfile::TempDir;
        let cache = TempDir::new().unwrap();
        std::env::set_var("ENGRAMDB_MODEL_CACHE_DIR", cache.path());

        // Absent → not cached.
        assert!(!hf_repo_cached("Some/Repo"));

        // hf-hub layout: <cache>/models--Some--Repo/snapshots/<rev>/file
        let snap = cache
            .path()
            .join("models--Some--Repo")
            .join("snapshots")
            .join("abc123");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(snap.join("model.onnx"), b"x").unwrap();
        assert!(hf_repo_cached("Some/Repo"));

        std::env::remove_var("ENGRAMDB_MODEL_CACHE_DIR");
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_with_exact_match() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let test_id = "abc123";
        let file_path = temp_dir.path().join(format!("{}.md", test_id));
        tokio::fs::write(&file_path, "test content").await.unwrap();

        let result = find_memory_in_dir(temp_dir.path(), test_id).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), file_path);
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_with_prefix_match() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let full_id = "abc123-456-789";
        let file_path = temp_dir.path().join(format!("{}.md", full_id));
        tokio::fs::write(&file_path, "test content").await.unwrap();

        let result = find_memory_in_dir(temp_dir.path(), "abc").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), file_path);
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_not_found() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let result = find_memory_in_dir(temp_dir.path(), "nonexistent").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_nonexistent_dir() {
        use std::path::Path;

        let nonexistent_path = Path::new("/nonexistent/directory");
        let result = find_memory_in_dir(nonexistent_path, "test").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_ambiguous_prefix_returns_none() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        // Create two files that share the prefix "abc"
        let file1 = temp_dir.path().join("abc-111.md");
        let file2 = temp_dir.path().join("abc-222.md");
        tokio::fs::write(&file1, "content 1").await.unwrap();
        tokio::fs::write(&file2, "content 2").await.unwrap();

        // Searching for "abc" should return None because of ambiguity
        let result = find_memory_in_dir(temp_dir.path(), "abc").await;
        assert!(
            result.is_none(),
            "Ambiguous prefix should return None, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_exact_match_over_prefix() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        // Create files: "abc.md" (exact) and "abc-extra.md" (prefix)
        let exact_file = temp_dir.path().join("abc.md");
        let prefix_file = temp_dir.path().join("abc-extra.md");
        tokio::fs::write(&exact_file, "exact").await.unwrap();
        tokio::fs::write(&prefix_file, "prefix").await.unwrap();

        // Should return the exact match despite prefix matches existing
        let result = find_memory_in_dir(temp_dir.path(), "abc").await;
        assert_eq!(result, Some(exact_file));
    }
}
