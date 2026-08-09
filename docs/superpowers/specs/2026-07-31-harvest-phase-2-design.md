# Design: harvest phase 2 — MCP surface, budgets, hooks, and the archive

Four follow-ups to the `/engram:harvest` feature. Each is analyzed below with
the constraints that shape it; two of them cannot be built as originally
described and the reasons are load-bearing.

## Summary of constraints discovered

| Constraint | Source | Consequence |
|---|---|---|
| `SessionEnd` is cleanup-only — cannot inject context or show output | Claude Code hooks docs | The "prompt me at session end" idea must move to `Stop` |
| `Stop` fires per turn, user present, can inject `additionalContext` | hooks docs | This is the nudge hook — but it fires *every turn*, so it must self-limit |
| `transcript_path` is in every hook event, but **lags** the live conversation | hooks docs | Stop-hook heuristics must use `last_assistant_message` for the current turn |
| `.engramdb/` contains **no** `.gitignore`, and shared memories travel by `git clone` | verified on a fresh `init`; `hook.rs` source comment | Transcript archives must **not** live under `.engramdb/` |
| `zstd` and `flate2` are already in `Cargo.lock` (via lance) | lockfile | Compression adds no new dependency tree |

---

## 1. MCP tool surface

**Reversing an earlier call.** Phase 1 deliberately shipped CLI-only, arguing
the slash command has Bash. That was too narrow: it assumes the plugin
install, assumes Bash is permitted, and forces the agent through shell
quoting for what is structured data. Exposing the operations as tools lets the
agent drive the whole flow natively, and returns typed JSON instead of text
the model has to re-parse.

**Proposed tools** (4, matching the existing `projects_*` / `compress_*`
naming precedent):

| Tool | Maps to | Notes |
|---|---|---|
| `harvest_list` | `harvest list` | filters: `since`, `limit`, `include_harvested`, `all_projects` |
| `harvest_show` | `harvest show` | returns the digest; `max_chars` defaults to the **fan-out** budget, not the single-session one |
| `harvest_mark` | `harvest mark` / `reset` | `memory_ids` + `decision`; `clear: true` subsumes `reset` |
| `harvest_ledger` | `harvest ledger *` | read + prune the ledger and archives |

Folding `reset` into `harvest_mark` keeps the count at 4. Every MCP tool costs
tokens in *every* session's tool list, and this surface is already at 23; four
more is defensible, six is not.

**Two things the MCP path must get right that the CLI does not:**

- `harvest_show` output lands directly in context with no human in between, so
  its default budget must be the conservative fan-out value. A tool that can
  silently return 50k tokens is a footgun.
- The `DIGEST_TRUST_HEADER` must be part of the returned payload, not added by
  the CLI renderer, or the MCP path loses the marking entirely. This means
  moving the header into `render_digest_markdown`'s output contract (already
  true) **and** into the structured JSON variant (currently only in the
  `markdown` field — needs a dedicated `trust` field so a client that reads
  `events` directly still sees it).

No model loading is involved, so these tools bypass `ProviderCache` entirely
and cost nothing at startup.

---

## 2. Configurable budgets

Today `DEFAULT_DIGEST_BUDGET` is a hardcoded 12,000 chars (~3k tokens).

**The tension to resolve.** "Huge by default" is right for reading *one*
session deeply and wrong for fanning out over twelve — the same number cannot
serve both. Measured: a 1.2 MB transcript digests to 63 KB uncapped. At a
200k-char default, twelve sessions inline is ~600k tokens.

**So: two budgets, not one.**

```toml
[harvest]
# Single-session deep read (`harvest show`, CLI). 0 = unlimited.
digest_budget = 200000
# Per-session budget when scanning many (MCP default, subagent fan-out).
fanout_budget = 20000
```

200,000 chars (~50k tokens) comfortably contains every session observed so
far, making the default effectively "complete digest" while keeping a ceiling
against a pathological transcript. `0` means genuinely unlimited for anyone
who wants it.

Also worth moving into config, since they are per-user taste rather than
per-invocation decisions:

