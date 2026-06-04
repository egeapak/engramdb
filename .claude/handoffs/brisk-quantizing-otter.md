# Session Handoff

## What Was Worked On

Started as "feature-gate MPS/Core ML backend for ONNX models, see if it speeds things up." Expanded (with the user steering) into a full ONNX acceleration + embedding-model-lifecycle effort, all on branch `claude/onnx-mps-feature-gate-XM4wY`, intended to become **one PR** off `master`.

Arc of the work:
1. **Core ML feature gate** (`ca18922`) + comprehensive benchmark harness (`88127af`, `06f2d02`). ONNX Runtime has no "MPS" EP; the Apple path is the **Core ML** EP. Result: Core ML **loses to CPU on every axis** for our small models (2.5–4.3× slower warm, worse under contention, 1.8–5× more RAM, T5+CoreML fails outright). Feature shipped **off by default**, kept as evidence.
2. **T5 fixes** (`c6628c8`): the hardcoded repo `ArsenyParamonov/t5-small-onnx` is gated (HTTP 401); repointed to `Xenova/t5-small` int8 via a new `T5ModelSpec` pattern after an A/B vs `optimum/t5-small`. Also fixed a **pre-existing latent bug**: `encode()` returned encoder shape `[1,seq]` instead of `[1,seq,hidden]` → T5 title generation had **never actually worked**.
3. **Four CPU-speedup levers** investigated via a research "forum" (parallel agents) + benchmarks:
   - **A** (NLI/T5 intra-op threads 1→`min(4,cores/2)`): ~2× faster. **GO, baked** (`40b43e8`, `ef53010`).
   - **B** (int8 all-MiniLM embeddings): 1.4–1.9× faster, 2.5–6× under contention, ~4× smaller, no quality loss. **GO** — but needed migration infra (phases 1–5 below). `f2b6a5a` (A/B), flipped in `e6331c4`.
   - **C** (XNNPACK EP): **NO-GO — hard SIGSEGV** in our stack (`5a4bfca`, default-off, kept as evidence).
   - **D** (int8 NLI via `NliModelSpec`): 2× faster, ~3.7× less RAM, 5/5 fp32 label parity. **GO, baked** (`6dfca9b`, `6b3934c`).
4. **Embedding-model-identity lifecycle** (phases 1–5, commits `8567ed9`→`e6331c4`) so flipping embeddings to int8 is safe on existing deployments (the model identity was previously not persisted → silent mixed-vector corruption). Now persisted in the manifest, surfaced by `doctor`, stamped by `reindex`, and policed by config `embeddings.reindex_on_model_change = off|warn(default)|auto|error`.
5. **Review team** (5 specialist agents) reviewed the whole branch; **Lever E** (session pool) analyzed for expected gains. Both synthesized to memory; **no fixes applied** (user: "we will explore for next session").

Everything verified at each step: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, targeted `cargo nextest run`. All committed and pushed.

## Key Decisions

