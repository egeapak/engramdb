#![no_main]

use libfuzzer_sys::fuzz_target;

// Dot-notation logical-scope proximity does unbounded `split('.')` and
// lowest-common-ancestor math over caller-supplied strings. Exercise it with
// arbitrary scope vectors to ensure it neither panics nor returns NaN/inf.
fuzz_target!(|input: (Vec<String>, Vec<String>)| {
    let (memory_scopes, current_scopes) = input;
    let score = engramdb::scope::logical::proximity(&memory_scopes, &current_scopes);
    assert!(score.is_finite(), "logical proximity produced a non-finite score");
});
