# Handoff: Depth-Decaying Physical Scope Scoring

## Problem

Root-scoped memories (physical scope `"/"`) always get a flat score of 0.4 regardless of how deep the queried file is. Combined with the `scope_multiplier = 0.5 + 0.5 * scope_score` transform and the 0.3 default threshold, this means **any root-scoped memory with criticality >= 0.5 will surface on every file read in the project**. This creates noise — e.g., a "use yarn not npm" preference appearing when editing a React component.

### Current behavior (from `src/scope/physical.rs:104-131`)

```rust
fn calculate_pattern_score(pattern: &str, current_path: &str) -> f64 {
    if pattern == "/" { return 0.4; }        // flat, no depth awareness
    if pattern == current_path { return 1.0; } // exact file
    // ... glob handling ...
    if is_same_directory(pattern, current_path) { return 0.85; }
    if is_parent_directory(pattern, current_path) { return 0.6; } // also flat
    0.0
}
```

The `parent_directory` case also has the same problem: `src/` scoped to `src/a/b/c/d.rs` scores 0.6 regardless of depth.

### Real-world example

Two memories scoped to `/` (project root):
- "Azure DevOps: use # for board items, ! for PRs" (criticality 0.9)
- "User prefers yarn" (criticality 0.8)

Reading `src/components/COMMON/shared/footer/index.jsx` (5 levels deep):
- Current: scope_score=0.4 → multiplier=0.70 → final scores 0.58 and 0.52 → both pass threshold 0.3
- These appear on **every single file read** regardless of relevance

## Solution: Depth-Based Exponential Decay

Replace flat scores for `/` and parent directory matches with `max(floor, base^depth)` where:
- `base` = 0.82 (configurable)
- `floor` = 0.3 (configurable, prevents scores from reaching zero)
- `depth` = number of directory levels between memory scope and queried file

### Target curve: `max(0.3, 0.82^depth)`

| Depth | Score | Example (memory scope → queried file) |
|-------|-------|---------------------------------------|
| 0 | 1.0 | exact file match (unchanged) |
| 1 | 0.82 | `footer/` → `footer/index.jsx` |
| 2 | 0.67 | `shared/` → `shared/footer/index.jsx` |
| 3 | 0.55 | `COMMON/` → `COMMON/shared/footer/index.jsx` |
| 4 | 0.45 | `components/` → `components/.../index.jsx` |
| 5 | 0.37 | `src/` → `src/components/.../index.jsx` |
| 6+ | 0.30 | `/` (root) → any deep file (floor) |

### Combined with threshold raise to 0.5

With depth-decay AND threshold=0.5, the root-scoped memories from the example:
- Azure DevOps (crit 0.9): `0.9 * 0.3_scope * 0.925_trust = 0.25` → **filtered out**
- Yarn (crit 0.8): `0.8 * 0.3_scope * 0.925_trust = 0.22` → **filtered out**

But a same-folder memory (depth 1, score 0.82):
- Footer decision (crit 0.8): `0.8 * 0.82_scope * 0.925_trust = 0.61` → **surfaces correctly**

## Implementation Plan

### Step 1: Add depth calculation utility to `src/scope/physical.rs`

Add a function to calculate directory depth between two paths:

```rust
/// Calculate the directory depth between a scope pattern and a file path.
/// Returns 0 for exact file match, 1 for same directory, etc.
/// For root "/", returns the full depth of the file path.
fn directory_depth(pattern: &str, current_path: &str) -> usize
```

Logic:
- For `"/"`: count the number of `/` separators in `current_path`
- For parent directories: count the `/` separators between the pattern path and the current file's directory
- For same directory: return 1
- For exact match: return 0

Test cases to add:
- `directory_depth("/", "src/a/b/c.rs")` → 3 (src, a, b)
- `directory_depth("src/", "src/a/b/c.rs")` → 2 (a, b)
- `directory_depth("src/a/", "src/a/b.rs")` → 1 (same dir of file within)
- `directory_depth("src/a/b.rs", "src/a/b.rs")` → 0 (exact)

### Step 2: Add config fields to `src/types/config.rs`

Add to `ScoringConfig`:

```rust
/// Base for exponential depth decay of physical scope scores (default 0.82).
/// Score = max(depth_decay_floor, depth_decay_base^depth)
#[serde(default = "ScoringConfig::default_depth_decay_base")]
pub depth_decay_base: f64,

/// Floor for depth decay — minimum scope score regardless of depth (default 0.3).
#[serde(default = "ScoringConfig::default_depth_decay_floor")]
pub depth_decay_floor: f64,
```

Add default functions:
```rust
fn default_depth_decay_base() -> f64 { 0.82 }
fn default_depth_decay_floor() -> f64 { 0.3 }
```

Add validation in `EngramConfig::validate()`:
```rust
if !(0.0..=1.0).contains(&self.retrieval.scoring.depth_decay_base) {
    anyhow::bail!("scoring.depth_decay_base must be in [0.0, 1.0]");
}
if !(0.0..=1.0).contains(&self.retrieval.scoring.depth_decay_floor) {
    anyhow::bail!("scoring.depth_decay_floor must be in [0.0, 1.0]");
}
```

Also raise default `relevance_threshold` from 0.3 to 0.5 in `RetrievalConfig::default()`.

### Step 3: Update `src/scope/physical.rs` scoring functions

Change `calculate_pattern_score` to accept config and use depth decay:

