#![no_main]

use libfuzzer_sys::fuzz_target;

// Parsing an on-disk memory file is the largest hand-written parser surface
// (TOML/YAML frontmatter + bespoke V2 markdown sections). It must never panic
// on arbitrary bytes — malformed input should always surface as an `Err`.
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = engramdb::storage::memory_file::parse_memory_file(s);
    }
});
