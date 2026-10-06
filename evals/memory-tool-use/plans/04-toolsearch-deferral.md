# Plan: get the EngramDB tools past ToolSearch deferral

Status: planned (not implemented). Source: eval run of 2026-10-01, `.claude/hillclimb/memory-tool-use/`.

## 1. Measurement (main runs, debrief excluded; verified)

| | Opus (123 runs) | Sonnet (122 runs) |
|---|---|---|
| Runs that called ToolSearch | 39 | 7 |
| The result contained EngramDB tools | 39/39 | 7/7 |
| A memory call followed | 37 | 7 |
| Memory calls with no ToolSearch first | 0 | 0 |
| Keyword (non-`select:`) searches | 0 | 0 |

- Every search was an exact `select:mcp__plugin_engram_memory__…`. The model already knows the names from the deferred-tools list.
- **With the server connected, finding the tools is not the problem. The extra step is:** Sonnet usually decides not to load the tools. Its debriefs say "I never loaded their schemas" and "I would have wanted `query` to be loaded by default".
- Sonnet read the store files directly instead: `direct_store_access` in 13 runs, against 9 for Opus.

## 2. Mechanism (found in the claude binary)

- **Mode:** `ENABLE_TOOL_SEARCH` selects the mode. The default defers MCP tools.
- **Server-level switch:** the config key `alwaysLoad`, described as "all tools from this server are always included in the prompt and never deferred … also blocks startup until the server is connected (capped at the standard 5s connect timeout)". It is marked `@internal`.
- **Per-tool switch:** a tool's `_meta` can carry `anthropic/alwaysLoad`, `anthropic/searchHint`, `anthropic/maxResultSizeChars` and `anthropic/requiresUserInteraction`. A tool with `_meta["anthropic/alwaysLoad"] === true` is always loaded, whatever the user's config says.
- **ToolSearch matching:**
  - An exact name returns that tool.
  - A query starting with an `mcp__` prefix returns tools with that prefix.
  - Otherwise terms are scored: name parts 12 points, substrings of name parts 6/4, `searchHint` 4, description 2.
  - Server `instructions` are not part of the score; they reach the model separately.

## 3. What EngramDB controls

- **Tools:** the 28 `#[tool]` entries in `crates/engram-mcp/src/server.rs`. rmcp-macros 3.4 accepts `meta = …`. The list is pinned by `tool_names()` and `list_all` in `server_tests.rs`.
- **Rarely needed in a session:** `gc`, `reindex`, `doctor`, `compress_*`, `projects_*`, and `harvest_*` (used by `/engram:harvest`).
- **Text and wiring:** the `get_info()` instructions, the hooks (deliberately MCP-agnostic), `mcpServers.memory` in plugin.json, and `setup.rs` (`engramdb_mcp_entry()`, `ENGRAM_MD_CONTENT`).

## 4. Options

- **A. Pin a core set** with per-tool `_meta {"anthropic/alwaysLoad": true}`: `query`, `get`, `create`, `update`, `challenge`. It removes the barrier exactly where the eval grades, for every install type. Cost: the five schemas go into every request (an estimated 3–5k tokens). Other MCP clients ignore the key.
- **B. Server-level `alwaysLoad: true`** in plugin.json. Simplest, but it loads all 28 schemas and makes startup wait for the server. The plugin schema for this key is unconfirmed.
- **C. `anthropic/searchHint` and keyword-rich descriptions.** Helps keyword search only, and the eval shows none, so the gain is low.
- **D. Hook text naming exact tool ids.** This conflicts with the MCP-agnostic hook rule unless the plugin passes a flag. Memory ids are covered by plan 01.
- **E. Fewer tools** (`engramdb serve --tools core|all`). Shrinks the deferred list, but does not remove the step.

## 5. Recommendation: A first; then C; memory ids per plan 01

1. In `server.rs`:
   - Add `fn always_load_meta()` that returns `{"anthropic/alwaysLoad": true}`.
   - Add `meta = always_load_meta()` to the five core tools.
   - Add a `searchHint` on every tool.
2. Make the set configurable: `[mcp] always_load = ["query","get","create","update","challenge"]` in `engram-types`. Apply it in a custom `list_tools` post-pass. Keep the logic in `engram-mcp`.
3. Tests in `server_tests.rs`:
   - Core tools carry the meta; the others do not.
   - Every tool has a hint.
   - The config override works.
4. `ENGRAM_MD_CONTENT`: add "If a memory tool is deferred, load it with ToolSearch `select:` before use." Update the setup tests.
5. Run `cargo fmt --all`, clippy with `-D warnings`, and `nextest --workspace --all-features`.

## 6. Eval

- **New signal in `gap_signals`:** `tool_search: [{query, matched, hit_memory, followed_by_memory_call}]`. The parser must keep `tool_reference.tool_name` items from ToolSearch results, which today come out as `""`.
- **Success on Sonnet:**
  - `query_before_act` reaches 10/53 or more (from 1/53), with Wilson intervals that separate.
  - `explicit_create` rises above 0.
  - `direct_store_access` falls.
  - No guardrail regresses.
  - Input tokens rise by 10% or less.
- **Opus:** rerun to check it does not over-query.

## 7. Risks and open questions

- The hooks already inject memories, so a low query rate can be correct for some cases. Grade per tag.
- Confirm with a smoke run that `_meta` is honored for plugin-scoped servers (check that init lists the tools as loaded).
- The token cost of five schemas, and how stable the prompt cache stays.
- The `anthropic/*` keys are undocumented and may change between Claude Code versions. Pin the version in the eval.
