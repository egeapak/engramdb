# Plan: get Claude Code to save project facts to EngramDB instead of auto-memory

Status: planned (not implemented). Source: eval run of 2026-10-01, `.claude/hillclimb/memory-tool-use/`.

## 1. What we are up against

- **The system prompt's rule.** The `init` event names the auto-memory directory (`memory_paths.auto`, under `$CLAUDE_CONFIG_DIR/projects/<enc>/memory/`). The system prompt describes "a persistent, file-based memory system". It says: "If the user explicitly asks you to remember something, save it immediately as whichever type fits best." It has a `project` type for facts not derivable from the code.
- **Sonnet followed that rule literally.** In `nk-jobs-explicit` rep0, its first action was a `Write` of `background-jobs-use-jobs-enqueue.md`, then `MEMORY.md`, with no ToolSearch call.
- **The tooling is unequal.** The EngramDB MCP tools are deferred behind ToolSearch, while auto-memory only needs `Write`, which is always loaded. Debriefs from both models also say "nothing told me which one is canonical".
- **Controls in the claude binary (verified by grep):**
  - `autoMemoryEnabled` setting ("When false, Claude will not read from or write to the auto-memory directory").
  - `CLAUDE_CODE_DISABLE_AUTO_MEMORY` env var.
  - `autoMemoryDirectory` (ignored in checked-in project settings).
  - MCP server key `alwaysLoad`. It sits next to an `@internal` marker. Its text: "all tools from this server are always included in the prompt and never deferred behind tool search, except a tool the server itself lists with _meta anthropic/alwaysLoad set to false". The binary notes it also blocks startup until the server connects.
  - Per-tool `_meta["anthropic/alwaysLoad"]` and `"anthropic/searchHint"`.
- **No hook lets a plugin become the auto-memory backend.** The best a plugin can do is notice the `Write`.

## 2. Channels EngramDB controls

| Channel | When the model sees it | Strength |
|---|---|---|
| MCP `instructions` (`server.rs::get_info`) | System prompt, every turn | Medium; never mentions auto-memory |
| Tool descriptions (`create`, server.rs:1912) | Only after ToolSearch loads them | Weak while deferred |
| `alwaysLoad` (plugin.json or per-tool `_meta`) | Decides whether the schemas are present at all | Strong, structural |
| SessionStart `REFLECTION_NUDGE` (hook.rs:87) | Turn 1 | Medium; "Suggested, not required", end-of-task |
| UserPromptSubmit | Next to the prompt; silent unless memories match | Strong, well timed |
| PreToolUse / PostToolUse on `Write` | At the moment of the auto-memory write | Strong, precise |
| `ENGRAM.md` (setup installs only) | Through CLAUDE.md | Medium-strong; never mentions auto-memory |
| plugin.json description, `commands/*.md`, PreCompact | Rarely or never | Weak |
| `engramdb setup` | One time, interactive | Can write a setting, with consent only |

## 3. Options

- **A. Routing rule in text**, in the MCP instructions, `ENGRAM.md` and the `create` description: "Project facts, conventions, hazards and decisions, including when the user says 'remember', go to EngramDB `create`. Auto-memory is fine for personal collaboration preferences." Cheap. On its own it may not beat the system prompt.
- **B. Remove the deferral gap.** Set per-tool `_meta` `anthropic/alwaysLoad: true` on `query`, `create` and `get` only (rmcp-macros 3.4.0 supports `meta`). This avoids loading all 28 schemas. See plan 04.
- **C. Catch it at write time.** In PostToolUse: if `file_path` is under `dirname(transcript_path)/memory/` and is not `MEMORY.md`, inject "[EngramDB] You saved this to Claude Code's auto-memory. If it is a project fact, also `create` it in EngramDB so collaborators and EngramDB queries see it." Precise, and it fails open.
- **D. UserPromptSubmit nudge on remember-cues** ("remember", "from now on", "always", "never", "note that"). A heuristic.
- **E. `setup` offers to disable auto-memory.** Interactive, default **No**. Writes `autoMemoryEnabled: false` to `settings.local.json` and says so. Never silently, never from the plugin.
- **F. Import auto-memory files into EngramDB** (harvest-like). Later.

## 4. Recommendation: A + B + C now; D behind config; E and F later

1. **B:** in `crates/engram-mcp/src/server.rs`, add the `alwaysLoad` meta to `create`, `query` and `get`. Test that `list_tools` reports it for exactly those three. First verify `alwaysLoad` works with a one-case run, because it may be internal.
2. **A:** add the routing sentence to `get_info` instructions and to the `create` description. Add a "Where to save" bullet to `ENGRAM_MD_CONTENT`. Extend `REFLECTION_NUDGE` without naming MCP tools. Stay within `SESSION_CONTEXT_BUDGET`. Update the setup tests and snapshots (run twice).
3. **C:** add `is_auto_memory_write(input)`, built on `extract_transcript_path` and `extract_file_path`. Branch on it in `process_post_tool_use` before the `relativize_path` early return. Tests:
   - a memory file fires;
   - `MEMORY.md` stays silent;
   - a project file keeps its current behavior;
   - malformed input returns None.
4. **D:** `[hooks].remember_nudge` (default on) with an `infer_remember_cue(prompt)` modeled on `infer_situation`.
5. Run `cargo fmt --all`, clippy with `-D warnings`, and `cargo nextest run --workspace --all-features`.

## 5. Measuring it

- **grade.py:**
  - Add `wrote_auto_memory`: any `Write`/`Edit` under `init.memory_paths.auto`. It is a rate, never part of `pass`.
  - Add `auto_memory_writes` to `gap_signals`.
  - Add `both_stores`: an EngramDB create plus an auto-memory write.
  - Add a `test_grade.py` case built from `nk-jobs-explicit` rep0.
- **Run:** both models, 3 reps, nk-* plus all `no_false_create` cases, with and without `--engram-md`.
- **Success:**
  - Sonnet: explicit_create ≥ 67% (from 0%), implicit_capture ≥ 40% (from 0%), "auto-memory only" ≤ 20% (from 16/24).
  - Opus: explicit ≥ 90% (from 67%), implicit ≥ 65% (from 53%).
  - Both: no_false_create and no_spurious_revise do not drop, and cost rises by less than 10%.

## 6. Risks and open questions

- **User trust:** route and remind, but never disable auto-memory without an explicit yes in `setup`.
- **Personal vs. shared:** `.engramdb/` is committed. Personal preferences must stay in auto-memory or in `project="global"`.
- **Double saves:** C duplicates by design. Should the message say "move" instead of "also save"?
- **`alwaysLoad`** may be internal or gated. Verify it before relying on it.
- **Custom directory:** if `autoMemoryDirectory` moves the folder, C misses the writes.
- **The system prompt wins:** if A+B+C still underperforms on Sonnet, E (with consent) is the fallback.
