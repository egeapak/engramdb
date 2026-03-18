# Plan: Add Optional Title to Memory Files

## Problem
Memory files are saved as `<uuid>.md` with no human-readable title. When browsing `.engramdb/memories/` without EngramDB tooling, files like `019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8.md` are opaque. Adding an optional `title` field makes files self-describing when opened.

## Approach
Add an optional `title` field to the `Memory` struct. Agents can provide it explicitly, or if omitted, auto-generate one from the summary (simple truncation/slugification — no ML model needed since `summary` already exists and is ≤100 chars). The title appears as an H1 heading at the top of the markdown body, before `## Content`.

## Changes

### 1. `src/types/memory.rs` — Add `title` field to `Memory` and `MemoryUpdate`
- Add `pub title: Option<String>` to `Memory` (with `#[serde(skip_serializing_if = "Option::is_none")]`)
- Add `pub title: Option<String>` to `MemoryUpdate`
- Apply title in `MemoryUpdate::apply_to()`
- Initialize to `None` in `Memory::new()`

### 2. `src/storage/memory_file.rs` — Render title as H1 in markdown
- **`write_memory_file()`**: If `memory.title` is `Some`, write `# <title>\n\n` before `## Content`
- **`parse_memory_file()`**: After parsing frontmatter+body, look for a top-level `# ` heading before `## Content` and extract it as the title. This ensures round-trip fidelity.
- The title is stored in YAML frontmatter AND rendered as H1 — frontmatter is the source of truth, H1 is for human readability.

### 3. `src/ops/create.rs` — Accept title in `CreateParams`, auto-generate if absent
- Add `pub title: Option<String>` to `CreateParams`
- After building the `Memory`, if `title` is `None`, auto-generate from summary: take the summary as-is (it's already ≤100 chars and human-readable)
- Set `memory.title = params.title.or_else(|| Some(summary.clone()))`

### 4. `src/mcp/server.rs` — Add `title` to MCP `CreateInput` and `UpdateInput`
- Add `title: Option<String>` to `CreateInput` with schemars description
- Pass it through to `CreateParams`
- Add `title: Option<String>` to `UpdateInput`
- Pass it through to `MemoryUpdate`

### 5. `src/cli/commands/add.rs` — Add `--title` flag to CLI
- Add `pub title: Option<String>` to `AddParams`
- Pass through to `CreateParams` in `run_direct_mode`, `run_interactive_mode`, `run_editor_mode`
- Add `# Title:` line to editor template

### 6. CLI arg definition — Add `--title` clap arg
- Find where clap args are defined and add `--title` / `--T` option

### 7. Tests

#### Unit tests in `src/storage/memory_file.rs`:
- `test_write_with_title` — title renders as `# Title\n\n## Content\n\n...`
- `test_roundtrip_with_title` — write then parse preserves title
- `test_parse_without_title` — backward compat: files without title still parse (title = None from frontmatter)
- `test_write_without_title` — no H1 rendered when title is None

#### Unit tests in `src/types/memory.rs`:
- `test_memory_update_apply_title` — MemoryUpdate with title applies correctly

#### Unit tests in `src/ops/create.rs`:
- `test_create_memory_auto_generates_title` — when title is None, title is set from summary
- `test_create_memory_preserves_explicit_title` — when title is provided, it's kept as-is

#### Existing tests:
- Update `minimal_create_params()` helper to include `title: None`
- Update any test Memory struct constructions that need the new field
- Verify all existing tests still pass

## Non-goals
- No ML-based title generation (summary already serves as a concise description)
- No file renaming (files stay as `<uuid>.md` — title is metadata inside the file)
- No migration needed (existing files without `title` field will parse fine with `Option<String>` + serde defaults)
