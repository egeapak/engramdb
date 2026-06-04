# Session Handoff

## What Was Worked On

Continuation of the `claude/onnx-mps-feature-gate-XM4wY` branch (ONNX CPU acceleration + embedding model-identity lifecycle). This session took it from "feature-complete, review-flagged" all the way to **merged into `master`**:

1. **Fixed the 3 PR-blocker review bugs** (commit `b2dac4f`): SSE never ran the embedding model-change check; MCP `reindex` silently downgraded to index-only in `error` mode; `embedding_startup_report` swallowed transient I/O. Also unified `expected_embedding_fingerprint` ↔ `resolve_provider` via one `provider_specs()` table, and made `NliConfig::default().model` derive from `DEFAULT_NLI_MODEL.repo`.
2. **Lever E re-evaluated twice** via background subagents. First eval (stdio-assumption) → CONDITIONAL-NO-GO. User correctly pushed back: daemon mode = one daemon serving N concurrent agent sessions. Re-eval under that corrected model → **GO** (single-session throughput flat/mutex-bound; pool-of-2 +50–65% throughput, T5 is the dominant create-path chokepoint at K=4 p99 ~383ms). **Lever E deferred to its own follow-up PR** (the main remaining work — see Pending).
3. **`prepacked_weights` A/B'd → NO-GO** (0–0.8%, noise) and reverted; bench harness kept (`ENGRAMDB_BENCH_WORKLOADS=prepacked_ab`).
4. **Added 13 tests total** (`faa3643` + `9260a92`): 6 review-followup guards + 7 lifecycle coverage-gap tests (manifest status truth-table, fingerprint round-trip/back-compat, reindex partial-failure guard via `FailingEmbeddingProvider` mock, `ReindexOnModelChange` serde back-compat, T5 `encoder_hidden_shape` regression, `intra_threads` env parsing). A `pr-test-analyzer` subagent drove the gap list.
5. **Opened PR #39**, then **resolved a substantial merge conflict** with `master` (which had advanced by #38 shared-Unix-socket embedding daemon + #37 global flags), then **squash-merged #39 into master** (now `561f9ed`) and checked out `master`.
6. **Disk cleanup**: machine hit 100% full. `cargo clean` (−22 GB) + deleted ~24 GB of leaked Core ML `.mlmodelc` temp artifacts under `$TMPDIR` (ORT CoreML EP leaks them; our benchmarking generated 17,473). Now ~59 GB free.
7. **Fixed the 2 long-standing macOS `storage::worktree` test failures** (user explicitly asked) — they were a `/var`→`/private/var` `$TMPDIR` symlink artifact; fix canonicalizes both sides of the assertion (no-op on Linux).

## Key Decisions

