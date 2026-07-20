//! Manifest file read/write operations.
//!
//! This module manages the manifest.toml file, which stores project metadata
//! and statistics. The manifest includes:
//! - Schema version (for future format changes)
//! - Project name and description
//! - Creation timestamp
//! - Statistics (memory count, logical scopes)
//!
//! The manifest is automatically updated when memories are created, updated,
//! or deleted, ensuring statistics remain accurate.

use super::error::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Current memories-table schema version. Bumped when the LanceDB `memories`
/// table gains columns so an existing store can be migrated (rebuilt from its
/// `.md` files) on open. `0.2.0` added the `decay` + `has_embedding` columns
/// (R2/R3); `0.3.0` added the seven epistemic columns (`epistemic`,
/// `verified_at`, `generality`, `origin_task`, `valid_from`,
/// `invalidated_at`, `watch_paths`); `0.4.0` added the `audience` column
/// (multi-project memories). A store whose manifest records an older
/// version is transparently re-indexed once on open (seconds, no re-embed)
/// and stamped up to this.
pub const CURRENT_SCHEMA_VERSION: &str = "0.4.0";

/// The pre-migration baseline, used as the serde default so a manifest written
/// before the field existed parses as "needs migration" rather than failing.
fn default_schema_version() -> String {
    "0.1.0".to_string()
}

/// True when a store recording `stored` needs **no** schema migration — i.e. it
/// is at or ahead of [`CURRENT_SCHEMA_VERSION`].
///
/// The comparison is by semantic-version ordering, not string equality: a
/// version *behind* current (or an unparseable one) needs migration, but a
/// version *ahead* — written by a newer binary against the same store — must
/// **not** be "migrated". Reindexing a newer store with this binary's older
/// column set would rebuild the table without the newer columns and stamp the
/// version backwards, silently dropping data. When ahead, we leave it untouched.
pub fn schema_version_is_current(stored: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let mut it = v.split('.').map(|p| p.parse::<u64>().ok());
        let major = it.next()??;
        let minor = it.next().unwrap_or(Some(0))?;
        let patch = it.next().unwrap_or(Some(0))?;
        Some((major, minor, patch))
    }
    match (parse(stored), parse(CURRENT_SCHEMA_VERSION)) {
        (Some(s), Some(c)) => s >= c,
        // Unparseable stored version → treat as needing migration.
        _ => false,
    }
}

/// Project manifest stored in manifest.toml.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version for the on-disk LanceDB memories table (see
    /// [`CURRENT_SCHEMA_VERSION`]).
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    /// Project name
    pub project: String,
    /// When this manifest was created
    pub created_at: DateTime<Utc>,
    /// Human-readable project description
    pub description: String,
    /// If this project is a sub-project of another project, the parent's
    /// project ID.  Primarily surfaced here for `engramdb projects info`;
    /// the registry is the source of truth for hierarchy routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_project_id: Option<String>,
    /// Project statistics (updated automatically)
    pub stats: ManifestStats,
    /// Identity of the embedding model the stored vectors were produced
    /// with. `None` on legacy stores created before model tracking
    /// (treated as "untracked" — a reindex stamps it). Used to detect a
    /// model swap so search isn't silently served from mixed/stale vectors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingFingerprint>,
}

/// Composition id recorded when the metadata-vector embed composition is in
/// use (`embeddings.metadata_vector = true`): a per-memory
/// `"{title}. {summary}. tags: …"` row plus content-only chunks. The absent /
/// `None` composition means the legacy `"{summary} {content}"` single text —
/// which is exactly what every pre-tracking manifest deserializes to, so
/// upgraded stores are detected without a schema migration. Defined in
/// `engram-types` next to `EmbeddingsConfig::composition_id` (the single
/// config→manifest mapping); re-exported here beside the fingerprint it
/// stamps.
pub use engram_types::COMPOSITION_METADATA_V1;

/// Identity of the embedding model a store's vectors were generated with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    /// Stable model id, e.g. `onnx/all-MiniLM-L6-v2-q` or
    /// `ollama/all-minilm` (from `EmbeddingProvider::model_id`).
    pub model: String,
    /// Embedding dimensionality (also baked into the Arrow schema; kept
    /// here for diagnostics and early, clear dimension-change detection).
    pub dimensions: usize,
    /// Embed-text composition the vectors were built from. `None` = legacy
    /// `"{summary} {content}"`; [`COMPOSITION_METADATA_V1`] = metadata row +
    /// content chunks. Same model, different text — a mismatch means
    /// title/tag signal is missing from (or unexpectedly present in) stored
    /// vectors until a `reindex --embeddings-only`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<String>,
}