- **Core ML / XNNPACK shipped default-off, NO-GO.** Comprehensive bench (cold/warm/sustained/contended/memory) proved CPU wins for our small transformer models on Apple Silicon. Kept the feature gates + harnesses as reproducible evidence, not removed.
- **XNNPACK availability dispute resolved by `nm`.** One review/research agent (reasoning from `ort-sys` cargo features + `dist.txt`) wrongly concluded XNNPACK isn't in the prebuilt binary. Another agent ran `nm` on the actual cached `libonnxruntime.a` and found it IS compiled in. I verified myself (`nm` → `XnnpackProviderFactoryCreator::Create` defined). Lesson baked into memory: **verify binary contents with `nm`, don't infer from cargo plumbing.** XNNPACK is available but segfaults at runtime in our fastembed+tokio stack → NO-GO anyway.
- **`DEFAULT_*_MODEL` single-source-of-truth pattern** (user-directed). T5 introduced `T5ModelSpec` + `DEFAULT_T5_MODEL`. User said "create other specs as well similar to t5 and update the default" — so NLI got `NliModelSpec` + `DEFAULT_NLI_MODEL`, embeddings got `DEFAULT_ONNX_EMBEDDING`, all aliasing the chosen named const. This replaced an earlier ad-hoc string-matching approach I'd started.
- **A & D baked now, B deferred then completed; one PR.** User: "let's start with A and D. we will tackle B after." A/D have no persistence implications (per-process/per-call) → safe. B changes stored vectors → needed the lifecycle infra.
- **Embedding model identity is NOT persisted (root finding).** Investigated before B: manifest/chunks/memory-files store no model id; only the dimension (implicit in Arrow schema). fp32 vs int8 all-MiniLM are both 384-dim → a swap is silent and undetectable. This justified the whole phases-1–5 effort.
- **Phase 4 enforcement: warn-default, not hard-error (user reframing).** Originally agreed hard-error. User asked "maybe instead we can warn on mcp and daemon startup by default." Decisive factor: our own Lever B data (cosine fp32↔int8 ≈ 0.99, 4/4 ranking) **undercuts the "near useless" premise** — degradation is mild. Final 4-state config `off | warn (default) | auto | error`. `warn` = log + `get_info` banner so the connecting agent prompts the user; `auto` = background reindex at stdio startup; `error` = hard-fail embedding ops. Enforcement lives in `build_engine_for` (the MCP chokepoint), NOT `open()` (so `reindex`/`doctor` still work).
- **`Untracked` (legacy stores) is treated like a mismatch under enforcement.** Consequence the user explicitly accepted: every existing deployment, on upgrade, will be flagged until one `engramdb reindex --embeddings-only` (or `auto`). Default `warn` keeps search working (mildly degraded) meanwhile.
- **T5 branch could not be split from Core ML.** `claude/t5-repo-compare` depended on `onnx_ep` (from the Core ML commit). User chose to fast-forward-merge T5 into the mps branch and do one PR. Done.

## Lessons Learned

- **T5 title generation never worked** before this branch — latent `encode()` shape bug; no T5 tests + default strategy is Keyword hid it.
- **Quantized fastembed models panic on empty batch** ("chunk size must be non-zero") where fp32 returned empty. Surfaced when flipping the embedding default; fixed defensively in `OnnxProvider::embed_batch` (short-circuit empty). Hardens ingest against zero-chunk memories regardless of model.
- **Don't infer EP availability from cargo features** — verify the actual downloaded binary with `nm`. A reasoning-only agent was confidently wrong about XNNPACK.
- **rust-analyzer diagnostics lag during multi-edit sequences** — they reference pre-edit line numbers. Trust `cargo check`/`clippy`, not the inline `<new-diagnostics>` mid-refactor.
- **`cargo fmt --all && cargo fmt --all -- --check` can show a diff**: the first applies formatting; if rustfmt reflows (e.g. a long `match`), re-run once and it's clean. Not a real failure.
- **Review agents over-claim**; several "CRITICAL" findings were self-retracted on closer reading (e.g. `embed_batch` single `?` is correct, T5 `hidden_dim` div-by-zero guarded, `Clone`/serve mutation sound). Always de-dup + verify agent output before acting.
- Web-sandbox builds need workarounds (protoc, prebuilt ort lib via curl, pre-staged HF models) — see project `.claude/CLAUDE.md`. On macOS it's all normal/online.

## Open Task List

