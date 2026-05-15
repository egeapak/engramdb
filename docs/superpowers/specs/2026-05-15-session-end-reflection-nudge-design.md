# Design: End-of-task reflection nudge + `/engram:reflect` slash command

**Date:** 2026-05-15
**Status:** Approved (pending written-spec review)

## Problem

EngramDB's MCP server exposes a `memory-session-end` **prompt** (`src/mcp/server.rs:2081`)
that reminds the agent to capture durable learnings before a session ends. It is
never triggered automatically: MCP prompts are surfaced to the *user* as slash
commands, not invoked autonomously by the agent. There is also no Claude Code
`Stop`/`SessionEnd` hook wiring it in. As a result, end-of-task reflection
effectively never happens.

"Session end" here means **the agent finished the task it was assigned and is
about to hand control back to the user** — not the Claude Code process
lifecycle.

## Goals

- When the agent finishes its assigned task, it should be *nudged* (suggested,
  not forced) to reflect: capture anything durable about the **project,
  environment, or the user's preferences** — explicitly **not** task minutiae.
- Provide a **mandatory** user-invocable slash command to trigger the same
  reflection on demand.
- Reflection means: `query` existing memories first (avoid duplicates / find
  what to update), `create` durable learnings, `challenge` contradictions,
  optionally `review` flagged memories with the user.

## Non-goals

- **No `Stop` hook.** Explicitly out of scope (fires every turn; would require
  Rust changes + a release; user declined).
- **No `SessionEnd` hook.** Structurally cannot inject context into the model
  (confirmed via Claude Code docs) — useless for driving reflection.
- No changes to the existing `memory-session-end` MCP prompt behavior.
- No new Rust hook commands (`HookCommand` enum untouched).

## Design

Two components, neither requiring new Rust hook logic.

### Component 1 — Standing reflection nudge (instruction-only)

Add a concise standing instruction so it is reliably present in the agent's
context, in two places:

1. **MCP server `instructions` field** — `src/mcp/server.rs:1836-1851`
   (the `get_info()` `instructions` string, surfaced as the
   `# MCP Server Instructions` block at session start). This is always present
   whenever the MCP server is connected and is the primary delivery channel.

2. **SessionStart hook preamble** — `src/cli/commands/hook.rs`
   `run_hook_session_start` (calls `format_detailed_context("[EngramDB] Key
   project memories:", …)` at line ~302). Secondary / belt-and-suspenders.

   Caveat to address: `run_hook_session_start` currently early-returns when
   there are no memories (`if result.memories.is_empty() { return Ok(()) }`,
   ~line 298). The nudge must be emitted **even when the store is empty**, so
   this path must still output the nudge context instead of returning silently.

**Nudge text (canonical wording, reused in both places and the slash command):**

> When you finish the task(s) assigned to you and are about to hand control
> back to the user, briefly reflect: is there anything durable worth
> remembering about the **project**, the **environment/tooling**, or the
> **user's preferences** — *not* minutiae about the current task? If so:
> `query` existing memories first (to update rather than duplicate), then
> `create` the durable ones, and `challenge` anything that contradicts an
> existing memory. This is a suggested nudge, not a required step.

The MCP-instructions copy may be condensed to one or two sentences to keep that
block tight; the SessionStart copy and the slash command use the fuller form.

### Component 2 — `/engram:reflect` slash command (mandatory)

A new plugin command file: `commands/reflect.md` in the plugin root, surfaced
as `/engram:reflect`.

- Plugin commands are markdown; **no Rust changes**.
- Content instructs the agent to run the reflection flow on demand:
  1. `query` (mode `rank` / `filter`) to review existing project memories.
  2. Identify durable learnings about project / environment / user preferences
     discovered this session (skip task minutiae).
  3. `create` new memories for them; `update`/`challenge` where existing
     memories are stale or contradicted.
  4. Optionally `review` to address flagged (`NeedsReview`/`Challenged`)
     memories with the user.
- The command body may also mention that the user can alternatively pull the
  `memory-session-end` MCP prompt for the stats-augmented variant.

Requires creating the plugin `commands/` directory (does not exist yet).

## Files touched

| File | Change | Rust? |
|---|---|---|
| `src/mcp/server.rs` (~1836) | Append condensed nudge to `instructions` string | Yes (string only) |
| `src/cli/commands/hook.rs` (`run_hook_session_start`, ~298–305) | Append nudge to SessionStart context; emit even when no memories | Yes |
| `commands/reflect.md` | New plugin slash command | No |
| `.claude-plugin/plugin.json` | Bump `version` (0.5.0 → next); reconcile with crate version (0.6.0) | No |

`src/cli/app.rs` (`HookCommand` enum), `src/cli/mod.rs` dispatch, and
`src/cli/commands/setup.rs` hook fallback are **untouched** (no new hook).

## Release impact

- The slash command (`commands/reflect.md`) ships via the plugin manifest →
  needs a `plugin.json` `version` bump and a plugin/marketplace refresh, but
  **no binary release** for that component.
- The Component 1 nudge text lives inside the `engramdb` binary
  (`server.rs` instructions + `hook.rs` SessionStart). Users only receive it
  after upgrading the binary, so it rides the next normal crate release. It is
  additive and low-risk (string + a no-empty-return tweak).
- Recommendation: reconcile the lagging `plugin.json` version (0.5.0) with the
  crate version (0.6.0) as part of this change.

## Testing

- `hook.rs`: unit test that `run_hook_session_start` output includes the nudge
  marker text, including the **empty-store** case (regression for the
  early-return change).
- `hook.rs`: existing SessionStart tests still pass (nudge appended, memories
  still grouped/budgeted as before).
- `server.rs`: assert `get_info().instructions` contains the nudge phrase.
- Manual: `/engram:reflect` resolves and produces the reflection flow; verify
  `.claude-plugin/plugin.json` is valid JSON with bumped version.
- Gate: `cargo fmt --all` + `cargo clippy --all-targets --all-features
  -- -D warnings` clean; full suite via `cargo nextest run`.

## Open questions

None. Stop/SessionEnd hooks confirmed out of scope.
