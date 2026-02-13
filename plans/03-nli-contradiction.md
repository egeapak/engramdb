# Feature 2: NLI Contradiction Detection

## Goal
Automatically detect contradictions between new and existing memories using Natural Language Inference (NLI), triggering the challenge protocol when contradictions are found.

## Motivation
When an agent stores a new memory that contradicts an existing one (e.g., "Use PostgreSQL for the database" vs "Use SQLite for the database"), the system should detect this conflict automatically. Without NLI, contradictions silently coexist, confusing future retrievals. By integrating an NLI model, EngramDB can classify sentence pairs as entailment, neutral, or contradiction, and auto-challenge conflicting memories.

## Architecture

### Model Choice
- **Model:** `cross-encoder/nli-deberta-v3-xsmall` (22M params, fast inference)
- **Why:** Small enough for local ONNX inference, accurate enough for NLI classification
- **Labels:** entailment (0), neutral (1), contradiction (2) — standard NLI label ordering
- **Input:** Sentence pair `(premise, hypothesis)` — tokenized and fed to the model
- **Output:** 3 logits → softmax → probabilities for each label

### Flow: Memory Creation with Contradiction Detection
```
create_memory(params)
  ├── validate & build Memory
  ├── store.create(&memory)
  ├── engine.embed_memory(&memory)         # existing step
  └── engine.detect_contradictions(&memory) # NEW step
        ├── vector_search(memory_embedding, similarity_threshold)
        │   └── returns top-N similar memories as candidates
        ├── nli_provider.classify_batch(candidates)
        │   └── for each candidate: classify(new_summary, existing_summary)
        ├── filter results where contradiction_prob > contradiction_threshold
        └── for each contradiction:
              └── challenge_memory(existing_id, evidence="NLI contradiction detected")
```

## Detailed Changes

### 1. `Cargo.toml` — New Dependencies

```toml
ort = { version = "2", default-features = false, features = ["download-binaries"] }
tokenizers = { version = "0.21", default-features = false, features = ["onig"] }
hf-hub = "0.4"
ndarray = "0.16"
```

These are already transitive dependencies via `fastembed`, so they add minimal binary size overhead. We pin compatible versions.

### 2. New module: `src/nli/mod.rs`

#### Types

```rust
/// NLI classification label
pub enum NliLabel {
    Entailment,
    Neutral,
    Contradiction,
}

/// Result of classifying a sentence pair
pub struct NliResult {
    pub label: NliLabel,
    pub entailment: f32,
    pub neutral: f32,
    pub contradiction: f32,
}
```

#### Trait

```rust
#[async_trait]
pub trait NliProvider: Send + Sync {
    /// Classify a single premise-hypothesis pair
    async fn classify(&self, premise: &str, hypothesis: &str) -> Result<NliResult>;

    /// Classify multiple pairs in batch
    async fn classify_batch(
        &self,
        pairs: &[(&str, &str)],
    ) -> Result<Vec<NliResult>>;
}
```

### 3. New module: `src/nli/onnx.rs` — `OnnxNliProvider`

#### Initialization
- Use `hf_hub::api::sync::ApiBuilder` to download `cross-encoder/nli-deberta-v3-xsmall`
- Cache in `~/.cache/engramdb/models/nli/` (same pattern as embedding models)
- Load ONNX model via `ort::Session`
- Load tokenizer via `tokenizers::Tokenizer`

#### Inference Pipeline
1. Tokenize the sentence pair using the model's tokenizer: `tokenizer.encode((premise, hypothesis))`
2. Extract `input_ids`, `attention_mask`, `token_type_ids` as `ndarray::Array2<i64>`
3. Run ONNX inference: `session.run(inputs)` → logits tensor of shape `[batch, 3]`
4. Apply softmax to get probabilities for `[entailment, neutral, contradiction]`
5. Return `NliResult` with the highest-probability label and all three scores

#### Async Compatibility
- All ONNX inference runs inside `tokio::task::spawn_blocking` since it's CPU-bound
- The provider holds `Arc<Session>` and `Arc<Tokenizer>` for thread safety

#### Graceful Degradation
- `OnnxNliProvider::try_new() -> Option<Self>` — returns None if model download or ONNX init fails
- Logs a warning but never panics or errors out

### 4. `src/types/config.rs` — `NliConfig`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NliConfig {
    /// Whether NLI contradiction detection is enabled
    pub enabled: bool,

    /// Model name (HuggingFace repo ID)
    pub model: String,

    /// Minimum contradiction probability to trigger a challenge (0.0-1.0)
    pub contradiction_threshold: f64,

    /// Maximum number of similar memories to compare against
    pub max_comparisons: usize,

    /// Minimum cosine similarity to consider a memory as a candidate for NLI check.
    /// Only memories with similarity >= this threshold are checked.
    pub similarity_threshold: f64,
}
```

**Defaults:**
- `enabled: false` — opt-in feature, does not affect existing behavior
- `model: "cross-encoder/nli-deberta-v3-xsmall"`
- `contradiction_threshold: 0.7` — only challenge when >70% probability of contradiction
- `max_comparisons: 10` — check at most 10 similar memories
- `similarity_threshold: 0.3` — only check memories with cosine similarity >= 0.3

**Integration with `EngramConfig`:**
- Add `#[serde(default)] pub nli: NliConfig` field

