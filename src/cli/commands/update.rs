//! Update an existing memory.

use crate::cli::output::OutputFormatter;
use crate::ops::{self, parse_memory_type, parse_status, parse_visibility};
use crate::ops::{update_memory, UpdateParams as OpsUpdateParams};
use crate::storage::paths::memory_path;
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Parameters for the update command.
pub struct UpdateParams {
    pub id: String,
    pub type_: Option<String>,
    pub content: Option<String>,
    pub summary: Option<String>,
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
    pub editor: bool,
}

/// Update an existing memory.
///
/// Only the fields provided in params will be updated; others remain unchanged.
/// Automatically updates the `updated_at` timestamp.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `params` - Update parameters (only non-None fields are updated)
/// * `formatter` - Output formatter for success/error messages
pub async fn run_update(
    dir: &Path,
    registry: &dyn RegistryBackend,
    params: UpdateParams,
    formatter: &OutputFormatter,
) -> Result<()> {
    // Handle editor flag first if present
    if params.editor {
        let memory_file_path = memory_path(dir, &params.id)
            .await
            .with_context(|| format!("Memory {} not found", params.id))?;

        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

        let status = std::process::Command::new(&editor)
            .arg(&memory_file_path)
            .status()
            .with_context(|| format!("Failed to launch editor '{}'", editor))?;

        if !status.success() {
            anyhow::bail!("Editor exited with non-zero status");
        }

        formatter.print_success(&format!("Edited memory {} with {}", params.id, editor));

        // If only editor flag was provided, return early
        if params.type_.is_none()
            && params.content.is_none()
            && params.summary.is_none()
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
        {
            return Ok(());
        }
    }

    let store = MemoryStore::open(dir, registry).await?;

    // Build engine for auto-embedding on update
    let config_path = dir.join(".engramdb").join("config.toml");
    let engine_store = MemoryStore::open(dir, registry).await?;
    let engine = ops::build_engine(engine_store, &config_path).await;

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

    update_memory(
        &store,
        &params.id,
        OpsUpdateParams {
            type_,
            content: params.content,
            summary: params.summary,
            physical,
            logical,
            tags,
            tags_add,
            tags_remove,
            criticality: params.criticality,
            confidence: params.confidence,
            details,
            visibility,
            status,
            supersedes,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
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
}
