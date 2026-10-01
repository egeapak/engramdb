# Plan (step 5): rank-mode empty results, and direct `.engramdb/` reads

Status: **planned, measured, ready to implement.** Replaces the first draft of this plan.

## 1. The bug, in one paragraph

With a query string, `query` in `rank` mode returns nothing most of the time, even when the right memory exists and the engine ranks it first.

The engine drops every candidate whose pre-rerank score is below `retrieval.relevance_threshold` (0.45), and only then runs the cross-encoder. That threshold was set for scope-only scoring. Query scores sit on a different scale: unrelated memories score about 0.41–0.46 on semantic similarity alone. On top of that, a memory that matches some of the query's words is moved to the keyword formula, which can score it *lower* than an unrelated memory.

When rank returns nothing, agents fall back to `git ls-files` and `Grep`, find `.engramdb/memories/*.md`, and read the files directly. That is the "store-read" signal in the eval.

## 2. Evidence

### 2.1 Code (verified)

| What | Where |
|---|---|
| Rank threshold, applied before rerank | `src/retrieval/engine.rs:1487-1495` |
| Rerank: top `rerank.top_n` (10), blend `0.5·pre + 0.5·sigmoid(logit)` | `engine.rs:1515-1526`, `apply_rerank` at `engine.rs:809` |
| `relevance_threshold` default 0.45 | `crates/engram-types/src/config.rs:705` |
| A keyword hit selects the keyword formula (`0.45kw + 0.30sem + 0.25rel`) over the semantic one (`0.55sem + 0.45rel`) | `src/scoring/composite.rs` (`composite_score`, weights at the "Determine which weights" block) |
| Keyword score: sigmoid centred on 3× the query's term count, so a partial match gives kw ≈ 0.1–0.35 | `src/search/keyword.rs:270` |
| The empty response gives no hint, and the dropped candidates are not counted anywhere | `crates/engram-mcp/src/server.rs:2185-2189` |
| `RetrievalConfig` has no `#[serde(default)]`: a partial `[retrieval]` section makes the whole config file be ignored | `config.rs:686-699` |

### 2.2 Measured on real agent queries

**Probe set.** Every `query` call the agents made during the eval on the default fixture: 149 unique queries.
- 99 are labeled with the case's target memory.
- 50 come from cases with no relevant memory (duplicate checks before `create`, general questions).

**Method.** Each query ran against a seeded fixture with the threshold at 0, so every candidate came back with its full score breakdown, including the reranker logit. Each policy below was then applied offline to the same scores (`pre = (final − 0.5·sigmoid(logit)) / 0.5`).

| Policy (applied as rank mode to all 99 labeled queries) | recall | right memory first | empty | noise per query | results on no-match queries |
|---|---|---|---|---|---|
| **Today**: pre ≥ 0.45, then rerank | 52% | 52% | **45%** | 0.2 | 0.2 |
| Threshold after rerank, 0.45 | 55% | 55% | 42% | 0.0 | 0.0 |
| Threshold after rerank, 0.30 | 76% | 76% | 21% | 0.0 | 0.0 |
| After rerank + top-1 fallback | 87% | 87% | 0% | 0.1 | 1.0 |
| Keyword fix + after rerank, 0.45 | 61% | 61% | 36% | 0.0 | 0.0 |
| **Keyword fix + after rerank + top-1 fallback** | **91%** | **91%** | **0%** | 0.1 | 1.0 |
| Keyword fix + after rerank + top-3 fallback | 97% | 91% | 0% | 0.8 | 3.0 |
| Relative cut (≥ 0.8 × top score) | 94% | 87% | 0% | 0.8 | **8.1** |
| Reranker logit ≥ −2 | 87% | 87% | 8% | 0.2 | 0.2 |
| Top 3, no threshold | 94% | 87% | 0% | 2.1 | 3.0 |
| *Filter mode as today (reference)* | 98% | — | 0% | — | 1.3 |

Split by the mode the agent actually used:
- **Rank queries (33 labeled):** today recall 24%, **76% empty**. With the keyword fix, the threshold after rerank and a top-3 fallback: recall **100%**, right memory first 82%.
- **Filter queries (66 labeled):** filter mode itself already finds the target 98% of the time. These queries matter here only as extra probe data for the rank fix.

**The remaining misses are label ambiguity.** All 9 misses under the recommended policy are `ed-discount` queries about `src/billing/invoices.py`. The finalize memory, which is scoped to that file, ranks first, and the labeled money memory ranks 2nd or 3rd. Both are relevant.

