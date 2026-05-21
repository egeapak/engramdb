# Embedding Models

Every memory's `summary + content` is embedded into a fixed-dimensional vector and stored alongside metadata in a single LanceDB table. Embeddings are **optional** — with `--no-embeddings` or a failed model load, query degrades to relevance-only (keyword search still works).

## Available models

| Provider string | Backend | Dimensions | Notes |
|-----------------|---------|------------|-------|
| `all-minilm` (alias `onnx`) | ONNX | 384 | **Default.** all-MiniLM-L6-v2. ~90 MB. Fast, decent quality for short snippets. |
| `nomic-embed-text` | ONNX or Ollama | 768 | Better quality, longer context support, slower. |
| `mxbai-embed-large` | ONNX or Ollama | 1024 | Best quality, biggest model, slowest. |

### Backends

- **ONNX** (default) — local inference via ONNX Runtime. Models cache to `<cache_dir>/engramdb/models/`; first use downloads from Hugging Face.
- **Ollama** — calls a local Ollama instance on `http://localhost:11434`.
- **auto** — tries ONNX first, falls back to Ollama.

Set `[embeddings]` in `config.toml` (see [configuration.md](./configuration.md)) or override per-invocation with `--embedding-backend` / `ENGRAMDB_EMBEDDING_BACKEND`.

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