/// How a store's stored embedding fingerprint compares to the embedding
/// model currently in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingModelStatus {
    /// Stored vectors were produced by the model currently in use.
    Match,
    /// A different model is in use — search would mix/stale-compare vectors.
    Mismatch { stored: String, current: String },
    /// Embedding dimensionality changed (writes would fail the Arrow schema).
    DimensionMismatch { stored: usize, current: usize },
    /// Same model, but the embed-text composition changed (e.g. the
    /// metadata-vector default flipped on upgrade): old vectors lack the
    /// metadata row, so ranking is skewed between old and new memories.
    CompositionMismatch {
        stored: Option<String>,
        current: Option<String>,
    },
    /// Legacy store with no fingerprint — model identity unknown/unverified.
    Untracked { current: String },
}

impl EmbeddingModelStatus {
    /// Whether stored vectors are safe to search with the current model.
    pub fn is_consistent(&self) -> bool {
        matches!(self, EmbeddingModelStatus::Match)
    }
}

impl EmbeddingFingerprint {
    /// Compare this stored fingerprint against the model currently in use.
    pub fn status(
        &self,
        current_model: &str,
        current_dims: usize,
        current_composition: Option<&str>,
    ) -> EmbeddingModelStatus {
        if self.dimensions != current_dims {
            EmbeddingModelStatus::DimensionMismatch {
                stored: self.dimensions,
                current: current_dims,
            }
        } else if self.model != current_model {
            EmbeddingModelStatus::Mismatch {
                stored: self.model.clone(),
                current: current_model.to_string(),
            }
        } else if self.composition.as_deref() != current_composition {
            EmbeddingModelStatus::CompositionMismatch {
                stored: self.composition.clone(),
                current: current_composition.map(str::to_string),
            }
        } else {
            EmbeddingModelStatus::Match
        }
    }
}

/// Status of `stored` (the manifest fingerprint, or `None` for a legacy
/// store) against the model currently in use.
pub fn embedding_status(
    stored: Option<&EmbeddingFingerprint>,
    current_model: &str,
    current_dims: usize,
    current_composition: Option<&str>,
) -> EmbeddingModelStatus {
    match stored {
        Some(fp) => fp.status(current_model, current_dims, current_composition),
        None => EmbeddingModelStatus::Untracked {
            current: current_model.to_string(),
        },
    }
}

/// Statistics tracked in the manifest.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestStats {
    /// Total number of memories (shared + personal)
    pub memory_count: usize,
    /// All unique logical scopes used in memories
    pub logical_scopes: Vec<String>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            // New stores are born at the current schema (their empty tables
            // already have the latest columns), so they never migrate.
            schema_version: CURRENT_SCHEMA_VERSION.to_string(),
            project: "engramdb-project".to_string(),
            created_at: Utc::now(),
            description: "Agent memory store. See config.toml for retrieval settings.".to_string(),
            parent_project_id: None,
            stats: ManifestStats {
                memory_count: 0,
                logical_scopes: Vec::new(),
            },
            embedding: None,
        }
    }
}

/// Load manifest from manifest.toml
pub async fn load_manifest(path: &Path) -> Result<Manifest> {
    let content = tokio::fs::read_to_string(path).await?;
    let manifest: Manifest = toml::from_str(&content)?;
    Ok(manifest)
}

/// Save manifest to manifest.toml atomically via write-to-temp-then-rename.
pub async fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let content = toml::to_string_pretty(manifest)
        .map_err(|e| super::error::StorageError::Validation(e.to_string()))?;
    super::store::atomic_write(path, &content).await?;
    Ok(())
}

