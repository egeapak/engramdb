//! Update an existing memory.

use crate::output::OutputFormatter;
use crate::validation::validate_score;
use anyhow::{Context, Result};
use engramdb::daemon::{DaemonCell, DaemonPolicy};
use engramdb::ops::{self, parse_memory_type, parse_status, parse_visibility};
use engramdb::ops::{update_memory, UpdateParams as OpsUpdateParams};
use engramdb::storage::paths::memory_path;
use engramdb::storage::MemoryStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Parameters for the update command.
pub struct UpdateParams {
    pub id: String,
    pub type_: Option<String>,
    pub content: Option<String>,
    pub summary: Option<String>,
    pub title: Option<String>,
    pub physical: Vec<String>,
    pub logical: Vec<String>,
    pub tags: Vec<String>,
    pub tags_add: Option<String>,
    pub tags_remove: Option<String>,
    pub criticality: Option<f64>,
    pub confidence: Option<f64>,
    pub details: Option<String>,
    pub details_file: Option<PathBuf>,
    pub visibility: Option<String>,
    pub status: Option<String>,
    pub supersedes: Option<String>,
    pub decay_strategy: Option<String>,
    pub decay_half_life: Option<u64>,
    pub decay_ttl: Option<u64>,
    pub decay_floor: Option<f64>,
    pub editor: bool,
}

/// Update an existing memory.
///
/// Only the fields provided in params will be updated; others remain unchanged.
/// Automatically updates the `updated_at` timestamp.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `params` - Update parameters (only non-None fields are updated)
/// * `formatter` - Output formatter for success/error messages
pub async fn run_update(
    dir: &Path,
    global: bool,
    params: UpdateParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
) -> Result<()> {
    run_update_with_daemon(
        dir,
        global,
        params,
        embedding_backend,
        formatter,
        None,
        DaemonPolicy::InProcess,
    )
    .await
}

