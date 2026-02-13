//! Initialize a new EngramDB store.

use crate::cli::output::OutputFormatter;
#[cfg(feature = "ollama")]
use crate::embeddings::{
    OllamaModelSpec, OllamaProvider, ALL_MINILM, MXBAI_EMBED_LARGE, NOMIC_EMBED_TEXT,
};
use crate::embeddings::{OnnxProvider, ONNX_MXBAI_EMBED_LARGE, ONNX_NOMIC_EMBED_TEXT};
use crate::storage::{MemoryStore, RegistryBackend};
use crate::types::EmbeddingBackend;
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
    embedding_backend: Option<EmbeddingBackend>,
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
        let config_path = dir.join(".engramdb/config.toml");
        let config = crate::storage::config::load_config(&config_path)
            .await
            .unwrap_or_default();

        let backend = crate::ops::resolve_backend(config.embeddings.backend, embedding_backend);

        match config.embeddings.provider.as_str() {
            "onnx" | "all-minilm" => {
                init_model_with_backend(
                    backend,
                    "all-minilm",
                    "~23MB",
                    || OnnxProvider::new().map(|_| ()),
                    #[cfg(feature = "ollama")]
                    ALL_MINILM,
                    formatter,
                )
                .await;
            }
            "nomic-embed-text" => {
                init_model_with_backend(
                    backend,
                    "nomic-embed-text",
                    "~270MB",
                    || OnnxProvider::with_model(ONNX_NOMIC_EMBED_TEXT).map(|_| ()),
                    #[cfg(feature = "ollama")]
                    NOMIC_EMBED_TEXT,
                    formatter,
                )
                .await;
            }
            "mxbai-embed-large" => {
                init_model_with_backend(
                    backend,
                    "mxbai-embed-large",
                    "~650MB",
                    || OnnxProvider::with_model(ONNX_MXBAI_EMBED_LARGE).map(|_| ()),
                    #[cfg(feature = "ollama")]
                    MXBAI_EMBED_LARGE,
                    formatter,
                )
                .await;
            }
            other => {
                formatter.print_error(&format!(
                    "Unknown embedding provider '{}', skipping initialization.",
                    other
                ));
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

/// Initialize an embedding model respecting the backend preference.
async fn init_model_with_backend(
    backend: EmbeddingBackend,
    model_name: &str,
    download_size: &str,
    try_onnx: impl FnOnce() -> anyhow::Result<()>,
    #[cfg(feature = "ollama")] ollama_spec: crate::embeddings::OllamaModelSpec,
    formatter: &OutputFormatter,
) {
    // Handle explicit Ollama backend
    if backend == EmbeddingBackend::Ollama {
        #[cfg(feature = "ollama")]
        {
            formatter.print_message(&format!(
                "Backend set to 'ollama', initializing {} via Ollama...",
                model_name
            ));
            init_ollama_model(ollama_spec, formatter).await;
        }
        #[cfg(not(feature = "ollama"))]
        {
            formatter.print_error(
                "Embedding backend 'ollama' selected but Ollama support is not compiled in.",
            );
        }
        return;
    }

    // Try ONNX (for Auto and Onnx backends)
    formatter.print_message(&format!(
        "Initializing {} model (first run downloads {})...",
        model_name, download_size
    ));
    match try_onnx() {
        Ok(()) => {
            formatter.print_success(&format!("Embedding model ready ({} via ONNX).", model_name));
        }
        Err(_) => {
            if backend == EmbeddingBackend::Onnx {
                formatter
                    .print_error("ONNX model unavailable (backend set to 'onnx', no fallback).");
                return;
            }
            // Auto mode: fall back to Ollama
            #[cfg(feature = "ollama")]
            {
                formatter.print_message("ONNX model unavailable, trying Ollama fallback...");
                init_ollama_model(ollama_spec, formatter).await;
            }
            #[cfg(not(feature = "ollama"))]
            {
                formatter.print_error("ONNX model unavailable and Ollama support is not enabled.");
            }
        }
    }
}

#[cfg(feature = "ollama")]
/// Try to initialize an Ollama-backed embedding model.
///
/// Checks if the model is already pulled; if not, auto-pulls it.
/// Prints progress and errors via the formatter.
async fn init_ollama_model(spec: OllamaModelSpec, formatter: &OutputFormatter) {
    let provider = match OllamaProvider::new(spec) {
        Ok(p) => p,
        Err(e) => {
            formatter.print_error(&format!("Could not create Ollama client: {}", e));
            return;
        }
    };

    match provider.check_model_available().await {
        Ok(true) => {
            formatter.print_success(&format!(
                "Embedding model ready ({} via Ollama).",
                spec.model_name
            ));
        }
        Ok(false) => {
            formatter.print_message(&format!(
                "Pulling embedding model '{}' from Ollama (this may take a minute)...",
                spec.model_name
            ));
            match provider.pull_model().await {
                Ok(()) => {
                    formatter.print_success(&format!(
                        "Embedding model ready ({} via Ollama).",
                        spec.model_name
                    ));
                }
                Err(e) => {
                    formatter.print_error(&format!("Failed to pull model: {}", e));
                    formatter.print_message(&provider.installation_hint());
                }
            }
        }
        Err(_) => {
            formatter.print_error("Could not connect to Ollama server. Is Ollama running?");
            formatter.print_message(&provider.installation_hint());
        }
    }
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

        let result = run_init(project_dir, &registry, true, None, None, &formatter).await;
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
            None,
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

        let result = run_init(project_dir, &registry, true, None, None, &formatter).await;
        assert!(result.is_ok(), "Init with --no-embeddings should succeed");
    }

    #[tokio::test]
    async fn test_init_writes_valid_registry() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        run_init(project_dir, &registry, true, None, None, &formatter)
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
