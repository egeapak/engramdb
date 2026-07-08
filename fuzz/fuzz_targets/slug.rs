#![no_main]

use libfuzzer_sys::fuzz_target;

use engramdb::storage::memory_file::{extract_id_from_stem, slugify, stem_matches_id_prefix};

// The filename helpers do byte slicing over strings that originate from
// untrusted places: titles come from users/agents (and from repo-shipped
// memory frontmatter), stems from arbitrary on-disk filenames. A previous
// crash here was a non-char-boundary slice in slugify's 50-byte truncation
// (long CJK/accented titles). Assert the weakest invariants: no panic, and
// the slug stays within its documented bounds.
fuzz_target!(|input: (String, String, String)| {
    let (title, stem, prefix) = input;

    let slug = slugify(&title);
    assert!(slug.len() <= 50, "slug exceeds its documented max length");
    assert!(!slug.starts_with('-') && !slug.ends_with('-'));

    let id = extract_id_from_stem(&stem);
    assert!(stem.contains(id), "extracted id must be a substring");

    let _ = stem_matches_id_prefix(&stem, &prefix);
});
