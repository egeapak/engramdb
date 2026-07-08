# engramdb: pure-Rust (tract) fallback inference backend for Intel Mac

> **Status: planned, not started.** Feasibility is proven (see
> `experiments/tract-vs-ort/`); this document is the implementation brief for a
> future session.

## Context

The `ort` / `fastembed` model stack requires **ONNX Runtime 1.24.x (ABI/API 24)**
at runtime (`fastembed 5.15` pins `ort = "=2.0.0-rc.12"`). **No prebuilt ONNX
Runtime 1.24 exists for Intel Mac (`x86_64-apple-darwin`) anywhere** — Microsoft
dropped x86_64 macOS builds after 1.23.x, conda-forge tops out at 1.22, pyke's
`download-binaries` never carried it, and `ort-sys` rc.12 has no
compile-from-source strategy. Consequences already handled on branch
`claude/fable-project-review-szis0q`:

- The release workflow's `build-intel` job (which shipped ORT 1.23.2 / API 23
  next to an API-24 binary → crash at first model load) was **removed**
  (`9b880b3`).
- `docs/users/troubleshooting.md` documents the manual from-source ORT 1.24
  build as the only current Intel-Mac path.

That leaves Intel-Mac users with **no easy install**. `tract` (Sonos's
pure-Rust ONNX engine) is the fix: no native library, builds anywhere Rust
builds, no runtime to install. This plan wires a tract-backed provider as an
Intel-Mac fallback.

## Evidence (from `experiments/tract-vs-ort/`)

Measured, all-MiniLM-L6-v2, this-sandbox CPU:

| engine / model | single p50 | batch-8 p50 | cosine vs ORT | tract loads? |
|---|---|---|---|---|
| ort int8 (current default) | ~14 ms | ~69 ms | — | — |
| tract int8 | — | — | — | ❌ fails to load |
| ort fp32 | ~21 ms | ~94 ms | — | — |
| **tract fp32** | ~65 ms | ~578 ms | **1.00000** | ✅ optimized |

- tract's fp32 output is **bit-identical** to ONNX Runtime (cosine 1.00000).
- tract **cannot load the int8-quantized MiniLM** — a static shape-analysis
  failure on the attention-mask `Unsqueeze` chain in the optimum/transformers.js
  quantized export (NOT a missing int8 op; tract implements
  `DynamicQuantizeLinear`/`MatMulInteger`). See the experiment README.
- tract is **~3× slower single / ~6× batched**. Acceptable for an on-demand
  memory store (embed on create/query, not a hot loop), and it *works* where
  ORT has no runtime at all.

Reference inference + mean-pool + L2-normalize code already exists in
`experiments/tract-vs-ort/src/main.rs` — lift it into the provider.

## Design decisions (locked)

1. **tract backend uses the fp32 MiniLM** (`Qdrant/all-MiniLM-L6-v2-onnx`,
   `model.onnx`), never the int8 default. int8 does not load under tract.
2. **Direct `tract-onnx` provider, not the `ort-tract` shim.** `ort-tract`
   (ort's `alternative-backend`) is experimental and conflicts with fastembed's
   forced `ort/download-binaries` feature, so fastembed can't ride it. The
   experiment validated `tract-onnx` used directly; implement a
   `TractEmbeddingProvider` that owns tokenization + inference + pooling and
   bypasses `ort`/`fastembed` entirely on the tract path.
3. **Distinct `model_id()`** for the tract provider (e.g.
   `tract/all-MiniLM-L6-v2-fp32`). This is what makes the manifest embedding
   fingerprint detect a backend swap and drive a `reindex --embeddings-only`.
   Add its spec to the single `provider_specs` map in `src/ops/mod.rs` (the
   fingerprint table and provider resolver share that one map).
4. **MVP = embeddings only.** On an Intel-Mac tract build the *entire*
   `ort`/`fastembed` stack is unavailable, so the optional models must also
   avoid ort or be disabled. For the MVP: the reranker, NLI, and T5 features
   are **compiled out / disabled** on the tract build (all are off by default
   anyway). Follow-up can add a tract reranker (BGE is fp32 → likely loads;
   test it). NLI (deberta) and T5 are quantized exports → likely hit the same
   shape wall and would need fp32 exports; out of scope.
5. **Keep ORT + int8 as the default everywhere it has a prebuilt runtime.**
   tract is a fallback, selected only where ORT can't run (Intel Mac; also a
   natural future fit for WASM). Never make tract the global default — it's 3×
   slower.

## Open questions (resolve during implementation)

1. **Selection mechanism — `cfg(target)` auto-default vs explicit feature.**
   Two options (probably do both):
   - **Target-gated deps**: in `crates/engram-models/Cargo.toml`, make `ort`
     `fastembed` dependencies for every target *except* `x86_64-apple-darwin`,
     and `tract-onnx` a dependency *for* `x86_64-apple-darwin`, via
     `[target.'cfg(...)'.dependencies]`. Then `#[cfg]` picks the provider. This
     makes Intel Mac "just build and work" with no user flag — the cleanest UX.
   - **Explicit `tract` feature**: `--no-default-features --features tract` for
     opt-in on any platform (needed for testing tract on Linux/CI, and for
     WASM later). Requires making `ort`/`fastembed` optional behind a default
     `onnxruntime` feature.
   Recommendation: implement the explicit feature first (testable on Linux),
   then layer the Intel-Mac `cfg` auto-default on top. Verify Cargo feature
   unification doesn't drag `ort` into the tract-only build (it must not, or
   the Intel link fails — this is the crux; test with `cargo tree`).
