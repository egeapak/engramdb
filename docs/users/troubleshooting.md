# Troubleshooting

When something's off, start with:

```bash
engramdb doctor
```

It runs a full environment check: platform paths, embedding backend, model files, daemon reachability, and store consistency. Most problems below are diagnosed by `doctor` already; this page is the index from symptom to fix.

## Build / install

**`cargo install` fails with `protoc not found`.**
Install the Protocol Buffers compiler. macOS: `brew install protobuf`. Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`.

**`ort-sys` download fails with `UnknownIssuer` / certificate error.**
You're behind a corporate proxy or in a restricted-egress sandbox that the rustls-based downloader doesn't trust. See the `CLAUDE.md` "Building & testing in Claude Code on the web" section for the curl + `ORT_STRATEGY=system` workaround.

**`The requested API version [24] is not available, only API versions [1, 23]... Current ORT Version is: 1.23.x` (Intel Mac).**
You built the default (ONNX Runtime) configuration on Intel Mac. EngramDB's ONNX build targets ONNX Runtime **1.24.x** (ABI/API 24, pinned via `fastembed`/`ort 2.0.0-rc.12`), and **no prebuilt ONNX Runtime 1.24 exists for Intel Mac (`x86_64-apple-darwin`)** — Microsoft dropped x86_64 macOS builds after 1.23.x, so the build links a system/Homebrew 1.23.x runtime that the API-24 binary rejects at startup with this error.

**Fix: build the pure-Rust `tract` backend instead** — no ONNX Runtime, nothing to install:

```bash
cargo install --git https://github.com/egeapak/engramdb \
  --no-default-features --features tract
# or, in a clone:
cargo build --release --bin engramdb --no-default-features --features tract
```

The default build on this target now also prints a `cargo:warning` pointing you here. The tract backend uses the fp32 MiniLM at ~3× ONNX latency, with numerically identical vectors; NLI / reranker / T5 titling are unavailable on it (all ONNX-only). Prebuilt Intel-Mac release binaries already ship with tract, so downloading a release avoids building entirely. See [installation.md](./installation.md#platform-support) and [embeddings.md](./embeddings.md#backends).

**Prefer real ONNX Runtime on Intel Mac?** You can still build it from a self-compiled ORT 1.24.x (`ort-sys` rc.12 has no automatic compile mode, so this is manual):

```bash
# 1. Build ONNX Runtime 1.24.x for x86_64 macOS from source (~30–60 min; needs cmake + Xcode CLT).
git clone --depth 1 --branch v1.24.2 https://github.com/microsoft/onnxruntime
cd onnxruntime
./build.sh --config Release --build_shared_lib --parallel --skip_tests \
  --osx_arch x86_64 --compile_no_warning_as_error
# libonnxruntime.dylib lands under build/MacOS/Release/

# 2. Build engramdb against it (point ORT at the lib dir; dynamic link keeps the dylib external).
ORT_LIB_LOCATION="$PWD/build/MacOS/Release" ORT_PREFER_DYNAMIC_LINK=1 \
  cargo install --git https://github.com/egeapak/engramdb
