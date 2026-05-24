#![no_main]

use libfuzzer_sys::fuzz_target;

// Physical-scope matching compiles caller-supplied glob patterns at runtime
// (`Glob::new`) and decays a score over the path. Arbitrary patterns/paths
// must never panic and the score must stay finite.
fuzz_target!(|input: (Vec<String>, String)| {
    let (patterns, path) = input;
    let _ = engramdb::scope::physical::matches(&patterns, &path);
    let score = engramdb::scope::physical::proximity(&patterns, &path, 1.0, 0.1);
    assert!(score.is_finite(), "physical proximity produced a non-finite score");
});
