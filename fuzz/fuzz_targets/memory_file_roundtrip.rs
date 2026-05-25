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
});
