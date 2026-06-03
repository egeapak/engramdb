# Daemon Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the CLI use the shared embedding daemon (opt-in, connect-only), make MCP processes self-heal a dead/replaced daemon via heartbeats, keep the daemon resident while any session is alive, and surface heartbeat stats in `daemon status`.

**Architecture:** One `ops`-level provider resolver replaces the duplicated daemon-vs-in-process logic in MCP and CLI, backed by a re-resolvable daemon cell (replacing the permanent `OnceCell`) whose single `ensure_daemon()` spawn site is rate-limited by backoff. MCP adds a background heartbeat task that `Ping`s on a throwaway connection every `idle_timeout/3`; each ping refreshes the daemon's `last_activity` (keeping it alive while sessions exist) and a failed ping re-spawns. The daemon tracks ping count + last-ping time in memory and reports them in `Status`.

**Tech Stack:** Rust 2021, Tokio, `rmcp` (MCP), LanceDB, ONNX via `fastembed`/`ort`. Tests via `cargo nextest`; gates `cargo fmt --all` + `cargo clippy --workspace --all-targets --all-features -- -D warnings`.

**Spec:** `docs/superpowers/specs/2026-06-03-daemon-lifecycle-design.md`

**Conventions (mandatory, every task):**
- Tests run with `cargo nextest run`, **never** `cargo test`.
- Before every commit: `cargo fmt --all` then `cargo clippy --workspace --all-targets --all-features -- -D warnings` (zero warnings).
- snake_case test names. Commit with `--no-gpg-sign` (GPG key is passphrase-locked in this env) and trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Daemon-path tests live in `src/daemon/tests.rs` (the MCP server compiles its daemon branch out under `cfg(test)`), modeled on the existing `daemon_answers_ping` / `daemon_status_reports_metrics` harness (`tokio::spawn(run_daemon(socket, dur))` + poll-until-connectable).

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/engram-types/src/config.rs` | `DaemonConfig` | add `use_for_cli: bool` (default true) |
| `src/daemon/protocol.rs` | wire types | bump `PROTOCOL_VERSION` → `"3"`; add `ping_count` + `last_ping_secs_ago` to `DaemonStatus` |
| `src/daemon/server.rs` | daemon process | `Ctx` gains `ping_count`/`last_ping`; `dispatch` stamps them on `Ping`; `Status` reports them |
| `src/daemon/client.rs` | client handle | expose `spawn_daemon` reuse for re-resolve; keep `connect_or_spawn` |
| `src/ops/daemon_resolve.rs` *(new)* | shared resolver + `DaemonPolicy` + re-resolvable `DaemonCell` + `ensure_daemon` | create |
| `src/ops/mod.rs` | ops surface | re-export resolver; `build_engine` grows a `DaemonPolicy` param |
| `crates/engram-mcp/src/server.rs` | MCP server | replace `OnceCell` field with `DaemonCell`; use shared resolver; add heartbeat task |
| `crates/engram-cli/src/app.rs` | Clap defs | global `--in-process` / `--spawn-daemon` flags |
| `crates/engram-cli/src/commands/*.rs` | CLI handlers | thread `DaemonPolicy` into engine builds for model-needing ops |
| `crates/engram-cli/src/output.rs` | formatting | render `pings: N (last Xs ago)` in daemon status |

---

## Phase 1 — Backbone: shared resolver + re-resolvable cell

### Task 1: `DaemonPolicy` + config flag

**Files:**
- Modify: `crates/engram-types/src/config.rs` (DaemonConfig ~709-745)
- Create: `src/ops/daemon_resolve.rs`
- Modify: `src/ops/mod.rs` (add `pub mod daemon_resolve;` + re-exports)
- Test: `crates/engram-types/src/config.rs` (tests mod), `src/ops/daemon_resolve.rs` (tests mod)

- [ ] **Step 1: Failing test for `use_for_cli` default.** In `config.rs` tests, add:
```rust
#[test]
fn daemon_use_for_cli_defaults_true_and_is_overridable() {
    let cfg: EngramConfig = toml::from_str("").unwrap();
    assert!(cfg.daemon.use_for_cli);
    let cfg: EngramConfig =
        toml::from_str("[daemon]\nuse_for_cli = false\n").unwrap();
    assert!(!cfg.daemon.use_for_cli);
}
```
- [ ] **Step 2: Run → fail.** `cargo nextest run -p engram-types -E 'test(daemon_use_for_cli_defaults_true)'` → FAIL (no field `use_for_cli`).
- [ ] **Step 3: Implement.** Add to `DaemonConfig`: `#[serde(default = "default_daemon_use_for_cli")] pub use_for_cli: bool,`; add `fn default_daemon_use_for_cli() -> bool { true }`; set it in `impl Default for DaemonConfig`. Add `DaemonPolicy` to `daemon_resolve.rs`:
```rust
/// How a front-end may obtain model providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonPolicy {
    /// Use a live daemon, spawning one if absent (MCP default).
    ConnectOrSpawn,
    /// Use a live daemon only if already running, else in-process (CLI default).
    ConnectOnly,
    /// Never touch the daemon.
    InProcess,
}
```
- [ ] **Step 4: Run → pass.** Same nextest filter → PASS. Then `cargo check -p engram-types` and `cargo check -p engramdb`.
- [ ] **Step 5: fmt + clippy + commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A && git commit --no-gpg-sign -m "feat(daemon): add daemon.use_for_cli config + DaemonPolicy"
```

### Task 2: re-resolvable `DaemonCell` + `ensure_daemon` (backoff)

**Files:**
- Modify: `src/ops/daemon_resolve.rs`
- Modify: `src/daemon/client.rs` (make `spawn_daemon` reusable: change `fn spawn_daemon` to `pub(crate)`; add `pub async fn probe_or_spawn` if needed, or reuse `connect_or_spawn`)
- Test: `src/daemon/tests.rs`

- [ ] **Step 1: Failing test for re-spawn after death.** In `src/daemon/tests.rs`:
```rust
#[tokio::test]
async fn daemon_cell_respawns_after_handle_lost() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("cell.sock");
    let cell = crate::ops::daemon_resolve::DaemonCell::new();
    // No daemon yet → ConnectOnly yields None.
    assert!(cell.get(&socket, 3600, DaemonPolicy::ConnectOnly).await.is_none());
    // ConnectOrSpawn spawns one and caches it.
    let h1 = cell.get(&socket, 3600, DaemonPolicy::ConnectOrSpawn).await;
    assert!(h1.is_some());
    // Kill it.
    crate::daemon::request_shutdown(&socket).await.unwrap();
    poll_until_unconnectable(&socket).await;
    // Next ConnectOrSpawn re-spawns (cell did not poison).
    let h2 = cell.get(&socket, 3600, DaemonPolicy::ConnectOrSpawn).await;
    assert!(h2.is_some());
}
```
(add a small `poll_until_unconnectable` helper mirroring the existing connect-poll loop)
- [ ] **Step 2: Run → fail.** `cargo nextest run -p engramdb -E 'test(daemon_cell_respawns_after_handle_lost)'` → FAIL (no `DaemonCell`).
- [ ] **Step 3: Implement `DaemonCell`.**
```rust
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::path::Path;
use tokio::sync::Mutex;
use crate::daemon::DaemonHandle;

struct State { current: Option<Arc<DaemonHandle>>, last_spawn_attempt: Option<Instant> }

pub struct DaemonCell { state: Mutex<State> }

impl DaemonCell {
    pub fn new() -> Self { Self { state: Mutex::new(State { current: None, last_spawn_attempt: None }) } }

    /// Resolve a live daemon handle per `policy`. Re-validates the cached
    /// handle each call (a cached handle whose daemon died is dropped), and
    /// rate-limits spawns to one attempt per `idle_timeout/3` window.
    pub async fn get(&self, socket: &Path, idle_secs: u64, policy: DaemonPolicy)
        -> Option<Arc<DaemonHandle>>
    {
        if policy == DaemonPolicy::InProcess { return None; }
        let mut st = self.state.lock().await;
        // Fast path: cached handle still answers Ping.
        if let Some(h) = &st.current {
            if h.check_health().await { return Some(Arc::clone(h)); }
            st.current = None; // dead — drop it
        }
        // Try a bare connect (no spawn) first.
        let sock = socket.to_path_buf();
        if let Some(h) = DaemonHandle::connect_only(sock.clone()).await {
            st.current = Some(Arc::clone(&h));
            return Some(h);
        }
        if policy == DaemonPolicy::ConnectOnly { return None; }
        // ConnectOrSpawn, with backoff.
        let window = Duration::from_secs((idle_secs / 3).max(1));
        if let Some(t) = st.last_spawn_attempt {
            if t.elapsed() < window { return None; }
        }
        st.last_spawn_attempt = Some(Instant::now());
        let h = DaemonHandle::connect_or_spawn(sock, idle_secs).await;
        st.current = h.clone();
        h
    }
}
```
Add `pub(crate) async fn connect_only(socket: PathBuf) -> Option<Arc<Self>>` to `client.rs` (a `connect_or_spawn` variant that runs only the initial `healthy()` check and returns `None` instead of spawning). Make `check_health` non-`cfg(test)` (promote to `pub(crate) async fn`).

> **Backoff note:** the test uses a 3600s idle, so its window is 1200s — but `connect_only`/health re-validation handle the kill/respawn path without waiting on the spawn-backoff window because the prior handle is health-checked and a fresh `connect_or_spawn` runs once. Verify the test's second spawn path is not gated by `last_spawn_attempt` from the first (first spawn sets it; second is within window). **Fix:** only set `last_spawn_attempt` is for *failed* spawn storms — set it before the attempt but the re-spawn test kills then expects respawn. To keep the test honest, reset `last_spawn_attempt = None` on a *successful* spawn so a confirmed-dead daemon can be respawned immediately. Implement that reset.
- [ ] **Step 4: Run → pass.** Same filter → PASS.
- [ ] **Step 5: fmt + clippy + commit.**
```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A && git commit --no-gpg-sign -m "feat(daemon): re-resolvable DaemonCell with spawn backoff"
```

### Task 3: shared `resolve_providers` helper

**Files:**
- Modify: `src/ops/daemon_resolve.rs`, `src/ops/mod.rs` (`build_engine` grows policy param)
- Test: `src/daemon/tests.rs`

- [ ] **Step 1: Failing test.** Assert `resolve_providers(cfg, None, dir, InProcess)` returns providers without touching a socket, and with `ConnectOnly` against a live daemon returns remote-backed providers (model_id matches the daemon's). Model the daemon-backed half on `remote_embedding_end_to_end`.
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement.**
```rust
pub async fn resolve_providers(
    cell: &DaemonCell, config: &EngramConfig,
    backend: Option<EmbeddingBackend>, dir: &Path, policy: DaemonPolicy,
) -> EngineProviders {
    if config.daemon.enabled && policy != DaemonPolicy::InProcess {
        let idle = config.daemon.idle_timeout_secs;
        let socket = crate::daemon::resolve_socket(None, &config.daemon);
        if let Some(handle) = cell.get(&socket, idle, policy).await {
            let resolved = Some(ops::resolve_backend(config.embeddings.backend, backend));
            if let Some(p) = crate::daemon::remote_providers(
                handle, dir.to_string_lossy().into_owned(), resolved, config).await
            { return p; }
        }
    }
    crate::ops::resolve_engine_providers(config, backend, 1) // in-process fallback
}
```
Change `build_engine(store, config_path, backend)` → `build_engine(store, config_path, backend, cell: &DaemonCell, policy: DaemonPolicy)` and route through `resolve_providers`. Update its one current caller chain (CLI) in later tasks; for now give MCP/tests a `DaemonCell`.
- [ ] **Step 4: Run → pass.**
- [ ] **Step 5: fmt + clippy + commit** (`feat(daemon): unified resolve_providers across front-ends`).

---

## Phase 2 — CLI opt-in

### Task 4: CLI flags + plumbing

**Files:**
- Modify: `crates/engram-cli/src/app.rs` (global args), `crates/engram-cli/src/lib.rs` (dispatch passes policy), `crates/engram-cli/src/commands/query.rs` / `create.rs` / `update.rs` / `challenge.rs` / `compress.rs` / `reindex.rs` / `review.rs`
- Test: `crates/engram-cli` integration tests + `src/daemon/tests.rs`

- [ ] **Step 1: Failing test for policy resolution.** Add a unit test for a pure helper `cli_daemon_policy(in_process_flag, spawn_flag, config) -> DaemonPolicy`:
```rust
#[test]
fn cli_policy_precedence() {
    let mut c = EngramConfig::default();
    assert_eq!(cli_daemon_policy(false, false, &c), DaemonPolicy::ConnectOnly);
    assert_eq!(cli_daemon_policy(true,  false, &c), DaemonPolicy::InProcess);
    assert_eq!(cli_daemon_policy(false, true,  &c), DaemonPolicy::ConnectOrSpawn);
    c.daemon.use_for_cli = false;
    assert_eq!(cli_daemon_policy(false, false, &c), DaemonPolicy::InProcess);
    c.daemon.enabled = false; // master switch wins
    assert_eq!(cli_daemon_policy(false, true,  &c), DaemonPolicy::InProcess);
}
```
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement.** Add to the Clap global args: `--in-process` and `--spawn-daemon` (bool flags). Also honor `ENGRAMDB_IN_PROCESS` (truthy) as equivalent to `--in-process`. Implement `cli_daemon_policy`: `enabled==false || in_process || (!use_for_cli && !spawn) → InProcess`; `spawn → ConnectOrSpawn`; else `ConnectOnly`. Thread the resolved policy + a process-wide `DaemonCell` (built once in `lib.rs::run`) into each model-needing command's `build_engine` call. Leave `list/get/delete/stats/gc/projects` untouched.
- [ ] **Step 4: Run → pass.**
- [ ] **Step 5: fmt + clippy + commit** (`feat(cli): route model ops through daemon (connect-only) with --in-process override`).

### Task 5: CLI end-to-end behavior test

- [ ] **Step 1:** Add a daemon-tests case: with a live daemon on a temp socket and `ENGRAMDB_DAEMON_SOCKET` set, `build_engine(..., ConnectOnly)` produces a remote-backed engine; with `InProcess` it does not connect (assert via a sentinel: kill the daemon and confirm `InProcess` still resolves providers).
- [ ] **Step 2-4:** fail → implement (if any helper missing) → pass.
- [ ] **Step 5:** commit (`test(cli): daemon connect-only and in-process override e2e`).

---

## Phase 3 — Heartbeat + self-heal

### Task 6: MCP server uses `DaemonCell` + heartbeat task

**Files:**
- Modify: `crates/engram-mcp/src/server.rs` (replace `daemon: Arc<OnceCell<...>>` field at ~545/601/647 with `daemon: Arc<DaemonCell>`; rewrite `resolve_providers`/`daemon_handle` to delegate to `ops::daemon_resolve::resolve_providers` with `ConnectOrSpawn`; add `spawn_daemon_heartbeat`)
- Test: `src/daemon/tests.rs`

- [ ] **Step 1: Failing test: heartbeat keeps daemon alive past idle.**
```rust
#[tokio::test]
async fn heartbeat_pings_keep_daemon_alive_past_idle() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("hb.sock");
    // idle_timeout 1s; ping every ~330ms keeps it alive.
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(1)));
    poll_until_connectable(&socket).await;
    let sock = socket.clone();
    let hb = tokio::spawn(async move {
        for _ in 0..6 { ping_once(&sock).await; tokio::time::sleep(Duration::from_millis(330)).await; }
    });
    tokio::time::sleep(Duration::from_millis(1500)).await; // > idle_timeout
    assert!(UnixStream::connect(&socket).await.is_ok(), "daemon should still be alive");
    hb.abort();
}
```
Plus a self-heal test: kill the daemon mid-life, run one heartbeat tick that calls `cell.get(.., ConnectOrSpawn)`, assert a new daemon is connectable.
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement.** Replace the `OnceCell` field with `Arc<DaemonCell>`. `resolve_providers` becomes a thin call into the shared helper (`ConnectOrSpawn`). Add:
```rust
pub fn spawn_daemon_heartbeat(&self) {
    if !self.config_daemon_enabled() { return; }
    let cell = Arc::clone(&self.daemon);
    let dir = self.effective_dir.clone();
    let config_path = dir.join(".engramdb").join("config.toml");
    let backend = self.embedding_backend;
    tokio::spawn(async move {
        loop {
            let config = load_config(&config_path).await.unwrap_or_default();
            let idle = config.daemon.idle_timeout_secs;
            let interval = Duration::from_secs((idle / 3).max(30));
            // Resolve (spawns/heals if needed) — this both keeps last_activity
            // fresh and self-heals a dead daemon, updating the shared cell.
            let _ = resolve_providers(&cell, &config, backend, &dir, DaemonPolicy::ConnectOrSpawn).await;
            tokio::time::sleep(interval).await;
        }
    });
}
```
Call `spawn_daemon_heartbeat()` next to the existing `spawn_provider_warmup()` call site. Remove the now-dead `daemon_handle`/`daemon_path_enabled` `cfg(test)` shims if unused (or keep `daemon_path_enabled` gating inside the shared helper via `config.daemon.enabled` — the `cfg(test)` compile-out still applies through the `resolve_providers` call which checks `config.daemon.enabled`; tests set it false or the helper’s socket resolves to a temp path).
- [ ] **Step 4: Run → pass.** Run the full daemon module: `cargo nextest run -p engramdb -E 'test(daemon::tests::)'`.
- [ ] **Step 5: fmt + clippy + commit** (`feat(mcp): heartbeat task + self-healing daemon resolution`).

---

## Phase 4 — Ping stats in status

### Task 7: protocol + daemon ping tracking

**Files:**
- Modify: `src/daemon/protocol.rs` (`PROTOCOL_VERSION` → `"3"`; add fields to `DaemonStatus`)
- Modify: `src/daemon/server.rs` (`Ctx` + `dispatch` `Ping`/`Status`)
- Test: `src/daemon/tests.rs`

- [ ] **Step 1: Failing test.**
```rust
#[tokio::test]
async fn daemon_status_reports_ping_count_and_last_ping() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("p.sock");
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    poll_until_connectable(&socket).await;
    ping_once(&socket).await;
    ping_once(&socket).await;
    let st = query_status(&socket).await.unwrap().unwrap();
    assert_eq!(st.ping_count, 2);
    assert!(st.last_ping_secs_ago.is_some());
}
```
- [ ] **Step 2: Run → fail** (no field `ping_count`).
- [ ] **Step 3: Implement.** Bump `PROTOCOL_VERSION` to `"3"`. Add to `DaemonStatus`: `pub ping_count: u64,` and `pub last_ping_secs_ago: Option<u64>,`. In `server.rs` `Ctx` add `ping_count: AtomicU64` and `last_ping: Mutex<Option<Instant>>`. In `dispatch`, on `DaemonOp::Ping` (before returning `Pong`): `ctx.ping_count.fetch_add(1, Relaxed); *ctx.last_ping.lock().unwrap() = Some(Instant::now());`. In the `Status` arm, populate the two new fields (`ping_count.load(Relaxed)`; `last_ping_secs_ago` = `last_ping.map(|t| t.elapsed().as_secs())`). **Do not** touch `metrics.rs` / `MetricsSnapshot` (in-memory only, per spec §4).
- [ ] **Step 4: Run → pass.** Confirm the existing `protocol_roundtrip` / `daemon_status_reports_metrics` tests still pass (the version-string assertion may need updating to `"3"`).
- [ ] **Step 5: fmt + clippy + commit** (`feat(daemon): track and report ping_count + last_ping in status`).

### Task 8: CLI renders ping line

**Files:**
- Modify: `crates/engram-cli/src/output.rs` (daemon status formatter — find where `requests (cumulative...)` block is printed)
- Test: `crates/engram-cli` output test (or a string-assert unit test on the formatter)

- [ ] **Step 1: Failing test** asserting the pretty/plain status output contains `pings: 2 (last 0s ago)` for a `DaemonStatus { ping_count: 2, last_ping_secs_ago: Some(0), .. }`.
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement.** Add a line after the model-bundles line: when `last_ping_secs_ago` is `Some(n)`, print `pings: {ping_count} (last {n}s ago)`; when `None`, print `pings: {ping_count}`. Mirror existing pretty/json/plain branches (JSON: the field is already serialized via `DaemonStatus`).
- [ ] **Step 4: Run → pass.**
- [ ] **Step 5: fmt + clippy + commit** (`feat(cli): show heartbeat pings in daemon status`).

---

## Phase 5 — Benches + final verification

### Task 9: lifecycle bench (best-effort)

**Files:**
- Create: `benches/daemon_lifecycle.rs`
- Modify: `Cargo.toml` (`[[bench]] name = "daemon_lifecycle" harness = false`)

- [ ] **Step 1:** Criterion bench comparing (a) in-process `resolve_engine_providers` cold build vs (b) warm-daemon `resolve_providers(ConnectOnly)` round-trip latency, guarded to no-op/skip if the embedding model isn't staged (check `OnnxProvider::try_new().is_some()` first; if absent, `eprintln!` a skip and return). This quantifies the item-1 CLI win without failing CI when models are unavailable.
- [ ] **Step 2:** `cargo bench --bench daemon_lifecycle` compiles and runs (or cleanly skips).
- [ ] **Step 3:** Commit (`bench: daemon warm-resolve vs in-process cold-load`).

### Task 10: full workspace verification

- [ ] **Step 1:** `cargo fmt --all`
- [ ] **Step 2:** `cargo clippy --workspace --all-targets --all-features -- -D warnings` → zero warnings.
- [ ] **Step 3:** `cargo nextest run --workspace --all-features` → green (modulo the documented pre-existing flakies under full parallelism: `ops::doctor::tests::test_doctor_many_memories_healthy`, `ops::projects::tests::test_get_project_info_with_memories`, `mcp::server::tests::global_retrieve_with_semantic_query` — re-run in isolation to confirm not a regression).
- [ ] **Step 4:** Manual smoke (optional): `engramdb daemon start`; `engramdb query --rank ...` (uses daemon); `engramdb --in-process query ...` (does not); `engramdb stats --daemon` shows `pings:` after a heartbeat tick.
- [ ] **Step 5:** Final commit if any fixups (`chore: workspace verification fixups`).

---

## Self-Review

**Spec coverage:**
- §0 backbone → Tasks 1–3. §1 CLI → Tasks 4–5. §2+3 heartbeat/idle → Task 6. §4 ping stats → Tasks 7–8. Validation/benches → Tasks 9–10. ✓ all spec sections mapped.
- Protocol bump (cross-cutting) → Task 7. Config field → Task 1. Provider-cache key unaffected → no task needed (asserted: no model-affecting field added). ✓

**Type consistency:** `DaemonPolicy` (Task 1) used identically in Tasks 2–6. `DaemonCell::new/get` (Task 2) used in Tasks 3,4,6. `resolve_providers` signature (Task 3) called in Tasks 4,6. `DaemonStatus.ping_count/last_ping_secs_ago` (Task 7) consumed in Task 8. ✓

**Known implementation risk to verify during execution:** the `last_spawn_attempt` backoff vs. the respawn-after-kill test (Task 2) — the plan resolves it by resetting `last_spawn_attempt = None` on a confirmed-successful spawn so a dead daemon is respawned immediately while only *failed* spawns are rate-limited. The executor must confirm this interaction with the heartbeat self-heal test (Task 6).
