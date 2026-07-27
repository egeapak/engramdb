# Embedding Models

Every memory is embedded as one **metadata vector** (`"{title}. {summary}. tags: …"`) plus its content chunked into one or more content vectors, stored alongside metadata in a single LanceDB table; at query time the best-matching vector represents the memory (max-score aggregation). Set `embeddings.metadata_vector = false` to revert to the legacy single `summary + content` composition (then run `engramdb reindex --embeddings-only`). Embeddings are **optional** — with `--no-embeddings` or a failed model load, query degrades to relevance-only (keyword search still works).

## Available models

| Provider string | Backend | Dimensions | Notes |
|-----------------|---------|------------|-------|
| `all-minilm` (alias `onnx`) | ONNX | 384 | **Default.** Tracks the shipped default — today all-MiniLM-**L12**-v2 **uint8**, ~32 MB. |
| `all-minilm-l12` | ONNX | 384 | Pins the 12-layer uint8 model. Same as the default today. |
| `all-minilm-l6` | ONNX | 384 | Pins the 6-layer uint8 model (~22 MB). ~2× faster to embed, measurably worse ranking. |
| `all-minilm-l12-int8` / `all-minilm-l6-int8` | ONNX | 384 | The **signed-int8** exports. Kept only so an existing store can avoid a reindex — ONNX Runtime executes these non-reproducibly under CPU load ([onnxruntime#6004](https://github.com/microsoft/onnxruntime/issues/6004)), so the same text can be indexed as an unrelated vector. Don't pick these for new stores. |
| `all-minilm-l12-fp32` | ONNX | 384 | fp32 build of the default model (~127 MB, 1.3× slower per query, 1.7× per batch). **Reproducible on any runtime** — pick this if you need guaranteed-stable vectors on a stock build. See [embedding-model-alternatives.md](../contributors/embedding-model-alternatives.md) (R6). |
| `nomic-embed-text` | ONNX or Ollama | 768 | Better quality, longer context support, slower. |
| `mxbai-embed-large` | ONNX or Ollama | 1024 | Best quality, biggest model, slowest. |

### Backends

- **ONNX** (default) — local inference via ONNX Runtime. Models cache to `<cache_dir>/engramdb/models/`; first use downloads from Hugging Face.
- **Ollama** — calls a local Ollama instance on `http://localhost:11434`.
- **auto** (default) — tries ONNX first, falls back to Ollama.

Set `[embeddings]` in `config.toml` (see [configuration.md](./configuration.md)) or override per-invocation with `--embedding-backend` / `ENGRAMDB_EMBEDDING_BACKEND` (`auto` | `onnx` | `ollama`).

> **Upgrading from the tract backend.** EngramDB used to select a pure-Rust `tract` backend on Intel Mac (fp32 6-layer, `tract/all-MiniLM-L6-v2-fp32`). It was removed once the ONNX Runtime became a separately installed library, which gave Intel Mac a real ONNX path. Because the two record distinct model fingerprints, a store built under tract will detect the change and prompt `engramdb reindex --embeddings-only` once. A `backend = "tract"` line left in `config.toml` still loads and now behaves as `auto`.

## Model fingerprinting

Each store records the embedding model it was built with. The fingerprint includes:

- `model_id()` from the provider (e.g. `onnx/all-MiniLM-L12-v2-q`, `onnx/all-MiniLM-L6-v2-q` — note the `-q` suffix for quantized variants, and that the layer count is part of the id),
- the dimensionality,

and lives in `<project>/.engramdb/manifest.toml`.

When the MCP server (or any CLI command that opens a store) starts, it compares the stored fingerprint to the live provider's. The `[embeddings].reindex_on_model_change` setting decides what happens on a mismatch:

| Setting | Behavior |
|---------|----------|
| `off` | Silent. Vectors may be mismatched against queries — **don't use this.** |
| `warn` (default) | Surfaces a warning that says exactly which command to run. Operations continue, but search quality is degraded. |
| `auto` | Auto-runs the reindex on daemon startup. Can be expensive — every memory is re-embedded. |
| `error` | Refuses to serve until you reindex. Safest in shared / CI environments. |

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

Each project's `config.toml` and manifest are independent — repeat in every project.

## Where models cache

All ML downloads (embeddings, reranker, NLI) cache to `~/Library/Caches/engramdb/models/` (macOS) or `~/.cache/engramdb/models/` (Linux). The layout mirrors the Hugging Face hub cache — restricted-egress environments can pre-stage models into this exact path. See the CLAUDE.md "web sandbox" section for the layout.

## Latency

Default ONNX MiniLM: ~5-15 ms per call after warmup; ~240 ms cold-start. The daemon eliminates cold-start beyond the first call.

## Troubleshooting

See [troubleshooting.md](./troubleshooting.md#embeddings).