### 5. `src/retrieval/engine.rs` — Engine Integration

#### New Fields
```rust
pub struct RetrievalEngine {
    store: MemoryStore,
    config: EngramConfig,
    embedding_provider: Option<Box<dyn EmbeddingProvider>>,
    nli_provider: Option<Box<dyn NliProvider>>,  // NEW
}
```

#### New Methods
```rust
/// Add an NLI provider to the engine
pub fn with_nli_provider(mut self, provider: Box<dyn NliProvider>) -> Self

/// Check if NLI is available
pub fn nli_available(&self) -> bool

/// Accessor for the NLI provider
pub fn nli_provider(&self) -> Option<&dyn NliProvider>

/// Detect contradictions between a new memory and existing similar memories.
///
/// 1. Embed the new memory and find similar candidates via vector search
/// 2. Run NLI classification on (new_summary, existing_summary) pairs
/// 3. Return list of (memory_id, NliResult) where contradiction > threshold
pub async fn detect_contradictions(
    &self,
    memory: &Memory,
) -> Result<Vec<(String, NliResult)>>
```

#### `detect_contradictions()` Algorithm
1. If NLI provider is not available, return empty vec
2. If embedding provider is not available, return empty vec (need embeddings for candidate search)
3. Embed the new memory text: `summary + " " + content`
4. Vector search for similar memories with limit = `config.nli.max_comparisons`
5. Filter by `similarity_threshold`
6. Load each candidate memory from store
7. Skip self (same ID)
8. Build pairs: `(new_memory.summary, candidate.summary)`
9. Run `nli_provider.classify_batch(pairs)`
10. Filter results where `contradiction > contradiction_threshold`
11. Return the contradicting (memory_id, NliResult) pairs

### 6. `src/ops/mod.rs` — Provider Initialization

In `build_engine()`, after embedding provider setup:

```rust
if config.nli.enabled {
    match OnnxNliProvider::try_new() {
        Some(provider) => {
            engine = engine.with_nli_provider(Box::new(provider));
        }
        None => {
            eprintln!("Warning: NLI contradiction detection enabled but model unavailable");
        }
    }
}
```

### 7. `src/ops/create.rs` — Auto-Challenge on Contradiction

After the existing embedding step, add:

```rust
// Detect contradictions with existing memories
if let Some(engine) = engine {
    if engine.nli_available() {
        if let Ok(contradictions) = engine.detect_contradictions(&saved).await {
            for (existing_id, nli_result) in &contradictions {
                let evidence = format!(
                    "NLI contradiction detected (score: {:.2}): new memory '{}' contradicts this memory",
                    nli_result.contradiction, saved.summary
                );
                let _ = challenge_memory(store, existing_id, &evidence, None).await;
            }
        }
    }
}
```

**Behavior:**
- Contradictions challenge the *existing* memory, not the new one
- The new memory is assumed to be more current/authoritative
- Challenge evidence includes the contradiction score and the new memory's summary
- Failures are silently ignored (best-effort)

### 8. `src/lib.rs`

Add `pub mod nli;` to module declarations.

## Configuration Example

```toml
# config.toml
[nli]
enabled = true
model = "cross-encoder/nli-deberta-v3-xsmall"
contradiction_threshold = 0.7
max_comparisons = 10
similarity_threshold = 0.3
```

## Tests

### Unit Tests (`src/nli/onnx.rs`)
1. `test_provider_creation` — OnnxNliProvider::try_new() returns Some
2. `test_classify_contradiction` — known contradictory pair scores high on contradiction
3. `test_classify_entailment` — known entailing pair scores high on entailment
4. `test_classify_neutral` — unrelated pair scores high on neutral
5. `test_classify_batch` — batch classification matches individual classifications
6. `test_softmax_sums_to_one` — probabilities sum to ~1.0

### Unit Tests (`src/nli/mod.rs`)
1. `test_nli_result_label` — NliResult correctly reports the dominant label
2. `test_nli_label_display` — labels display correctly

### Config Tests (`src/types/config.rs`)
1. `test_nli_config_defaults` — NliConfig defaults are correct (disabled, threshold 0.7, etc.)
2. `test_nli_config_toml_roundtrip` — serialization/deserialization preserves values
3. `test_nli_config_omitted_uses_defaults` — missing [nli] section uses defaults

### Integration Tests (`src/ops/create.rs`)
1. `test_create_memory_no_contradiction_when_nli_disabled` — NLI off = no challenges
2. `test_create_contradicting_memory_auto_challenges` — contradicting memory triggers challenge on existing

### Engine Tests (`src/retrieval/engine.rs`)
1. `test_detect_contradictions_no_provider` — returns empty when NLI not configured
2. `test_nli_provider_builder` — with_nli_provider sets provider correctly

## Non-Goals (Deferred)
- NLI on content (not just summaries) — summaries are sufficient for contradiction detection
- Custom model training — using pre-trained cross-encoder is sufficient
- GPU acceleration — ONNX CPU inference is fast enough for the small model
- Ollama-based NLI — only ONNX provider for now, Ollama can be added later
