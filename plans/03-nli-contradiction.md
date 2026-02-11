# Feature 2: NLI Contradiction Detection

## Goal
Automatically detect contradictions between new and existing memories using Natural Language Inference, triggering the challenge protocol when contradictions are found.

## Changes

### New module: `src/nli/`
- `mod.rs`: `NliResult` struct, `NliProvider` trait with `classify()` and `classify_batch()`
- `onnx.rs`: ONNX-based NLI provider using `cross-encoder/nli-deberta-v3-xsmall`
  - Downloads model from HF Hub, caches locally
  - Tokenize pair -> ONNX inference -> softmax -> NliResult
  - Wrapped in spawn_blocking for async compat

### `Cargo.toml`
- Add direct deps: `ort`, `tokenizers`, `hf-hub`, `ndarray` (all already transitive)

### `src/lib.rs`
- Add `pub mod nli;`

### `src/types/config.rs`
- Add `NliConfig` struct: `enabled`, `model`, `contradiction_threshold`, `max_comparisons`, `similarity_threshold`
- Add `nli` field to `EngramConfig`

### `src/retrieval/engine.rs`
- Add `nli_provider: Option<Box<dyn NliProvider>>` field
- Add `with_nli_provider()` builder
- Add `nli_provider()` accessor
- Add `detect_contradictions()` method

### `src/ops/mod.rs`
- Initialize NLI provider in `build_engine()` if enabled

### `src/ops/create.rs`
- After embedding, check for contradictions if NLI available
- Auto-challenge existing memories that contradict the new one

### Tests
- NLI provider returns sensible scores for known pairs
- Creating contradicting memory auto-challenges existing one
- NLI disabled by default
- similarity_threshold gates which memories are checked