- [x] #10 Lever C XNNPACK feature gate + benchmark — NO-GO (segfault), shipped default-off
- [x] #11 Lever A intra-op thread sweep — ~2× at 4 threads
- [x] #12 Lever B int8 embeddings A/B — strong GO, quality verified
- [x] #13 Lever D int8 NLI A/B — strong GO, 5/5 parity
- [x] #16 Persist embedding model id + reindex-on-mismatch — phases 1–5 done
- [x] #17 Bake Lever A default (`intra_threads min(4,cores/2)`)
- [x] #18 Bake Lever D default (int8 NLI, `DEFAULT_NLI_MODEL`)
- [x] #19 Phase 1 model_id() + manifest EmbeddingFingerprint
- [x] #20 Phase 4 enforcement modes + MCP/startup surfacing
- [x] #21 Phase 5 flip `DEFAULT_ONNX_EMBEDDING` to int8 (+ empty-batch fix)
- [ ] #14 **FUTURE: Lever E session pool (N=2-3) for concurrency** — analyzed (low/conditional ROI); needs a concurrency bench scenario added to `onnx_bench` before implementing on spec
- [ ] #15 **FUTURE: opt-level/prepacking/free-dim/mem-pattern** — lower ROI, not started
- [ ] **Review follow-ups (next session, before PR):**
  - [ ] Fix bug: SSE transport never runs the embedding model-change check (`run_sse` per-connection servers have `embedding_warning: None`)
  - [ ] Fix bug: MCP `reindex` tool does `build_engine_for(...).ok()` → in `error` mode silently runs index-only, reports `embedded:0/errors:[]` as success
  - [ ] Fix bug: `embedding_startup_report` `.ok()?` chains swallow transient I/O → warning silently absent (at least `tracing::warn!`)
  - [ ] Decide on test-gap list (status truth table, `ReindexOnModelChange` serde + missing-field backward-compat, fingerprint round-trip, reindex partial-failure guard, expected-vs-resolve parity) — all pure-logic, deferred per "don't add tests unless asked"
  - [ ] Design: unify `expected_embedding_fingerprint` ↔ `resolve_provider` via one `spec_for_config()` (divergence risk, flagged by 3 agents)
  - [ ] Add `const`-assert/test: `NliConfig::default().model == DEFAULT_NLI_MODEL.repo`
  - [ ] Decide: `T5TitleGenerator::with_repo` removal is a breaking public API change — add shim/`#[deprecated]`?
  - [ ] Verify `--features coreml`/`xnnpack` on Linux CI with `--all-features` (not confirmed broken)

## In Progress

None — all started work is committed/pushed. Session ended on review synthesis + Lever E analysis (deliverables, no code changes). Next session picks from the review-follow-up list above.

## Current State
- **Branch**: claude/onnx-mps-feature-gate-XM4wY
- **Base branch**: master
- **HEAD**: e6331c42fb5753c22b17ac396620a97fb298d80a
- **Status**: clean (only pre-existing untracked `.claude/handoffs/`, `docs/` — NOT ours; leave them)
- **Commits since base**: 15 (base `ad84bc4`)
- **Saved at**: 2026-05-19T16:54:29Z

## Important Files

- `src/onnx_ep.rs` — central EP/thread policy. `Backend{Cpu,CoreMl,Xnnpack}`, `coreml_available()`/`xnnpack_available()`, `default_backend()`, `intra_threads()` (env `ENGRAMDB_ONNX_INTRA_THREADS`, default `min(4,cores/2)`), `providers_for`/`apply_backend`. cfg-gated `coreml_eps`/`xnnpack_eps` helper-fn pattern (clippy-clean across all feature combos).
- `src/embeddings/onnx.rs` — `OnnxModelSpec`(+`name`), `ONNX_ALL_MINILM`/`_Q`, `DEFAULT_ONNX_EMBEDDING = ONNX_ALL_MINILM_Q` (int8 now default), `model_id()`, empty-batch fix, `with_model`/`with_model_on` (note: naming inconsistent vs `with_spec` elsewhere — review item).
- `src/nli/onnx.rs` — `NliModelSpec`, `NLI_DEBERTA_XSMALL`/`_Q`, `DEFAULT_NLI_MODEL = _Q`, `new_on` repo→file string-dispatch (review flagged as fragile), `build()` private ctor. `DEFAULT_MODEL_REPO` (test-only) still fp32 — review item.
- `src/title/t5.rs` — `T5ModelSpec`, `T5_OPTIMUM`/`T5_XENOVA_Q`, `DEFAULT_T5_MODEL=_Q`, `with_spec_on`, the fixed `encode()` shape logic (`hidden_dim = data.len()/length.max(1)`).
- `src/storage/manifest.rs` — `EmbeddingFingerprint{model,dimensions}`, `Manifest.embedding: Option<_>` (serde default/skip = backward compat), `EmbeddingModelStatus` (Match/Mismatch/DimensionMismatch/Untracked) + `embedding_status()`. Praised by review as well-designed.
- `src/storage/store.rs` — `MemoryStore::embedding_fingerprint()` / `set_embedding_fingerprint()` (manifest read / atomic write).
- `src/ops/mod.rs` — `expected_embedding_fingerprint(config)` (model-load-free, mirrors `resolve_provider` — **divergence risk, top design item**), `embedding_model_report()`, `EmbeddingModelReport`.
- `src/ops/reindex.rs` — stamps fingerprint only on `errors.is_empty()` (partial failure leaves store flagged).
- `src/ops/doctor.rs` — `check_embedding_model_identity` environment check.
- `src/mcp/server.rs` — `build_engine_for` (error-mode hard-fail chokepoint), `embedding_startup_report`, `auto_reindex_default`, `run_stdio` startup block, `get_info` instructions augmentation, new `embedding_warning` field. **SSE path (`run_sse`) does NOT wire this — review bug #1.**
- `src/types/config.rs` — `ReindexOnModelChange` enum (off|warn|auto|error, default warn), `EmbeddingsConfig.reindex_on_model_change`, `NliConfig.model` default = Xenova int8.
- `examples/{onnx_bench,t5_compare,embed_quality,nli_quality}.rs` — re-runnable benchmark/quality harnesses (kept intentionally). `onnx_bench` lacks a concurrency-submission scenario (needed for Lever E).
- `benches/benchmarks.rs` — criterion `onnx_backend` group.

