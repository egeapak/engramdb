# Daemon lifecycle: CLI opt-in, heartbeat self-heal, session-aware idle

**Date:** 2026-06-03
**Status:** Approved (design)
**Branch:** `feat/daemon-lifecycle`

## Problem

The shared embedding daemon is auto-spawned only from the MCP `serve` path and
reaped after `idle_timeout_secs` (default 900s) of no *inference*. Three gaps
follow from that:

1. **CLI never uses the daemon.** `ops::build_engine` (`src/ops/mod.rs:419`)
   resolves providers in-process with `pool=1`, so every `engramdb query` /
   `create` pays the full ~240ms ONNX cold-load. The only non-MCP
   `connect_or_spawn` caller is the `daemon` subcommand.
2. **A live MCP session never re-spawns a dead daemon.** `daemon_handle`
   (`crates/engram-mcp/src/server.rs:1076`) caches the resolved handle — *including
   a `None` failure* — in a `OnceCell` for the process lifetime. If the first
   spawn fails, that session is in-process forever; if the daemon later
   idle-dies, the session degrades to in-process and only a brand-new process
   brings a daemon back.
3. **Idle reaps while sessions are still active.** Because clients use one-shot
   connections, the daemon's `active` connection counter is ~0 between requests,
   so an idle-but-open agent session does nothing to keep the daemon alive.

## Goals

- CLI routes model-needing ops through the daemon when one is reachable, with a
  clean in-process override.
- An MCP process self-heals: it detects a dead/replaced daemon and re-spawns,
  rather than silently degrading for the rest of its life.
- The daemon stays resident while *any* agent session is alive and reaps only
  after the last one ends.
- Heartbeat activity is observable in `daemon status`.

## Non-goals

- No request-ID multiplexing or persistent-connection pooling (YAGNI; the
  one-shot connection model is kept).
- No "hard idle" cap on inference inactivity — alive-while-connected is the
  intended behavior (explicit decision).
- No change to the embedding/NLI/rerank model code or the provider-cache key.

## Design

### 0. Backbone — shared resolver + re-resolvable daemon cell

The change everything else hangs off.

- **Unify provider resolution.** Lift the "daemon vs in-process" decision out of
  MCP's `resolve_providers` (`server.rs:1042`) and CLI's `build_engine`
  (`ops/mod.rs:419`) into a single `ops`-level helper:

  ```
  resolve_providers(config, backend, dir, policy) -> EngineProviders
  enum DaemonPolicy { ConnectOrSpawn, ConnectOnly, InProcess }
  ```

  Both front-ends call it; the policy expresses the only behavioral difference.

- **Re-resolvable cell.** Replace the `OnceCell<Option<Arc<DaemonHandle>>>` with a
  `tokio::sync::Mutex<DaemonState>` holding `current: Option<Arc<DaemonHandle>>`
  and `last_spawn_attempt: Instant`. One `ensure_daemon()` routine is the single
  spawn site, called by warmup, the heartbeat task, and the op-path. Spawn
  attempts are rate-limited to **at most one per heartbeat tick** (backoff), which
  preserves the original "no spawn storm" property the `OnceCell` provided while
  allowing recovery. This alone fixes gaps #2's "first spawn fails → forever
  in-process".

### 1. CLI opt-in (connect-only)

- Model-needing ops route through the shared resolver with policy
  `ConnectOnly`: `query`, `create`, `update`, `reindex --embeddings-only`,
  `challenge`, `compress`, `review`. Model-free ops (`list`, `get`, `delete`,
  `stats`, `gc`, `projects`) never connect.
- **Override ladder** (mirrors `daemon::resolve_socket` precedence):
  `--in-process` flag → `ENGRAMDB_IN_PROCESS` env → `config.daemon.use_for_cli`
  (**default `true`**) → otherwise use the daemon.
- **Spawning stays opt-in.** Default CLI policy is `ConnectOnly` (use a live
  daemon, else in-process — a one-shot command never leaves a 15-min daemon
  resident). `--spawn-daemon` promotes it to `ConnectOrSpawn`.
  `config.daemon.enabled = false` forces fully in-process for the CLI too.

### 2 + 3. Heartbeat (Design B) + session-aware idle

- A per-MCP-process background task opens a **throwaway** connection and sends
  `Ping` every `max(30s, idle_timeout/3)` (~5 min against the 900s default). No
  persistent connection, no concurrency rework.
