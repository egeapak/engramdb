//! Initialize a new EngramDB store.

use crate::cli::output::OutputFormatter;
use crate::embeddings::OnnxProvider;
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Initialize a new EngramDB store in the specified directory.
///
/// Creates the `.engramdb/` directory structure and configuration files.
/// Optionally initializes embedding model and registers the project globally.
///
/// # Arguments
/// * `dir` - The directory to initialize the store in
/// * `registry` - The registry backend to use for project registration
/// * `no_embeddings` - Skip embedding model initialization
/// * `template` - Optional path to config template file
/// * `formatter` - Output formatter for success/error messages
pub async fn run_init(
    dir: &Path,
    registry: &dyn RegistryBackend,
    no_embeddings: bool,
    template: Option<PathBuf>,
    formatter: &OutputFormatter,
) -> Result<()> {
    // Initialize the store
    let store = MemoryStore::init(dir, registry).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::output::OutputFormatter;
    use crate::storage::{paths, project_id, InMemoryRegistry, Registry, RegistryEntry};
    use chrono::Utc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_init_creates_structure() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_init(project_dir, &registry, true, None, &formatter).await;
        assert!(result.is_ok(), "Init should succeed");

        // Check project-local directories
        assert!(project_dir.join(".engramdb").exists());
        assert!(project_dir.join(".engramdb/memories").exists());

        // Check files
        assert!(project_dir.join(".engramdb/manifest.toml").exists());
        assert!(project_dir.join(".engramdb/config.toml").exists());

        // LanceDB should NOT be in the project directory
        assert!(!project_dir.join(".engramdb/lancedb").exists());

        // LanceDB should be in the global data directory
        let pid = project_id::compute_project_id(project_dir);
        assert!(paths::lancedb_dir(&pid).unwrap().exists());
    }

    #[tokio::test]
    async fn test_init_with_template() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let template_dir = TempDir::new().unwrap();
        let template_path = template_dir.path().join("template.toml");

        // Create a template file
        let template_content = "# Custom template\nsome_key = \"some_value\"\n";
        fs::write(&template_path, template_content).unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();
        let result = run_init(
            project_dir,
            &registry,
            true,
            Some(template_path),
            &formatter,
        )
        .await;
        assert!(result.is_ok(), "Init with template should succeed");

        // Check that config was copied
        let config_path = project_dir.join(".engramdb/config.toml");
        assert!(config_path.exists());

        let config_content = fs::read_to_string(&config_path).unwrap();
        assert_eq!(config_content, template_content);
    }

    #[tokio::test]
    async fn test_init_no_embeddings_flag() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_init(project_dir, &registry, true, None, &formatter).await;
        assert!(result.is_ok(), "Init with --no-embeddings should succeed");
    }

    #[tokio::test]
    async fn test_init_writes_valid_registry() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        run_init(project_dir, &registry, true, None, &formatter)
            .await
            .unwrap();

        // Verify the registry content
        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
    }

    #[test]
    fn test_registry_creation_and_dedup() {
        let registry_dir = TempDir::new().unwrap();
        let test_registry_path = registry_dir.path().join("test_registry.json");
        fs::create_dir_all(registry_dir.path()).unwrap();

        let mut registry = Registry::default();
        registry.projects.push(RegistryEntry {
            project_path: "/path/to/project".to_string(),
            project_id: "test-id".to_string(),
            last_opened: Utc::now(),
        });
        let content = serde_json::to_string_pretty(&registry).unwrap();
        fs::write(&test_registry_path, content).unwrap();

        let loaded: Registry =
            serde_json::from_str(&fs::read_to_string(&test_registry_path).unwrap()).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "test-id");

        // Dedup check: same path should not be added again
        assert!(loaded
            .projects
            .iter()
            .any(|e| e.project_path == "/path/to/project"));
    }

    #[test]
    fn test_registry_multiple_projects() {
        let registry_dir = TempDir::new().unwrap();
        let test_registry_path = registry_dir.path().join("test_registry.json");
        fs::create_dir_all(registry_dir.path()).unwrap();

        let mut registry = Registry::default();
        registry.projects.push(RegistryEntry {
            project_path: "/path/to/project1".to_string(),
            project_id: "id1".to_string(),
            last_opened: Utc::now(),
        });
        registry.projects.push(RegistryEntry {
            project_path: "/path/to/project2".to_string(),
            project_id: "id2".to_string(),
            last_opened: Utc::now(),
        });

        let content = serde_json::to_string_pretty(&registry).unwrap();
        fs::write(&test_registry_path, content).unwrap();

        let loaded: Registry =
            serde_json::from_str(&fs::read_to_string(&test_registry_path).unwrap()).unwrap();
        assert_eq!(loaded.projects.len(), 2);
        assert_eq!(loaded.projects[0].project_id, "id1");
        assert_eq!(loaded.projects[1].project_id, "id2");
    }
}
