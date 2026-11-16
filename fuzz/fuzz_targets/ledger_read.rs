#![no_main]

use engramdb::storage::harvest_state::{compaction_pressure, read_harvested};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

// The harvest ledger (`.engramdb/state/harvest_ledger.jsonl`) is an
// append-only log that sits in a repository checkout: a clone, a merge or a
// hand edit decides its bytes. Every harvest command folds it. Malformed
// lines must be skipped, never panic, and reading must be repeatable.

fn project_dir() -> &'static PathBuf {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = DIR.get_or_init(|| tempfile::TempDir::new().expect("temp dir"));
        std::fs::create_dir_all(dir.path().join(".engramdb").join("state")).expect("state dir");
        dir.path().to_path_buf()
    })
}

fn snapshot(dir: &std::path::Path) -> BTreeMap<String, String> {
    read_harvested(dir)
        .into_iter()
        .map(|(id, entry)| (id, format!("{entry:?}")))
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let dir = project_dir();
    let ledger = dir
        .join(".engramdb")
        .join("state")
        .join("harvest_ledger.jsonl");
    if std::fs::write(&ledger, data).is_err() {
        return;
    }
    let first = snapshot(dir);
    let _ = compaction_pressure(dir);
    let second = snapshot(dir);
    assert_eq!(
        first, second,
        "reading the ledger twice gave different maps"
    );
});