- Each served request — including `Ping` — already refreshes the daemon's
  `last_activity` (`server.rs:195`). So while any session pings, `last_activity`
  stays fresh and the daemon never idle-exits. When the last session ends, pings
  stop and the daemon reaps one `idle_timeout` later. **Item 3 needs no change to
  the daemon's idle watchdog.**
- **Self-heal.** A failed ping (connection refused / timeout) calls
  `ensure_daemon()` (respawn with backoff) and updates the shared cell, so the
  op-path immediately picks up the new daemon. During the gap, ops fall back
  in-process exactly as today — no errors, no hangs (refusal is sub-ms; the 60s
  request timeout only guards an alive-but-wedged daemon).

### 4. Ping stats in `daemon status`

- The daemon `Ctx` gains `ping_count: AtomicU64` and
  `last_ping: Mutex<Option<Instant>>`, incremented/stamped in `dispatch` on
  `DaemonOp::Ping`.
- `DaemonStatus` gains `ping_count: u64` and `last_ping_secs_ago: Option<u64>`.
- **Both are in-memory / current-daemon-only** — *not* added to the persisted
  `Counters` / `MetricsSnapshot`. Rationale: the metrics table is a fixed-schema
  LanceDB table, so persisting `ping` would force a column migration for a
  cumulative count that has little cross-restart meaning; and `last_ping_secs_ago`
  is inherently relative to "now". Ping info therefore appears only when a live
  daemon answers `Status` (which is the only context where "last ping ago" is
  meaningful). When no daemon runs, `stats --daemon` shows the persisted request
  counters as before, with no ping line.
- The CLI status formatter prints, e.g. `pings: 42 (last 12s ago)`.

### Cross-cutting

- **Protocol bump `v2` → `v3`** (wire-struct change to `Status`). Rollout is
  graceful: a new-binary session facing a still-running old (`v2`) daemon fails
  the version check in `healthy()`, cannot rebind the held socket, and runs
  in-process until the old daemon idle-reaps — then all processes converge on a
  `v3` daemon.
- **Provider-cache key unaffected** — no new model-affecting config fields
  (`use_for_cli` is a routing flag, not a model selector).
- **New config fields:** `[daemon].use_for_cli` (bool, default true). No new
  heartbeat-interval field — it is derived from `idle_timeout_secs` (YAGNI).

## Validation & verification

Per the project's mandatory gates: `cargo fmt --all`, then
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, then
`cargo nextest run --workspace --all-features`. Daemon-path tests live in
`engramdb::daemon::tests` (the MCP server compiles the daemon branch out under
`cfg(test)`, `server.rs:1021`).

Test matrix:

- **Backbone:** re-resolvable cell re-spawns after the daemon is killed; a
  failed first spawn does not poison subsequent resolutions; backoff prevents
  more than one spawn attempt per window.
- **CLI:** `ConnectOnly` uses a live daemon; `--in-process` / `ENGRAMDB_IN_PROCESS`
  / `use_for_cli=false` each force in-process; model-free ops never connect;
  `--spawn-daemon` spawns.
- **Heartbeat / idle:** a heartbeat ping refreshes `last_activity` so the daemon
  outlives `idle_timeout` while pinging; the daemon reaps after pings stop; a
  killed daemon is re-spawned by the next heartbeat and the cell is updated.
- **Status:** `ping_count` increments and persists; `last_ping_secs_ago` is
  present and monotonic within a daemon's life; CLI renders both lines.

Benches (Criterion, `benches/`): a daemon-lifecycle bench comparing warm-daemon
provider resolution vs in-process cold-load latency, to quantify item 1's CLI
win. Best-effort — skipped if model staging is unavailable in the run
environment.

## Risks

- **Protocol transitions** leave new sessions in-process until an old daemon
  reaps (bounded by `idle_timeout`, graceful). Acceptable.
- **Heartbeat keeps models resident** for the lifetime of the longest-open agent
  session. Intended; revisit a hard cap only if memory pressure is observed.
- **Backoff tuning:** too aggressive a backoff slows self-heal; too loose risks
  spawn storms. One-attempt-per-heartbeat-tick is the chosen middle.

## Phasing

1. Backbone (shared resolver, re-resolvable cell, `ensure_daemon`, protocol v3
   scaffold) + its tests.
2. CLI opt-in + overrides + tests.
3. Heartbeat task + self-heal + idle tests.
4. Ping stats in status (Ctx, protocol, metrics persistence, CLI formatter) +
   tests.
5. Benches + full-workspace verification pass.
