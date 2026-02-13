# Feature 3: Mandatory Summary Validation

**Status: DONE**

## Goal
Make `summary` a required field on `CreateParams` instead of auto-generating low-quality summaries by truncating content.

## Changes

### `src/ops/create.rs`
- [x] Change `CreateParams.summary` from `Option<String>` to `String`
- [x] Add `validate_summary()` function: rejects empty, whitespace-only, and >100 char summaries
- [x] Call `validate_summary()` at start of `create_memory()`
- [x] Remove auto-generation block (lines 46-54)
- [x] Update `minimal_create_params()` in tests to provide a summary

### `src/ops/compress.rs`
- [x] Change `summary: Some(...)` to `summary: "...".to_string()` (both production and test code)

### `src/mcp/server.rs`
- [x] Change `CreateInput.summary` from `Option<String>` to `String`
- [x] Update schema description to "One-line summary, max 100 chars (required)"

### `src/cli/commands/add.rs`
- [x] Direct mode: error if `--summary` not provided with clear message
- [x] Interactive mode: changed `Some(summary)` to `summary` (already requires input)
- [x] Editor mode: changed `Some(parsed.summary)` to `parsed.summary` (already validates non-empty)

### `src/ops/mod.rs`
- [x] Export `validate_summary` from create module

### Tests added
- [x] `test_validate_summary_rejects_empty` — empty, whitespace-only, tab/newline
- [x] `test_validate_summary_rejects_too_long` — 101 chars
- [x] `test_validate_summary_accepts_valid` — short, exactly 100, single char
- [x] `test_create_memory_fails_with_empty_summary` — integration test
- [x] `test_create_memory_fails_with_too_long_summary` — integration test

## Verification
- `cargo fmt --all` — clean
- `cargo clippy --all-targets --all-features -- -D warnings` — zero warnings
- `cargo test` — 372 passed, 0 failed
