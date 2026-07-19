#![no_main]

use libfuzzer_sys::fuzz_target;

use engramdb::storage::memory_file::{parse_memory_file, write_memory_file};

// For any input that parses to a `Memory`, serializing and re-parsing must
// succeed, and re-serializing must be a fixed point. This catches data loss
// and write/parse asymmetry, not just panics.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(memory) = parse_memory_file(s) else {
        return;
    };

    let written = write_memory_file(&memory).expect("writing a parsed memory must succeed");
    let reparsed =
        parse_memory_file(&written).expect("re-parsing a written memory must succeed");
    let rewritten =
        write_memory_file(&reparsed).expect("writing a re-parsed memory must succeed");

    assert_eq!(written, rewritten, "memory file write is not idempotent");

    // Epistemic emission is off-diagonal-only: a memory whose class equals its
    // type-derived default must write NO `epistemic` key in the frontmatter
    // (the region between the first two `---` fences — body text is free to
    // contain the string). This is the no-rewrite-churn invariant for every
    // pre-epistemic file.
    if reparsed.epistemic == reparsed.type_.default_epistemic() {
        let frontmatter = written.split("---").nth(1).unwrap_or("");
        assert!(
            !frontmatter.contains("epistemic:"),
            "diagonal memory must not write an epistemic key"
        );
    }
});