/// Like [`run_update`] but routes model resolution through the shared daemon cell.
pub async fn run_update_with_daemon(
    dir: &Path,
    global: bool,
    params: UpdateParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    cell: Option<&Arc<DaemonCell>>,
    policy: DaemonPolicy,
) -> Result<()> {
    // Resolve the directory backing the target store (global or project).
    let store_dir: PathBuf = if global {
        engramdb::storage::paths::global_store_dir()?
    } else {
        dir.to_path_buf()
    };

    // Handle editor flag first if present
    if params.editor {
        let memory_file_path = memory_path(&store_dir, &params.id)
            .await
            .with_context(|| format!("Memory {} not found", params.id))?;

        let editor_raw = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let editor_parts = shell_words::split(&editor_raw)
            .map_err(|e| anyhow::anyhow!("Invalid EDITOR value '{}': {}", editor_raw, e))?;
        let (editor_cmd, editor_args) = editor_parts
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("EDITOR environment variable is empty"))?;

        let status = std::process::Command::new(editor_cmd)
            .args(editor_args)
            .arg(&memory_file_path)
            .status()
            .with_context(|| format!("Failed to launch editor '{}'", editor_cmd))?;

        if !status.success() {
            anyhow::bail!("Editor exited with non-zero status");
        }

        formatter.print_success(&format!("Edited memory {} with {}", params.id, editor_cmd));

        // If only editor flag was provided, return early
        if params.type_.is_none()
            && params.content.is_none()
            && params.summary.is_none()
            && params.title.is_none()
            && params.physical.is_empty()
            && params.logical.is_empty()
            && params.tags.is_empty()
            && params.tags_add.is_none()
            && params.tags_remove.is_none()
            && params.criticality.is_none()
            && params.confidence.is_none()
            && params.details.is_none()
            && params.details_file.is_none()
            && params.visibility.is_none()
            && params.status.is_none()
            && params.supersedes.is_none()
            && params.decay_strategy.is_none()
            && params.decay_half_life.is_none()
            && params.decay_ttl.is_none()
            && params.decay_floor.is_none()
        {
            return Ok(());
        }
    }

    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };

    // Build engine for auto-embedding on update
    let config_path = store.project_dir.join(".engramdb").join("config.toml");
    let engine_store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    let engine = if let Some(c) = cell {
        let config = engramdb::storage::config::load_config_or_default(&config_path).await;
        let project_dir = engine_store.project_dir.clone();
        let providers = engramdb::daemon::resolve_providers(
            c,
            &config,
            embedding_backend,
            &project_dir,
            policy,
        )
        .await;
        ops::assemble_engine(engine_store, config, providers)
    } else {
        ops::build_engine(engine_store, &config_path, embedding_backend).await
    };

    let type_ = params.type_.map(|s| parse_memory_type(&s)).transpose()?;
    let visibility = params
        .visibility
        .map(|s| parse_visibility(&s))
        .transpose()?;
    let status = params.status.map(|s| parse_status(&s)).transpose()?;

    let physical = if params.physical.is_empty() {
        None
    } else {
        Some(params.physical)
    };

    let logical = if params.logical.is_empty() {
        None
    } else {
        Some(params.logical)
    };

    let tags = if params.tags.is_empty() {
        None
    } else {
        Some(params.tags)
    };

    // Parse comma-separated tags_add
    let tags_add = params.tags_add.map(|s| {
        s.split(',')
            .map(|tag| tag.trim().to_string())
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<String>>()
    });

    // Parse comma-separated tags_remove
    let tags_remove = params.tags_remove.map(|s| {
        s.split(',')
            .map(|tag| tag.trim().to_string())
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<String>>()
    });

    // Parse comma-separated supersedes
    let supersedes = params.supersedes.map(|s| {
        s.split(',')
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect::<Vec<String>>()
    });

    // Read details from file if provided
    let details = if let Some(file_path) = params.details_file {
        Some(
            std::fs::read_to_string(&file_path)
                .with_context(|| format!("Failed to read details file: {:?}", file_path))?,
        )
    } else {
        params.details
    };

    // Validate decay_floor if provided
    if let Some(floor) = params.decay_floor {
        validate_score(floor, "decay-floor")?;
    }

    update_memory(
        &store,
        &params.id,
        OpsUpdateParams {
            type_,
            content: params.content,
            summary: params.summary,
            title: params.title,
            physical,
            logical,
            tags,
            tags_add,
            tags_remove,
            criticality: params
                .criticality
                .map(|v| validate_score(v, "criticality"))
                .transpose()?,
            confidence: params
                .confidence
                .map(|v| validate_score(v, "confidence"))
                .transpose()?,
            details,
            visibility,
            status,
            supersedes,
            decay_strategy: params.decay_strategy,
            decay_half_life: params.decay_half_life,
            decay_ttl: params.decay_ttl,
            decay_floor: params.decay_floor,
            // CLI exits after this returns; embed inline so the work isn't
            // dropped with the runtime.
            embed_async: false,
        },
        Some(&engine),
    )
    .await?;

    formatter.print_success(&format!("Updated memory {}", params.id));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_parse_tags_add_comma_separated() {
        let tags_str = "tag1,tag2,tag3".to_string();
        let tags_vec: Vec<String> = tags_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert_eq!(tags_vec.len(), 3);
        assert_eq!(tags_vec[0], "tag1");
        assert_eq!(tags_vec[1], "tag2");
        assert_eq!(tags_vec[2], "tag3");
    }

    #[test]
    fn test_parse_tags_add_with_spaces() {
        let tags_str = "tag1, tag2 , tag3".to_string();
        let tags_vec: Vec<String> = tags_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert_eq!(tags_vec.len(), 3);
        assert_eq!(tags_vec[0], "tag1");
        assert_eq!(tags_vec[1], "tag2");
        assert_eq!(tags_vec[2], "tag3");
    }

    #[test]
    fn test_parse_tags_add_empty_string() {
        let tags_str = "".to_string();
        let tags_vec: Vec<String> = tags_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert_eq!(tags_vec.len(), 0);
    }

    #[test]
    fn test_parse_tags_remove_comma_separated() {
        let tags_str = "remove1,remove2".to_string();
        let tags_vec: Vec<String> = tags_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert_eq!(tags_vec.len(), 2);
        assert_eq!(tags_vec[0], "remove1");
        assert_eq!(tags_vec[1], "remove2");
    }

    #[test]
    fn test_parse_supersedes_comma_separated() {
        let supersedes_str = "id1,id2,id3".to_string();
        let supersedes_vec: Vec<String> = supersedes_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert_eq!(supersedes_vec.len(), 3);
        assert_eq!(supersedes_vec[0], "id1");
        assert_eq!(supersedes_vec[1], "id2");
        assert_eq!(supersedes_vec[2], "id3");
    }

    #[test]
    fn test_details_file_reading() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        let content = "Test details content\nWith multiple lines";
        temp_file.write_all(content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let file_path = temp_file.path().to_path_buf();
        let read_content = std::fs::read_to_string(&file_path).unwrap();

        assert_eq!(read_content, content);
    }

    #[test]
    fn test_parse_tags_with_empty_elements() {
        let tags_str = "tag1,,tag2,".to_string();
        let tags_vec: Vec<String> = tags_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert_eq!(tags_vec.len(), 2);
        assert_eq!(tags_vec[0], "tag1");
        assert_eq!(tags_vec[1], "tag2");
    }

    #[test]
    fn test_editor_splitting_with_args() {
        let parts = shell_words::split("code --wait").unwrap();
        let (cmd, args) = parts.split_first().unwrap();
        assert_eq!(*cmd, "code");
        assert_eq!(args, &["--wait"]);
    }

    #[test]
    fn test_editor_splitting_empty_returns_none() {
        let parts = shell_words::split("").unwrap();
        assert!(parts.split_first().is_none());
    }
}
