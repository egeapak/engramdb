# Design: `/engram:harvest` — mining past sessions for memories

## Problem

EngramDB captures knowledge as it is discovered: hooks nudge, `/engram:reflect`
prompts at the end of a session, and the agent writes memories as it works.
All of that is *forward-looking*. A project that installs EngramDB after
months of Claude Code use starts with an empty store, and everything learned
in those earlier sessions stays learned only by the transcripts on disk.

The same gap opens for anyone who works without reflecting: sessions that
ended abruptly, sessions where the agent never got to the nudge, sessions
whose context was compacted away. The knowledge exists — it is sitting in
`~/.claude/projects/` — but nothing reads it.

## How Claude Code stores transcripts

```text
~/.claude/projects/<encoded-cwd>/<session-id>.jsonl
~/.claude/sessions/<pid>.json          # live sessions only, not history
```

- One append-only JSONL per session; one directory per working directory.
- `<encoded-cwd>` is the cwd with every non-alphanumeric byte replaced by
  `-`. **Lossy**: `/a/b.c` and `/a/b-c` produce the same name.
- Records carry `cwd`, `gitBranch`, `sessionId`, `timestamp`, `version`,
  `uuid`/`parentUuid`, and `isSidechain`. Types observed: `user`,
  `assistant`, `attachment`, `queue-operation`, `last-prompt` (older builds
  also emit `summary`).
- `uuid`/`parentUuid` form a DAG, not a list — a rewind leaves an abandoned
  branch in the same file.

Two consequences drive the design.

**Attribution must use the recorded `cwd`, not the directory name.** The
encoding collides, and a wrong attribution would feed one project's
conversations into another project's memory store. The encoded name is used
only as a fast path to avoid scanning unrelated directories; the `cwd` field
decides.

**Worktrees file separately.** A git worktree is a different cwd for the same
repository, so its sessions live in their own directory — while its
*memories* already route to the main checkout's store (`worktree.rs`).
Harvesting only the invoking directory would therefore miss conversations
whose memories belong in the very store being written to.

## The measurement that shapes everything

Block-level byte census of a real session transcript (179 KB at the time of
measurement):

| block | bytes | share |
|---|---:|---:|
| `tool_result` | 45,689 | 25.5% |
| attachments | 21,002 | 11.7% |
| `thinking` | 11,726 | 6.5% |
| `tool_use` args | 7,926 | 4.4% |
| **user prose** | **665** | **0.4%** |
| **assistant prose** | **275** | **0.2%** |

**Under 1% of a transcript is the prose that carries durable knowledge.** The
same session later grew to 1.2 MB. "Have the agent read the JSONL" is not a
viable instruction — it exhausts the context window on tool payloads.

So the binary digests, and the agent judges. Measured end-to-end: 1.2 MB of
transcript renders to 63 KB uncapped and ~12 KB at the default budget.

What survives digestion, and why:

- **User prompts, verbatim** — intent, and the highest-value bytes in the file.
- **Assistant prose, verbatim** — conclusions.
- **Tool calls as one line** — name + short target. The *sequence* of actions
  shows conventions; the arguments rarely add anything.
- **Tool results as a one-line preview, with the error flag preserved.** This
  is the non-obvious one: "command X failed, command Y worked" is frequently
  the single most durable fact in a session, and it exists only in a result.
- **Reasoning and subagent turns: excluded by default.** Verbose, and their
  conclusions resurface in the prose. Both are opt-in flags.

## Components

### 1. `engram-storage/src/transcripts.rs` — discovery and decoding

Leaf module, no core dependencies. Locates the projects root (honoring
`CLAUDE_CONFIG_DIR`), encodes/verifies project directories, and parses a
transcript into a normalized `Event` stream (`UserPrompt`, `AssistantText`,
`Thinking`, `ToolCall`). Tool calls are paired with their results in a single
forward pass via a `tool_use_id` → index map.

Parsing is **lenient**: an unparseable line is skipped, never fatal.
Transcripts are written by another program and may be truncated mid-write
while a session is live.

Synthetic user turns (`<command-name>`, `<system-reminder>`,
`<local-command-stdout>`, interrupt markers, …) are filtered out. They are
machine-generated scaffolding, and mining them as human intent would produce
memories about EngramDB's own hook output.

`list_sessions_in(root, paths)` takes the root as a parameter so tests use a
fixture directory instead of mutating a process-global env var.

### 2. `engram-storage/src/harvest_state.rs` — the ledger

Mirrors `task_state.rs` exactly: a small JSON map under
`.engramdb/state/harvested_sessions.json`, advisory `flock(2)`, atomic
temp-then-rename, malformed reads as empty.