2. **tract needs concrete input shapes.** Decide the shape strategy: pad every
   input to a fixed `max_tokens` (e.g. 256, from config) so sequence length is
   constant, and either fix batch=1 (simplest; loop the batch) or cache one
   runnable per batch size. Building a runnable is not free, so cache them
   (keyed by (seq_len, batch) or just seq_len if batch is fixed). The
   experiment rebuilt per shape — production must cache.
3. **Cross-machine store portability.** A store embedded on Intel Mac
   (tract/fp32, 384-dim) vs Apple Silicon (ort/int8, 384-dim) have the same
   dimensions but *different vector spaces*. The fingerprint correctly forces a
   reindex on backend switch, but a store synced between an Intel and a
   non-Intel machine will flip-flop fingerprints and reindex each time. Document
   this; consider a config to pin the backend/model regardless of platform.
4. **Thread control.** tract's default threading vs ORT's MLAS. Confirm tract
   respects a sane thread budget (the daemon pools sessions; tract runnables
   are cheap to clone/share — decide pooling shape).

## Files to add / modify

**Add**
- `crates/engram-models/src/embeddings/tract.rs` — `TractEmbeddingProvider`
  implementing `EmbeddingProvider` (load fp32 ONNX via tract-onnx, tokenize with
  `tokenizers`, mean-pool + L2-normalize; per-shape runnable cache; distinct
  `model_id()`). Lift the pipeline from `experiments/tract-vs-ort/src/main.rs`.

**Modify**
- `crates/engram-models/Cargo.toml` — add `tract-onnx` (target-gated and/or
  behind a `tract` feature); make `ort`/`fastembed` optional under an
  `onnxruntime` default feature.
- `crates/engram-onnx/Cargo.toml`, root `Cargo.toml` — propagate the feature so
  the Intel build excludes `ort`.
- `crates/engram-models/src/embeddings/mod.rs` — export the tract provider;
  `#[cfg]`-select it.
- `src/ops/mod.rs` — add the tract provider's spec to `provider_specs` /
  `expected_embedding_fingerprint`, and select it in `resolve_provider` /
  `resolve_engine_providers` when the tract backend is active. Extend the
  `EmbeddingBackend` enum (or add a resolution branch) for `Tract`.
- `crates/engram-types/src/config.rs` — if a config knob is wanted
  (`[embeddings].backend = "tract"` or an auto value), add it; keep it out of
  `provider_cache_key` only if it doesn't change the loaded model (it does —
  so it MUST be in the key).
- `docs/users/installation.md`, `troubleshooting.md`, `embeddings.md` — document
  the tract backend, the Intel-Mac auto-default, the fp32-model/reindex
  implication, and the ~3× cost.
- `.claude/CLAUDE.md` — note the tract provider + feature in the architecture
  section.
- Release workflow — optionally restore a `build-intel` job that builds with
  `--features tract` (pure Rust, no ORT download) once the backend exists.

## Implementation phases

1. **Provider (Linux-testable).** Add `tract-onnx` behind an explicit `tract`
   feature; implement `TractEmbeddingProvider`; wire into provider resolution +
   fingerprint. Prove on Linux: `--no-default-features --features tract`
   produces a working store, semantic search returns sane results, and
   `cargo tree` shows **no `ort`/`fastembed`** in that build.
2. **Correctness + reindex.** Test the fingerprint swap: a store built with ort
   then opened with tract warns + reindexes; embeddings match the experiment's
   1.00000 cosine vs ORT fp32.
3. **Shape/runnable cache + perf.** Add the per-shape runnable cache; add a
   bench point; confirm ~3× is the steady-state cost (not per-call rebuild).
4. **Intel-Mac auto-default.** Layer `[target.'cfg(...)'.dependencies]` so
   `x86_64-apple-darwin` builds pick tract automatically with no ORT dep.
   Validate the build excludes ORT (can only be fully confirmed on a real
   Intel-Mac / cross build).
5. **Release + docs.** Restore `build-intel` using `--features tract`; update
   docs.
6. **(Optional follow-up)** tract reranker on the fp32 BGE model; leave NLI/T5
   disabled on tract until fp32 exports exist.

## Testing & validation

- `--features tract` build on Linux: unit + integration tests pass; `cargo tree`
  proves ORT/fastembed absent.
- New test: tract embedding cosine vs a recorded ORT-fp32 reference ≥ 0.999.
- Fingerprint/reindex test: backend swap triggers the documented reindex path.
- Bench: tract single/batch latency tracked (expect ~3×/~6× ORT).
- Full existing suite green with default features (tract off) — no regression to
  the ORT path.

## Risks

- **Cargo feature unification pulling `ort` into the tract build** — the whole
  fix fails if `ort` still compiles/links on Intel. Must be verified with
  `cargo tree` and (ideally) a real Intel-Mac build. Highest-risk item.
- **tract-onnx op/shape coverage on future model bumps** — any model change must
  be re-validated against tract's static analyser (see the int8 failure).
- **Can't fully validate Intel-Mac from CI/Linux** — the `cfg(target)` behavior
  and final link are only truly confirmed on an Intel-Mac runner or cross build.
- **Perf** — 3× is fine for embeddings-on-demand; do not extend tract to any hot
  path without measuring.