- **Lever E is GO but deferred to a follow-up PR.** The current PR was feature-complete/green; Lever E needs a prerequisite refactor (T5 isn't in `EngineProviders` — built per-call) plus bounded-pool + config, so it earns its own isolated-review PR rather than scope-creeping #39.
- **`prepacked_weights` reverted, not shipped.** Measured ~0% gain on Apple Silicon int8 (ARM NEON doesn't benefit from x86-style weight packing; `Level3` already constant-folds). User values honest evidence-based + clean code over speculative keeps. Recoverable from git history; harness retained as the evidence.
- **Merge (not rebase) then squash.** Branch was 19 commits; merging `origin/master` once + squash-merge collapses history cleanly. Rebasing 19 commits through the #38 daemon refactor would mean repeated conflict resolution.
- **Merge conflict resolution — preserved BOTH architectures.** #38 replaced our `cached_providers`/`provider_cache: Arc<Mutex<HashMap>>` with `ops::ProviderCache` + a `daemon` `OnceCell` + `Self::resolve_providers(...)`. Resolution: take #38's daemon-aware provider plumbing, keep our `embedding_warning` field + enforcement (`build_engine_for` error-mode gate) + `assemble_engine_for` remediation split + `finish_engine` tail + `run_sse` per-connection seeding. All our `self.cached_providers(&config)` calls rewritten to `Self::resolve_providers(&self.provider_cache, &self.daemon, self.embedding_backend, &config, &dir)`.
- **Extended the daemon `Meta` protocol to carry `model_id`.** Our branch added `model_id()` to the `EmbeddingProvider` trait; #38's new `RemoteEmbeddingProvider` (default-on daemon path) didn't implement it. Correct fix (not a stub): daemon `DaemonResponse::Meta` now returns `model_id: p.model_id()`; `RemoteEmbeddingProvider` stores + returns it, so model-change detection works over the daemon with the daemon's *actual* loaded model identity.
- **`ops/mod.rs` add/add conflict**: kept #38's `provider_cache_key`/`ProviderCache` + tests, placed our `mod tests` LAST (clippy `items_after_test_module` fires under `-D warnings` if non-test items follow a test module).
- **Worktree test fix = resolve both sides of the assertion** (user's explicit direction), not canonicalize-in-helper — keeps the fix localized, doesn't change the shared helper's contract.

## Lessons Learned

- **rust-analyzer inline diagnostics lag badly during multi-edit / post-checkout** (E0308 reranker, E0046, `unexpected_cfgs` coreml/xnnpack all surfaced as false alarms). The CLAUDE.md/handoff warning is real: **trust `cargo check`/`clippy`, never the inline `<new-diagnostics>` mid-refactor**. Every single one was stale; `cargo check` was clean.
- **The user is technically sharp and will correct flawed analysis.** The first Lever E eval's "K≈1, multi-client rare" framing was wrong for the daemon deployment; the user caught it. Don't anchor on the easy assumption.
- **GPG commit signing requires the user's Touch ID** (`commit.gpgsign=true` + `pinentry-touchid`). Non-interactive `git commit` hangs or fails ("No passphrase given"). The user must be present to approve, or run commits via `! git commit` in their session. This bit us twice.
- **`git add -A` sweeps the not-ours untracked dirs** (`.claude/handoffs/`, `docs/`). Always `git reset -q HEAD .claude/handoffs docs` after staging, before committing.
- **Don't double-run the full `nextest` suite back-to-back** — it exhausted the (already near-full) disk via LanceDB tempdirs and produced 95 spurious failures. One run is the source of truth.
- **ORT CoreML EP leaks `$TMPDIR/onnxruntime-*.mlmodelc`** — ~24 GB accumulated from this branch's Core ML benchmarking. Default-off so it won't recur in normal use; periodic `find "$TMPDIR" -maxdepth 1 -name 'onnxruntime-*' -mtime +1 -delete`-style cleanup if benches are rerun.

## Open Task List

- [x] Fix 3 PR-blocker bugs (SSE check / reindex error-mode / startup-report swallow)
- [x] Unify provider→spec map (`provider_specs`); single-source NLI default
- [x] Lever E re-eval under multi-tenant daemon model → GO (deferred to follow-up)
- [x] `prepacked_weights` A/B → NO-GO, reverted
- [x] Add 13 regression/lifecycle-gap tests; gate green
- [x] Open PR #39, comprehensive conventional body
- [x] Disk cleanup (cargo clean + Core ML temp leak ~46 GB total)
- [x] Resolve master merge conflict (#38 daemon + #37 globals), wire `model_id` through daemon `Meta`
- [x] Fix the 2 macOS `$TMPDIR` worktree test failures (resolve both assertion sides)
- [x] Squash-merge PR #39 → master (`561f9ed`); checkout master
- [ ] **NEXT: Lever E session-pool follow-up PR** (see Pending Items — this is the main next item)
- [ ] FUTURE: #15 opt-level/free-dim (NO-GO per analysis; only `prepacked` was MAYBE and is now NO-GO)

## In Progress

None — all work committed and merged. `master` is at `561f9ed`, clean. Next session starts the Lever E follow-up fresh from `master`.

## Current State
- **Branch**: master
- **Base branch**: master (work merged here; start the follow-up on a new branch off master)
- **HEAD**: 561f9ed8da355981b4553a3f7e9fcb7bc4019379
- **Status**: clean (only non-ours untracked `.claude/handoffs/`, `docs/` — leave them; do NOT `git add -A`)
- **Recent commits**: `561f9ed` (#39 ours, squashed), `50c01fd` (#38 daemon), `2c075b7` (#37 globals), `ad84bc4` (#36 worktree)
- **Saved at**: 2026-05-19T19:30:08Z

## Important Files

For the Lever E follow-up, read these on `master` (post-merge state):
- `src/ops/mod.rs` — `ProviderCache` (process-wide, `Arc<Mutex<HashMap>>`), `provider_cache_key`, `resolve_engine_providers`, `EngineProviders { embedding, nli, reranker }`, `assemble_engine`. **T5 is NOT here** — it's built per-call in the title path; threading it into `EngineProviders` is the Lever E prerequisite.
- `src/mcp/server.rs` — `Self::resolve_providers(provider_cache, daemon, backend, config, dir)` (daemon-aware, the chokepoint), `build_engine_for` (error-mode gate), `assemble_engine_for` (non-enforcing remediation), `finish_engine` (shared tail), `run_sse`/`run_stdio` startup wiring, `with_shared_model_caches`.
- `src/daemon/{mod,server,remote,protocol,client}.rs` — #38's shared daemon. `remote.rs` `RemoteEmbeddingProvider` (now has `model_id` from `Meta`). The daemon ALREADY loads each model once machine-wide and serves over a Unix socket — **this materially overlaps Lever E's intent**; re-confirm Lever E still adds value on top of the daemon before implementing (the daemon may already give most of the multi-tenant win; pooling would be a further parallelism gain inside the daemon).
- `src/title/t5.rs` — `T5TitleGenerator`, `encoder_hidden_shape` (extracted helper + regression test). For pooling T5.
- `examples/onnx_bench.rs` — `bench_lever_e` (`ENGRAMDB_BENCH_WORKLOADS=lever_e`) + `bench_prepacked_ab` harnesses. Re-runnable.
- `src/storage/worktree.rs` — the 2 tests fixed (canonicalize both assertion sides).

## Pending Items

**Lever E session-pool follow-up PR** (the next session's main task):
1. **Branch off `master`** (`561f9ed`), do NOT work on master directly.
2. **Re-confirm value vs the daemon first.** #38's daemon already loads models once machine-wide. Re-run `ENGRAMDB_BENCH_WORKLOADS=lever_e cargo run --release --example onnx_bench` and reason about whether a *pool inside the daemon* (parallel sessions) still gives the measured +50–65%, or whether the daemon alone already captures most of it. The earlier Lever E numbers predate the daemon merge — they may need re-baselining against the daemon path.
3. **Prerequisite refactor**: thread T5 into `EngineProviders`/`assemble_engine`/`resolve_engine_providers` so it can be cached/pooled like embedding+NLI.
4. **Implement bounded pool** (recommended: pool-of-2 for embedding + T5; NLI optional): `pool_size × intra_threads ≤ cores`. Likely lives in `ops::ProviderCache`/the daemon server, not per-MCP-process. Add a `[daemon]`/embeddings config knob.
5. Gate: `cargo fmt --all` → `cargo clippy --all-targets --all-features -- -D warnings` → `cargo nextest run` (never `cargo test`).
6. Cold rebuild expected first (post `cargo clean`).

## Restore Commands

- macOS, online, standard `cargo`. First build is COLD (~minutes; `ort`+`lancedb`) after this session's `cargo clean`.
- Mandatory gate before any "done": `cargo fmt --all` then `cargo clippy --all-targets --all-features -- -D warnings` then `cargo nextest run` (NOT `cargo test`).
- Lever E bench: `ENGRAMDB_BENCH_WORKLOADS=lever_e cargo run --release --example onnx_bench`.
- If disk pressure returns: Core ML temp leak cleanup `find "$TMPDIR" -maxdepth 1 -name 'onnxruntime-*' -exec rm -rf {} +`.

## Context & Snippets

- **Lever E measured (pre-daemon-merge, 8-core Apple Silicon, int8):** embedding single-session flat ~298–303 ops/s K=1→8 (mutex-bound); pool-2 +50–65% throughput, ~−33% p99. NLI ~73 ops/s flat. T5 dominant: K=4 p99 ~383ms, combined create p99 ~470ms/agent. Pool rule: `pool_size × intra_threads ≤ cores` (pool-2 on 8-core/intra-4). RAM +~200 MB for pool-of-2.
- **`Self::resolve_providers` signature** (the merged chokepoint): `async fn resolve_providers(provider_cache: &ops::ProviderCache, daemon: &Arc<tokio::sync::OnceCell<Option<Arc<crate::daemon::DaemonHandle>>>>, backend_override: Option<EmbeddingBackend>, config: &EngramConfig, dir: &Path) -> ops::EngineProviders` — uses daemon if `daemon.enabled` + reachable, else in-process `ProviderCache`.
- EngramDB MCP memory has the full decision trail: ids ~`019e413d`, `019e4173`, `019e418b` (PR #39 + Lever E follow-up), plus the lever-e-redux agent's measured-verdicts memory. Query before re-deriving.

## User Preferences

- **Decisive, evidence-based, honest** — welcomes NO-GO findings backed by data; skeptical of optimistic hand-waving; will technically challenge flawed analysis (and be right).
- **One PR per cohesive unit**; defer large add-ons to focused follow-up PRs rather than scope-creep.
- **`cargo nextest run`, never `cargo test`**. Don't add tests unless asked (but this session they explicitly asked for adequate coverage).
- **Verify before destructive ops** — asked for `find … -print` dry-run before the temp-file `rm`. Mirror that: show what will be deleted/changed first.
- **Commits**: `commit.gpgsign=true` + Touch ID — they approve interactively; suggest `! git commit` if non-interactive.
- Untracked `.claude/handoffs/`, `docs/` are NOT ours — never `git add -A` them.
- Plain-text decisive recommendations with rationale; rejected `AskUserQuestion` previously.

## Notes

- PR #39 is **merged & squashed** — do not try to reopen/re-merge it. The branch `claude/onnx-mps-feature-gate-XM4wY` still exists locally/remote (not deleted); safe to delete if desired.
- The merge correctly integrated #38's daemon. The daemon being default-on (`daemon.enabled=true`) changes the Lever E calculus — re-baseline before implementing.
- `git:conflict-resolve` skill was used for the merge; `pr-test-analyzer` + `devops:performance-expert` subagents were used (re-spawn if deeper detail needed; their syntheses are in EngramDB memory).
- Two prior handoffs exist (`brisk-quantizing-otter`, `depth-decaying-scope`) — this one supersedes them for current state.
