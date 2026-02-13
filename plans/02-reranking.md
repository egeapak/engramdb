# Feature 2: Reranking via fastembed TextRerank

**Status: IN PROGRESS**

## Goal
Add cross-encoder reranking as an optional post-processing step after vector search, improving retrieval quality by catching nuanced query-document relevance that bi-encoder embeddings miss.

Cross-encoder rerankers (like BAAI/bge-reranker-base) jointly encode query+document pairs, producing more accurate relevance scores than the cosine similarity from bi-encoder embeddings. The trade-off is speed: reranking is slower, so it should only run on the top-N candidates from the initial retrieval.

## Architecture

```
                     ┌──────────────┐
   query ──────────► │ Bi-encoder   │ ── cosine scores ──┐
                     │ (fast, rough)│                     │
                     └──────────────┘                     ▼
                                                   ┌────────────┐
                                                   │ Sort top-N │
                                                   │ candidates │
                                                   └─────┬──────┘
                                                         │
                                                         ▼
                                                   ┌──────────────┐
                                                   │ Cross-encoder│
                                                   │ (slow, exact)│
                                                   └──────┬───────┘
                                                          │
                                                          ▼
                                                   ┌──────────────┐
                                                   │ Blend scores │
                                                   │ re-sort      │
                                                   └──────────────┘
```

The reranker operates as a post-processing step:
1. Retrieval/search returns scored candidates sorted by initial score
2. Take the top `top_n` candidates (default 50)
3. Build query-document strings: `"{summary} {content}"` for each candidate
4. Call `TextRerank::rerank()` to get cross-encoder relevance scores
5. Blend: `blended = (1 - weight) * original_score + weight * rerank_score`
6. Re-sort by blended score
7. Store the raw rerank score in `ScoreBreakdown.rerank` for transparency

## Detailed Changes

### 1. `src/types/config.rs` — Add `RerankConfig`

Add a new configuration struct:

```rust
/// Configuration for cross-encoder reranking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankConfig {
    /// Whether reranking is enabled (default: false)
    pub enabled: bool,

    /// Reranker model name (default: "bge-reranker-base")
    /// Maps to fastembed::RerankerModel variants.
    pub model: String,

    /// Number of top candidates to rerank (default: 50)
    /// Higher values improve quality but are slower.
    pub top_n: usize,

    /// Blend weight for rerank score (default: 0.5)
    /// 0.0 = ignore rerank, 1.0 = use only rerank score.
    /// Blended: (1-weight)*original + weight*rerank
    pub weight: f64,
}
```

Default values:
- `enabled`: `false` (opt-in)
- `model`: `"bge-reranker-base"`
- `top_n`: `50`
- `weight`: `0.5`

Add `rerank` field to `EngramConfig`:
```rust
/// Reranking configuration
#[serde(default)]
pub rerank: RerankConfig,
```

Add tests:
- `test_rerank_config_defaults` — verify all default values
- `test_rerank_config_toml_roundtrip` — serialize/deserialize
- `test_rerank_config_partial_toml` — partial TOML with defaults for missing fields

### 2. `src/scoring/composite.rs` — Add `rerank` field to `ScoreBreakdown`

Add a new optional field:
```rust
/// Raw cross-encoder rerank score (if reranking was applied)
pub rerank: Option<f64>,
```

Update construction sites:
- In `composite_score()`: set `rerank: None`
- In `engine.rs` search: set `rerank: None`

This field is populated later by the rerank step in the engine.

### 3. `src/retrieval/engine.rs` — Reranker integration

#### New fields on `RetrievalEngine`:
```rust
pub struct RetrievalEngine {
    store: MemoryStore,
    config: EngramConfig,
    embedding_provider: Option<Box<dyn EmbeddingProvider>>,
    reranker: Option<Arc<Mutex<TextRerank>>>,  // Mutex because rerank() takes &mut self
}
```

Note: `TextRerank::rerank()` takes `&mut self`, so we need `Mutex` for interior mutability. Using `std::sync::Mutex` is fine since we'll use it inside `spawn_blocking`.

#### New builder method:
```rust
pub fn with_reranker(mut self, reranker: Arc<Mutex<TextRerank>>) -> Self {
    self.reranker = Some(reranker);
    self
}
```

