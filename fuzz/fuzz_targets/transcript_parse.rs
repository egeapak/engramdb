#![no_main]

use engramdb::storage::transcripts::{
    last_assistant_text, parse_session, ParseOptions, ToolDetail,
};
use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use std::sync::OnceLock;

// A Claude Code session log is another program's file, fed by whatever the
// session read (web pages, PR text, dependency source). Both readers take it
// from disk: `parse_session` walks every record, and `last_assistant_text`
// (UserPromptSubmit, on every prompt) reads a tail that can start inside a
// record or inside a multi-byte character.
//
// The input is written to one file per process; the first byte picks the
// parse options and the next two the tail size, so cuts land anywhere.
fn transcript_path() -> &'static PathBuf {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = DIR.get_or_init(|| tempfile::TempDir::new().expect("temp dir"));
        dir.path()
            .join("00000000-0000-0000-0000-000000000000.jsonl")
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let (tail, body) = match rest {
        [a, b, body @ ..] => (u64::from(u16::from_le_bytes([*a, *b])), body),
        _ => (64, rest),
    };
    let path = transcript_path();
    if std::fs::write(path, body).is_err() {
        return;
    }

    let opts = ParseOptions {
        include_thinking: pick & 1 != 0,
        include_sidechains: pick & 2 != 0,
        tools: match (pick >> 2) % 3 {
            0 => ToolDetail::All,
            1 => ToolDetail::FailuresOnly,
            _ => ToolDetail::None,
        },
    };
    // Malformed records are skipped or reported; only a panic is a bug.
    let _ = parse_session(path, opts);

    if let Some(text) = last_assistant_text(path, tail) {
        // Every character of the answer came from inside the tail: JSON
        // unescaping only shrinks text, and a lossy-decoded byte becomes
        // one U+FFFD.
        assert!(
            text.chars().count() as u64 <= tail,
            "last_assistant_text returned {} chars from a {tail}-byte tail",
            text.chars().count()
        );
    }
});
