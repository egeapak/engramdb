//! Add a new memory to the store.

use crate::cli::output::OutputFormatter;
use crate::cli::prompter::Prompter;
use crate::cli::validation::validate_score;
use crate::ops::{self, create_memory, parse_memory_type, parse_visibility, CreateParams};
use crate::storage::{MemoryStore, RegistryBackend};
use crate::types::{MemoryType, Provenance, Visibility};
use anyhow::{anyhow, bail, Context, Result};
use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Parameters for the add command.
pub struct AddParams {
    pub type_str: Option<String>,
    pub content: Option<String>,
    pub summary: Option<String>,
    pub physical: Vec<String>,
    pub logical: Vec<String>,
    pub tags: Vec<String>,
    pub criticality: Option<f64>,
    pub confidence: f64,
    pub details: Option<String>,
    pub visibility_str: Option<String>,
    pub supersedes: Option<String>,
    pub decay_strategy: Option<String>,
    pub decay_half_life: Option<u64>,
    pub decay_ttl: Option<u64>,
    pub decay_floor: Option<f64>,
    pub interactive: bool,
    pub editor: bool,
    pub details_file: Option<PathBuf>,
}

/// Add a new memory to the store.
///
/// Creates a new memory with the specified parameters, automatically generating
/// a summary if not provided and defaulting the physical scope to "/" if empty.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `params` - Memory creation parameters
/// * `formatter` - Output formatter for success/error messages
pub async fn run_add(
    dir: &Path,
    registry: &dyn RegistryBackend,
    params: AddParams,
    embedding_backend: Option<crate::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
) -> Result<()> {
    // Open or initialize store
    let store = match MemoryStore::open(dir).await {
        Ok(s) => s,
        Err(_) => MemoryStore::init(dir, registry).await?,
    };

    // Build engine for auto-embedding on create
    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = ops::build_engine(store.clone(), &config_path, embedding_backend).await;

    // Handle details file
    let details_from_file = if let Some(ref details_file) = params.details_file {
        Some(fs::read_to_string(details_file).context("Failed to read details file")?)
    } else {
        None
    };

    let final_details = params.details.clone().or(details_from_file);

    // Determine mode: interactive, editor, or direct CLI
    if params.editor {
        // Editor mode
        run_editor_mode(&store, params, final_details, formatter, &engine).await
    } else if params.interactive
        || (params.type_str.is_none() || params.content.is_none() || params.summary.is_none())
    {
        // Check if we're in a terminal before trying interactive mode
        if !std::io::stdin().is_terminal() && !params.interactive {
            let mut missing = Vec::new();
            if params.type_str.is_none() {
                missing.push("--type");
            }
            if params.content.is_none() {
                missing.push("--content");
            }
            if params.summary.is_none() {
                missing.push("--summary");
            }
            bail!(
                "Missing required arguments: {}. Provide all required flags or run from an interactive terminal.",
                missing.join(", ")
            );
        }
        // Interactive mode: if --interactive is set OR if required fields are missing
        run_interactive_mode(&store, params, final_details, formatter, &engine, prompter).await
    } else {
        // Direct CLI mode: all required fields provided
        run_direct_mode(&store, params, final_details, formatter, &engine).await
    }
}