### 2.3 What the numbers say

1. **Moving the threshold alone fixes almost nothing** (+3 points). The real problem is an absolute cut on a scale that is not calibrated: no fixed threshold between 0.25 and 0.45 gets empties below 9% without adding noise.
2. **A fallback removes the empties.** The ranking itself is good: the right memory is first for 87% of queries even today. Only the cut throws it away.
3. **The keyword fix adds 4 points to "right memory first"** (87% → 91%), by removing the case where a partial keyword match scores below no match.
4. **Relative cuts flood no-match queries** (8 results per query), so they are rejected.
5. **Rank is "browse context".** Returning a few results marked as weak is better than returning none. Agents already treat an empty rank result as "search the files instead".

## 3. Design

### A. Rank never returns empty while candidates exist

- **Apply the threshold after the reranker**, on the blended score, whenever the reranker ran on the candidate. A candidate outside `rerank.top_n` keeps its pre-rerank score, which is compared against the same threshold. Without a reranker, behavior is unchanged.
- **Fallback.** If no candidate passes, return the top `retrieval.rank_fallback` candidates (new setting, default **3**; `0` restores the old empty result). Mark each with `below_threshold = true`.
  - Top-3 rather than top-1: recall 97% instead of 91% (100% instead of 82% on the agents' own rank queries). The cost is at most 3 marked weak results on a query with no match.
- **Always report the loss.** `RetrievalResult` gets `dropped_below_threshold: usize`, set on every rank query, including when the fallback was not needed. This declares the loss path (CLAUDE.md: "a loss path the code does not declare is a bug").
- **Where the mark lives:** `ScoreBreakdown.below_threshold: bool`. It derives `Default`, is built literally in only 2 places, and is already serialized, so every consumer gets the mark without touching the 38 `ScoredMemory` constructors.
- **Both rank paths:** the query path (`engine.rs` Step 6) and the scope-only path (`engine.rs` around line 1645) use the same helper, so they cannot drift.
- **Filter mode is unchanged.** It is "find specific memories", and it already finds the target 98% of the time. It does get a hint (D).

### B. Merging results across stores

`ops::query::query_with_extra_stores` merges the primary store with group and global stores. With a per-store fallback, weak results from a store with no match would sit next to confident results from another store.

**Rule, applied after the merge:**
- If any merged result is confident (`!below_threshold`), drop every weak one.
- Otherwise keep the top `rank_fallback` weak ones.
- `dropped_below_threshold` sums across stores, plus any weak results dropped here.

### C. A keyword hit never lowers a score

In `composite_score`, when both keyword and semantic evidence exist, the base score is the **maximum** of the keyword formula and the semantic formula.
- A keyword hit can then only add evidence.
- Update the doc table in `query`'s doc comment and the `scoring/mod.rs` doc.
- The `composite_score` fuzz target keeps its `is_finite()` invariant; add a unit test for the max.

### D. Empty results say what to try next

Both the MCP `query` response and the CLI JSON get a `hint` when no result is confident:
- **Rank, fallback used:** "No memory scored above the relevance threshold (0.45); these N are the closest weak matches (below_threshold: true). M memories were dropped."
- **Rank, empty** (rank_fallback = 0, or the store is empty): "0 of M memories scored above the relevance threshold. Retry with mode "filter" and literal terms, or with fewer words."
- **Filter, empty:** "No memory matched these words. Filter mode needs a keyword, tag or scope match; retry with synonyms, or with mode "rank" to browse by meaning."

The MCP response also carries `dropped_below_threshold`. The CLI pretty and plain renderers print one line saying the results are weak matches, with tier-1 snapshots for the new line.

### E. `#[serde(default)]` on `RetrievalConfig` (and `ScoringConfig`)

A partial `[retrieval]` section, such as only `relevance_threshold = 0.3`, must parse and keep the other defaults. Today it is rejected with a warning, and the whole config file falls back to defaults.

### F. Steer away from reading memory files (PreToolUse, non-blocking)

In `process_hook_input` (`crates/engram-cli/src/commands/hook.rs`), before the scope query: when a `Read` targets `.engramdb/memories/<slug>_<id>.md` under the project, emit:

> [EngramDB] This is an EngramDB memory file (id: <id>). `get <id>` returns the same content, and `query` finds related memories; change it with `update` or `challenge`, not by editing the file.

- The ID is parsed from the file name with `storage::memory_file` helpers, never with a regex of our own. It is printed only if it passes the same allowlist as `id_marker`.
- The hook fails open. The file is still read.
- Bash `cat` and `Grep` are not covered, because the plugin matcher is `Read|Write|Edit`. The retrieval fix removes the main reason agents reach for those.
- Not doing:
  - A deny, because it blocks a user who asks to inspect a memory.
  - A `setup` permission rule, for the same reason and because it is silent.

## 4. Implementation steps

1. **Config** (`crates/engram-types/src/config.rs`):
   - Add `#[serde(default)]` on `RetrievalConfig` and `ScoringConfig` (each field keeps its default fn).
   - Add `rank_fallback: usize` (default 3).
   - Update `docs/users/configuration.md`.
2. **Scoring** (`src/scoring/composite.rs`): max of keyword and semantic formulas when both exist; update the docs.
3. **Engine** (`src/retrieval/engine.rs`):
   - `ScoreBreakdown.below_threshold`.
   - `RetrievalResult.dropped_below_threshold`.
   - A shared `apply_rank_threshold(candidates, threshold, fallback)` used by both rank paths, called after rerank on the query path.
   - Order: score → sort → rerank (top_n) → threshold or fallback → truncate.
4. **Fan-in** (`src/ops/query.rs`): the merge rule from B, plus `dropped_below_threshold` summing.
5. **MCP** (`crates/engram-mcp/src/server.rs`): `below_threshold` in `ScoreBreakdownOutput`; `dropped_below_threshold` and `hint` in the query response.
6. **CLI** (`crates/engram-cli`): the same fields in JSON; a "weak matches" line in pretty and plain output.
7. **Hook** (F), with unit tests.
8. **Probe harness:** commit `evals/retrieval-probe/` (collect + simulate + a live mode that runs the CLI against the seeded fixture with the real config). It is the regression check for ranking quality; nothing else in the repo measures composite ranking (`fts_quality` measures only the keyword scorer).

## 5. Tests

- **Engine:**
  - A rank query whose right memory is below 0.45 before rerank but above it after: returned and confident.
  - A rank query where nothing passes: exactly `rank_fallback` results, all marked; `dropped_below_threshold` correct.
  - `rank_fallback = 0` keeps today's empty result.
  - The scope-only rank path behaves the same as the query path.
  - Filter mode is unchanged.
  - Use the existing stub reranker in the engine tests.
- **Scoring:** keyword + semantic is never below semantic-only for the same inputs.
- **Fan-in:** confident + weak gives confident only; weak + weak gives the top `rank_fallback`; dropped counts sum.
- **Config:** a partial `[retrieval]` section parses with defaults; an invalid value still fails validation.
- **MCP:** a snapshot of the empty-filter response with a hint, and of a rank response with fallback.
- **Hook:** a Read of a memory file emits the note with the full ID; a malformed name or a non-memory path gives the normal path; a missing store gives no output.
- **Gates:** `cargo fmt --all`, clippy with `-D warnings`, and `cargo nextest run --workspace --all-features`. Snapshot suites run twice.

## 6. Acceptance

**Live probe**, the same 149 queries through the CLI with the real default config:
- rank recall ≥ 95%, right memory first ≥ 90%, empty 0%;
- no-match queries return at most `rank_fallback` results, all marked;
- filter recall not lower than today (98%).

**Eval variant** (Opus v14, Sonnet v15, default fixture, compared with v12/v13):
- rank-mode empty results fall from 13/14 (v4) to near 0;
- "store-read runs" fall below Opus v12's count;
- `pass` stays within noise.

## 7. Risks

- **Weak results presented as relevant.** The fallback returns weak matches, and an agent may treat a weak match as an answer. Mitigations: the per-result mark, the `hint`, and a cap of 3.
- **The keyword fix changes ranking for every query.** The probe covers it for this fixture only. Real stores have more memories and more near-duplicates. The `situation` and scope multipliers still apply on top, unchanged.
- **Rerank latency is unchanged.** Rerank already ran on the pre-threshold top_n in the common case; it now always runs on the top 10, with no survivors-only shortcut.
- **Behavior change for existing callers that relied on an empty rank result:** the hooks use scope-only rank with no query (unaffected unless they hit the fallback) and filter mode. Check `process_hook_input` and the SessionStart path: they should keep today's behavior there, so hooks request `rank_fallback = 0` explicitly. A hook must not inject weak matches as "relevant memories".
