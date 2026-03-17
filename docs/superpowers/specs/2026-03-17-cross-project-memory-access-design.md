# Cross-Project Memory Access via MCP

## Problem

The MCP server is initialized with a fixed project directory (`self.dir`) at startup. All 16 tool handlers operate exclusively on that project. There is no way for an agent to read or write memories from another registered EngramDB project without starting a separate MCP server instance.

## Solution

Add an optional `project` parameter to every MCP tool input. When provided, the tool operates on the specified project instead of the server's default. When omitted, behavior is unchanged.

## Project Resolution

A new `resolve_dir` method on `EngramDbServer` handles the `project` parameter:

```rust
async fn resolve_dir(&self, project: Option<&str>) -> Result<PathBuf, String>
```

Resolution rules:

1. **`None`** — return `self.dir.clone()` (current behavior)
2. **16-char hex string** — treat as project ID, look up path in `self.registry`
3. **Path-like string** — treat as absolute path, canonicalize it, compute its project ID via `compute_project_id`, verify the ID exists in the registry, return the canonicalized path
4. **Error** — if the project is not found in the registry, return a `ProjectNotFound` error directing the user to run `engramdb init` in the target project first

Detection heuristic: if the string is exactly 16 characters and all hex digits, treat as project ID. Otherwise, treat as path. A 16-char hex string is **never** treated as a path, even if a directory with that name exists.

### Edge cases

- **Relative path** — return a `ValidationError`. The `project` field must be either a project ID or an absolute path.
- **Canonicalization failure** (path doesn't exist on disk) — return `ProjectNotFound` with a message noting the directory does not exist.
- **`compute_project_id`** is the existing function in `src/storage/project_id.rs`.

### No auto-init for cross-project stores

The existing `open_store()` silently auto-initializes if `.engramdb/` doesn't exist in `self.dir`. For cross-project access (`project` is `Some`), auto-init is **disabled** — the target project must already be initialized. If `.engramdb/` does not exist in the resolved directory, return a `StoreNotInitialized` error. Auto-init only applies to the server's own project (`self.dir`).

## Error Code

Add a new `ProjectNotFound` variant to `ErrorCode` in `src/mcp/error.rs`:

```rust
ProjectNotFound, // "PROJECT_NOT_FOUND"
```

Used when `resolve_dir` cannot find the project in the registry.

## Input Struct Changes

Every MCP tool input struct gains one optional field:

```rust
#[schemars(description = "Target project: absolute path or 16-char project ID (from registry). Omit for current project.")]
project: Option<String>,
```

Affected structs (14 existing + 2 new): `CreateInput`, `RetrieveInput`, `SearchInput`, `GetInput`, `UpdateInput`, `DeleteInput`, `ChallengeInput`, `ReviewInput`, `ResolveInput`, `CompressCandidatesInput`, `CompressApplyInput`, `GcInput`, `ReindexInput`, `ListInput`.

`memory_stats` and `memory_doctor` currently take no input parameters. New structs `StatsInput` and `DoctorInput` must be created with only the `project` field, and the tool handler signatures updated to accept `Parameters<StatsInput>` / `Parameters<DoctorInput>`.

## Store/Engine Changes

New methods that accept the project override:

```rust
async fn open_store_for(&self, project: Option<&str>) -> Result<MemoryStore, String> {
    let dir = self.resolve_dir(project).await?;
    if project.is_some() {
        // Cross-project: no auto-init, require existing .engramdb/
        let engramdb_dir = dir.join(".engramdb");
        if !engramdb_dir.exists() {
            return Err(error_response(ErrorCode::StoreNotInitialized, "..."));
        }
    } else {
        // Default project: auto-init if needed (existing behavior)
        let engramdb_dir = dir.join(".engramdb");
        if !engramdb_dir.exists() {
            MemoryStore::init(&dir, self.registry.as_ref()).await...;
        }
    }
    MemoryStore::open(&dir).await...
}

async fn build_engine_for(&self, project: Option<&str>) -> Result<RetrievalEngine, String> {
    let dir = self.resolve_dir(project).await?;
    let store = self.open_store_for(project).await?;
    // Config must be loaded from the RESOLVED dir, not self.dir
    let config_path = dir.join(".engramdb").join("config.toml");
    Ok(ops::build_engine(store, &config_path, self.embedding_backend).await)
}
```

Key details:
- `open_store_for` always uses `self.registry` for any init calls, ensuring test isolation with `InMemoryRegistry`
- `build_engine_for` loads config from the resolved dir, not `self.dir`, to avoid embedding dimension mismatches
- `load_config` calls in tool handlers that use it independently must also use the resolved dir

Existing `open_store()` and `build_engine()` become thin wrappers: `self.open_store_for(None)` and `self.build_engine_for(None)`.

Each tool handler changes from:
```rust
let store = self.open_store().await?;
```
to:
```rust
let store = self.open_store_for(input.project.as_deref()).await?;
```

### Write operations on cross-project stores

All 16 tools (including mutating ones: `create`, `update`, `delete`, `gc`, `compress_apply`, `reindex`) support the `project` override uniformly. Write operations on a cross-project store:

- Acquire a write lock scoped to the **target** project's ID (not `self.dir`'s project)
- Write to the **target** project's `.engramdb/memories/` directory
- Update the **target** project's `manifest.toml`
- Do not affect `self.dir`'s data in any way

## CLI

No CLI changes. The existing `--dir` global flag already provides project override capability for CLI commands.

## Testing

Tests use `InMemoryRegistry` pre-populated with test project entries and `tempdir`-based project directories.

### Unit tests for `resolve_dir`:

1. **`None`** — returns `self.dir`
2. **Valid project ID** — returns the registered path
3. **Valid absolute path** (registered) — canonicalizes, computes project ID, returns the path
4. **Unregistered project ID** — returns `ProjectNotFound` error
5. **Unregistered path** — returns `ProjectNotFound` error
6. **Ambiguous 16-char hex input** — always treated as project ID, never as path
7. **Relative path** — returns `ValidationError`
8. **Non-existent absolute path** — returns `ProjectNotFound`

### Integration tests for cross-project operations:

7. **Cross-project create+get roundtrip** — create a memory in project B from server anchored at project A, then get it from project B and verify contents
8. **Cross-project search** — create memories in project B, search from server A, verify results come from B
9. **Cross-project delete** — create in project B, delete from server A, verify gone from B and A is unaffected
10. **Cross-project stats/doctor** — verify they report on project B's data, not project A's
11. **Default behavior preserved** — tools without `project` field continue to use `self.dir`
12. **Cross-project on uninitialized store** — returns `StoreNotInitialized` error (no auto-init)

## Files Changed

- `src/mcp/server.rs` — input structs, resolve_dir, open_store_for, build_engine_for, all 16 tool handlers
- `src/mcp/error.rs` — new `ProjectNotFound` error code

## Out of Scope

- MCP resources and prompts (they use fixed `self.dir`; cross-project access for resources can be added later)
- Hook handlers (they run in the context of the current project by design)
- Registry management (no new registry operations needed)