```toml
include_thinking = false      # reasoning blocks
include_sidechains = false    # subagent turns
```

**Wiring notes.** `[harvest]` goes on `EngramConfig` with `#[serde(default)]`
like every other section. It must **not** be added to
`provider_cache_key` — that key covers model-affecting fields only, and
harvest touches no model. Adding it there would needlessly evict cached
provider bundles on every harvest config tweak.

---

## 3. The nudge hook

**The original shape does not work.** `SessionEnd` cannot inject context or
show the user anything — it is explicitly cleanup-only, with "no decision
control". A prompt there reaches nobody.

**`Stop` is the correct hook**: it fires when the agent finishes responding,
the user is present, and it can return `hookSpecificOutput.additionalContext`.

But `Stop` fires **every single turn**, which creates three problems the
design has to solve or the feature becomes an irritant:

### 3a. It cannot judge worth — only candidacy

Deciding whether a conversation holds durable knowledge is exactly the LLM
judgment the harvest flow delegates to the agent. A Rust hook cannot do it.
It can only compute cheap signals:

| Signal | Why it correlates with durable knowledge |
|---|---|
| **no memory created this session** | *Necessary condition* — if the agent already saved, there is nothing to nudge about |
| failure → success on the same command family | "X failed, Y worked" is the highest-value pattern in a transcript |
| user standing-instruction language | "always", "never", "going forward", "from now on", "instead of" |
| ≥ N file modifications | substantive work happened |
| turn count / duration over threshold | filters trivial exchanges |

Fire only when the necessary condition holds **and** the remaining score
clears a threshold. This is a two-stage filter: cheap heuristic in the hook,
real judgment in the agent, which can still suppress the suggestion if it
disagrees. The injected text must say "consider asking", not "tell the user".

### 3b. Noise control is the whole ballgame

The user's requirement — *say nothing when there is nothing* — is the hard
part. A nudge that fires often trains the user to ignore it, which is strictly
worse than no feature. Mitigations, all required:

- **At most once per session.** Recorded in state, not re-derived.
- **Honor a `Skipped` verdict** (§4): a session the user declined never nudges
  again.
- **Default off**, or default on only at a conservative threshold. Recommend
  shipping `enabled = false` and turning it on after real-world tuning — a
  false-positive rate we cannot measure yet should not be on by default.

### 3c. Latency

`Stop` runs per turn. Phase-1 hooks already treat latency as critical
(`build_engine_without_providers` exists solely to dodge a 240 ms ONNX init).
Re-scanning a growing transcript every turn is a per-turn tax — 0.25 s at
40 MB, measured.

Mitigations: check the "already created a memory" condition first and exit
immediately; persist a byte offset per session and scan only the tail; cap
bytes scanned per invocation. Note also that `transcript_path` **lags** the
live conversation, so the current turn's final message must come from the
event's `last_assistant_message` field instead.

**Loop safety.** A `Stop` hook that injects context gives the agent another
turn, which can fire `Stop` again. Use `additionalContext` only — never
`decision: "block"` — and rely on the once-per-session guard. (Whether a
`stop_hook_active` field is available to detect re-entry could not be
confirmed from the docs and must be verified against the runtime before
implementation.)

### 3d. What `SessionEnd` *is* good for

It cannot talk, but it can do side effects — which is exactly what §4 needs.
Division of labor:

- **`Stop`** → the nudge (needs to talk, needs the user present).
- **`SessionEnd`** → silent archiving and candidate scoring (needs to write,
  not to speak).

---

## 4. Decisions and the transcript archive

### 4a. Recording the decision

Today the ledger conflates "reviewed, found nothing" with "user declined".
Make it explicit:

```rust
pub enum HarvestDecision { Harvested, Skipped, Deferred }
```

`Skipped` additionally suppresses future nudges (§3b), which is why the
distinction earns its keep rather than being bookkeeping for its own sake.

Backward compatibility: existing ledgers have no `decision` field. Add it as
`Option<HarvestDecision>` with `#[serde(default)]` and infer on read
(`memories_created > 0` → `Harvested`, else `Skipped`). No migration step, and
note this is a plain JSON file — **not** the LanceDB schema, so
`CURRENT_SCHEMA_VERSION` is not involved.

