# Embedding Models

EngramDB uses text embeddings to enable semantic search. This page covers what models are available, how to swap them, and the fingerprinting system that prevents silent vector corruption when you do.

## What an embedding is, in this codebase

Every memory's `summary + content` gets embedded into a fixed-dimensional float vector. The vectors are stored alongside metadata in a single LanceDB table. Semantic search runs a similarity query against the LanceDB ANN index.

Embeddings are **optional**. If you `engramdb init --no-embeddings` or the model fails to load, every query path still works — `with_query` falls back to `degraded` scoring (relevance-only), and keyword search still functions.

## Available models

| Provider string | Backend | Dimensions | Notes |
|-----------------|---------|------------|-------|
| `all-minilm` (alias `onnx`) | ONNX | 384 | **Default.** all-MiniLM-L6-v2. ~90 MB. Fast, decent quality for short snippets. |
| `nomic-embed-text` | ONNX or Ollama | 768 | Better quality, longer context support, slower. |
| `mxbai-embed-large` | ONNX or Ollama | 1024 | Best quality, biggest model, slowest. |

The provider→model mapping is the **single source of truth** in `src/ops/mod.rs::provider_specs`. Any change to embedding providers must update that one map.

### Backends

- **ONNX** (default) — local inference via ONNX Runtime, no external services. Models cache to `<cache_dir>/engramdb/models/`. First use downloads them from Hugging Face.
- **Ollama** — uses a local Ollama instance. Requires `ollama` running on `http://localhost:11434`. Useful if you already have Ollama for other purposes.
- **auto** — tries ONNX first, falls back to Ollama if ONNX is unavailable.

Set in `<project>/.engramdb/config.toml`:

```toml
[embeddings]
backend = "onnx"
provider = "all-minilm"
dimensions = 384
max_tokens = 256
```

Override per-invocation with `--embedding-backend <auto|onnx|ollama>` or the `ENGRAMDB_EMBEDDING_BACKEND` env var.

## Model fingerprinting

Each store records the embedding model it was built with. The fingerprint includes:

- `model_id()` from the provider (e.g. `onnx/all-MiniLM-L6-v2`, `onnx/all-MiniLM-L6-v2-q` — note the `-q` suffix for quantized variants),
- the dimensionality,

and lives in `<project>/.engramdb/manifest.toml`.

When the MCP server (or any CLI command that opens a store) starts, it compares the stored fingerprint to the live provider's. The `[embeddings].reindex_on_model_change` setting decides what happens on a mismatch:

| Setting | Behavior |
|---------|----------|
| `off` | Silent. Vectors may be mismatched against queries — **don't use this.** |
| `warn` (default) | Surfaces a warning that says exactly which command to run. Operations continue, but search quality is degraded. |
| `auto` | Auto-runs the reindex on daemon startup. Can be expensive — every memory is re-embedded. |
| `error` | Refuses to serve until you reindex. Safest in shared / CI environments. |

The fingerprint also captures **quantized vs full-precision** distinctions — `onnx/all-MiniLM-L6-v2-q` and `onnx/all-MiniLM-L6-v2` are different fingerprints, so swapping in or out of quantization is detected.

## Reindexing

Reindexing is the recovery path for any embedding change. Three forms:

```bash
# Re-embed everything + rebuild the LanceDB index (default)
engramdb reindex

# Re-embed only — index is fine, vectors are not
engramdb reindex --embeddings-only

# Rebuild index only — keep existing vectors
engramdb reindex --index-only

# Same flags work against the global store
engramdb reindex --global
```

Use `--embeddings-only` when you've changed `[embeddings].provider` or `[embeddings].dimensions`. Use `--index-only` when you suspect the LanceDB index is stale or corrupt but vectors are fine (e.g. after a process crash mid-write).

After a successful reindex, the manifest fingerprint is updated to match.

## Swapping models — full procedure

```bash
# 1. Stop any long-running daemon so it doesn't keep the old model loaded
engramdb daemon stop

# 2. Edit config.toml — pick the new provider and matching dimensions
$EDITOR .engramdb/config.toml
# [embeddings]
# provider = "nomic-embed-text"
# dimensions = 768

# 3. Reindex
engramdb reindex --embeddings-only

# 4. (Optional) Restart the daemon to pre-load the new model
engramdb daemon restart

# 5. Verify
engramdb doctor
```

For a multi-project setup, repeat steps 2-3 in each project. Each project's `config.toml` and manifest are independent.

## Where models cache

All ML model downloads (embeddings, reranker, NLI) cache to a single directory:

- **macOS:** `~/Library/Caches/engramdb/models/`
- **Linux:** `$XDG_CACHE_HOME/engramdb/models/` (default `~/.cache/engramdb/models/`)

This is set by `storage::paths::model_cache_dir()`. The cache layout mirrors Hugging Face's hub layout — restricted-egress environments can pre-stage models into this exact path, and `hf-hub` will serve them without hitting the network. See the CLAUDE.md "web sandbox" section for the precise file layout.

## Quality and chunking

- Default `max_tokens = 256`. Longer content is chunked in `src/embeddings/chunking.rs` before embedding. The chunking strategy is simple sentence-boundary splitting with overlap.
- Embedding latency on default ONNX MiniLM: ~5-15 ms per call after warmup; ~240 ms cold-start. The daemon eliminates cold-start beyond the first call.
- Quality tip: most retrievals are dominated by the `summary` field, not the `content`. Write summaries that read like search queries someone would actually type.

## Troubleshooting

**"Embedding model unavailable" warning at startup.** First-run download is failing or the cache is corrupt. Check network connectivity, delete `<cache_dir>/engramdb/models/`, and retry. In restricted-egress environments, pre-stage the model manually.

**Dimension mismatch warning.** Your `config.toml` `[embeddings].dimensions` doesn't match the provider. Either fix the config to match the model (`all-minilm`=384, `nomic-embed-text`=768, `mxbai-embed-large`=1024) or reindex into the dimension you want.

**Search quality dropped after a sync from a teammate.** They may have a different `provider` in their `config.toml`, and the manifest fingerprint moved. Run `engramdb doctor` — the embedding section will tell you exactly what's mismatched, and the warning includes the exact reindex command to fix it.

**`reindex` is slow.** It's O(N) in number of memories and embedding cost per memory dominates. For a large store, expect minutes. With the daemon running it's modestly faster because batches share an open model session.
