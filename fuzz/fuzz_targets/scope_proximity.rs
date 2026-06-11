#![no_main]

use libfuzzer_sys::fuzz_target;

// `scope_proximity` is the top-level combiner that folds physical depth-decay
// and logical LCA bonus into a single multiplier used by the scorer. Drive it
// with arbitrary scopes, paths and decay constants to ensure the combined
// score is always finite — a non-finite multiplier would corrupt (and with
// NaN, unorder) every downstream composite score. The documented [0, 1] bound
// only holds for config-validated decay constants, so we assert finiteness
// only, matching the scope_physical / scope_logical targets.
fuzz_target!(|input: (
    Vec<String>,
    Vec<String>,
    Option<String>,
    Vec<String>,
    f64,
    f64,
    f64
)| {
    let (memory_physical, memory_logical, current_path, current_logical, base, floor, logical_floor) =
        input;

    // A NaN/inf logical-only floor is returned verbatim for unscoped memories;
    // that's an input-validation concern (config validates it into [0, 1]),
    // not the combiner arithmetic this target probes. Skip it so the
    // assertion isolates the path/LCA math.
    if !logical_floor.is_finite() {
        return;
    }

    let score = engramdb::scope::scope_proximity(
        &memory_physical,
        &memory_logical,
        current_path.as_deref(),
        &current_logical,
        base,
        floor,
        logical_floor,
    );
    assert!(score.is_finite(), "scope_proximity produced a non-finite score");
});