## Pending Items

See "Review follow-ups" in the task list — the 3 genuine bugs are PR-blockers. Branch is otherwise feature-complete and pushed. The single PR off this branch has not been opened yet (user does merges/PRs locally; `--all-features` clippy must pass on Linux CI too).

## Restore Commands

macOS (online): standard `cargo` works. Mandatory gate before any "done": `cargo fmt --all` then `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo nextest run` (NOT `cargo test`). Re-run a CPU/CoreML bench: `cargo run --release --features coreml --example onnx_bench`. (Web sandbox needs protoc + prebuilt ort lib + staged HF models — see project `.claude/CLAUDE.md`.)

## Context & Snippets

- **Lever E expected gains (analysis, not yet built):** single-agent stdio ≈0 gain (calls ~sequential, mutex rarely contended). Concurrent load (SSE multi-client / ingest-overlap): ceiling ≈2× throughput, **capped by `cores/intra_threads`** — with `intra_threads=4` on 8 cores only ~N=2 fits before oversubscription (Lever A proved oversubscription degrades). RAM cost real (+50–200 MB/embedding session; NLI/T5 heavier). If pursued: pool **embeddings only**, N=2, with reduced `intra_threads`; keep NLI/T5 single. Prerequisite: add concurrent-submission scenario to `onnx_bench` and measure real concurrency K (likely ~1 for stdio).
- Bench numbers (8-core Apple Silicon, int8, warm): embed_single ~3.7 ms, nli_classify ~13.5 ms (4 threads), t5 ~85 ms. Core ML ~2.5–4.3× slower. int8 vs fp32 cosine ≈ 0.99, 4/4 ranking parity; NLI 5/5 label parity.
- EngramDB MCP memory in active use — many memories created this session (search the store; key ids include `019e40c4…` "B SHIPPED", `019e40d2…` review+Lever E synthesis, plus per-lever/per-hazard entries). Global `~/.claude/CLAUDE.md` now mandates EngramDB MCP for persistent memory.

## User Preferences

- **Background work**: use Claude Code background tasks (`run_in_background`), **never `nohup`** (explicitly corrected; saved to memory).
- **Tests**: always `cargo nextest run`, never `cargo test` (LanceDB concurrency). Don't add tests unless explicitly asked.
- **Git**: no `git -C`/`-C` flags when already in repo dir; pre-existing uncommitted/untracked work is sacred (the untracked `.claude/handoffs/`, `docs/` are not ours). User does merges/PRs locally; wants **one PR** off this branch.
- **Decision style**: rejected `AskUserQuestion` tool more than once — prefers plain-text questions and decisive recommendations with rationale. Wants results reported per step.
- **Workflow**: commit + push each logical unit (per lever / per phase); keep harnesses as re-runnable artifacts; mandatory fmt+clippy gate before declaring anything done.
- Prefers honest, evidence-based conclusions over optimistic ones (welcomed the Core ML/XNNPACK NO-GO findings; cared about quality sanity before flipping defaults).

## Notes

- The whole branch is **one cohesive PR-to-be**; don't merge to master or split — user merges locally.
- `master` is the base; branch tracks `origin/claude/onnx-mps-feature-gate-XM4wY` (pushed through `e6331c4`).
- Project `.claude/CLAUDE.md` mandates the fmt+clippy gate and EngramDB MCP memory; root may have `AGENTS.md` (check).
- Review specialists' full output is summarized in memory `019e40d2…`; re-spawn agents only if deeper detail needed.