Its reason for existing is the **zero-yield** case. A session that produced
memories can be attributed after the fact; a session that legitimately held
nothing worth saving is indistinguishable from one never examined. Recording
the *examination* — with its outcome, including `0` — is what makes a second
harvest cheap and stops previously-declined candidates being re-proposed
forever.

### 3. `src/ops/harvest.rs` — scope, selection, budget

- `session_scope` walks the registry from the root of the invoking
  directory's hierarchy (`resolve_root_project_id` + `collect_descendants`),
  yielding the main checkout plus every worktree. Registered-but-missing
  paths are kept: a deleted worktree still has transcripts worth mining.
- `select_sessions` applies `--since` / `--limit` / current-session exclusion
  / ledger filtering. Split into a pure `filter_sessions` so the rules are
  testable without IO.
- `budget_digest` drops by **class** rather than truncating uniformly:
  tool calls first, then reasoning. Prompts and prose are never dropped as a
  class; if they alone overrun, the tail is cut and the digest is **marked
  partial**. An agent that believes it saw a whole session when it saw a
  prefix will report "nothing worth saving" with unearned confidence, so the
  marking is load-bearing, not cosmetic.

### 4. `engramdb harvest` CLI

`list` / `show` / `mark` / `reset`. Presents sessions; **never writes a
memory**. Not worktree-exempt, so it routes to the main worktree root like
every other memory command — which is exactly right, since the ledger and the
scope both belong to the root project.

### 5. `commands/harvest.md` → `/engram:harvest`

Markdown plugin command, no Rust. Drives: list → digest → (subagent fan-out
above 3 sessions) → `query` for dedup → **numbered candidate table with
evidence** → confirmation → `create`/`update`/`challenge` → `mark`.

The listing gate is mandatory and stated as such: nothing is saved before the
user sees every candidate, its supporting quote, and which session it came
from. Sessions that yielded nothing are reported explicitly rather than
silently omitted.

## Why not an MCP tool

The slash command runs inside Claude Code, which has Bash. A tool would add
surface to a 3,485-line `server.rs` for a case the plugin does not hit. It
remains available later if a Bash-less front-end needs it.

## Relationship to `/engram:reflect`

Complementary, and worth keeping distinct in the docs so they don't converge:
`reflect` mines the *current, in-context* session and needs no transcript
access; `harvest` mines *past, out-of-context* sessions and does nothing else.

## Files touched

| File | Change | Rust? |
|---|---|---|
| `crates/engram-storage/src/transcripts.rs` | New: discovery + parsing | Yes |
| `crates/engram-storage/src/harvest_state.rs` | New: ledger | Yes |
| `crates/engram-storage/src/lib.rs` | Register both modules | Yes |
| `src/ops/harvest.rs` | New: scope, selection, budget, render | Yes |
| `src/ops/mod.rs` | Register + re-export | Yes |
| `crates/engram-cli/src/app.rs` | `Harvest` command + `HarvestCommand` enum | Yes |
| `crates/engram-cli/src/commands/harvest.rs` | New handler | Yes |
| `crates/engram-cli/src/commands/mod.rs` | Register + re-export | Yes |
| `crates/engram-cli/src/lib.rs` | Dispatch arm | Yes |
| `crates/engram-cli/src/output.rs` | `HarvestSessionOutput` + printer | Yes |
| `commands/harvest.md` | New slash command | No |
| `.claude-plugin/{plugin,marketplace}.json`, `Cargo.toml` | 0.9.0 → 0.10.0 | No |
| `docs/users/{claude-code,cli-reference}.md`, `.claude-plugin/README.md` | Docs | No |

## Release impact

The slash command ships with the plugin; the CLI it depends on ships in the
binary, so the two must release together. All three version fields
(`plugin.json`, `marketplace.json`, `[workspace.package]`) were moved to
`0.10.0` in lockstep — `release.yml`'s `version-check` job fails the release
if they disagree with the pushed tag.

## Testing

- `transcripts.rs`: encoding, block extraction, tool/result pairing,
  unterminated calls, sidechain + synthetic-prompt filtering, malformed
  lines, cwd-collision rejection, subdirectory cwd match, missing root.
- `harvest_state.rs`: roundtrip, zero-yield recording, overwrite-not-duplicate,
  malformed ledger, empty-id rejection.
- `ops/harvest.rs`: class drop order, complete digests, tail truncation
  flagging, single-event capping, markdown rendering of partial digests and
  failed tools, `--since` parsing, and each selection filter.
- Manual end-to-end against this machine's own transcript: list → show →
  mark → list (hidden) → `--include-harvested` (shown) → reset → list.
- Gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings`, `cargo nextest run --workspace
  --all-features`.
