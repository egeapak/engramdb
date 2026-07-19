//! Effective-configuration view and tag-vocabulary aggregation.
//!
//! Surfaces the config values and store vocabulary an agent needs to use the
//! memory tools well — summary/content limits it should respect on `create`,
//! the thresholds and result caps that govern `query`/`search`, which optional
//! features (rerank, NLI contradiction detection) are live, and a short list of
//! the most-used tags so the agent gets a sense of what is already in memory
//! without paging through every entry.
//!
//! Read-only: it never mutates the store. The MCP `config` tool is the primary
//! caller; the tag aggregation is factored out here so it stays unit-testable
//! independent of the MCP layer.

use crate::storage::MemoryStore;
use crate::types::{EngramConfig, TitleStrategy, CONTENT_SOFT_TOKEN_TARGET, MAX_SUMMARY_CHARS};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;

/// A tag and how many memories carry it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TagCount {
    pub tag: String,
    pub count: usize,
}

/// The top `limit` unique tags across the store, most-used first.
///
/// Ties break alphabetically so the ordering is deterministic (important for
/// tests and for a stable agent-facing view). An empty store yields an empty
/// list. Reads only the lightweight filtering projection — no `.md` I/O.
pub async fn top_tags(store: &MemoryStore, limit: usize) -> Result<Vec<TagCount>> {
    let entries = store.list_for_filtering().await?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for entry in &entries {
        for tag in &entry.tags {
            *counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<TagCount> = counts
        .into_iter()
        .map(|(tag, count)| TagCount { tag, count })
        .collect();
    // Most-used first; alphabetical within a count so the order is stable.
    ranked.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
    ranked.truncate(limit);
    Ok(ranked)
}

/// How many top tags the `config` view surfaces by default — enough to convey
/// the shape of the vocabulary without turning the response into a full listing.
pub const DEFAULT_TOP_TAGS: usize = 20;

/// Limits an agent should respect when authoring a memory.
#[derive(Debug, Clone, Serialize)]
pub struct LimitsView {
    /// Hard cap on `summary` length (bytes; == chars for ASCII). `create`
    /// rejects longer summaries.
    pub summary_max_chars: usize,
    /// Soft target for `content` length in tokens (not enforced). Longer
    /// content is not lost — it is chunked and every chunk is embedded — but
    /// keeping the main point in `content` and moving bulk detail to `details`
    /// (which is not embedded) keeps retrieval focused.
    pub content_soft_token_target: usize,
    /// Per-chunk embedding window, in tokens. `summary + content` is split
    /// into chunks of this size and each chunk is embedded and searched
    /// independently — so content past one window is still embedded, just in
    /// additional chunks (it is not truncated).
    pub embedding_chunk_tokens: usize,
}

/// Retrieval / search knobs that govern what `query` and `search` return.
#[derive(Debug, Clone, Serialize)]
pub struct RetrievalView {
    /// Default number of results when a query does not specify `max_results`.
    pub default_max_results: usize,
    /// Minimum composite score a `query` result must clear.
    pub relevance_threshold: f64,
    /// Minimum score a keyword `search` result must clear.
    pub search_threshold: f64,
    /// Weight applied to semantic similarity in keyword search scoring.
    pub search_semantic_weight: f64,
    /// Whether expired memories are included by default.
    pub include_expired: bool,
}

/// Which optional, config-gated features are live for this store.
#[derive(Debug, Clone, Serialize)]
pub struct FeaturesView {
    /// Cross-encoder reranking of retrieval results.
    pub rerank_enabled: bool,
    /// Top-N candidates passed to the reranker when enabled.
    pub rerank_top_n: usize,
    /// NLI contradiction detection powering the `challenge` flow.
    pub contradiction_detection_enabled: bool,
    /// Automatic title-generation strategy for `create` when no title is given.
    pub title_strategy: TitleStrategy,
}

/// Embedding backend/model facts (help interpret why semantic search behaves
/// as it does).
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingView {
    pub provider: String,
    pub dimensions: usize,
}

/// The agent-facing effective-config view (config values only; tags are
/// attached separately by the caller since they require store access).
#[derive(Debug, Clone, Serialize)]
pub struct AgentConfigView {
    pub limits: LimitsView,
    pub retrieval: RetrievalView,
    pub features: FeaturesView,
    pub embedding: EmbeddingView,
}

impl AgentConfigView {
    /// Build the view from an effective [`EngramConfig`]. Pure — no I/O — so it
    /// is trivially testable and reusable by any front-end.
    pub fn from_config(config: &EngramConfig) -> Self {
        Self {
            limits: LimitsView {
                summary_max_chars: MAX_SUMMARY_CHARS,
                content_soft_token_target: CONTENT_SOFT_TOKEN_TARGET,
                embedding_chunk_tokens: config.embeddings.max_tokens,
            },
            retrieval: RetrievalView {
                default_max_results: config.retrieval.max_results,
                relevance_threshold: config.retrieval.relevance_threshold,
                search_threshold: config.search.threshold,
                search_semantic_weight: config.search.semantic_weight,
                include_expired: config.retrieval.include_expired,
            },
            features: FeaturesView {
                rerank_enabled: config.rerank.enabled,
                rerank_top_n: config.rerank.top_n,
                contradiction_detection_enabled: config.nli.enabled,
                title_strategy: config.title.strategy,
            },
            embedding: EmbeddingView {
                provider: config.embeddings.provider.clone(),
                dimensions: config.embeddings.dimensions,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    fn mem_with_tags(summary: &str, tags: &[&str]) -> Memory {
        let mut m = Memory::new(MemoryType::Context, summary, "Content", Provenance::human());
        m.tags = tags.iter().map(|t| t.to_string()).collect();
        m
    }

    #[tokio::test]
    async fn top_tags_ranks_by_count_then_alpha() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &registry).await.unwrap();

        // rust x3, testing x2, async x1, zebra x1
        store
            .create(&mem_with_tags("a", &["rust", "async"]))
            .await
            .unwrap();
        store
            .create(&mem_with_tags("b", &["rust", "testing"]))
            .await
            .unwrap();
        store
            .create(&mem_with_tags("c", &["rust", "testing", "zebra"]))
            .await
            .unwrap();

        let tags = top_tags(&store, 10).await.unwrap();
        assert_eq!(
            tags,
            vec![
                TagCount {
                    tag: "rust".into(),
                    count: 3
                },
                TagCount {
                    tag: "testing".into(),
                    count: 2
                },
                // count-1 ties in alphabetical order
                TagCount {
                    tag: "async".into(),
                    count: 1
                },
                TagCount {
                    tag: "zebra".into(),
                    count: 1
                },
            ]
        );
    }

    #[tokio::test]
    async fn top_tags_respects_limit_and_empty_store() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &registry).await.unwrap();

        assert!(top_tags(&store, 5).await.unwrap().is_empty());

        store
            .create(&mem_with_tags("a", &["one", "two", "three"]))
            .await
            .unwrap();
        let tags = top_tags(&store, 2).await.unwrap();
        assert_eq!(tags.len(), 2, "limit caps the number of tags returned");
    }

    #[test]
    fn config_view_reflects_defaults() {
        let view = AgentConfigView::from_config(&EngramConfig::default());
        assert_eq!(view.limits.summary_max_chars, 100);
        assert_eq!(view.limits.content_soft_token_target, 500);
        assert_eq!(view.limits.embedding_chunk_tokens, 256);
        assert_eq!(view.retrieval.default_max_results, 10);
        assert!((view.retrieval.relevance_threshold - 0.45).abs() < f64::EPSILON);
        assert!((view.retrieval.search_threshold - 0.2).abs() < f64::EPSILON);
        assert!(!view.features.rerank_enabled);
        assert!(!view.features.contradiction_detection_enabled);
        assert_eq!(view.embedding.dimensions, 384);
    }

    #[test]
    fn config_view_reflects_overrides() {
        let mut config = EngramConfig::default();
        config.rerank.enabled = true;
        config.nli.enabled = true;
        config.retrieval.max_results = 25;
        let view = AgentConfigView::from_config(&config);
        assert!(view.features.rerank_enabled);
        assert!(view.features.contradiction_detection_enabled);
        assert_eq!(view.retrieval.default_max_results, 25);
    }
}
