#![no_main]

use engramdb::ops::harvest::{
    defang_metadata, defang_prose, escape_harness_tags, MAX_META_CHARS, MAX_SUMMARY_CHARS,
};
use engramdb::storage::transcripts::{
    is_invisible_format, sanitize_for_terminal, sanitize_one_line,
};
use libfuzzer_sys::fuzz_target;

// Every recorded string that reaches a terminal or a model goes through
// these. The invariants are the ones their docs promise; the tag check uses
// the defang implementation itself (`escape_harness_tags` is idempotent, so
// an output it would still change carries an unescaped tag), not a copy of
// its list.

/// The characters `cap` appends to a value it cut: `"… [truncated]"`
/// (`src/ops/harvest.rs`).
const TRUNCATION_MARKER_CHARS: usize = 13;

fn assert_terminal_safe(what: &str, out: &str) {
    for c in out.chars() {
        assert!(c != '\u{1b}', "{what}: ESC survived in {out:?}");
        assert!(
            !('\u{80}'..='\u{9f}').contains(&c),
            "{what}: C1 control {c:?} survived in {out:?}"
        );
        assert!(
            !is_invisible_format(c),
            "{what}: invisible {c:?} survived in {out:?}"
        );
        assert!(
            c == '\n' || c == '\t' || ((c as u32) >= 0x20 && c != '\u{7f}'),
            "{what}: C0 control {c:?} survived in {out:?}"
        );
    }
}

fn assert_defanged(what: &str, out: &str, max: usize) {
    assert_terminal_safe(what, out);
    assert!(!out.contains(['\n', '\r']), "{what}: not one line: {out:?}");
    // Each escaped tag adds one backslash after the cap, so the backslashes
    // in the output bound what the escape added.
    let added = out.matches('\\').count();
    assert!(
        out.chars().count() <= max + TRUNCATION_MARKER_CHARS + added,
        "{what}: {} chars over a {max} cap",
        out.chars().count()
    );
    assert_eq!(
        escape_harness_tags(out),
        out,
        "{what}: a harness tag survived"
    );
}

fuzz_target!(|text: &str| {
    let term = sanitize_for_terminal(text);
    assert_terminal_safe("sanitize_for_terminal", &term);
    assert!(!term.contains('\r'), "a CR survived: {term:?}");
    assert_eq!(
        sanitize_for_terminal(&term),
        term,
        "sanitize_for_terminal is not idempotent"
    );

    let line = sanitize_one_line(text);
    assert_terminal_safe("sanitize_one_line", &line);
    assert!(!line.contains(['\n', '\r']), "not one line: {line:?}");
    assert_eq!(
        sanitize_one_line(&line),
        line,
        "sanitize_one_line is not idempotent"
    );

    assert_defanged("defang_metadata", &defang_metadata(text), MAX_META_CHARS);
    assert_defanged("defang_prose", &defang_prose(text), MAX_SUMMARY_CHARS);
});