#### Rerank helper method:
```rust
async fn apply_rerank(
    &self,
    query_text: &str,
    candidates: &mut Vec<ScoredMemory>,
) -> anyhow::Result<()>
```

Logic:
1. Guard: if `self.reranker.is_none()` or `!self.config.rerank.enabled`, return early
2. Guard: if `candidates.is_empty()`, return early
3. Take top `config.rerank.top_n` candidates (by index, keep rest unchanged)
4. Build document strings: `format!("{} {}", mem.summary, mem.content)` for each
5. Clone `Arc<Mutex<TextRerank>>` and call in `spawn_blocking`:
   ```rust
   let mut reranker = reranker_arc.lock().unwrap();
   reranker.rerank(query_text, &documents, false, None)
   ```
6. Map rerank results back: normalize scores to [0, 1] using min/max normalization
7. Blend scores: `blended = (1 - weight) * original + weight * normalized_rerank`
8. Update `scored_memory.score = blended` and `scored_memory.score_breakdown.rerank = Some(raw_rerank_score)`
9. Re-sort candidates by blended score descending

#### Integration in `retrieve()`:
After step 6 (sort by score descending), before step 7 (truncate):
```rust
// Step 6.5: Apply reranking if configured
if let Some(ref q) = query.query {
    if let Err(e) = self.apply_rerank(q, &mut scored_memories).await {
        eprintln!("Warning: reranking failed, using original scores: {}", e);
    }
}
```

#### Integration in `search()`:
After sort by score descending, before returning:
```rust
// Apply reranking if configured
if let Err(e) = self.apply_rerank(query_text, &mut scored_memories).await {
    eprintln!("Warning: reranking failed, using original scores: {}", e);
}
```

### 4. `src/ops/mod.rs` — Initialize TextRerank in `build_engine()`

After embedding provider setup, add reranker initialization:

```rust
if config.rerank.enabled {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("engramdb")
        .join("models");

    let model = resolve_reranker_model(&config.rerank.model);
    let options = RerankInitOptions::new(model)
        .with_cache_dir(cache_dir)
        .with_show_download_progress(false);

    match TextRerank::try_new(options) {
        Ok(reranker) => {
            engine = engine.with_reranker(Arc::new(Mutex::new(reranker)));
        }
        Err(e) => {
            eprintln!("Warning: reranker init failed, continuing without: {}", e);
        }
    }
}
```

Add helper to map model string to enum:
```rust
fn resolve_reranker_model(name: &str) -> RerankerModel {
    match name {
        "bge-reranker-v2-m3" => RerankerModel::BGERerankerV2M3,
        "jina-reranker-v1-turbo-en" => RerankerModel::JINARerankerV1TurboEn,
        "jina-reranker-v2-base-multilingual" => RerankerModel::JINARerankerV2BaseMultiligual,
        _ => RerankerModel::BGERerankerBase, // default
    }
}
```

### 5. Tests

#### Unit tests in `src/types/config.rs`:
- `test_rerank_config_defaults` — all defaults match expected values
- `test_rerank_config_toml_roundtrip` — serialize then deserialize preserves values
- `test_rerank_config_custom_toml` — parse custom TOML values
- `test_rerank_config_disabled_by_default` — `EngramConfig::default().rerank.enabled == false`

#### Unit tests in `src/scoring/composite.rs`:
- Verify `rerank` field is `None` after `composite_score()`

#### Integration tests in `src/retrieval/engine.rs`:
- `test_retrieve_without_reranker` — existing behavior unchanged
- `test_search_without_reranker` — existing behavior unchanged
- `test_rerank_blend_weight_zero` — with weight=0.0, scores unchanged
- `test_retrieve_reranking_integration` — with real reranker, verify rerank scores populated
- `test_search_reranking_integration` — with real reranker, verify rerank scores populated
- `test_rerank_graceful_degradation` — engine works when reranker fails to init

## Verification Checklist
- [ ] `cargo fmt --all` — clean
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — zero warnings
- [ ] `cargo test` — all tests pass (existing + new)
- [ ] Reranker disabled by default — no behavior change for existing users
- [ ] Graceful degradation when reranker init fails