### 4b. Archive location — the one decision that is hard to undo

**Transcript archives must go in the global data dir, not `.engramdb/`.**

Verified: a fresh `engramdb init` writes no `.gitignore` into `.engramdb/`,
and the codebase's own comment confirms shared memories "arrive with a
`git clone` (`.engramdb/memories/` is repo-adjacent)". Users commit that
directory.

Transcripts routinely contain secrets — env vars echoed by commands, keys
pasted into chat, contents of untracked files. Archiving them under
`.engramdb/` would quietly commit conversation logs to shared repositories.
That is a serious, hard-to-reverse leak.

The codebase already has the right home: personal memories live at
`<global_data_dir>/projects/<id>/personal/` precisely because they must not be
repo-adjacent. Archives follow:

```
<global_data_dir>/projects/<root_id>/transcripts/<session-id>.jsonl.zst
```

(Worth considering separately: the *existing* `.engramdb/state/*.json` files
carry session ids and task names into git. Lower magnitude, but the same
class of question, and probably deserves a `.gitignore` written at `init`.)

### 4c. Compression and bounds

`zstd` is already in `Cargo.lock` via lance, so this is a direct-dependency
declaration, not a new tree. JSONL with repetitive record structure should
compress roughly 10–20×; expect ~60–120 KB per typical session.

```toml
[harvest]
archive = true
archive_retention_days = 365
archive_max_bytes = 2147483648   # 2 GiB, oldest-first eviction
```

Each entry records compressed size, original size, and a SHA-256 so an
exported archive can be shown to be intact.

### 4d. When to archive — the point that decides whether this is useful

Archiving only at `mark` time protects nothing: you already have the
transcript at that moment. The archive earns its keep only if it captures
sessions **before** Claude Code prunes them (there is a `~/.claude/.last-cleanup`
marker, so pruning happens).

So archive at **`SessionEnd`** — silent side effects are precisely what that
hook can do. This is what makes deferred harvesting possible weeks later, and
it is why §3 and §4 are one piece of work rather than two.

Bounded by the retention settings above, with `harvest ledger prune` for
manual reclamation.

### 4e. Ledger subcommands

```bash
engramdb harvest ledger list [--decision harvested|skipped|deferred] [--with-archive]
engramdb harvest ledger show <session-id>
engramdb harvest ledger export <session-id> [-o <path>]   # decompress
engramdb harvest ledger rm <session-id> [--archive-only]
engramdb harvest ledger prune [--older-than 90d] [--max-bytes 500MB] [--dry-run]
```

`prune` defaults to a dry run, consistent with `gc` and `compress`.

---

## Sequencing

Config first — the other three consume it. Ledger schema before hooks,
because the hooks write verdicts into it. MCP last, so it exposes the finished
surface rather than being amended twice.

1. **`[harvest]` config** + dual budgets + `--max-chars` default wiring.
2. **Ledger v2**: `decision`, archive metadata, `harvest ledger` subcommands.
3. **Archive**: zstd writer, global-dir location, retention/eviction, `SessionEnd` hook.
4. **`Stop` nudge**: heuristics, once-per-session guard, `Skipped` suppression, default off.
5. **MCP tools**: 4 tools over the finished surface.

Steps 1–2 are low-risk and self-contained. Step 3 carries the privacy decision
(§4b) and should not be rushed. Step 4 is the one whose *value* is uncertain
until tuned against real sessions, which is the argument for shipping it off
by default.

## Open questions for the maintainer

1. **Archive default.** On by default (with the 2 GiB cap), or opt-in? On is
   more useful; opt-in is more conservative about disk and about retaining
   conversation logs at all.
2. **Nudge default.** Recommend `enabled = false` initially. Agree?
3. **`digest_budget = 200000`** — is "effectively complete, with a ceiling"
   the right reading of "huge", or do you want `0` (unlimited) as the default?
4. **`.engramdb/.gitignore` at init** — worth fixing the pre-existing
   state-file exposure in the same pass, or keep it out of scope?