/// Update manifest stats (memory count and logical scopes)
pub fn update_stats(manifest: &mut Manifest, memory_count: usize, logical_scopes: Vec<String>) {
    manifest.stats.memory_count = memory_count;
    manifest.stats.logical_scopes = logical_scopes;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_manifest_default() {
        let manifest = Manifest::default();
        assert_eq!(manifest.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(manifest.stats.memory_count, 0);
        assert!(manifest.stats.logical_scopes.is_empty());
    }

    /// A manifest written before `schema_version` existed parses as the
    /// pre-migration baseline (so it triggers migration), not a parse error.
    #[test]
    fn legacy_manifest_without_schema_version_defaults_to_baseline() {
        let toml = r#"
project = "p"
created_at = "2020-01-01T00:00:00Z"
description = "d"
[stats]
memory_count = 0
logical_scopes = []
"#;
        let m: Manifest = toml::from_str(toml).unwrap();
        assert_eq!(m.schema_version, "0.1.0");
        assert_ne!(m.schema_version, CURRENT_SCHEMA_VERSION);
    }

    /// The migration gate compares by version *ordering*, not equality: a store
    /// behind current migrates, one at/ahead of current does not (so a newer
    /// binary's store is never silently downgraded), and garbage migrates.
    #[test]
    fn schema_version_ordering_gates_migration() {
        // At current → no migration.
        assert!(schema_version_is_current(CURRENT_SCHEMA_VERSION));
        // Behind current → needs migration.
        assert!(!schema_version_is_current("0.1.0"));
        assert!(!schema_version_is_current("0.1.9"));
        assert!(!schema_version_is_current("0.2.0"));
        assert!(!schema_version_is_current("0.3.0")); // pre-audience, behind 0.4.0
                                                      // Ahead of current → must NOT migrate (no silent downgrade).
        assert!(schema_version_is_current("0.5.0"));
        assert!(schema_version_is_current("1.0.0"));
        // Short / unparseable forms.
        assert!(!schema_version_is_current("0.3")); // 0.3.0, behind 0.4.0
        assert!(!schema_version_is_current("0.2")); // 0.2.0, behind
        assert!(!schema_version_is_current("garbage"));
        assert!(!schema_version_is_current(""));
    }

    #[tokio::test]
    async fn test_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.toml");

        let mut original = Manifest {
            project: "test_project".to_string(),
            description: "Test description".to_string(),
            ..Default::default()
        };
        original.stats.memory_count = 42;
        original.stats.logical_scopes = vec!["scope1".to_string(), "scope2".to_string()];

        save_manifest(&manifest_path, &original).await.unwrap();
        let loaded = load_manifest(&manifest_path).await.unwrap();

        assert_eq!(loaded.schema_version, original.schema_version);
        assert_eq!(loaded.project, original.project);
        assert_eq!(loaded.description, original.description);
        assert_eq!(loaded.stats.memory_count, original.stats.memory_count);
        assert_eq!(loaded.stats.logical_scopes, original.stats.logical_scopes);
    }

    #[tokio::test]
    async fn test_load_manifest_file_not_found() {
        let result = load_manifest(Path::new("/nonexistent/path/manifest.toml")).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            super::super::error::StorageError::Io(_)
        ));
    }

    #[tokio::test]
    async fn test_load_manifest_invalid_toml() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.toml");

        tokio::fs::write(&manifest_path, "invalid { toml content")
            .await
            .unwrap();

        let result = load_manifest(&manifest_path).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            super::super::error::StorageError::Toml(_)
        ));
    }

    // --- Embedding model-identity lifecycle (silent-corruption guards) ---

    #[test]
    fn embedding_status_truth_table() {
        let fp = EmbeddingFingerprint {
            model: "onnx/all-MiniLM-L6-v2-q".to_string(),
            dimensions: 384,
            composition: None,
        };

        // Match: same model, same dims, same (legacy) composition.
        assert_eq!(
            embedding_status(Some(&fp), "onnx/all-MiniLM-L6-v2-q", 384, None),
            EmbeddingModelStatus::Match
        );
        assert!(embedding_status(Some(&fp), "onnx/all-MiniLM-L6-v2-q", 384, None).is_consistent());

        // Mismatch: same dims, different model.
        assert_eq!(
            embedding_status(Some(&fp), "onnx/all-MiniLM-L6-v2", 384, None),
            EmbeddingModelStatus::Mismatch {
                stored: "onnx/all-MiniLM-L6-v2-q".to_string(),
                current: "onnx/all-MiniLM-L6-v2".to_string(),
            }
        );

        // DimensionMismatch must win even when the model name ALSO differs
        // (arm priority: dimensions checked before model — a regression here
        // would misclassify a schema-breaking change as a soft Mismatch).
        assert_eq!(
            embedding_status(Some(&fp), "onnx/nomic-embed-text-v1.5", 768, None),
            EmbeddingModelStatus::DimensionMismatch {
                stored: 384,
                current: 768,
            }
        );

        // CompositionMismatch: same model + dims, composition changed — the
        // upgrade path (legacy-stamped store, metadata-vector default on).
        assert_eq!(
            embedding_status(
                Some(&fp),
                "onnx/all-MiniLM-L6-v2-q",
                384,
                Some(COMPOSITION_METADATA_V1)
            ),
            EmbeddingModelStatus::CompositionMismatch {
                stored: None,
                current: Some(COMPOSITION_METADATA_V1.to_string()),
            }
        );
        // ...and the reverse (user turned the flag off after stamping).
        let fp_meta = EmbeddingFingerprint {
            composition: Some(COMPOSITION_METADATA_V1.to_string()),
            ..fp.clone()
        };
        assert_eq!(
            embedding_status(Some(&fp_meta), "onnx/all-MiniLM-L6-v2-q", 384, None),
            EmbeddingModelStatus::CompositionMismatch {
                stored: Some(COMPOSITION_METADATA_V1.to_string()),
                current: None,
            }
        );
        // Matching metadata-v1 composition on both sides is consistent.
        assert!(embedding_status(
            Some(&fp_meta),
            "onnx/all-MiniLM-L6-v2-q",
            384,
            Some(COMPOSITION_METADATA_V1)
        )
        .is_consistent());
        // A model swap outranks the composition delta (one remediation
        // message, the more severe one).
        assert_eq!(
            embedding_status(
                Some(&fp),
                "onnx/all-MiniLM-L6-v2",
                384,
                Some(COMPOSITION_METADATA_V1)
            ),
            EmbeddingModelStatus::Mismatch {
                stored: "onnx/all-MiniLM-L6-v2-q".to_string(),
                current: "onnx/all-MiniLM-L6-v2".to_string(),
            }
        );

        // Untracked: legacy store, no fingerprint.
        assert_eq!(
            embedding_status(None, "onnx/all-MiniLM-L6-v2-q", 384, None),
            EmbeddingModelStatus::Untracked {
                current: "onnx/all-MiniLM-L6-v2-q".to_string(),
            }
        );

        // Only Match is consistent.
        for s in [
            EmbeddingModelStatus::Mismatch {
                stored: "a".into(),
                current: "b".into(),
            },
            EmbeddingModelStatus::DimensionMismatch {
                stored: 1,
                current: 2,
            },
            EmbeddingModelStatus::CompositionMismatch {
                stored: None,
                current: Some(COMPOSITION_METADATA_V1.to_string()),
            },
            EmbeddingModelStatus::Untracked {
                current: "x".into(),
            },
        ] {
            assert!(!s.is_consistent(), "{s:?} must not be consistent");
        }
    }

    #[tokio::test]
    async fn manifest_embedding_fingerprint_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.toml");

        let mut original = Manifest {
            project: "p".to_string(),
            ..Default::default()
        };
        original.embedding = Some(EmbeddingFingerprint {
            model: "onnx/all-MiniLM-L6-v2-q".to_string(),
            dimensions: 384,
            composition: Some(COMPOSITION_METADATA_V1.to_string()),
        });

        save_manifest(&path, &original).await.unwrap();
        let loaded = load_manifest(&path).await.unwrap();
        assert_eq!(loaded.embedding, original.embedding);
    }

    #[tokio::test]
    async fn manifest_without_embedding_section_loads_as_none() {
        // Backward-compat: a manifest written before the lifecycle (no
        // `[embedding]`) must still parse, with `embedding == None`. A
        // `None` fingerprint must also not serialize a stray `embedding`
        // key (skip_serializing_if) — old readers would choke on it.
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.toml");

        let legacy = Manifest {
            project: "p".to_string(),
            ..Default::default()
        };
        assert!(legacy.embedding.is_none());
        save_manifest(&path, &legacy).await.unwrap();

        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            !raw.contains("embedding"),
            "a None fingerprint must not serialize an `embedding` key; got:\n{raw}"
        );
        let loaded = load_manifest(&path).await.unwrap();
        assert!(loaded.embedding.is_none());
    }

    #[test]
    fn test_update_stats() {
        let mut manifest = Manifest::default();
        let scopes = vec!["scope_a".to_string(), "scope_b".to_string()];

        update_stats(&mut manifest, 100, scopes.clone());

        assert_eq!(manifest.stats.memory_count, 100);
        assert_eq!(manifest.stats.logical_scopes, scopes);
    }
}
