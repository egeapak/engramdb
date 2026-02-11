# Feature 1: Reranking via fastembed TextRerank

## Goal
Add cross-encoder reranking as an optional post-processing step after vector search, improving retrieval quality by catching nuanced query-document relevance that bi-encoder embeddings miss.

## Changes

### `src/types/config.rs`
- Add `RerankConfig` struct: `enabled`, `model`, `top_n`, `weight`
- Add `rerank` field to `EngramConfig` with `#[serde(default)]`

### `src/scoring/composite.rs`
- Add `pub rerank: Option<f64>` field to `ScoreBreakdown`
- Update all construction sites (composite_score, search in engine.rs)

### `src/retrieval/engine.rs`
- Add `reranker: Option<Arc<TextRerank>>` field
- Add `rerank_config: Option<RerankConfig>` field
- Add `with_reranker()` builder method
- In `retrieve()`: after sort, before truncate, insert rerank step
- In `search()`: after sort, insert rerank step
- Rerank logic: take top_n candidates, build document strings, call reranker.rerank() in spawn_blocking, blend scores, re-sort

### `src/ops/mod.rs`
- In `build_engine()`: initialize TextRerank if config.rerank.enabled
- Use `RerankInitOptions::new(model).with_cache_dir(cache_dir)`
- Graceful degradation on init failure

### Tests
- Reranker changes result ordering
- Graceful degradation when unavailable
- Blend weight 0.0 = no rerank effect
- Existing tests pass with reranker disabled