```

The library **must be 1.24.x** — a 1.23.x lib reproduces the same error (API 23 ≠ 24). For most users the tract build above is far simpler.

**Apple Silicon (`aarch64-apple-darwin`) is unaffected** — `download-binaries` ships a prebuilt 1.24 runtime for it, and the default ONNX build is used.

**Long compile times on first build.**
LanceDB and `ort` pull in heavy native code. Subsequent builds are cached. Use `cargo build --release` for the production binary; debug builds are slower at runtime but compile faster.

## CLI

**"No EngramDB store found."**
You haven't run `engramdb init` in this directory (or any ancestor). Either init, or pass `--dir` to point at a project that exists, or use `--global` for cross-project memories.

**"Ambiguous ID prefix"** when running `get`/`update`/`delete`/`challenge`.
Add more characters to disambiguate, or use `engramdb list` to find the full ID.

**`add` succeeded but `query` doesn't find the memory.**
Three common causes:

1. The query filters drop it (wrong `--type`, `--min-criticality` too high, etc.).
2. Embeddings aren't enabled, so semantic search is degraded — try `--mode filter --query "<keyword>"` instead.
3. You're in a different project than you think (worktree routing, wrong CWD). Run `engramdb projects info` to see which project ID is in scope.

**Search results look stale or wrong.**
Run `engramdb reindex`. Then run `engramdb doctor` to see if there's a manifest/embedding fingerprint mismatch.

**A memory disappeared from `list` / `query` but the file is still on disk.**
It was probably invalidated — its validity window was closed by a superseding memory, an explicit invalidation (`engramdb update <id> --invalidate`, the "Invalidate" choice in `engramdb review`, or the MCP `resolve` tool), or compression (which invalidates its sources instead of deleting them). Invalidated memories are excluded by default; confirm with `engramdb list --include-invalidated` (also available on `query`). If it was closed by mistake, reopen it with `engramdb update <id> --clear-invalidated`. GC purges invalidated memories only after `[epistemic].invalidated_retention_days` (default 180; `0` = keep forever).

## Embeddings

**"Embedding model unavailable" on every command.**
First-run model download is failing. Check:
- Network connectivity to `huggingface.co`.
- That `~/.cache/engramdb/models/` is writable.
- Delete the cache dir and retry: `rm -rf ~/.cache/engramdb/models && engramdb add ...`.
- In a sandboxed environment, pre-stage models manually (see [embeddings.md](./embeddings.md) and the CLAUDE.md web-sandbox section).

**"embedding model changed" warning.**
The store's manifest fingerprint doesn't match the live provider. Run the exact command in the warning (typically `engramdb reindex --embeddings-only`). This is harmless to ignore short-term but means semantic search quality is degraded until you reindex.

**Want to disable embeddings entirely.**
Pass `--no-embeddings` at init, or set `[embeddings].provider = "none"` (or any unrecognized string) in `config.toml`. Queries fall back to keyword and relevance scoring.

## Daemon

**`daemon status` says "not running" but I see a process.**
The visible process is bound to a different socket. Use `--socket` or set `ENGRAMDB_DAEMON_SOCKET`. The socket-resolution order is `--socket` > env > config > default.

**Daemon won't start; `sun_path too long`.**
Your default socket path exceeds the OS limit (~104 bytes). Set `ENGRAMDB_DAEMON_SOCKET=/tmp/engramdb-$USER.sock` or put `socket_path = "..."` under `[daemon]` in `config.toml`.

**Daemon keeps using the old embedding model after a config change.**
Run `engramdb daemon restart`. A running daemon caches loaded models for its lifetime.

**Auto-spawn races.** Two MCP processes spawn the daemon at the same time and one fails.
This is expected and self-recovering — only one process binds the socket. The other(s) get a `connection refused`, retry with backoff, and connect to the winner. If you see persistent errors, run `engramdb daemon status` to confirm a daemon is up.

**A CLI command is slow / doesn't seem to use the daemon.**
Model-needing CLI commands use a daemon only if one is *already running* (connect-only); they don't spawn one. Start one with `engramdb --spawn-daemon <command> …`, or just let your MCP session's daemon stay up. To force local loading instead, use `--in-process` (or `ENGRAMDB_IN_PROCESS=1`, or `[daemon].use_for_cli = false`).

**The daemon stays running longer than `idle_timeout_secs`.**
That's intended. Each MCP `serve` session sends a heartbeat that keeps the daemon resident while the session is connected; it reaps `idle_timeout_secs` after the **last** session disconnects, not after the last inference. `engramdb daemon status` shows a `pings: N (last Xs ago)` line so you can see the heartbeat. If the daemon dies mid-session it is re-spawned automatically on the next request.

## Claude Code

**MCP tools not appearing.**
- Run `engramdb serve --dir <project>` manually — if it errors, that's your problem.
- Restart the Claude Code session after install/setup.
- Confirm `engramdb` is on Claude Code's `PATH` (`which engramdb`; `engramdb --version`).

**Tools prompt for approval every time.**
Run `engramdb setup --global` to write `mcp__*` permissions to `~/.claude/settings.json`.

**Duplicate hook output.**
The plugin and `engramdb setup` both wrote hooks. Remove the manual entries from `~/.claude/settings.json` (`hooks` + `mcpServers`) — the plugin manages them.

**Hooks fire but produce no output.**
- Check that the project has memories: `engramdb list`.
- Check working directory: hooks pass `--dir .`, so Claude Code's CWD matters.
- For `pre-tool-use`, only `Read`/`Write`/`Edit` events fire, and only when `tool_input.file_path` is present.
- For `session-start`, only memories with `criticality >= 0.6` (default) appear. Lower the threshold with `--min-criticality`.
- Task-scoped memories (`--generality task`) are hidden from hook injection until the session declares the matching task (`engramdb task current <NAME>` or the MCP `task_current` tool).

**Hooks keep warning "this edit may invalidate memory ...".**
The edited file matches a memory's watch paths (`--invalidated-by`). Act on it once: `engramdb verify <id>` if the memory still holds, `engramdb update <id>` if it needs amending, or `engramdb update <id> --invalidate` if it no longer applies — an invalidated memory stops warning.

## Projects and worktrees

**Memory written in one worktree doesn't show up in another.**
It should. Worktrees auto-route to the main project. If they're not, run:
- `engramdb projects info` in both — they should report the same project ID,
- `engramdb projects list` — confirm both worktrees are registered (one root + one or more linked sub-projects).

If a worktree was init'd before this routing was added, you may have a stray `.engramdb/` in the worktree. EngramDB will auto-consolidate it on the next memory op; otherwise run any memory command in the worktree to trigger consolidation.

**`projects prune` lists projects I want to keep.**
Those projects' on-disk paths no longer exist. Either restore the path, or accept the prune. The data isn't lost — `prune` deletes only the registry entry; the global data dir is preserved unless you confirm.

**Want this worktree to be its own project, not a sub-project of main.**
After init: `engramdb projects unlink <worktree_id>`. It becomes a root project.

## Performance

**Queries are slow.**
- First call in a process is ~240 ms (cold model load). Subsequent calls should be <20 ms. If they're not, run `engramdb stats --daemon` to see latencies and figure out which stage is slow.
- For very large stores (>10k memories), turn on the reranker (`[rerank].enabled = true`) and lower `top_n` to e.g. 30 — reranking quality stays high while latency drops.

**Reindex is slow.**
O(N × embedding-cost) where N = memory count. Expect minutes for thousands of memories. Run with the daemon up — batches share an open model session.

**MCP server uses a lot of RAM.**
Per-process model load. Use the daemon (`[daemon].enabled = true`, the default) to share one copy across all sessions.

## Data and migrations

**Migrating from a previous EngramDB version.**
`engramdb migrate --dry-run` first to see what would change. Then `engramdb migrate`. To revert, `engramdb rollback --target-version <N> --dry-run` then `engramdb rollback`.

**Lost / corrupted manifest.**
Delete `.engramdb/manifest.toml` and re-init — the index can be rebuilt from memory files with `engramdb reindex`. Memory files themselves are the source of truth.

**Lost / corrupted LanceDB index.**
`engramdb reindex --index-only`. Vectors are recomputed from memory files.

## Getting help

When asking for help, include:

```bash
engramdb --version
engramdb doctor
```

and (if relevant) `engramdb stats` and the output of the failing command with `-v` / `--verbose`. Set `RUST_LOG=engramdb=debug` to capture detailed logs from CLI / MCP / daemon.