/// Run the add command in direct CLI mode.
async fn run_direct_mode(
    store: &MemoryStore,
    params: AddParams,
    final_details: Option<String>,
    formatter: &OutputFormatter,
    engine: &crate::retrieval::engine::RetrievalEngine,
) -> Result<()> {
    let type_ = parse_memory_type(
        params
            .type_str
            .as_deref()
            .ok_or_else(|| anyhow!("Type is required"))?,
    )?;
    let visibility = parse_visibility(params.visibility_str.as_deref().unwrap_or("shared"))?;

    let summary = params.summary.ok_or_else(|| {
        anyhow!("Summary is required. Use --summary or -s flag, or use interactive mode.")
    })?;

    // Parse comma-separated supersedes
    let supersedes = params
        .supersedes
        .map(|s| {
            s.split(',')
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    // Validate decay_floor if provided
    if let Some(floor) = params.decay_floor {
        validate_score(floor, "decay-floor")?;
    }

    let result = create_memory(
        store,
        CreateParams {
            type_,
            content: params
                .content
                .ok_or_else(|| anyhow!("Content is required"))?,
            summary,
            physical: params.physical,
            logical: params.logical,
            tags: params.tags,
            criticality: validate_score(params.criticality.unwrap_or(0.5), "criticality")?,
            confidence: validate_score(params.confidence, "confidence")?,
            details: final_details,
            visibility,
            provenance: Provenance::human(),
            supersedes,
            decay_strategy: params.decay_strategy,
            decay_half_life: params.decay_half_life,
            decay_ttl: params.decay_ttl,
            decay_floor: params.decay_floor,
        },
        Some(engine),
    )
    .await?;

    formatter.print_success(&format!("Created memory {}", result.id));
    Ok(())
}

/// Run the add command in interactive mode.
async fn run_interactive_mode(
    store: &MemoryStore,
    params: AddParams,
    final_details: Option<String>,
    formatter: &OutputFormatter,
    engine: &crate::retrieval::engine::RetrievalEngine,
    prompter: &dyn Prompter,
) -> Result<()> {
    // Prompt for memory type
    let type_ = if let Some(type_str) = params.type_str {
        parse_memory_type(&type_str)?
    } else {
        let options = vec![
            "decision",
            "convention",
            "hazard",
            "context",
            "intent",
            "relationship",
            "debug",
            "preference",
        ];
        let selected = prompter.select("Memory type:", &options)?;
        parse_memory_type(&selected)?
    };

    // Prompt for summary
    let summary = if let Some(s) = params.summary {
        s
    } else {
        prompter.text("Summary (required):", None)?
    };

    // Prompt for content
    let content = if let Some(c) = params.content {
        c
    } else {
        prompter.text("Content (required):", None)?
    };

    // Prompt for physical scope
    let physical = if !params.physical.is_empty() {
        params.physical
    } else {
        let physical_input =
            prompter.text("Physical scope (optional, e.g., src/**/*.rs):", Some(""))?;
        if physical_input.is_empty() {
            vec![]
        } else {
            vec![physical_input]
        }
    };

    // Prompt for logical scopes
    let logical = if !params.logical.is_empty() {
        params.logical
    } else {
        let logical_input =
            prompter.text("Logical scopes (optional, comma-separated):", Some(""))?;
        if logical_input.is_empty() {
            vec![]
        } else {
            logical_input
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
    };

    // Prompt for tags
    let tags = if !params.tags.is_empty() {
        params.tags
    } else {
        let tags_input = prompter.text("Tags (optional, comma-separated):", Some(""))?;
        if tags_input.is_empty() {
            vec![]
        } else {
            tags_input
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
    };

    // Prompt for criticality
    let criticality = if let Some(c) = params.criticality {
        c
    } else {
        let default_criticality = default_criticality_for_type(type_);
        prompter.float_validated("Criticality (0.0-1.0):", default_criticality)?
    };

    // Prompt for visibility
    let visibility = if let Some(vis_str) = params.visibility_str {
        parse_visibility(&vis_str)?
    } else {
        let options = vec!["shared", "personal"];
        let selected = prompter.select("Visibility:", &options)?;
        parse_visibility(&selected)?
    };

    let result = create_memory(
        store,
        CreateParams {
            type_,
            content,
            summary,
            physical,
            logical,
            tags,
            criticality,
            confidence: params.confidence,
            details: final_details,
            visibility,
            provenance: Provenance::human(),
            supersedes: vec![],
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
        },
        Some(engine),
    )
    .await?;

    formatter.print_success(&format!("Created memory {}", result.id));
    Ok(())
}

/// Run the add command in editor mode.
async fn run_editor_mode(
    store: &MemoryStore,
    params: AddParams,
    final_details: Option<String>,
    formatter: &OutputFormatter,
    engine: &crate::retrieval::engine::RetrievalEngine,
) -> Result<()> {
    // Create a temporary file with template
    let temp_dir = env::temp_dir();
    let temp_file = temp_dir.join(format!("engramdb-add-{}.txt", uuid::Uuid::new_v4()));

    let template = format!(
        "# Type: {} (decision, convention, hazard, context, intent, relationship, debug, preference)
# Summary: {}
# Tags: {}
# Physical: {}
# Logical: {}
# Criticality: {}
# Visibility: {} (shared, personal)

{}",
        params.type_str.as_deref().unwrap_or("convention"),
        params.summary.as_deref().unwrap_or(""),
        params.tags.join(", "),
        params.physical.join(", "),
        params.logical.join(", "),
        params
            .criticality
            .map(|c| c.to_string())
            .unwrap_or_else(|| "0.7".to_string()),
        params.visibility_str.as_deref().unwrap_or("shared"),
        params.content.as_deref().unwrap_or("")
    );

    fs::write(&temp_file, template).context("Failed to write template file")?;

    // Get editor from environment
    let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    // Open editor
    let status = Command::new(&editor)
        .arg(&temp_file)
        .status()
        .context("Failed to open editor")?;

    if !status.success() {
        bail!("Editor exited with non-zero status");
    }

    // Read back the file
    let file_contents = fs::read_to_string(&temp_file).context("Failed to read edited file")?;

    // Clean up temp file
    let _ = fs::remove_file(&temp_file);

    // Parse the file
    let parsed = parse_editor_template(&file_contents)?;

    let result = create_memory(
        store,
        CreateParams {
            type_: parsed.type_,
            content: parsed.content,
            summary: parsed.summary,
            physical: parsed.physical,
            logical: parsed.logical,
            tags: parsed.tags,
            criticality: parsed.criticality,
            confidence: params.confidence,
            details: final_details,
            visibility: parsed.visibility,
            provenance: Provenance::human(),
            supersedes: vec![],
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
        },
        Some(engine),
    )
    .await?;

    formatter.print_success(&format!("Created memory {}", result.id));
    Ok(())
}

/// Parsed template data from editor mode.
struct ParsedTemplate {
    type_: MemoryType,
    summary: String,
    tags: Vec<String>,
    physical: Vec<String>,
    logical: Vec<String>,
    criticality: f64,
    visibility: Visibility,
    content: String,
}

/// Parse the template file from editor mode.
fn parse_editor_template(contents: &str) -> Result<ParsedTemplate> {
    let mut type_str = String::from("convention");
    let mut summary = String::new();
    let mut tags = Vec::new();
    let mut physical = Vec::new();
    let mut logical = Vec::new();
    let mut criticality: f64 = 0.7;
    let mut visibility_str = String::from("shared");
    let mut content_lines = Vec::new();

    let mut in_content = false;

    for line in contents.lines() {
        if in_content {
            content_lines.push(line);
        } else if line.starts_with("# Type:") {
            let value = line
                .strip_prefix("# Type:")
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("convention");
            type_str = value.to_string();
        } else if line.starts_with("# Summary:") {
            summary = line
                .strip_prefix("# Summary:")
                .unwrap_or("")
                .trim()
                .to_string();
        } else if line.starts_with("# Tags:") {
            let value = line.strip_prefix("# Tags:").unwrap_or("").trim();
            if !value.is_empty() {
                tags = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        } else if line.starts_with("# Physical:") {
            let value = line.strip_prefix("# Physical:").unwrap_or("").trim();
            if !value.is_empty() {
                physical = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        } else if line.starts_with("# Logical:") {
            let value = line.strip_prefix("# Logical:").unwrap_or("").trim();
            if !value.is_empty() {
                logical = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        } else if line.starts_with("# Criticality:") {
            let value = line
                .strip_prefix("# Criticality:")
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("0.7");
            criticality = value.parse().unwrap_or(0.7);
        } else if line.starts_with("# Visibility:") {
            visibility_str = line
                .strip_prefix("# Visibility:")
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("shared")
                .to_string();
        } else if line.starts_with('#') {
            // Skip other comment lines
            continue;
        } else if !line.trim().is_empty() || in_content {
            // Start collecting content
            in_content = true;
            content_lines.push(line);
        }
    }

    let content = content_lines.join("\n").trim().to_string();

    if summary.is_empty() {
        bail!("Summary is required");
    }
    if content.is_empty() {
        bail!("Content is required");
    }

    Ok(ParsedTemplate {
        type_: parse_memory_type(&type_str)?,
        summary,
        tags,
        physical,
        logical,
        criticality: criticality.clamp(0.0, 1.0),
        visibility: parse_visibility(&visibility_str)?,
        content,
    })
}

/// Get default criticality for a memory type.
fn default_criticality_for_type(type_: MemoryType) -> f64 {
    match type_ {
        MemoryType::Decision => 0.9,
        MemoryType::Convention => 0.7,
        MemoryType::Hazard => 0.95,
        MemoryType::Context => 0.5,
        MemoryType::Intent => 0.6,
        MemoryType::Relationship => 0.6,
        MemoryType::Debug => 0.4,
        MemoryType::Preference => 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::prompter::MockPrompter;
    use crate::retrieval::engine::RetrievalEngine;
    use crate::storage::InMemoryRegistry;
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    /// Helper: create a store + engine pair for testing (no embeddings).
    async fn test_store_and_engine(
        dir: &std::path::Path,
        registry: &dyn RegistryBackend,
    ) -> (MemoryStore, RetrievalEngine) {
        let store = MemoryStore::init(dir, registry).await.unwrap();
        let engine_store = MemoryStore::open(dir).await.unwrap();
        let engine = RetrievalEngine::new(engine_store, EngramConfig::default());
        (store, engine)
    }

    #[tokio::test]
    async fn test_interactive_add_all_prompted() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (store, engine) = test_store_and_engine(temp_dir.path(), &registry).await;
        let formatter = crate::cli::output::OutputFormatter::new(None, false, true);

        // Mock responses: type, summary, content, physical, logical, tags, criticality, visibility
        let prompter = MockPrompter::new(vec![
            "decision",
            "Test summary",
            "Test content",
            "", // physical (empty => default)
            "", // logical (empty => default)
            "", // tags (empty => default)
            "0.7",
            "shared",
        ]);

        let params = AddParams {
            type_str: None,
            content: None,
            summary: None,
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: None,
            confidence: 0.8,
            details: None,
            visibility_str: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            interactive: true,
            editor: false,
            details_file: None,
        };

        run_interactive_mode(&store, params, None, &formatter, &engine, &prompter)
            .await
            .unwrap();

        // Verify memory was created
        let ids = store.list_ids().await.unwrap();
        assert_eq!(ids.len(), 1);
        let memory = store.get(&ids[0]).await.unwrap();
        assert_eq!(memory.type_, MemoryType::Decision);
        assert_eq!(memory.summary, "Test summary");
        assert_eq!(memory.content, "Test content");
        assert_eq!(memory.visibility, Visibility::Shared);
    }

    #[tokio::test]
    async fn test_interactive_add_with_presets() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (store, engine) = test_store_and_engine(temp_dir.path(), &registry).await;
        let formatter = crate::cli::output::OutputFormatter::new(None, false, true);

        // Only prompts for fields not provided: physical, logical, tags, criticality, visibility
        let prompter = MockPrompter::new(vec![
            "",    // physical
            "",    // logical
            "",    // tags
            "0.9", // criticality
            "personal",
        ]);

        let params = AddParams {
            type_str: Some("hazard".to_string()),
            content: Some("Preset content".to_string()),
            summary: Some("Preset summary".to_string()),
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: None,
            confidence: 0.8,
            details: None,
            visibility_str: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            interactive: true,
            editor: false,
            details_file: None,
        };

        run_interactive_mode(&store, params, None, &formatter, &engine, &prompter)
            .await
            .unwrap();

        let ids = store.list_ids().await.unwrap();
        assert_eq!(ids.len(), 1);
        let memory = store.get(&ids[0]).await.unwrap();
        assert_eq!(memory.type_, MemoryType::Hazard);
        assert_eq!(memory.summary, "Preset summary");
        assert_eq!(memory.content, "Preset content");
        assert_eq!(memory.visibility, Visibility::Personal);
    }

    #[tokio::test]
    async fn test_interactive_add_with_tags_and_scopes() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (store, engine) = test_store_and_engine(temp_dir.path(), &registry).await;
        let formatter = crate::cli::output::OutputFormatter::new(None, false, true);

        let prompter = MockPrompter::new(vec![
            "convention",
            "Scoped memory",
            "Content here",
            "src/**/*.rs",       // physical
            "auth, database",    // logical (comma-separated)
            "rust, testing, ci", // tags (comma-separated)
            "0.8",
            "shared",
        ]);

        let params = AddParams {
            type_str: None,
            content: None,
            summary: None,
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: None,
            confidence: 0.8,
            details: None,
            visibility_str: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            interactive: true,
            editor: false,
            details_file: None,
        };

        run_interactive_mode(&store, params, None, &formatter, &engine, &prompter)
            .await
            .unwrap();

        let ids = store.list_ids().await.unwrap();
        assert_eq!(ids.len(), 1);
        let memory = store.get(&ids[0]).await.unwrap();
        assert_eq!(memory.physical, vec!["src/**/*.rs"]);
        assert_eq!(memory.logical, vec!["auth", "database"]);
        assert_eq!(memory.tags, vec!["rust", "testing", "ci"]);
    }

    #[tokio::test]
    async fn test_interactive_add_criticality_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (store, engine) = test_store_and_engine(temp_dir.path(), &registry).await;
        let formatter = crate::cli::output::OutputFormatter::new(None, false, true);

        // For criticality, pass empty string => use default for hazard type (0.95)
        let prompter = MockPrompter::new(vec![
            "hazard",
            "Critical hazard",
            "Don't do this",
            "", // physical
            "", // logical
            "", // tags
            "", // criticality (empty => default for hazard = 0.95)
            "shared",
        ]);

        let params = AddParams {
            type_str: None,
            content: None,
            summary: None,
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: None,
            confidence: 0.8,
            details: None,
            visibility_str: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            interactive: true,
            editor: false,
            details_file: None,
        };

        run_interactive_mode(&store, params, None, &formatter, &engine, &prompter)
            .await
            .unwrap();

        let ids = store.list_ids().await.unwrap();
        let memory = store.get(&ids[0]).await.unwrap();
        assert_eq!(memory.criticality, 0.95);
    }

    #[test]
    fn test_parse_editor_template_full() {
        let template = r#"# Type: hazard
# Summary: Use snake_case for variables
# Tags: rust, style
# Physical: src/**/*.rs
# Logical: coding.style
# Criticality: 0.8
# Visibility: shared

Always use snake_case for variable names in Rust code.
This is required by rustfmt and clippy."#;

        let parsed = parse_editor_template(template).unwrap();
        assert_eq!(parsed.type_, MemoryType::Hazard);
        assert_eq!(parsed.summary, "Use snake_case for variables");
        assert_eq!(parsed.tags, vec!["rust", "style"]);
        assert_eq!(parsed.physical, vec!["src/**/*.rs"]);
        assert_eq!(parsed.logical, vec!["coding.style"]);
        assert_eq!(parsed.criticality, 0.8);
        assert_eq!(parsed.visibility, Visibility::Shared);
        assert!(parsed.content.contains("Always use snake_case"));
    }

    #[test]
    fn test_parse_editor_template_minimal() {
        let template = r#"# Type: context
# Summary: Project uses Rust
# Tags:
# Physical:
# Logical:
# Criticality: 0.5
# Visibility: shared

This project is written in Rust."#;

        let parsed = parse_editor_template(template).unwrap();
        assert_eq!(parsed.type_, MemoryType::Context);
        assert_eq!(parsed.summary, "Project uses Rust");
        assert!(parsed.tags.is_empty());
        assert!(parsed.physical.is_empty());
        assert!(parsed.logical.is_empty());
        assert_eq!(parsed.criticality, 0.5);
    }

    #[test]
    fn test_parse_editor_template_missing_summary() {
        let template = r#"# Type: context
# Summary:
# Tags:
# Physical:
# Logical:
# Criticality: 0.5
# Visibility: shared

Content here."#;

        assert!(parse_editor_template(template).is_err());
    }

    #[test]
    fn test_parse_editor_template_missing_content() {
        let template = r#"# Type: context
# Summary: Test summary
# Tags:
# Physical:
# Logical:
# Criticality: 0.5
# Visibility: shared

"#;

        assert!(parse_editor_template(template).is_err());
    }

    #[test]
    fn test_parse_editor_template_multiline_content() {
        let template = r#"# Type: convention
# Summary: Use multiline strings
# Tags:
# Physical:
# Logical:
# Criticality: 0.5
# Visibility: shared

Line 1
Line 2
Line 3"#;

        let parsed = parse_editor_template(template).unwrap();
        assert_eq!(parsed.content, "Line 1\nLine 2\nLine 3");
    }

    #[test]
    fn test_default_criticality_for_type() {
        assert_eq!(default_criticality_for_type(MemoryType::Decision), 0.9);
        assert_eq!(default_criticality_for_type(MemoryType::Convention), 0.7);
        assert_eq!(default_criticality_for_type(MemoryType::Hazard), 0.95);
        assert_eq!(default_criticality_for_type(MemoryType::Context), 0.5);
        assert_eq!(default_criticality_for_type(MemoryType::Intent), 0.6);
        assert_eq!(default_criticality_for_type(MemoryType::Relationship), 0.6);
        assert_eq!(default_criticality_for_type(MemoryType::Debug), 0.4);
        assert_eq!(default_criticality_for_type(MemoryType::Preference), 0.5);
    }
}
