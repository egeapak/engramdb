//! Initialize a new EngramDB store.

use crate::cli::output::OutputFormatter;
use crate::embeddings::OnnxProvider;
use crate::storage::{paths, MemoryStore};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Registry entry for tracking a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryEntry {
    project_path: String,
    project_id: String,
    initialized_at: String,
}

/// Global registry of EngramDB projects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Registry {
    projects: Vec<RegistryEntry>,
}

/// Initialize a new EngramDB store in the specified directory.
///
/// Creates the `.engramdb/` directory structure and configuration files.
/// Optionally initializes embedding model and registers the project globally.
///
/// # Arguments
/// * `dir` - The directory to initialize the store in
/// * `no_embeddings` - Skip embedding model initialization
/// * `template` - Optional path to config template file
/// * `formatter` - Output formatter for success/error messages
pub fn run_init(
    dir: &Path,
    no_embeddings: bool,
    template: Option<PathBuf>,
    formatter: &OutputFormatter,
) -> Result<()> {
    // Initialize the store
    let store = MemoryStore::init(dir)?;

    // Copy template if provided
    if let Some(template_path) = template {
        let config_path = dir.join(".engramdb/config.toml");
        fs::copy(&template_path, &config_path)
            .with_context(|| format!("Failed to copy template from {}", template_path.display()))?;
        formatter.print_success("Applied config template");
    }

    // Initialize embeddings unless --no-embeddings
    if !no_embeddings {
        formatter.print_message("Initializing embedding model (first run downloads ~23MB)...");
        match OnnxProvider::new() {
            Ok(_) => {
                formatter.print_success("Embedding model ready.");
            }
            Err(e) => {
                formatter.print_error(&format!("Warning: Could not initialize embeddings: {}", e));
                formatter.print_message("Run 'engramdb reindex --embeddings-only' later to retry.");
            }
        }
    }

    // Update global registry
    update_global_registry(dir, &store.project_id)?;

    // Print success and helpful info
    formatter.print_success(&format!(
        "Initialized EngramDB store at {}",
        dir.join(".engramdb").display()
    ));
    formatter.print_message(&format!("Project ID: {}", store.project_id));
    formatter.print_message(
        "Try: engramdb add --type convention --summary 'Your first memory' --content '...'",
    );

    Ok(())
}

/// Update the global registry with this project.
fn update_global_registry(dir: &Path, project_id: &str) -> Result<()> {
    let registry_path = paths::registry_path()?;

    // Create registry directory if needed
    if let Some(parent) = registry_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Load or create registry
    let mut registry: Registry = if registry_path.exists() {
        let content = fs::read_to_string(&registry_path)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Registry::default()
    };

    // Get absolute path
    let abs_path = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let path_str = abs_path.to_string_lossy().to_string();

    // Check if project already exists (dedup by project_path)
    if !registry.projects.iter().any(|e| e.project_path == path_str) {
        registry.projects.push(RegistryEntry {
            project_path: path_str,
            project_id: project_id.to_string(),
            initialized_at: Utc::now().to_rfc3339(),
        });

        // Save registry
        let content = serde_json::to_string_pretty(&registry)?;
        fs::write(&registry_path, content)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::output::OutputFormatter;
    use crate::storage::project_id;
    use tempfile::TempDir;

    #[test]
    fn test_init_creates_structure() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_init(project_dir, true, None, &formatter);
        assert!(result.is_ok(), "Init should succeed");

        // Check main directories
        assert!(project_dir.join(".engramdb").exists());
        assert!(project_dir.join(".engramdb/memories").exists());

        // Check files
        assert!(project_dir.join(".engramdb/manifest.toml").exists());
        assert!(project_dir.join(".engramdb/config.toml").exists());
        assert!(project_dir.join(".engramdb/index.json").exists());
    }

    #[test]
    fn test_init_with_template() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let template_dir = TempDir::new().unwrap();
        let template_path = template_dir.path().join("template.toml");

        // Create a template file
        let template_content = "# Custom template\nsome_key = \"some_value\"\n";
        fs::write(&template_path, template_content).unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_init(project_dir, true, Some(template_path), &formatter);
        assert!(result.is_ok(), "Init with template should succeed");

        // Check that config was copied
        let config_path = project_dir.join(".engramdb/config.toml");
        assert!(config_path.exists());

        let config_content = fs::read_to_string(&config_path).unwrap();
        assert_eq!(config_content, template_content);
    }

    #[test]
    fn test_init_no_embeddings_flag() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let formatter = OutputFormatter::new(None, false, true);

        // This test verifies that --no-embeddings doesn't cause errors
        // (actual embedding init is tested separately in embeddings module)
        let result = run_init(project_dir, true, None, &formatter);
        assert!(result.is_ok(), "Init with --no-embeddings should succeed");
    }

    #[test]
    fn test_registry_creation_and_dedup() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        // Use a custom registry path for testing
        let registry_dir = TempDir::new().unwrap();
        let test_registry_path = registry_dir.path().join("test_registry.json");

        // Compute project ID
        let project_id = project_id::compute_project_id(project_dir);
        let abs_path = project_dir
            .canonicalize()
            .unwrap_or_else(|_| project_dir.to_path_buf());
        let path_str = abs_path.to_string_lossy().to_string();

        // Manually create registry directory and file for testing
        fs::create_dir_all(registry_dir.path()).unwrap();

        // Create initial registry
        let mut registry = Registry::default();
        registry.projects.push(RegistryEntry {
            project_path: path_str.clone(),
            project_id: project_id.clone(),
            initialized_at: Utc::now().to_rfc3339(),
        });
        let content = serde_json::to_string_pretty(&registry).unwrap();
        fs::write(&test_registry_path, content).unwrap();

        // Load registry and verify
        let loaded: Registry =
            serde_json::from_str(&fs::read_to_string(&test_registry_path).unwrap()).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_path, path_str);
        assert_eq!(loaded.projects[0].project_id, project_id);

        // Try adding the same project again (simulate dedup logic)
        if !loaded.projects.iter().any(|e| e.project_path == path_str) {
            // This should NOT execute because we're deduping
            panic!("Dedup logic failed");
        }
    }

    #[test]
    fn test_registry_multiple_projects() {
        let registry_dir = TempDir::new().unwrap();
        let test_registry_path = registry_dir.path().join("test_registry.json");
        fs::create_dir_all(registry_dir.path()).unwrap();

        // Create registry with multiple projects
        let mut registry = Registry::default();
        registry.projects.push(RegistryEntry {
            project_path: "/path/to/project1".to_string(),
            project_id: "id1".to_string(),
            initialized_at: Utc::now().to_rfc3339(),
        });
        registry.projects.push(RegistryEntry {
            project_path: "/path/to/project2".to_string(),
            project_id: "id2".to_string(),
            initialized_at: Utc::now().to_rfc3339(),
        });

        let content = serde_json::to_string_pretty(&registry).unwrap();
        fs::write(&test_registry_path, content).unwrap();

        // Load and verify
        let loaded: Registry =
            serde_json::from_str(&fs::read_to_string(&test_registry_path).unwrap()).unwrap();
        assert_eq!(loaded.projects.len(), 2);
        assert_eq!(loaded.projects[0].project_id, "id1");
        assert_eq!(loaded.projects[1].project_id, "id2");
    }
}