```rust
fn calculate_pattern_score(pattern: &str, current_path: &str, base: f64, floor: f64) -> f64 {
    if pattern == current_path { return 1.0; } // exact match, unchanged

    let depth = directory_depth(pattern, current_path);
    if depth == 0 { return 0.0; } // no match

    // Exponential decay: max(floor, base^depth)
    floor.max(base.powi(depth as i32))
}
```

This replaces the fixed tiers (1.0, 0.85, 0.6, 0.4) with a continuous curve. The curve naturally produces:
- depth 1: 0.82 (was 0.85 for same-dir — close enough)
- depth 2: 0.67 (was 0.6 for parent — slightly higher, reasonable)
- depth 5+: 0.30 (was 0.4 for root — now properly penalized)

Update the `proximity` function signature to accept config values:

```rust
pub fn proximity(memory_scopes: &[String], current_path: &str, base: f64, floor: f64) -> f64
```

### Step 4: Thread config through the call chain

The config values need to flow from `EngramConfig` → `composite_score()` → `scope_proximity()` → `physical::proximity()`.

In `src/scope/mod.rs`, update `scope_proximity` to accept `base` and `floor` parameters:

```rust
pub fn scope_proximity(
    memory_physical: &[String],
    memory_logical: &[String],
    current_path: Option<&str>,
    current_logical: &[String],
    depth_decay_base: f64,
    depth_decay_floor: f64,
) -> f64
```

In `src/scoring/composite.rs` at line 223, update the call:

```rust
let scope_score = crate::scope::scope_proximity(
    &memory.physical,
    &memory.logical,
    context.path,
    context.logical,
    config.retrieval.scoring.depth_decay_base,
    config.retrieval.scoring.depth_decay_floor,
);
```

### Step 5: Remove the `scope_multiplier_floor` transform

**Important**: With depth decay, the `scope_multiplier = floor + (1 - floor) * scope_score` transform in `composite.rs:287-292` is no longer needed. The depth decay floor already prevents scores from going to zero. The multiplier should now just be the scope score directly:

```rust
let scope_multiplier = if has_scope_context {
    scope_score  // depth decay already handles the floor
} else {
    1.0
};
```

Remove `scope_multiplier_floor` from `ScoringConfig` (mark deprecated or remove entirely). This simplifies the mental model: scope_score IS the multiplier.

If keeping backward compatibility is preferred, you can keep the field but default it to 0.0 so the transform becomes a no-op: `0.0 + 1.0 * scope_score = scope_score`.

### Step 6: Update glob scoring

`calculate_glob_score` in `physical.rs:133-175` also returns fixed tiers. Update it to use the same depth-based approach:

```rust
fn calculate_glob_score(pattern: &str, current_path: &str, base: f64, floor: f64) -> f64 {
    // ... existing glob matching logic ...
    // After confirming the glob matches:
    let depth = directory_depth(pattern_dir, current_path);
    floor.max(base.powi(depth as i32))
}
```

### Step 7: Update all tests

Files with tests to update:
- `src/scope/physical.rs` — all `test_proximity_*` tests need new scores matching the curve
- `src/scope/mod.rs` — `test_scope_proximity_combined` needs updated expected values and new function signature
- `src/scoring/composite.rs` — any tests that assert specific final scores
- `src/types/config.rs` — add tests for new config fields, validation, and serialization roundtrip

New tests to add:
- `test_depth_decay_curve` — verify `max(0.3, 0.82^n)` for depths 0-10
- `test_depth_root_deep_file` — root scope on deeply nested file gets floor score
- `test_depth_parent_one_level` — parent dir one level up
- `test_depth_configurable` — different base/floor values produce expected curves
- `test_backward_compat_default_threshold` — ensure default config change doesn't break existing behavior for close-scoped memories

### Step 8: Update config documentation

Update the config.toml example/documentation (if any) to include:

```toml
[retrieval]
relevance_threshold = 0.5  # raised from 0.3

[retrieval.scoring]
depth_decay_base = 0.82    # exponential base for scope depth decay
depth_decay_floor = 0.3    # minimum scope score at any depth
```

## Files to modify (summary)

| File | Changes |
|------|---------|
| `src/scope/physical.rs` | Add `directory_depth()`, update `calculate_pattern_score()` and `calculate_glob_score()` to use depth decay, update `proximity()` signature, update all tests |
| `src/scope/mod.rs` | Update `scope_proximity()` signature to pass through base/floor, update tests |
| `src/scoring/composite.rs` | Pass config values to `scope_proximity()`, simplify scope_multiplier (remove floor transform), update tests |
| `src/types/config.rs` | Add `depth_decay_base`/`depth_decay_floor` fields, raise default `relevance_threshold` to 0.5, add validation, update tests |
| `src/cli/commands/hook.rs` | No changes needed (uses config.retrieval.relevance_threshold which auto-updates) |

## Verification

After implementation, run the existing test suite:
```bash
cargo test
```

Then manually verify with a real project:
```bash
cd /Users/egeapak/Projects/ceiba/eclinics-frontend
engramdb retrieve --path src/components/COMMON/shared/footer/index.jsx --verbose
```

The root-scoped memories should now score ~0.25 and be filtered by the 0.5 threshold. Same-folder memories should still surface.

## Notes

- The `scope_multiplier_floor` config field becomes redundant with depth decay. Decide whether to deprecate (breaking change) or keep as a no-op (default to 0.0). Deprecation is cleaner.
- The threshold change from 0.3→0.5 is the most impactful part. Even without depth decay, just raising the threshold would filter out most noise. But depth decay makes the scores meaningful and predictable.
- Consider adding a `--verbose` or `--debug` flag to hook output so users can see why memories were filtered, to aid tuning.
