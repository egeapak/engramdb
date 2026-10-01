# Plan: rank-mode empty results, and steering away from direct `.engramdb/` reads

Status: planned (not implemented). Source: eval variants v4–v11, and a probe of 12 agent-style queries on the seeded fixture.

## Finding: direct reads follow empty queries

- **Opus reads memory `.md` files directly** (`Read`, or `cat` through Bash) in 7–12 runs per variant.
- **Most of those reads come right after a `query` that returned nothing.** In v4, 13 of the 14 rank-mode queries returned `{"memories":[],"total":0}`.
- **Claude then finds the files** with `git ls-files` or `Grep`.
- **Reproduced with the CLI:**
  - `rank` + a query string → 0 results.
  - `rank` with no query → 10 results.
  - `filter` with the same words → the right memory.

## Root cause (verified in code)

1. **Wrong threshold placement.** Rank mode drops candidates below `retrieval.relevance_threshold` (0.45, `crates/engram-types/src/config.rs:705`). It does this **before** the reranker runs (threshold `src/retrieval/engine.rs:1490-1495`, rerank `:1515-1526`).
2. **The threshold is on the wrong scale for query scores.**
   - It was set for scope-only scoring.
   - With a query, semantic similarity from `1/(1+d²)` puts unrelated memories at about 0.41–0.46, so the noise floor straddles 0.45.
3. **A keyword hit can lower a score.**
   - Any keyword hit switches a memory to the keyword formula (`engine.rs:1418-1424`).
   - A partial hit gives kw ≈ 0.1–0.35 (`src/search/keyword.rs:270`).
   - So the matching memory scores **below** unrelated ones. Example: "migration schema customers column" scores 0.330 for the right memory and 0.41–0.43 for unrelated ones.
4. **The reranker agreed but never ran.** In the invoices example, the reranker scored the right memory 0.570 after blending, but it never saw the candidate.
5. **The empty response gives no hint**, and the dropped candidates are not reported. By the CLAUDE.md rule, an undeclared loss path is a bug.
6. **Side bug: `RetrievalConfig` (`config.rs:688`) has no `#[serde(default)]`.** A partial `[retrieval]` section makes the whole config file be ignored.

On the 12 probe queries:
- **Rank mode returned nothing for 6.**
- **Filter mode returned the right memory for all 12.** Its empties come from the keyword sufficiency gate (`engine.rs:1453-1468`) on pure-synonym queries; a hint fixes those, a threshold change does not.

## Fix (recommended now)

1. **Move the rank threshold after the reranker** whenever a reranker ran.
2. **Rank never returns empty when candidates exist.** It returns the top 3, each marked `below_threshold: true`, plus a top-level `dropped_below_threshold: N`.
3. **An empty result carries a `hint`** in both modes. Example: "0 matches above 0.45; 10 memories exist. Retry with mode filter, fewer or literal terms, or no query."
4. **Add `#[serde(default)]` to `RetrievalConfig`.**

Later, behind a benchmark run:
- a keyword hit must never lower a score (`max(keyword formula, semantic formula)`);
- a per-regime or relative threshold.

## Steering away from direct reads

**Recommended: a `PreToolUse` branch.**
- In `process_hook_input` (`crates/engram-cli/src/commands/hook.rs:521`): when a `Read` targets `.engramdb/memories/*_<uuid>.md`, emit context: "This is an EngramDB memory file. Use `get <id>` or `query`; change it with `update`, not Edit."
- Non-blocking, and it fails open.
- It does not cover Bash `cat` or `Grep`, because the plugin's matcher is `Read|Write|Edit`.

Not recommended:
- **A deny** (`permissionDecision: "deny"`): it blocks legitimate human-requested inspection, and it goes against the fail-open rule.
- **A `setup` permission rule** `Read(.engramdb/**)`: the same objection, and silent.

The main lever is the retrieval fix: an agent that gets results has no reason to read the files.

## Tests

- **Engine:**
  - A rank query that the reranker rescues.
  - Rank with every candidate below the threshold returns a non-empty, marked result.
  - A filter query with no keyword match returns a hint.
- **MCP:** a snapshot of the empty-result JSON.
- **Config:** a partial `[retrieval]` section parses.
- **Hook:** a Read of a memory file emits context with the id; a malformed file name gives no output.

## Eval

- **Split `empty_queries` by mode.**
- **Targets:**
  - rank empties under 10% (today 13 of 14);
  - filter empties unchanged;
  - "store-read runs" below 7 (Opus, default fixture).
- **Cheap regression check:** keep the 12-query probe.
