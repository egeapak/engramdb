use super::*;
use crate::registry::InMemoryRegistry;
use engram_types::{Memory, MemoryType, Provenance, Visibility};
use tempfile::TempDir;

/// `atomic_write` must replace an existing file in place. On Unix this is
/// `rename(2)`; on Windows `tempfile::persist` uses `MoveFileEx`-with-
/// replace. Guard that overwriting works on every platform (and leaves no
/// stray temp file behind).
#[tokio::test]
async fn atomic_write_overwrites_existing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("data.md");

    atomic_write(&path, "first").await.unwrap();
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "first");

    atomic_write(&path, "second").await.unwrap();
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "second");

    // Only the target file remains — the temp file was consumed by persist.
    let entries: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("data.md")]);
}

#[tokio::test]
async fn list_watched_returns_only_rows_with_watch_paths() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut watching = create_test_memory("watch-1", Visibility::Shared);
    watching.valid_while = Some(engram_types::Validity {
        invalidated_by: vec!["src/lib.rs".to_string()],
        ..Default::default()
    });
    let mut closed = create_test_memory("watch-2", Visibility::Shared);
    closed.valid_while = Some(engram_types::Validity {
        invalidated_by: vec!["src/**".to_string()],
        ..Default::default()
    });
    let at = Utc::now() - chrono::Duration::days(1);
    closed.invalidated_at = Some(at);
    let plain = create_test_memory("plain-1", Visibility::Shared);
    for m in [&watching, &closed, &plain] {
        store.create(m).await.unwrap();
    }
    let mut rows = store.list_watched().await.unwrap();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["watch-1", "watch-2"],
        "a row watching nothing is left out"
    );
    assert_eq!(rows[0].watch_paths, vec!["src/lib.rs".to_string()]);
    assert_eq!(rows[0].invalidated_at, None);
    // Carried, not filtered: the caller decides what a closed window means.
    assert_eq!(
        rows[1].invalidated_at.map(|t| t.timestamp()),
        Some(at.timestamp())
    );
}

fn create_test_memory(id: &str, visibility: Visibility) -> Memory {
    let mut memory = Memory::new(
        MemoryType::Decision,
        "Test summary",
        "Test content",
        Provenance::human(),
    );
    memory.id = id.to_string();
    memory.visibility = visibility;
    memory
}

fn index_reads() -> (usize, usize, usize) {
    use crate::lance_index::test_counters::{CHUNK_PROBES, ID_PROBES, STATS_SCANS};
    use std::sync::atomic::Ordering::Relaxed;
    (
        ID_PROBES.load(Relaxed),
        CHUNK_PROBES.load(Relaxed),
        STATS_SCANS.load(Relaxed),
    )
}

async fn manifest_stats(dir: &Path) -> (usize, Vec<String>) {
    let m = manifest::load_manifest(&paths::project_dir(dir).join("manifest.toml"))
        .await
        .unwrap();
    (m.stats.memory_count, m.stats.logical_scopes)
}

/// A single create used to cost a prefix `LIKE` probe, a chunk probe and
/// a full `logical` column scan for the manifest: three O(N) reads for an
/// id that cannot have a row, chunks or anything else yet.
#[tokio::test]
async fn create_of_a_fresh_id_makes_one_probe() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let mut first = create_test_memory("probe-1", Visibility::Shared);
    first.logical = vec!["api.auth".into()];
    store.create(&first).await.unwrap();

    let before = index_reads();
    let mut second = create_test_memory("probe-2", Visibility::Shared);
    second.logical = vec!["api.auth".into(), "db.schema".into()];
    store.create(&second).await.unwrap();
    let after = index_reads();

    assert_eq!(after.0 - before.0, 1, "one id probe");
    assert_eq!(after.1 - before.1, 0, "no chunk probe for a fresh id");
    assert_eq!(
        after.2 - before.2,
        0,
        "no stats scan when the manifest was current"
    );
    assert_eq!(
        manifest_stats(tmp.path()).await,
        (2, vec!["api.auth".to_string(), "db.schema".to_string()])
    );
}

/// The manifest is a committed file: a branch switch can change it under
/// the index. The shortcut must notice and fall back to the scan.
#[tokio::test]
async fn manifest_stats_fall_back_to_a_scan_when_the_count_disagrees() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let mut first = create_test_memory("drift-1", Visibility::Shared);
    first.logical = vec!["real.scope".into()];
    store.create(&first).await.unwrap();

    // As after checking out a branch whose manifest says something else.
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    manifest::update_stats(&mut m, 7, vec!["stale.scope".into()]);
    manifest::save_manifest(&manifest_path, &m).await.unwrap();

    let before = index_reads();
    let mut second = create_test_memory("drift-2", Visibility::Shared);
    second.logical = vec!["other.scope".into()];
    store.create(&second).await.unwrap();
    assert_eq!(index_reads().2 - before.2, 1, "the scan ran");
    assert_eq!(
        manifest_stats(tmp.path()).await,
        (2, vec!["other.scope".to_string(), "real.scope".to_string()])
    );
}

/// `create` then a chunk upsert commits the memories table twice (the
/// row, then its `has_embedding` flag).
#[tokio::test]
async fn create_with_chunks_commits_the_row_once() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    store
        .create(&create_test_memory("cwc-0", Visibility::Shared))
        .await
        .unwrap();

    let v0 = store.lance_index.memories_version().await.unwrap();
    let m = create_test_memory("cwc-1", Visibility::Shared);
    store
        .create_with_chunks(
            &m,
            vec![vec![0.5; 384], vec![0.25; 384]],
            Some("digest-1".into()),
        )
        .await
        .unwrap();
    let v1 = store.lance_index.memories_version().await.unwrap();
    assert_eq!(v1 - v0, 1, "one memories-table commit");

    assert!(has_embedding_flag(&store, "cwc-1").await);
    let chunks = store.export_chunks("cwc-1").await.unwrap();
    assert_eq!(chunks, vec![vec![0.5; 384], vec![0.25; 384]]);
    assert_eq!(store.get("cwc-1").await.unwrap().id, "cwc-1");
    assert_eq!(manifest_stats(tmp.path()).await.0, 2);

    // The two-step path it replaces, for the comparison this test is about.
    let m2 = create_test_memory("cwc-2", Visibility::Shared);
    store.create(&m2).await.unwrap();
    store
        .upsert_chunks("cwc-2", vec![vec![0.5; 384]])
        .await
        .unwrap();
    let v2 = store.lance_index.memories_version().await.unwrap();
    assert_eq!(v2 - v1, 2);

    // No chunks: plain create.
    let m3 = create_test_memory("cwc-3", Visibility::Shared);
    store.create_with_chunks(&m3, vec![], None).await.unwrap();
    assert!(!has_embedding_flag(&store, "cwc-3").await);
}

/// Existing behavior the probe change must keep: re-creating an id under
/// the other visibility removes the old file and keeps its vectors' flag.
#[tokio::test]
async fn a_recreate_of_an_existing_id_still_sweeps_both_dirs() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let shared = create_test_memory("sweep-1", Visibility::Shared);
    store.create(&shared).await.unwrap();
    store
        .upsert_chunks("sweep-1", vec![vec![0.5; 384]])
        .await
        .unwrap();
    let shared_dir = store.get_memories_dir(&Visibility::Shared).unwrap();
    let personal_dir = store.get_memories_dir(&Visibility::Personal).unwrap();
    assert_eq!(
        find_memory_files(&shared_dir, "sweep-1")
            .await
            .unwrap()
            .len(),
        1
    );

    let before = index_reads();
    let personal = create_test_memory("sweep-1", Visibility::Personal);
    store.create(&personal).await.unwrap();
    let after = index_reads();
    assert_eq!(
        after.1 - before.1,
        1,
        "an existing id is still chunk-probed"
    );

    assert!(find_memory_files(&shared_dir, "sweep-1")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        find_memory_files(&personal_dir, "sweep-1")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        has_embedding_flag(&store, "sweep-1").await,
        "the re-create reset has_embedding"
    );
    assert_eq!(manifest_stats(tmp.path()).await.0, 1);
}

/// The batched chunk write must be observably identical to N single
/// writes — same vectors readable back, same `has_embedding` flag.
#[tokio::test]
async fn upsert_chunks_batch_matches_per_memory_writes() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

    let mut mems = Vec::new();
    for i in 0..5 {
        let m = create_test_memory(&format!("batch-{i}"), Visibility::Shared);
        store.create(&m).await.unwrap();
        mems.push(m);
    }

    let entries: Vec<_> = mems
        .iter()
        .enumerate()
        .map(|(i, m)| {
            (
                m.id.clone(),
                m.updated_at,
                vec![vec![i as f32; 384], vec![(i + 100) as f32; 384]],
                None,
            )
        })
        .collect();
    let written = store.upsert_chunks_batch(entries).await.unwrap();
    assert_eq!(written.len(), 5);

    for (i, m) in mems.iter().enumerate() {
        let chunks = store.export_chunks(&m.id).await.unwrap();
        assert_eq!(chunks.len(), 2, "memory {i} should have both chunks");
        assert_eq!(chunks[0], vec![i as f32; 384], "chunk order must be stable");
        assert_eq!(chunks[1], vec![(i + 100) as f32; 384]);
        assert!(
            has_embedding_flag(&store, &m.id).await,
            "has_embedding must be set by the batch"
        );
    }
}

/// The freshness guard is per-entry: a memory modified since its snapshot
/// is skipped while its neighbours in the same batch are still written.
#[tokio::test]
async fn upsert_chunks_batch_skips_only_the_stale_entry() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

    let fresh = create_test_memory("batch-fresh", Visibility::Shared);
    let stale = create_test_memory("batch-stale", Visibility::Shared);
    store.create(&fresh).await.unwrap();
    store.create(&stale).await.unwrap();

    let entries = vec![
        (
            fresh.id.clone(),
            fresh.updated_at,
            vec![vec![1.0f32; 384]],
            None,
        ),
        // A snapshot timestamp that no longer matches the stored memory.
        (
            stale.id.clone(),
            stale.updated_at - chrono::Duration::seconds(60),
            vec![vec![2.0f32; 384]],
            None,
        ),
    ];
    let written = store.upsert_chunks_batch(entries).await.unwrap();

    assert_eq!(written, vec![fresh.id.clone()]);
    assert_eq!(store.export_chunks(&fresh.id).await.unwrap().len(), 1);
    assert!(
        store.export_chunks(&stale.id).await.unwrap().is_empty(),
        "the stale entry must not have been written"
    );
}

/// A batch write must not touch chunks belonging to memories outside it —
/// the `when_not_matched_by_source_delete_expr` has to be scoped.
#[tokio::test]
async fn upsert_chunks_batch_leaves_other_memories_alone() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

    let kept = create_test_memory("batch-kept", Visibility::Shared);
    let rewritten = create_test_memory("batch-rewritten", Visibility::Shared);
    store.create(&kept).await.unwrap();
    store.create(&rewritten).await.unwrap();
    store
        .upsert_chunks(&kept.id, vec![vec![9.0f32; 384]])
        .await
        .unwrap();

    store
        .upsert_chunks_batch(vec![(
            rewritten.id.clone(),
            rewritten.updated_at,
            vec![vec![3.0f32; 384]],
            None,
        )])
        .await
        .unwrap();

    assert_eq!(
        store.export_chunks(&kept.id).await.unwrap(),
        vec![vec![9.0f32; 384]],
        "a memory outside the batch must keep its chunks"
    );
}

/// Shrinking a memory's chunk count must delete the surplus rows, matching
/// `upsert_chunks`'s stale-row cleanup.
#[tokio::test]
async fn upsert_chunks_batch_drops_surplus_chunks() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

    let m = create_test_memory("batch-shrink", Visibility::Shared);
    store.create(&m).await.unwrap();
    store
        .upsert_chunks_batch(vec![(
            m.id.clone(),
            m.updated_at,
            vec![vec![1.0f32; 384], vec![2.0f32; 384], vec![3.0f32; 384]],
            None,
        )])
        .await
        .unwrap();
    assert_eq!(store.export_chunks(&m.id).await.unwrap().len(), 3);

    store
        .upsert_chunks_batch(vec![(
            m.id.clone(),
            m.updated_at,
            vec![vec![4.0f32; 384]],
            None,
        )])
        .await
        .unwrap();
    assert_eq!(
        store.export_chunks(&m.id).await.unwrap(),
        vec![vec![4.0f32; 384]],
        "surplus chunks from the previous write must be gone"
    );
}

/// `export_chunks_batch` must agree with N `export_chunks` calls, including
/// chunk order and absent ids.
#[tokio::test]
async fn export_chunks_batch_matches_per_memory_reads() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

    let mut ids = Vec::new();
    for i in 0..6 {
        let m = create_test_memory(&format!("read-{i}"), Visibility::Shared);
        store.create(&m).await.unwrap();
        // Leave memory 4 without vectors.
        if i != 4 {
            store
                .upsert_chunks(&m.id, vec![vec![i as f32; 384], vec![(i + 50) as f32; 384]])
                .await
                .unwrap();
        }
        ids.push(m.id.clone());
    }

    let batched = store.export_chunks_batch(&ids).await.unwrap();
    for id in &ids {
        let single = store.export_chunks(id).await.unwrap();
        if single.is_empty() {
            assert!(
                !batched.contains_key(id),
                "a memory with no chunks must be absent from the map, not empty"
            );
        } else {
            assert_eq!(batched.get(id), Some(&single), "batched read must match");
        }
    }
    assert_eq!(batched.len(), 5);
}

async fn has_embedding_flag(store: &MemoryStore, id: &str) -> bool {
    store
        .lance_index
        .list_for_filtering()
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id == id)
        .unwrap()
        .has_embedding
}

/// A non-finite `decay.floor` (hand-edited memory file; serde_json writes
/// it as `null` without error) must not brick retrieval: the poisoned row
/// degrades to undecayed with a warning instead of failing the whole
/// `list_for_filtering` batch — the entry point of every query.
#[tokio::test]
async fn nonfinite_decay_floor_degrades_instead_of_failing_the_batch() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut poisoned = create_test_memory("decay-nan", Visibility::Shared);
    poisoned.decay = Some(engram_types::Decay {
        floor: f64::NAN,
        ..engram_types::Decay::linear(chrono::Duration::days(30))
    });
    store.create(&poisoned).await.unwrap();
    let mut healthy = create_test_memory("decay-ok", Visibility::Shared);
    healthy.decay = Some(engram_types::Decay::linear(chrono::Duration::days(30)));
    store.create(&healthy).await.unwrap();

    let entries = store.lance_index.list_for_filtering().await.unwrap();
    assert_eq!(entries.len(), 2, "both rows must survive");
    let get = |id: &str| entries.iter().find(|e| e.id == id).unwrap();
    assert!(
        get("decay-nan").decay.is_none(),
        "poisoned decay degrades to undecayed"
    );
    assert!(get("decay-ok").decay.is_some(), "healthy decay intact");
}

/// `clear_chunks` drops every vector, so it must also reset the
/// memories-table `has_embedding` mirror: a memory whose re-embed then
/// fails must score as "no evidence" (sem = None), not "checked, found
/// nothing" (sem = 0.0).
#[tokio::test]
async fn clear_chunks_resets_has_embedding_flags() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let memory = create_test_memory("clear-flag", Visibility::Shared);
    store.create(&memory).await.unwrap();
    store
        .upsert_chunks("clear-flag", vec![vec![0.5f32; 384]])
        .await
        .unwrap();
    assert!(has_embedding_flag(&store, "clear-flag").await);

    store.clear_chunks().await.unwrap();
    assert!(
        !has_embedding_flag(&store, "clear-flag").await,
        "flag must not claim embeddings that were just dropped"
    );
}

/// The R2/R3 schema migration: an older-version store is transparently
/// re-indexed on open, backfilling `decay` + `has_embedding` from the
/// authoritative `.md` files and chunk table, and stamping the current
/// version. A second open is a no-op.
#[tokio::test]
async fn schema_migration_on_open_backfills_decay_and_has_embedding() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

    // One decayed + embedded memory, one with decay explicitly disabled and
    // no chunks.
    let mut decayed = create_test_memory("mig-decayed", Visibility::Shared);
    decayed.decay = Some(engram_types::Decay::linear(chrono::Duration::days(30)));
    store.create(&decayed).await.unwrap();
    store
        .upsert_chunks("mig-decayed", vec![vec![0.1f32; 384]])
        .await
        .unwrap();
    let mut plain = create_test_memory("mig-plain", Visibility::Shared);
    plain.decay = None;
    store.create(&plain).await.unwrap();

    // Simulate a pre-migration store by downgrading the recorded version.
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.1.0".to_string();
    manifest::save_manifest(&manifest_path, &m).await.unwrap();

    // Re-open → migration runs (reindex from files, vectors preserved).
    let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    assert_eq!(
        manifest::load_manifest(&manifest_path)
            .await
            .unwrap()
            .schema_version,
        manifest::CURRENT_SCHEMA_VERSION,
        "migration must stamp the current schema version"
    );

    let entries = store2.lance_index.list_for_filtering().await.unwrap();
    let decayed = entries.iter().find(|e| e.id == "mig-decayed").unwrap();
    assert!(
        matches!(
            decayed.decay.as_ref().map(|d| &d.strategy),
            Some(engram_types::DecayStrategy::Linear)
        ),
        "decay strategy backfilled from the .md file"
    );
    assert!(
        decayed.has_embedding,
        "embedded memory → has_embedding=true"
    );
    let plain = entries.iter().find(|e| e.id == "mig-plain").unwrap();
    assert!(plain.decay.is_none(), "decay=None round-trips as null");
    assert!(
        !plain.has_embedding,
        "unembedded memory → has_embedding=false"
    );

    // Idempotent: opening again does not re-migrate.
    let _store3 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    assert_eq!(
        manifest::load_manifest(&manifest_path)
            .await
            .unwrap()
            .schema_version,
        manifest::CURRENT_SCHEMA_VERSION
    );
}

/// The 0.3.0 schema migration (epistemic columns): a store stamped 0.2.0
/// whose `.md` files predate the epistemic fields is transparently
/// re-indexed on open. The seven new columns materialize the type-derived
/// defaults (§2.5) — NOT the serde defaults — and vectors are preserved.
#[tokio::test]
async fn schema_migration_to_0_3_0_backfills_epistemic_columns() {
    use engram_types::{Epistemic, Generality};

    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

    // Diagonal memories write files byte-identical to the pre-epistemic
    // writer, so creating them and downgrading the stamp reproduces a
    // genuine pre-epistemic store. Debug's type default (Observation)
    // differs from the serde default (Fact), which is what makes the
    // materialization assertion meaningful.
    let mut debug_mem = create_test_memory("mig3-debug", Visibility::Shared);
    debug_mem.type_ = MemoryType::Debug;
    debug_mem.epistemic = MemoryType::Debug.default_epistemic();
    store.create(&debug_mem).await.unwrap();
    store
        .upsert_chunks("mig3-debug", vec![vec![0.1f32; 384]])
        .await
        .unwrap();
    let mut hazard_mem = create_test_memory("mig3-hazard", Visibility::Shared);
    hazard_mem.type_ = MemoryType::Hazard;
    hazard_mem.epistemic = MemoryType::Hazard.default_epistemic();
    store.create(&hazard_mem).await.unwrap();

    // Downgrade the recorded version to the pre-epistemic schema.
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.2.0".to_string();
    manifest::save_manifest(&manifest_path, &m).await.unwrap();

    // Re-open → migration reindexes and stamps 0.3.0.
    let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    assert_eq!(
        manifest::load_manifest(&manifest_path)
            .await
            .unwrap()
            .schema_version,
        manifest::CURRENT_SCHEMA_VERSION,
    );

    let entries = store2.lance_index.list_for_filtering().await.unwrap();
    let debug_row = entries.iter().find(|e| e.id == "mig3-debug").unwrap();
    assert_eq!(
        debug_row.epistemic,
        Epistemic::Observation,
        "pre-epistemic Debug memory must materialize its TYPE default"
    );
    assert_eq!(debug_row.generality, Generality::Project);
    assert_eq!(debug_row.origin_task, None);
    assert_eq!(debug_row.invalidated_at, None);
    assert_eq!(debug_row.verified_at, None);
    assert!(debug_row.watch_paths.is_empty());
    assert!(
        debug_row.has_embedding,
        "vectors preserved across the 0.3.0 migration"
    );
    let hazard_row = entries.iter().find(|e| e.id == "mig3-hazard").unwrap();
    assert_eq!(hazard_row.epistemic, Epistemic::Fact);
}

/// The 0.3.0 columns carry real values through the normal write path:
/// an off-diagonal memory with a full Validity lands in the filtering
/// projection without any file loads.
#[tokio::test]
async fn epistemic_columns_roundtrip_through_index() {
    use engram_types::{Epistemic, Generality, Validity};

    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut memory = create_test_memory("epi-idx", Visibility::Shared);
    memory.type_ = MemoryType::Hazard;
    memory.epistemic = Epistemic::Observation; // off-diagonal
    memory.verified_at = Some("2026-03-01T00:00:00Z".parse().unwrap());
    memory.valid_from = Some("2026-01-10T00:00:00Z".parse().unwrap());
    memory.valid_while = Some(Validity {
        premise: Some("while ort is pinned".into()),
        invalidated_by: vec!["Cargo.lock".into(), "crates/engram-onnx/**".into()],
        origin_task: Some("epistemic-memory".into()),
        generality: Generality::Task,
        derived_from: vec![],
    });
    store.create(&memory).await.unwrap();

    let entries = store.lance_index.list_for_filtering().await.unwrap();
    let row = entries.iter().find(|e| e.id == "epi-idx").unwrap();
    assert_eq!(row.epistemic, Epistemic::Observation);
    assert_eq!(row.verified_at, memory.verified_at);
    assert_eq!(row.generality, Generality::Task);
    assert_eq!(row.origin_task.as_deref(), Some("epistemic-memory"));
    assert_eq!(row.invalidated_at, None);
    assert_eq!(
        row.watch_paths,
        vec![
            "Cargo.lock".to_string(),
            "crates/engram-onnx/**".to_string()
        ]
    );

    // The displayable projection carries valid_from.
    let filterable = store.lance_index.list_filterable().await.unwrap();
    let frow = filterable.iter().find(|e| e.id == "epi-idx").unwrap();
    assert_eq!(frow.valid_from, memory.valid_from);
}

/// `invalidate_with` closes the validity window atomically and reversibly.
#[tokio::test]
async fn invalidate_with_closes_and_reopens_window() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("inv-1", Visibility::Shared))
        .await
        .unwrap();

    let now: chrono::DateTime<chrono::Utc> = "2026-07-19T12:00:00Z".parse().unwrap();
    let closed = store
        .invalidate_with("inv-1", Some("succ-9".into()), now)
        .await
        .unwrap();
    assert_eq!(closed.invalidated_at, Some(now));
    assert_eq!(closed.superseded_by.as_deref(), Some("succ-9"));
    assert!(closed.is_invalidated_at(now));

    // Persisted (file + index), not just returned.
    let reread = store.get("inv-1").await.unwrap();
    assert_eq!(reread.invalidated_at, Some(now));
    let row = store
        .lance_index
        .list_for_filtering()
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id == "inv-1")
        .unwrap();
    assert_eq!(row.invalidated_at, Some(now));

    // Reopening is a plain update clearing the two fields.
    store
        .update_with("inv-1", |m| {
            m.invalidated_at = None;
            m.superseded_by = None;
            Ok(())
        })
        .await
        .unwrap();
    let reopened = store.get("inv-1").await.unwrap();
    assert_eq!(reopened.invalidated_at, None);
    assert_eq!(reopened.superseded_by, None);
}

/// The `has_embedding` projection flag tracks the chunk lifecycle: false at
/// create, true after chunks are written, preserved across a metadata-only
/// update, false after chunks are deleted.
#[tokio::test]
async fn has_embedding_flag_tracks_chunk_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("he-1", Visibility::Shared))
        .await
        .unwrap();
    assert!(!has_embedding_flag(&store, "he-1").await, "no chunks yet");

    store
        .upsert_chunks("he-1", vec![vec![0.2f32; 384]])
        .await
        .unwrap();
    assert!(has_embedding_flag(&store, "he-1").await, "chunks written");

    // A metadata-only update must not reset the flag.
    store
        .update_with("he-1", |m| {
            m.criticality = 0.9;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        has_embedding_flag(&store, "he-1").await,
        "update must preserve has_embedding"
    );

    store.delete_chunks("he-1").await.unwrap();
    assert!(!has_embedding_flag(&store, "he-1").await, "chunks deleted");
}

/// REGRESSION: the BATCHED chunk delete must clear `has_embedding` too.
///
/// `delete_chunks` cleared the flag but `delete_chunks_batch` did not,
/// while `upsert_chunks_batch` routes every entry whose text embedded to
/// nothing into the batched path. A memory edited down to empty content
/// therefore ended with zero chunk rows and `has_embedding = true`, and
/// stayed eligible for `has_embedding`-gated semantic ranking (R3) — with
/// no vectors to rank on — until the next reindex rebuilt the flag.
#[tokio::test]
async fn delete_chunks_batch_clears_has_embedding_flag() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    for id in ["hb-1", "hb-2"] {
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
        store
            .upsert_chunks(id, vec![vec![0.2f32; 384]])
            .await
            .unwrap();
        assert!(has_embedding_flag(&store, id).await);
    }

    store
        .lance_index
        .delete_chunks_batch(&["hb-1".to_string(), "hb-2".to_string()])
        .await
        .unwrap();

    for id in ["hb-1", "hb-2"] {
        assert!(
            !has_embedding_flag(&store, id).await,
            "{id}: batched chunk delete must clear has_embedding, \
             exactly as the per-memory delete does"
        );
    }
}

/// The empty-text path through `upsert_chunks_batch` (which delegates to
/// `delete_chunks_batch`) must leave the flag false — the end-to-end shape
/// of the regression above, driven through the public API.
#[tokio::test]
async fn upsert_chunks_batch_with_no_chunks_clears_has_embedding() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mem = create_test_memory("hb-empty", Visibility::Shared);
    store.create(&mem).await.unwrap();
    store
        .upsert_chunks("hb-empty", vec![vec![0.2f32; 384]])
        .await
        .unwrap();
    assert!(has_embedding_flag(&store, "hb-empty").await);

    // Re-embedding a now-empty memory yields no chunks for it.
    let current = store.get("hb-empty").await.unwrap();
    store
        .upsert_chunks_batch(vec![(
            "hb-empty".to_string(),
            current.updated_at,
            Vec::new(),
            None,
        )])
        .await
        .unwrap();

    assert!(
        !has_embedding_flag(&store, "hb-empty").await,
        "a memory re-embedded to zero chunks must not stay has_embedding=true"
    );
}

/// A store predating `keyword_stems` must gain populated stems on open,
/// and they must equal what computing from the `.md` file would produce.
/// Equality is the whole point: the stored value is only useful because
/// the query path can trust it matches what it would have derived.
#[tokio::test]
async fn schema_migration_to_0_5_0_backfills_keyword_stems() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

    let mut mem = create_test_memory("mig5-stems", Visibility::Shared);
    mem.summary = "Compression merges near duplicate memories".to_string();
    mem.content = "Candidates surface by similarity; compression is deferred.".to_string();
    mem.tags = vec!["maintenance".to_string()];
    store.create(&mem).await.unwrap();

    // Downgrade both stamps to reproduce a genuine pre-0.5.0 store.
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.4.0".to_string();
    m.normalizer = None;
    manifest::save_manifest(&manifest_path, &m).await.unwrap();

    let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let after = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(after.schema_version, manifest::CURRENT_SCHEMA_VERSION);
    assert_eq!(
        after.normalizer.as_deref(),
        Some(engram_types::NORMALIZER_STAMP),
        "the normalizer stamp is recorded alongside the schema version"
    );

    let entries = store2.lance_index.list_for_filtering().await.unwrap();
    let row = entries.iter().find(|e| e.id == "mig5-stems").unwrap();
    let stored = row
        .keyword_stems
        .as_ref()
        .expect("migration must populate keyword_stems");
    assert_eq!(
        *stored,
        engram_types::KeywordStems::compute(&mem.summary, &mem.tags, &mem.content),
        "stored stems must be identical to freshly computed ones"
    );
    // Sanity that stemming actually happened: "merges" -> "merg".
    assert!(
        stored.in_summary("merg"),
        "expected a stemmed summary term, got {:?}",
        stored.summary
    );
}

/// The second invalidation axis. A store already at the current schema but
/// carrying stems from a different normalizer must still be rebuilt —
/// the column is well-formed, its contents are simply not comparable.
#[tokio::test]
async fn stale_normalizer_stamp_triggers_reindex() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    store
        .create(&create_test_memory("stamp-mem", Visibility::Shared))
        .await
        .unwrap();

    // Schema stays current; only the normalizer stamp is foreign.
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(m.schema_version, manifest::CURRENT_SCHEMA_VERSION);
    m.normalizer = Some("some-other-stemmer-v9".to_string());
    manifest::save_manifest(&manifest_path, &m).await.unwrap();

    let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    assert_eq!(
        manifest::load_manifest(&manifest_path)
            .await
            .unwrap()
            .normalizer
            .as_deref(),
        Some(engram_types::NORMALIZER_STAMP),
        "a foreign normalizer stamp must be regenerated, not left in place"
    );
    let entries = store2.lance_index.list_for_filtering().await.unwrap();
    assert!(
        entries
            .iter()
            .find(|e| e.id == "stamp-mem")
            .unwrap()
            .keyword_stems
            .is_some(),
        "stems must be present after the normalizer-triggered rebuild"
    );
}

// ===================================================================
// Schema 0.8.0: content digests (index currency).
// ===================================================================

/// The row's recorded digest for `id`, or `None` when unknown.
async fn row_digest(store: &MemoryStore, id: &str) -> Option<crate::digest::FileDigest> {
    let digests = store.lance_index.list_digests().await.unwrap();
    let row = digests.into_iter().find(|d| d.memory_id == id)?;
    Some(crate::digest::FileDigest {
        sha256: row.content_sha256?,
        len: row.content_len?,
    })
}

/// The digest of whatever `id`'s file actually contains right now.
async fn disk_digest(store: &MemoryStore, id: &str) -> crate::digest::FileDigest {
    let shared = paths::memories_dir(&store.project_dir);
    let personal = paths::personal_memories_dir(&store.project_id).unwrap();
    for dir in [shared, personal] {
        if let Some(path) = find_memory_files(&dir, id)
            .await
            .unwrap()
            .into_iter()
            .next()
        {
            let bytes = async_fs::read(&path).await.unwrap();
            return crate::digest::FileDigest::of(&bytes);
        }
    }
    panic!("no file on disk for {id}");
}

/// `create` must stamp the identity of the bytes it wrote — not of a
/// re-serialization, and not nothing.
#[tokio::test]
async fn content_digest_stamped_on_create_matches_file_bytes() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("cd-1", Visibility::Shared))
        .await
        .unwrap();

    assert_eq!(
        row_digest(&store, "cd-1").await,
        Some(disk_digest(&store, "cd-1").await),
        "the row must record exactly what is on disk"
    );
}

/// Every write path stamps: single update, batch update, and batch create.
#[tokio::test]
async fn content_digest_stamped_on_every_write_path() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // create_batch — the path where `content` is scoped to a different loop
    // than the index rows, so the digest has to be carried forward.
    store
        .create_batch(&[
            create_test_memory("cd-b1", Visibility::Shared),
            create_test_memory("cd-b2", Visibility::Shared),
        ])
        .await
        .unwrap();
    for id in ["cd-b1", "cd-b2"] {
        assert_eq!(
            row_digest(&store, id).await,
            Some(disk_digest(&store, id).await),
            "{id}: create_batch must stamp each row from its own file"
        );
    }

    // update_with — a content change must move the digest.
    let before = row_digest(&store, "cd-b1").await.unwrap();
    store
        .update_with("cd-b1", |m| {
            m.content = "materially different content".to_string();
            Ok(())
        })
        .await
        .unwrap();
    let after = row_digest(&store, "cd-b1").await.unwrap();
    assert_ne!(before.sha256, after.sha256, "update must restamp");
    assert_eq!(after, disk_digest(&store, "cd-b1").await);

    // update_batch_with — the separate batched path.
    store
        .update_batch_with(&["cd-b2".to_string()], |m| {
            m.content = "batched rewrite".to_string();
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        row_digest(&store, "cd-b2").await,
        Some(disk_digest(&store, "cd-b2").await),
        "update_batch_with must stamp too"
    );
}

/// A title change renames the file. The row must record the digest of the
/// file that now exists, not of the one that was just deleted.
#[tokio::test]
async fn content_digest_survives_title_rename() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory(
            "0190cdcd-bbbb-7ccc-8ddd-000000000001",
            Visibility::Shared,
        ))
        .await
        .unwrap();

    store
        .update_with("0190cdcd-bbbb-7ccc-8ddd-000000000001", |m| {
            m.title = Some("a brand new title".to_string());
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(
        row_digest(&store, "0190cdcd-bbbb-7ccc-8ddd-000000000001").await,
        Some(disk_digest(&store, "0190cdcd-bbbb-7ccc-8ddd-000000000001").await),
        "the surviving file's digest must be the one recorded"
    );
}

/// `reindex` backfills digests from disk for rows that had none.
#[tokio::test]
async fn reindex_backfills_content_digest_from_disk() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("cd-rx", Visibility::Shared))
        .await
        .unwrap();

    // Simulate a row that predates the column by clearing it in place.
    let mut entry = IndexEntry::without_digest(&store.get("cd-rx").await.unwrap());
    entry.has_embedding = false;
    store.lance_index.upsert(&entry).await.unwrap();
    assert_eq!(row_digest(&store, "cd-rx").await, None, "cleared");

    store.reindex().await.unwrap();

    assert_eq!(
        row_digest(&store, "cd-rx").await,
        Some(disk_digest(&store, "cd-rx").await),
        "reindex must backfill the digest from the file it read"
    );
}

/// When two files claim one ID, `reindex_dir` deletes the loser. The row
/// must carry the digest of the file left on disk — recording the deleted
/// one would report permanent drift against a file that no longer exists.
#[tokio::test]
async fn content_digest_follows_winner_of_duplicate_id_resolution() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut mem = create_test_memory("0190cdcd-bbbb-7ccc-8ddd-000000000002", Visibility::Shared);
    mem.title = Some("winner".to_string());
    store.create(&mem).await.unwrap();

    // A stale duplicate under a different slug, with an older mtime and
    // materially different content.
    let mut stale = mem.clone();
    stale.title = Some("loser".to_string());
    stale.content = "stale duplicate content".to_string();
    let shared = paths::memories_dir(&store.project_dir);
    let stale_path = shared.join(memory_file::memory_filename(&stale));
    std::fs::write(&stale_path, memory_file::write_memory_file(&stale).unwrap()).unwrap();
    age_file(&stale_path, 3600);

    store.reindex().await.unwrap();

    assert!(!stale_path.exists(), "the older duplicate is removed");
    assert_eq!(
        row_digest(&store, "0190cdcd-bbbb-7ccc-8ddd-000000000002").await,
        Some(disk_digest(&store, "0190cdcd-bbbb-7ccc-8ddd-000000000002").await),
        "the digest must be the surviving file's"
    );
}

/// The 0.8.0 migration against a store whose Arrow schema genuinely lacks
/// the columns: they appear, are populated from the `.md` files, vectors
/// survive, and the stamp advances.
#[tokio::test]
async fn schema_migration_to_0_8_0_upgrades_a_real_0_7_0_store() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("cd-mig", Visibility::Shared))
        .await
        .unwrap();
    store
        .upsert_chunks("cd-mig", vec![vec![0.4f32; 384]])
        .await
        .unwrap();
    drop(store);

    downgrade_to_0_7_0(tmp.path()).await;

    // Opening migrates.
    let store = MemoryStore::open(tmp.path()).await.unwrap();

    assert_eq!(
        row_digest(&store, "cd-mig").await,
        Some(disk_digest(&store, "cd-mig").await),
        "migration must backfill the digest from the file"
    );
    assert_eq!(
        store.export_chunks("cd-mig").await.unwrap(),
        vec![vec![0.4f32; 384]],
        "vectors must survive a metadata-only migration"
    );
    let m = manifest::load_manifest(&paths::project_dir(tmp.path()).join("manifest.toml"))
        .await
        .unwrap();
    assert_eq!(m.schema_version, manifest::CURRENT_SCHEMA_VERSION);
}

/// A file edited behind the store's back must be visible as drift: the
/// recorded digest no longer matches the bytes on disk. This is the whole
/// point of the column — counts and id sets are both unchanged here.
#[tokio::test]
async fn hand_edited_file_diverges_from_its_recorded_digest() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("cd-edit", Visibility::Shared))
        .await
        .unwrap();
    let recorded = row_digest(&store, "cd-edit").await.unwrap();

    // Edit the file directly, exactly as a human or `git checkout` would.
    let shared = paths::memories_dir(&store.project_dir);
    let path = find_memory_files(&shared, "cd-edit")
        .await
        .unwrap()
        .pop()
        .unwrap();
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace("Test content", "Edited behind the store's back");
    std::fs::write(&path, &edited).unwrap();

    assert_ne!(
        recorded,
        disk_digest(&store, "cd-edit").await,
        "an in-place edit must diverge from the recorded digest"
    );
    // ...and the checks that exist today cannot see it.
    assert_eq!(store.list_ids().await.unwrap().len(), 1);
    assert!(
        store
            .check_staleness(&tier(StalenessCheck::Counts))
            .await
            .unwrap()
            .is_none(),
        "the count-based check is blind to this — which is why the digest exists"
    );
}

/// The embed digest rides the same write as the vectors, and is readable
/// back per memory.
#[tokio::test]
async fn embed_digest_is_written_and_read_back_with_its_vectors() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let m = create_test_memory("ed-1", Visibility::Shared);
    store.create(&m).await.unwrap();

    store
        .upsert_chunks_batch(vec![(
            "ed-1".to_string(),
            m.updated_at,
            vec![vec![0.1f32; 384], vec![0.2f32; 384]],
            Some("digest-abc".to_string()),
        )])
        .await
        .unwrap();

    let digests = store.embed_digests().await.unwrap();
    assert_eq!(
        digests.get("ed-1"),
        Some(&Some("digest-abc".to_string())),
        "the digest must be readable back for the memory it was written with"
    );
    // The vectors are unaffected by carrying the digest alongside them.
    assert_eq!(store.export_chunks("ed-1").await.unwrap().len(), 2);
}

/// The un-digested entry point records `None`, which reads as *unknown* —
/// never as *current*. Worktree relocation and tests take this path.
#[tokio::test]
async fn upsert_chunks_without_digest_records_unknown() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("ed-2", Visibility::Shared))
        .await
        .unwrap();
    store
        .upsert_chunks("ed-2", vec![vec![0.3f32; 384]])
        .await
        .unwrap();

    assert_eq!(
        store.embed_digests().await.unwrap().get("ed-2"),
        Some(&None),
        "present but unknown — the memory has vectors, of unrecorded provenance"
    );
}

/// Deleting a memory's chunks takes its digest with them: absent from the
/// map entirely, not left behind as a stale claim about vectors that are
/// gone.
#[tokio::test]
async fn deleting_chunks_removes_the_digest_too() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let m = create_test_memory("ed-3", Visibility::Shared);
    store.create(&m).await.unwrap();
    store
        .upsert_chunks_batch(vec![(
            "ed-3".to_string(),
            m.updated_at,
            vec![vec![0.4f32; 384]],
            Some("digest-xyz".to_string()),
        )])
        .await
        .unwrap();
    assert!(store.embed_digests().await.unwrap().contains_key("ed-3"));

    store.delete_chunks("ed-3").await.unwrap();

    assert!(
        !store.embed_digests().await.unwrap().contains_key("ed-3"),
        "no vectors ⇒ no digest; the two are written and deleted together"
    );
}

/// A chunks table written before 0.8.0 has no `embed_sha256` column at all.
/// The migration must add it in place — the vectors cannot be rebuilt from
/// anything, so recreating the table would destroy exactly what the
/// metadata-only migration exists to preserve.
#[tokio::test]
async fn schema_migration_adds_the_chunks_digest_column_in_place() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("ed-mig", Visibility::Shared))
        .await
        .unwrap();
    store
        .upsert_chunks("ed-mig", vec![vec![0.7f32; 384]])
        .await
        .unwrap();
    drop(store);

    drop_chunks_column(tmp.path(), "embed_sha256").await;
    downgrade_to_0_7_0(tmp.path()).await;

    let store = MemoryStore::open(tmp.path()).await.unwrap();

    // The column is back, the vectors survived, and their provenance reads
    // as unknown (so they re-embed when asked, rather than being trusted).
    assert_eq!(
        store.export_chunks("ed-mig").await.unwrap(),
        vec![vec![0.7f32; 384]],
        "vectors must survive the in-place column add"
    );
    assert_eq!(
        store.embed_digests().await.unwrap().get("ed-mig"),
        Some(&None)
    );

    // Idempotent: opening again must not fail or duplicate the column.
    drop(store);
    let store = MemoryStore::open(tmp.path()).await.unwrap();
    assert_eq!(store.export_chunks("ed-mig").await.unwrap().len(), 1);
}

/// Rewrite a store's `chunks` table without `col`, reproducing a table
/// written before that column existed.
async fn drop_chunks_column(dir: &Path, col: &str) {
    use arrow_array::RecordBatchIterator;
    use futures_util::stream::StreamExt;
    use lancedb::query::ExecutableQuery;
    use std::sync::Arc;

    let project_id = project_id::compute_project_id(dir);
    let lance_path = paths::lancedb_dir(&project_id).unwrap();
    let conn = lancedb::connect(lance_path.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    let table = conn.open_table("chunks").execute().await.unwrap();

    let mut stream = table.query().execute().await.unwrap();
    let mut reduced = Vec::new();
    let mut reduced_schema = None;
    while let Some(batch) = stream.next().await {
        let batch = batch.unwrap();
        let schema = batch.schema();
        let keep: Vec<usize> = (0..schema.fields().len())
            .filter(|i| schema.field(*i).name() != col)
            .collect();
        assert_eq!(
            keep.len() + 1,
            schema.fields().len(),
            "fixture must actually remove {col}"
        );
        let fields: Vec<_> = keep.iter().map(|i| schema.field(*i).clone()).collect();
        let new_schema = Arc::new(arrow_schema::Schema::new(fields));
        let columns: Vec<_> = keep.iter().map(|i| batch.column(*i).clone()).collect();
        reduced.push(arrow_array::RecordBatch::try_new(new_schema.clone(), columns).unwrap());
        reduced_schema = Some(new_schema);
    }
    let schema = reduced_schema.expect("fixture needs at least one chunk");

    conn.drop_table("chunks", &[]).await.unwrap();
    let batches: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
        RecordBatchIterator::new(reduced.into_iter().map(Ok), schema),
    );
    conn.create_table("chunks", batches)
        .execute()
        .await
        .unwrap();
}

/// Rewrite a store's `memories` table without the `source_sessions`
/// column, and stamp the manifest at 0.6.0.
async fn downgrade_to_0_6_0(dir: &Path) {
    downgrade_dropping(dir, &["source_sessions"], "0.6.0").await
}

/// Rewrite a store's `memories` table without the 0.8.0 content-digest
/// columns, and stamp the manifest at 0.7.0.
async fn downgrade_to_0_7_0(dir: &Path) {
    downgrade_dropping(dir, &["content_sha256", "content_len"], "0.7.0").await
}

async fn downgrade_dropping(dir: &Path, drop_cols: &[&str], version: &str) {
    crate::test_support::downgrade_dropping(dir, drop_cols, version).await
}

/// Reproduce a 0.8.0 store: no `edges` table at all, manifest stamped
/// 0.8.0. The table is new in 0.9.0 rather than a column, so dropping it
/// IS the previous version's on-disk shape.
async fn downgrade_to_0_8_0(dir: &Path) {
    let project_id = project_id::compute_project_id(dir);
    let lance_path = paths::lancedb_dir(&project_id).unwrap();
    let conn = lancedb::connect(lance_path.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    conn.drop_table(crate::edge_index::EDGES_TABLE, &[])
        .await
        .unwrap();
    let manifest_path = paths::project_dir(dir).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.8.0".to_string();
    m.normalizer = Some(engram_types::NORMALIZER_STAMP.to_string());
    manifest::save_manifest(&manifest_path, &m).await.unwrap();
}

/// The 0.9.0 migration (edges), against a real 0.8.0 store whose files
/// already carry `supersedes` / `superseded_by`: a plain open creates the
/// table AND mirrors those fields into it, so inbound queries work at
/// once rather than after the next write.
#[tokio::test]
async fn schema_migration_to_0_9_0_builds_edges_from_a_real_0_8_0_store() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    {
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        let mut new = create_test_memory("mig9-new", Visibility::Shared);
        new.supersedes = vec!["mig9-old".into()];
        store.create(&new).await.unwrap();
        let mut old = create_test_memory("mig9-old", Visibility::Shared);
        old.superseded_by = Some("mig9-new".into());
        store.create(&old).await.unwrap();
    }
    downgrade_to_0_8_0(tmp.path()).await;
    {
        let conn = lancedb::connect(
            paths::lancedb_dir(&project_id::compute_project_id(tmp.path()))
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .execute()
        .await
        .unwrap();
        assert!(
            crate::edge_index::open(&conn).await.unwrap().is_none(),
            "fixture still has the edges table, so the test proves nothing"
        );
    }

    let store = MemoryStore::open(tmp.path()).await.unwrap();
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    assert_eq!(
        manifest::load_manifest(&manifest_path)
            .await
            .unwrap()
            .schema_version,
        manifest::CURRENT_SCHEMA_VERSION
    );
    let edges = store.list_edges().await.unwrap();
    assert_eq!(
        edges.len(),
        1,
        "both fields describe one edge and fold to one: {edges:?}"
    );
    assert_eq!(edges[0].src_id, "mig9-new");
    assert_eq!(edges[0].dst_id, "mig9-old");
    assert_eq!(edges[0].kind, engram_types::LinkKind::Supersedes);
    // Answered from the target's end too.
    assert_eq!(store.edges_touching("mig9-old").await.unwrap().len(), 1);
}

/// The column names of the `memories` table under `lance_path`, read from
/// the Arrow schema Lance stores — not from what the index layer expects.
async fn memories_columns_at(lance_path: &Path) -> Vec<String> {
    let conn = lancedb::connect(lance_path.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    let table = conn.open_table("memories").execute().await.unwrap();
    table
        .schema()
        .await
        .unwrap()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect()
}

async fn memories_columns(dir: &Path) -> Vec<String> {
    memories_columns_at(&paths::lancedb_dir(&project_id::compute_project_id(dir)).unwrap()).await
}

async fn memories_table_version(dir: &Path) -> u64 {
    let lance = paths::lancedb_dir(&project_id::compute_project_id(dir)).unwrap();
    let conn = lancedb::connect(lance.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    let table = conn.open_table("memories").execute().await.unwrap();
    table.version().await.unwrap()
}

async fn schema_version_of(manifest_path: &Path) -> String {
    manifest::load_manifest(manifest_path)
        .await
        .unwrap()
        .schema_version
}

const EPISTEMIC_COLUMNS: [&str; 7] = [
    "epistemic",
    "verified_at",
    "generality",
    "origin_task",
    "valid_from",
    "invalidated_at",
    "watch_paths",
];

/// The 0.3.0 migration against a table that really lacks the seven
/// epistemic columns (the stamp-only twin above keeps every column, so it
/// never exercises the case that breaks). The columns come back with the
/// values the `.md` files carry, including an off-diagonal one.
#[tokio::test]
async fn migration_from_0_2_0_backfills_the_seven_epistemic_columns() {
    use engram_types::{Epistemic, Generality};

    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    {
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        let mut debug_mem = create_test_memory("mig2r-debug", Visibility::Shared);
        debug_mem.type_ = MemoryType::Debug;
        debug_mem.epistemic = MemoryType::Debug.default_epistemic();
        store.create(&debug_mem).await.unwrap();
        store
            .upsert_chunks("mig2r-debug", vec![vec![0.1f32; 384]])
            .await
            .unwrap();
        let mut watched = create_test_memory("mig2r-watched", Visibility::Shared);
        watched.type_ = MemoryType::Hazard;
        watched.epistemic = Epistemic::Observation; // off-diagonal
        watched.verified_at = Some("2026-03-01T00:00:00Z".parse().unwrap());
        watched.valid_from = Some("2026-01-10T00:00:00Z".parse().unwrap());
        watched.valid_while = Some(engram_types::Validity {
            premise: Some("while ort is pinned".into()),
            invalidated_by: vec!["src/lib.rs".into()],
            origin_task: Some("task-42".into()),
            generality: Generality::Task,
            derived_from: vec![],
        });
        store.create(&watched).await.unwrap();
    }
    downgrade_dropping(tmp.path(), &EPISTEMIC_COLUMNS, "0.2.0").await;
    let before = memories_columns(tmp.path()).await;
    for col in EPISTEMIC_COLUMNS {
        assert!(
            !before.iter().any(|c| c == col),
            "fixture still has {col}, so the test proves nothing"
        );
    }

    let store = MemoryStore::open(tmp.path()).await.unwrap();
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    assert_eq!(
        schema_version_of(&manifest_path).await,
        manifest::CURRENT_SCHEMA_VERSION
    );
    let after = memories_columns(tmp.path()).await;
    for col in EPISTEMIC_COLUMNS {
        assert!(after.iter().any(|c| c == col), "{col} not backfilled");
    }

    let entries = store.lance_index.list_for_filtering().await.unwrap();
    let debug_row = entries.iter().find(|e| e.id == "mig2r-debug").unwrap();
    assert_eq!(debug_row.epistemic, Epistemic::Observation);
    assert_eq!(debug_row.generality, Generality::Project);
    assert!(debug_row.watch_paths.is_empty());
    assert!(debug_row.has_embedding, "vectors preserved");
    let watched_row = entries.iter().find(|e| e.id == "mig2r-watched").unwrap();
    assert_eq!(watched_row.epistemic, Epistemic::Observation);
    assert_eq!(watched_row.generality, Generality::Task);
    assert_eq!(watched_row.watch_paths, vec!["src/lib.rs".to_string()]);
    assert_eq!(watched_row.origin_task.as_deref(), Some("task-42"));
    assert_eq!(
        watched_row.verified_at,
        Some("2026-03-01T00:00:00Z".parse().unwrap())
    );
    assert_eq!(watched_row.invalidated_at, None);
    let filterable = store.lance_index.list_filterable().await.unwrap();
    assert_eq!(
        filterable
            .iter()
            .find(|e| e.id == "mig2r-watched")
            .unwrap()
            .valid_from,
        Some("2026-01-10T00:00:00Z".parse().unwrap())
    );
}

/// The 0.5.0 migration against a table that really lacks `keyword_stems`.
/// Only the schema axis is stale here (`downgrade_dropping` stamps the
/// current normalizer), so the version gate alone has to trigger it.
#[tokio::test]
async fn migration_from_0_4_0_backfills_keyword_stems() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let mut mem = create_test_memory("mig4r-stems", Visibility::Shared);
    mem.summary = "Compression merges near duplicate memories".to_string();
    mem.content = "Candidates surface by similarity; compression is deferred.".to_string();
    mem.tags = vec!["maintenance".to_string()];
    MemoryStore::init(tmp.path(), &reg)
        .await
        .unwrap()
        .create(&mem)
        .await
        .unwrap();
    downgrade_dropping(tmp.path(), &["keyword_stems"], "0.4.0").await;
    assert!(
        !memories_columns(tmp.path())
            .await
            .iter()
            .any(|c| c == "keyword_stems"),
        "fixture still has keyword_stems"
    );

    let store = MemoryStore::open(tmp.path()).await.unwrap();
    assert!(memories_columns(tmp.path())
        .await
        .iter()
        .any(|c| c == "keyword_stems"));
    let entries = store.lance_index.list_for_filtering().await.unwrap();
    let row = entries.iter().find(|e| e.id == "mig4r-stems").unwrap();
    assert_eq!(
        row.keyword_stems.as_ref().expect("stems backfilled"),
        &engram_types::KeywordStems::compute(&mem.summary, &mem.tags, &mem.content)
    );
}

/// A store stamped 0.5.0 migrates on a plain open, keeps its memories,
/// and gets **no** `conversations` table. The harvest path creates that
/// table on first use. The migration must not: an existing table is what
/// `paths::holds_irreplaceable_data` retains a data dir for, so an empty
/// one made by every migration would keep every migrated store's data dir
/// from `projects prune` for good.
#[tokio::test]
async fn migration_from_0_5_0_leaves_the_conversations_table_to_harvest() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    MemoryStore::init(tmp.path(), &reg)
        .await
        .unwrap()
        .create(&create_test_memory("mig5r-mem", Visibility::Shared))
        .await
        .unwrap();
    let lance = paths::lancedb_dir(&project_id::compute_project_id(tmp.path())).unwrap();
    let table_dir = crate::conversation_index::ConversationIndex::table_path_in(&lance);
    let _ = std::fs::remove_dir_all(&table_dir);
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.5.0".to_string();
    manifest::save_manifest(&manifest_path, &m).await.unwrap();

    let store = MemoryStore::open(tmp.path()).await.unwrap();
    assert_eq!(
        schema_version_of(&manifest_path).await,
        manifest::CURRENT_SCHEMA_VERSION
    );
    assert!(
        !table_dir.exists(),
        "the migration created a conversations table, which pins the data dir"
    );
    assert_eq!(
        store.get("mig5r-mem").await.unwrap().summary,
        "Test summary"
    );
}

/// A store a newer binary wrote (a later stamp, a column this binary does
/// not know) is opened as it is: no rebuild, no restamp, no new table
/// version — a rebuild here would drop the newer binary's column.
#[tokio::test]
async fn a_store_from_a_newer_binary_is_opened_without_reindex_or_restamp() {
    use arrow_array::RecordBatchIterator;
    use futures_util::stream::StreamExt;
    use lancedb::query::ExecutableQuery;

    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    MemoryStore::init(tmp.path(), &reg)
        .await
        .unwrap()
        .create(&create_test_memory("newer-mem", Visibility::Shared))
        .await
        .unwrap();

    // Add a nullable column the way a newer binary's table would carry
    // it: rebuild the table from its own batches plus one all-null field.
    {
        let lance = paths::lancedb_dir(&project_id::compute_project_id(tmp.path())).unwrap();
        let conn = lancedb::connect(lance.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let table = conn.open_table("memories").execute().await.unwrap();
        let mut stream = table.query().execute().await.unwrap();
        let mut batches = Vec::new();
        let mut wider = None;
        while let Some(batch) = stream.next().await {
            let batch = batch.unwrap();
            let mut fields: Vec<_> = batch.schema().fields().iter().cloned().collect();
            fields.push(std::sync::Arc::new(arrow_schema::Field::new(
                "from_the_future",
                arrow_schema::DataType::Utf8,
                true,
            )));
            let schema = std::sync::Arc::new(arrow_schema::Schema::new(fields));
            let mut columns = batch.columns().to_vec();
            columns.push(arrow_array::new_null_array(
                &arrow_schema::DataType::Utf8,
                batch.num_rows(),
            ));
            batches.push(arrow_array::RecordBatch::try_new(schema.clone(), columns).unwrap());
            wider = Some(schema);
        }
        drop(table);
        conn.drop_table("memories", &[]).await.unwrap();
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
            RecordBatchIterator::new(batches.into_iter().map(Ok), wider.unwrap()),
        );
        conn.create_table("memories", reader)
            .execute()
            .await
            .unwrap();
    }
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.99.0".to_string();
    m.normalizer = Some(engram_types::NORMALIZER_STAMP.to_string());
    manifest::save_manifest(&manifest_path, &m).await.unwrap();
    let version_before = memories_table_version(tmp.path()).await;
    MIGRATION_REBUILDS.store(0, std::sync::atomic::Ordering::SeqCst);

    let store = MemoryStore::open(tmp.path()).await.unwrap();
    assert_eq!(store.get("newer-mem").await.unwrap().id, "newer-mem");
    drop(store);

    assert_eq!(
        MIGRATION_REBUILDS.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a newer store must not be rebuilt"
    );
    assert_eq!(schema_version_of(&manifest_path).await, "0.99.0");
    assert!(
        memories_columns(tmp.path())
            .await
            .iter()
            .any(|c| c == "from_the_future"),
        "the newer binary's column must survive the open"
    );
    assert_eq!(
        memories_table_version(tmp.path()).await,
        version_before,
        "opening must commit nothing to the memories table"
    );
}

/// Two opens racing on a store that really lacks a column (not just a
/// stale stamp): both succeed, and the rows and vectors are what they were.
/// `concurrent_opens_migrate_once` covers the rebuild count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_opens_of_a_stale_store_both_succeed_and_agree() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    {
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        for i in 0..4 {
            let id = format!("race-{i}");
            store
                .create(&create_test_memory(&id, Visibility::Shared))
                .await
                .unwrap();
            if i % 2 == 0 {
                store
                    .upsert_chunks(&id, vec![vec![0.3f32; 384]])
                    .await
                    .unwrap();
            }
        }
    }
    downgrade_to_0_6_0(tmp.path()).await;

    let (a, b) = tokio::join!(MemoryStore::open(tmp.path()), MemoryStore::open(tmp.path()));
    let (a, b) = (a.unwrap(), b.unwrap());
    for store in [&a, &b] {
        assert_eq!(store.count().await.unwrap(), 4);
        for i in 0..4 {
            assert_eq!(
                has_embedding_flag(store, &format!("race-{i}")).await,
                i % 2 == 0,
                "vectors of race-{i} changed across the racing migration"
            );
        }
    }
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    assert_eq!(
        schema_version_of(&manifest_path).await,
        manifest::CURRENT_SCHEMA_VERSION
    );
    assert!(memories_columns(tmp.path())
        .await
        .iter()
        .any(|c| c == "source_sessions"));
}

/// The global store migrates on `open_global` like a project store: a
/// table that really lacks a column is rebuilt and stamped, vectors kept.
#[tokio::test]
async fn a_stale_global_store_migrates_on_open() {
    let GlobalStoreHandle { store, _lock } = setup_global_store().await;
    store
        .create(&create_test_memory("global-stale", Visibility::Shared))
        .await
        .unwrap();
    store
        .upsert_chunks("global-stale", vec![vec![0.4f32; 384]])
        .await
        .unwrap();
    drop(store);
    let lance = paths::global_lancedb_dir().unwrap();
    let manifest_path =
        paths::project_dir(&paths::global_store_dir().unwrap()).join("manifest.toml");
    crate::test_support::downgrade_dropping_at(
        &lance,
        &manifest_path,
        &["source_sessions"],
        "0.6.0",
    )
    .await;
    assert!(!memories_columns_at(&lance)
        .await
        .iter()
        .any(|c| c == "source_sessions"));

    let store = MemoryStore::open_global().await.unwrap();
    assert_eq!(
        schema_version_of(&manifest_path).await,
        manifest::CURRENT_SCHEMA_VERSION
    );
    assert!(memories_columns_at(&lance)
        .await
        .iter()
        .any(|c| c == "source_sessions"));
    assert_eq!(store.get("global-stale").await.unwrap().id, "global-stale");
    assert!(has_embedding_flag(&store, "global-stale").await);
}

/// The same for a group store, through `open_group`.
#[tokio::test]
async fn a_stale_group_store_migrates_on_open() {
    let group_id = paths::compute_group_id("test-group-stale-migrate");
    {
        let store = MemoryStore::init_group(&group_id).await.unwrap();
        store
            .create(&create_test_memory("group-stale", Visibility::Shared))
            .await
            .unwrap();
        store
            .upsert_chunks("group-stale", vec![vec![0.5f32; 384]])
            .await
            .unwrap();
    }
    let lance = paths::group_lancedb_dir(&group_id).unwrap();
    let manifest_path =
        paths::project_dir(&paths::group_store_dir(&group_id).unwrap()).join("manifest.toml");
    crate::test_support::downgrade_dropping_at(
        &lance,
        &manifest_path,
        &["source_sessions"],
        "0.6.0",
    )
    .await;
    assert!(!memories_columns_at(&lance)
        .await
        .iter()
        .any(|c| c == "source_sessions"));

    let store = MemoryStore::open_group(&group_id).await.unwrap();
    assert_eq!(
        schema_version_of(&manifest_path).await,
        manifest::CURRENT_SCHEMA_VERSION
    );
    assert!(memories_columns_at(&lance)
        .await
        .iter()
        .any(|c| c == "source_sessions"));
    assert_eq!(store.get("group-stale").await.unwrap().id, "group-stale");
    assert!(has_embedding_flag(&store, "group-stale").await);
}

/// A real merge conflict in a hidden block must not cost the memory its
/// row, its vectors or its edges on ANY reindex — full, incremental, or the
/// schema migration's forced rebuild — and every one must report it.
#[tokio::test]
async fn reindex_keeps_row_vectors_and_edges_of_a_conflicted_file() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let other = Memory::new(MemoryType::Decision, "other", "c", Provenance::human());
    store.create(&other).await.unwrap();
    let mut m = Memory::new(MemoryType::Decision, "conflicted", "c", Provenance::human());
    m.supersedes = vec!["old-1".into()];
    m.links = vec![engram_types::Link::new(
        other.id.clone(),
        engram_types::LinkKind::Supports,
        engram_types::LinkOrigin::Human,
    )];
    store.create(&m).await.unwrap();
    store
        .upsert_chunks(&m.id, vec![vec![0.25f32; 384]])
        .await
        .unwrap();

    let path = paths::memories_dir(tmp.path()).join(memory_file::memory_filename(&m));
    let clean = async_fs::read_to_string(&path).await.unwrap();
    let conflicted = clean.replace(
        "supersedes:\n- old-1\n",
        "<<<<<<< HEAD\nsupersedes:\n- old-1\n=======\nsupersedes: []\n>>>>>>> topic\n",
    );
    assert_ne!(clean, conflicted, "fixture did not inject the conflict");
    async_fs::write(&path, &conflicted).await.unwrap();

    let check = |label: &'static str| {
        let store = &store;
        let id = m.id.clone();
        async move {
            assert!(
                store.list_ids().await.unwrap().contains(&id),
                "{label}: the row was dropped"
            );
            assert!(
                store.has_chunks_for(&id).await.unwrap(),
                "{label}: the vectors were pruned"
            );
            assert!(
                store
                    .list_edges()
                    .await
                    .unwrap()
                    .iter()
                    .any(|e| e.src_id == id && e.kind == engram_types::LinkKind::Supports),
                "{label}: its edges were dropped"
            );
        }
    };

    let counts = store.reindex_counted().await.unwrap();
    assert_eq!(counts.unparseable.len(), 1, "{counts:?}");
    assert_eq!(counts.unparseable[0].id.as_deref(), Some(m.id.as_str()));
    assert!(counts.unparseable[0].preserved);
    assert!(counts.unparseable[0].error.contains("merge conflict"));
    check("full").await;

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.unparseable.len(), 1);
    check("incremental").await;

    drop(store);
    downgrade_to_0_8_0(tmp.path()).await;
    let store = MemoryStore::open(tmp.path()).await.unwrap();
    let ids = store.list_ids().await.unwrap();
    assert!(ids.contains(&m.id), "migration dropped the row");
    assert!(
        store.has_chunks_for(&m.id).await.unwrap(),
        "migration pruned vectors"
    );
    // The file itself is untouched throughout.
    assert_eq!(async_fs::read_to_string(&path).await.unwrap(), conflicted);
}

/// When the edges of an unparseable file cannot be read, a full reindex
/// refuses before clearing anything: its whole-table edge rebuild would
/// otherwise delete the rows that file owns.
#[tokio::test]
async fn reindex_refuses_when_an_unparseable_files_edges_cannot_be_read() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let other = Memory::new(MemoryType::Decision, "other", "c", Provenance::human());
    store.create(&other).await.unwrap();
    let mut m = Memory::new(MemoryType::Decision, "conflicted", "c", Provenance::human());
    m.supersedes = vec!["old-1".into()];
    m.links = vec![engram_types::Link::new(
        other.id.clone(),
        engram_types::LinkKind::Supports,
        engram_types::LinkOrigin::Human,
    )];
    store.create(&m).await.unwrap();
    let path = paths::memories_dir(tmp.path()).join(memory_file::memory_filename(&m));
    let clean = async_fs::read_to_string(&path).await.unwrap();
    let conflicted = clean.replace(
        "supersedes:\n- old-1\n",
        "<<<<<<< HEAD\nsupersedes:\n- old-1\n=======\nsupersedes: []\n>>>>>>> topic\n",
    );
    assert_ne!(clean, conflicted, "fixture did not inject the conflict");
    async_fs::write(&path, &conflicted).await.unwrap();

    // Break the edges table so it lists but will not open.
    let versions = paths::lancedb_dir(&store.project_id)
        .unwrap()
        .join(format!("{}.lance", crate::edge_index::EDGES_TABLE))
        .join("_versions");
    assert!(versions.is_dir(), "fixture: no edges table at {versions:?}");
    std::fs::remove_dir_all(&versions).unwrap();

    let err = store.reindex_counted().await.unwrap_err();
    assert!(
        err.to_string().contains("unparseable"),
        "the refusal names its cause: {err}"
    );
    assert!(
        store.list_ids().await.unwrap().contains(&m.id),
        "nothing was cleared"
    );
}

#[test]
fn recover_id_reads_frontmatter_then_stem() {
    let id = "018f2a1b-3c4d-7e5f-8a9b-000000000010";
    let fm = format!("---\nversion: 2\nid: {id}\n---\nbody");
    assert_eq!(
        memory_file::recover_id(&fm, Path::new("whatever.md")).as_deref(),
        Some(id)
    );
    let stem = format!("some-title_{id}.md");
    assert_eq!(
        memory_file::recover_id("<<<<<<< garbage", Path::new(&stem)).as_deref(),
        Some(id)
    );
    assert_eq!(memory_file::recover_id("x", Path::new("notes.md")), None);
}

/// The write path keeps the table in step with the files, and a delete
/// takes the deleted memory's own rows but leaves edges INTO it dangling.
#[tokio::test]
async fn edges_follow_writes_and_deletes() {
    use engram_types::{Link, LinkKind, LinkOrigin};
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    store
        .create(&create_test_memory("e-a", Visibility::Shared))
        .await
        .unwrap();
    store
        .create(&create_test_memory("e-b", Visibility::Shared))
        .await
        .unwrap();
    assert!(store.list_edges().await.unwrap().is_empty());

    store
        .update_with("e-a", |m| {
            m.links
                .push(Link::new("e-b", LinkKind::Supports, LinkOrigin::Human));
            Ok(())
        })
        .await
        .unwrap();
    store
        .update_with("e-b", |m| {
            m.links
                .push(Link::new("e-a", LinkKind::RelatedTo, LinkOrigin::Agent));
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(store.list_edges().await.unwrap().len(), 2);
    assert_eq!(store.edges_touching("e-a").await.unwrap().len(), 2);

    // A reindex reproduces exactly the same table from the files.
    let before = store.list_edges().await.unwrap();
    store.reindex().await.unwrap();
    let mut after = store.list_edges().await.unwrap();
    let mut before = before;
    before.sort_by_key(|r| r.edge_key());
    after.sort_by_key(|r| r.edge_key());
    assert_eq!(before, after);

    store.delete("e-b").await.unwrap();
    let left = store.list_edges().await.unwrap();
    assert_eq!(left.len(), 1, "e-a's edge into the deleted e-b stays");
    assert_eq!(left[0].src_id, "e-a");
    assert_eq!(left[0].dst_id, "e-b");
}

/// A store with two memories and one edge between them, then its edges
/// table broken so it lists but will not open (its `_versions` gone).
async fn store_with_a_broken_edges_table(tmp: &TempDir) -> MemoryStore {
    use engram_types::{Link, LinkKind, LinkOrigin};
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    store
        .create(&create_test_memory("w-a", Visibility::Shared))
        .await
        .unwrap();
    store
        .create(&create_test_memory("w-b", Visibility::Shared))
        .await
        .unwrap();
    store
        .update_with("w-a", |m| {
            m.links
                .push(Link::new("w-b", LinkKind::Supports, LinkOrigin::Human));
            Ok(())
        })
        .await
        .unwrap();
    let versions = paths::lancedb_dir(&store.project_id)
        .unwrap()
        .join(format!("{}.lance", crate::edge_index::EDGES_TABLE))
        .join("_versions");
    assert!(versions.is_dir(), "fixture: no edges table at {versions:?}");
    std::fs::remove_dir_all(&versions).unwrap();
    assert!(
        store.list_edges().await.is_err(),
        "fixture: the edges table still opens"
    );
    store
}

fn md_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(paths::memories_dir(dir))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect()
}

/// The `.md` is on disk before the edges are synced, so a failed edge
/// write must not fail the create: failing it made a retried create write
/// the memory twice.
#[tokio::test]
async fn create_succeeds_once_when_the_edges_table_cannot_be_written() {
    use engram_types::{Link, LinkKind, LinkOrigin};
    let tmp = TempDir::new().unwrap();
    let store = store_with_a_broken_edges_table(&tmp).await;
    let before = md_files(tmp.path()).len();

    let mut m = create_test_memory("w-c", Visibility::Shared);
    m.links
        .push(Link::new("w-a", LinkKind::Supports, LinkOrigin::Human));
    store.create(&m).await.expect("the create must succeed");

    let after = md_files(tmp.path());
    assert_eq!(after.len(), before + 1, "exactly one new file: {after:?}");
    assert!(after.iter().all(|f| !f.contains(".tmp")), "{after:?}");
    assert!(store.list_ids().await.unwrap().contains(&"w-c".to_string()));
}

#[tokio::test]
async fn update_succeeds_when_the_edges_table_cannot_be_written() {
    use engram_types::{Link, LinkKind, LinkOrigin};
    let tmp = TempDir::new().unwrap();
    let store = store_with_a_broken_edges_table(&tmp).await;

    store
        .update_with("w-b", |m| {
            m.links
                .push(Link::new("w-a", LinkKind::RelatedTo, LinkOrigin::Agent));
            Ok(())
        })
        .await
        .expect("the update must succeed");
    let got = store.get("w-b").await.unwrap();
    assert_eq!(got.links.len(), 1, "the link is in the file");
}

/// `reindex` is the documented repair for edge-table drift, so it must
/// rebuild a table that will not open (it used to fail with "Table
/// 'edges' exists but could not be loaded").
#[tokio::test]
async fn reindex_rebuilds_an_edges_table_that_will_not_open() {
    let tmp = TempDir::new().unwrap();
    let store = store_with_a_broken_edges_table(&tmp).await;
    store.reindex().await.expect("reindex repairs the table");
    assert_eq!(store.list_edges().await.unwrap().len(), 1);
}

#[tokio::test]
async fn delete_succeeds_when_edge_rows_cannot_be_removed() {
    let tmp = TempDir::new().unwrap();
    let store = store_with_a_broken_edges_table(&tmp).await;

    store
        .delete("w-a")
        .await
        .expect("the delete must succeed although w-a owns an edge row");
    assert!(!store.list_ids().await.unwrap().contains(&"w-a".to_string()));
    assert!(
        md_files(tmp.path()).iter().all(|f| !f.contains("w-a")),
        "the file is gone"
    );
}

/// The 0.7.0 schema migration (harvest provenance), against a store whose
/// table really was written by the previous version: the memories survive
/// intact, the `source_sessions` column appears, vectors are preserved, and
/// the stamp advances.
#[tokio::test]
async fn schema_migration_to_0_7_0_upgrades_a_real_0_6_0_store() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    {
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        let mut shared = create_test_memory("mig7-shared", Visibility::Shared);
        shared.tags = vec!["alpha".into(), "beta".into()];
        shared.details = Some("details that must survive".into());
        store.create(&shared).await.unwrap();
        store
            .upsert_chunks("mig7-shared", vec![vec![0.2f32; 384]])
            .await
            .unwrap();
        store
            .create(&create_test_memory("mig7-personal", Visibility::Personal))
            .await
            .unwrap();
    }
    downgrade_to_0_6_0(tmp.path()).await;

    // Control: the fixture is genuinely old. A projection of the new column
    // against it fails, which is exactly what would happen to every pin
    // scan if the migration did not run.
    {
        let old = LanceIndex::new(
            &paths::lancedb_dir(&project_id::compute_project_id(tmp.path())).unwrap(),
            384,
        )
        .await
        .unwrap();
        assert!(
            old.list_source_session_links().await.is_err(),
            "fixture still has the 0.7.0 column, so the test proves nothing"
        );
    }

    // The hot path: a plain open must upgrade it.
    let store = MemoryStore::open(tmp.path()).await.unwrap();
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    assert_eq!(
        manifest::load_manifest(&manifest_path)
            .await
            .unwrap()
            .schema_version,
        manifest::CURRENT_SCHEMA_VERSION,
        "migration must stamp the current schema version"
    );

    // Nothing lost: both memories, both visibilities, content and metadata.
    assert_eq!(store.count().await.unwrap(), 2);
    let shared = store.get("mig7-shared").await.unwrap();
    assert_eq!(shared.tags, vec!["alpha".to_string(), "beta".to_string()]);
    assert_eq!(shared.details.as_deref(), Some("details that must survive"));
    assert_eq!(shared.visibility, Visibility::Shared);
    assert!(
        shared.source_sessions.is_empty(),
        "a memory written before the column cites nothing — not an error"
    );
    let personal = store.get("mig7-personal").await.unwrap();
    assert_eq!(personal.visibility, Visibility::Personal);
    assert!(
        has_embedding_flag(&store, "mig7-shared").await,
        "vectors preserved across the 0.7.0 migration"
    );

    // The column is there and usable: the scan runs, and a link written
    // after the upgrade is visible to it.
    assert!(store.list_source_session_links().await.unwrap().is_empty());
    store
        .update_with("mig7-shared", |m| {
            m.link_source_session("sess-after-migration");
            Ok(())
        })
        .await
        .unwrap();
    let links = store.list_source_session_links().await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].memory_id, "mig7-shared");
    assert_eq!(links[0].sessions, vec!["sess-after-migration".to_string()]);

    // Idempotent: a second open does not re-migrate.
    let _store2 = MemoryStore::open(tmp.path()).await.unwrap();
    assert_eq!(
        manifest::load_manifest(&manifest_path)
            .await
            .unwrap()
            .schema_version,
        manifest::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
        store.list_source_session_links().await.unwrap().len(),
        1,
        "the link written after the migration must survive the next open"
    );
}

/// A store already current on both axes must NOT reindex — migration is
/// meant to be a one-time cost, not a per-open one.
#[tokio::test]
async fn current_store_does_not_remigrate() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    {
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        store
            .create(&create_test_memory("noop-mem", Visibility::Shared))
            .await
            .unwrap();
    }
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let before = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(
        before.normalizer.as_deref(),
        Some(engram_types::NORMALIZER_STAMP),
        "a store created by this binary is born current"
    );

    let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let after = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(after.schema_version, before.schema_version);
    assert_eq!(after.normalizer, before.normalizer);
    assert_eq!(store2.count().await.unwrap(), 1);
}

/// Migration must also run on the plain `open` path — the hot path for every
/// CLI command and the MCP server — not just `init`/global. A pre-migration
/// store opened via `open` is transparently re-indexed and stamped current.
#[tokio::test]
async fn schema_migration_runs_on_open_hot_path() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    // Create + embed a memory, then downgrade the recorded schema version.
    {
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        store
            .create(&create_test_memory("open-mig", Visibility::Shared))
            .await
            .unwrap();
        store
            .upsert_chunks("open-mig", vec![vec![0.3f32; 384]])
            .await
            .unwrap();
    }
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.1.0".to_string();
    manifest::save_manifest(&manifest_path, &m).await.unwrap();

    // Open via the hot path (NOT init) → migration must run.
    let store = MemoryStore::open(tmp.path()).await.unwrap();
    assert_eq!(
        manifest::load_manifest(&manifest_path)
            .await
            .unwrap()
            .schema_version,
        manifest::CURRENT_SCHEMA_VERSION,
        "open() must migrate a pre-migration store"
    );
    assert!(
        has_embedding_flag(&store, "open-mig").await,
        "has_embedding backfilled from chunks on the open() migration"
    );
}

/// Re-creating an existing memory (worktree consolidation / visibility flip)
/// must not reset `has_embedding` to false — the chunks table is untouched,
/// so a fresh `IndexEntry`'s default would otherwise drop the memory from
/// semantic ranking (R3) until a reindex.
#[tokio::test]
async fn recreate_preserves_has_embedding() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mem = create_test_memory("recreate-1", Visibility::Shared);
    store.create(&mem).await.unwrap();
    store
        .upsert_chunks("recreate-1", vec![vec![0.4f32; 384]])
        .await
        .unwrap();
    assert!(has_embedding_flag(&store, "recreate-1").await, "embedded");

    // Re-create the same id without touching chunks (e.g. a re-run).
    store.create(&mem).await.unwrap();
    assert!(
        has_embedding_flag(&store, "recreate-1").await,
        "re-create must preserve has_embedding while chunks still exist"
    );
}

// ===================================================================
// `create_batch` must be indistinguishable from `create` in a loop.
// Each test below pins one guarantee the batched path could plausibly
// drop, since it reimplements the probe/sweep/upsert sequence rather
// than calling `create` internally.
// ===================================================================

#[tokio::test]
async fn create_batch_writes_every_memory_and_indexes_them() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let batch: Vec<Memory> = (0..25)
        .map(|i| {
            create_test_memory(
                &format!("0190aaaa-bbbb-7ccc-8ddd-{:012}", i),
                Visibility::Shared,
            )
        })
        .collect();
    let ids = store.create_batch(&batch).await.unwrap();

    assert_eq!(ids.len(), 25, "one id returned per input, in order");
    assert_eq!(ids, batch.iter().map(|m| m.id.clone()).collect::<Vec<_>>());
    assert_eq!(store.count().await.unwrap(), 25, "every row indexed");
    for m in &batch {
        assert_eq!(store.get(&m.id).await.unwrap().id, m.id);
    }
}

#[tokio::test]
async fn create_batch_empty_is_a_noop() {
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    assert!(store.create_batch(&[]).await.unwrap().is_empty());
    assert_eq!(store.count().await.unwrap(), 0);
}

#[tokio::test]
async fn create_batch_preserves_has_embedding_for_recreated_memories() {
    // The batched `has_embedding` carry-forward is a single probe over the
    // whole set, so a mistake here would silently drop already-embedded
    // memories out of semantic ranking (R3) — and only for re-creates,
    // which is exactly the worktree consolidation re-run case.
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let embedded = create_test_memory("0190aaaa-bbbb-7ccc-8ddd-00000000000a", Visibility::Shared);
    let bare = create_test_memory("0190aaaa-bbbb-7ccc-8ddd-00000000000b", Visibility::Shared);
    store.create(&embedded).await.unwrap();
    store
        .upsert_chunks(&embedded.id, vec![vec![0.4f32; 384]])
        .await
        .unwrap();
    assert!(has_embedding_flag(&store, &embedded.id).await);

    // Re-create BOTH in one batch, without touching chunks.
    store
        .create_batch(&[embedded.clone(), bare.clone()])
        .await
        .unwrap();

    assert!(
        has_embedding_flag(&store, &embedded.id).await,
        "re-create in a batch must preserve has_embedding while chunks exist"
    );
    assert!(
        !has_embedding_flag(&store, &bare.id).await,
        "a memory with no chunks must not be marked embedded"
    );
}

#[tokio::test]
async fn create_batch_sweeps_old_visibility_file() {
    // Mirrors `create_same_id_different_visibility_does_not_orphan`. The
    // batched path takes its pre-existence probe once, before any write,
    // so this also pins that the probe is not accidentally reading the
    // batch's own freshly-written files.
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-00000000000c";

    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();

    // Re-create the same id as Personal, in a batch alongside a fresh one.
    let fresh = create_test_memory("0190aaaa-bbbb-7ccc-8ddd-00000000000d", Visibility::Shared);
    store
        .create_batch(&[create_test_memory(id, Visibility::Personal), fresh.clone()])
        .await
        .unwrap();

    let shared = paths::memories_dir(&store.project_dir);
    let personal = paths::personal_memories_dir(&store.project_id).unwrap();

    assert_eq!(
        count_files(&shared).await,
        1,
        "only the fresh Shared memory remains; the old Shared file is swept"
    );
    assert_eq!(count_files(&personal).await, 1);
    assert_eq!(store.count().await.unwrap(), 2);
    assert_eq!(
        store.get(id).await.unwrap().visibility,
        Visibility::Personal
    );
}

#[tokio::test]
async fn create_batch_refreshes_manifest_stats() {
    // The manifest refresh moved from once-per-memory to once-per-batch;
    // the end state must still be correct.
    let tmp = TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let batch: Vec<Memory> = (0..7)
        .map(|i| {
            create_test_memory(
                &format!("0190aaaa-bbbb-7ccc-8ddd-{:012}", 100 + i),
                Visibility::Shared,
            )
        })
        .collect();
    store.create_batch(&batch).await.unwrap();

    let manifest_path = paths::project_dir(&store.project_dir).join("manifest.toml");
    let manifest = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(
        manifest.stats.memory_count, 7,
        "manifest stats must reflect the whole batch"
    );
}

#[tokio::test]
async fn test_init_creates_structure() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();

    // Check project-local directories
    assert!(project_dir.join(".engramdb").exists());
    assert!(project_dir.join(".engramdb/memories").exists());

    // LanceDB should NOT be in the project directory
    assert!(!project_dir.join(".engramdb/lancedb").exists());

    // LanceDB should be in the global data directory
    assert!(paths::lancedb_dir(&store.project_id).unwrap().exists());

    // Check files
    assert!(project_dir.join(".engramdb/manifest.toml").exists());
    assert!(project_dir.join(".engramdb/config.toml").exists());

    // No index.json should be created
    assert!(!project_dir.join(".engramdb/index.json").exists());

    // Check personal directories
    assert!(paths::personal_memories_dir(&store.project_id)
        .unwrap()
        .exists());
}

/// Re-running `init` on an already-initialized project must not clobber
/// a user-customized config.toml and must not drop the embedding
/// fingerprint from the manifest (data-loss regression: `init` used to
/// unconditionally rewrite both files).
#[tokio::test]
async fn test_reinit_preserves_config_and_embedding_fingerprint() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    let registry = InMemoryRegistry::new();

    let store = MemoryStore::init(project_dir, &registry).await.unwrap();

    // Customize the config (valid TOML so re-init's load_config parses it).
    let config_path = project_dir.join(".engramdb/config.toml");
    let custom_config = "# user-customized configuration\n[nli]\nenabled = false\n";
    async_fs::write(&config_path, custom_config).await.unwrap();

    // Stamp an embedding fingerprint, as a successful (re)embed would.
    let fingerprint = manifest::EmbeddingFingerprint {
        model: "onnx/all-MiniLM-L6-v2-q".to_string(),
        dimensions: 384,
        composition: None,
    };
    store
        .set_embedding_fingerprint(fingerprint.clone())
        .await
        .unwrap();

    // Re-running init must preserve both files byte-for-byte semantics.
    MemoryStore::init(project_dir, &registry).await.unwrap();

    let config_after = async_fs::read_to_string(&config_path).await.unwrap();
    assert_eq!(
        config_after, custom_config,
        "re-init clobbered the user's config.toml"
    );

    let manifest_path = project_dir.join(".engramdb/manifest.toml");
    let manifest_after = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(
        manifest_after.embedding,
        Some(fingerprint),
        "re-init dropped the embedding fingerprint (store flipped to Untracked)"
    );
}

/// Re-running `init` on a store with existing memories must keep the
/// memories readable and preserve manifest stats / created_at.
#[tokio::test]
async fn test_reinit_preserves_memories_and_manifest_metadata() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    let registry = InMemoryRegistry::new();

    let store = MemoryStore::init(project_dir, &registry).await.unwrap();
    let memory = create_test_memory("reinit-keep-123", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let manifest_path = project_dir.join(".engramdb/manifest.toml");
    let before = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(before.stats.memory_count, 1);

    let store2 = MemoryStore::init(project_dir, &registry).await.unwrap();

    // Memory is still readable through the re-initialized store.
    let retrieved = store2.get("reinit-keep-123").await.unwrap();
    assert_eq!(retrieved.summary, "Test summary");

    let after = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(
        after.created_at, before.created_at,
        "re-init reset manifest created_at"
    );
    assert_eq!(
        after.stats.memory_count, before.stats.memory_count,
        "re-init reset manifest stats"
    );
    assert_eq!(after.project, before.project);
}

#[tokio::test]
async fn test_open_uninitialized() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let result = MemoryStore::open(project_dir).await;
    assert!(result.is_err());
    match result {
        Err(StorageError::NotInitialized) => {}
        _ => panic!("Expected NotInitialized error"),
    }
}

#[tokio::test]
async fn test_create_and_get() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("test-create-123", Visibility::Shared);

    let created_id = store.create(&memory).await.unwrap();
    assert_eq!(created_id, "test-create-123");

    let retrieved = store.get("test-create-123").await.unwrap();
    assert_eq!(retrieved.id, "test-create-123");
    assert_eq!(retrieved.summary, "Test summary");
    assert_eq!(retrieved.content, "Test content");
}

#[tokio::test]
async fn test_get_prefix_matching() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("abcd1234-5678-90ab-cdef-1234567890ab", Visibility::Shared);

    store.create(&memory).await.unwrap();

    let retrieved = store.get("abcd1234").await.unwrap();
    assert_eq!(retrieved.id, "abcd1234-5678-90ab-cdef-1234567890ab");

    let retrieved = store.get("abcd").await.unwrap();
    assert_eq!(retrieved.id, "abcd1234-5678-90ab-cdef-1234567890ab");
}

#[tokio::test]
async fn test_get_ambiguous_prefix() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();

    let memory1 = create_test_memory("aaaa1111-0000-0000-0000-000000000001", Visibility::Shared);
    let memory2 = create_test_memory("aaaa2222-0000-0000-0000-000000000002", Visibility::Shared);

    store.create(&memory1).await.unwrap();
    store.create(&memory2).await.unwrap();

    assert!(store
        .get("aaaa1111-0000-0000-0000-000000000001")
        .await
        .is_ok());
    assert!(store
        .get("aaaa2222-0000-0000-0000-000000000002")
        .await
        .is_ok());
    assert!(store.get("aaaa1111").await.is_ok());
    assert!(store.get("aaaa2222").await.is_ok());

    let result = store.get("aaaa").await;
    assert!(result.is_err());
    match result {
        Err(e @ StorageError::AmbiguousId { .. }) => {
            assert!(matches!(
                &e,
                StorageError::AmbiguousId { prefix, matches: 2 } if prefix == "aaaa"
            ));
            assert_eq!(
                e.to_string(),
                "Ambiguous ID prefix 'aaaa': matches 2 memories"
            );
        }
        other => panic!(
            "Expected AmbiguousId error for ambiguous prefix, got: {:?}",
            other
        ),
    }
    // The index-side resolver types it the same way.
    assert!(matches!(
        store.resolve_id("aaaa").await,
        Err(StorageError::AmbiguousId { matches: 2, .. })
    ));
}

#[tokio::test]
async fn test_get_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();

    let result = store.get("nonexistent-id").await;
    assert!(result.is_err());
    match result {
        Err(StorageError::NotFound(id)) => {
            assert_eq!(id, "nonexistent-id");
        }
        _ => panic!("Expected NotFound error"),
    }
}

#[tokio::test]
async fn test_update_modifies_memory() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("test-update-123", Visibility::Shared);

    store.create(&memory).await.unwrap();

    let mut update = MemoryUpdate::new();
    update.summary = Some("Updated summary".to_string());

    store.update("test-update-123", update).await.unwrap();

    let retrieved = store.get("test-update-123").await.unwrap();
    assert_eq!(retrieved.summary, "Updated summary");
    assert_eq!(retrieved.content, "Test content");
}

/// Set a file's mtime `secs` seconds into the past, so newest-wins
/// duplicate resolution has an unambiguous ordering.
fn age_file(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
        .unwrap();
}

/// All files in `dir` whose stem matches `id` (same matching rule the
/// store uses).
fn files_matching_id(dir: &Path, id: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path();
            let stem = path.file_stem()?.to_str()?.to_owned();
            memory_file::stem_matches_id_prefix(&stem, id).then_some(path)
        })
        .collect()
}

/// Issue-1 contract (a): an update that does not change the filename must
/// replace the file in place — the old path still exists afterwards and
/// no second file appears (the old delete-then-write order made the file
/// briefly absent, so racing lock-free readers saw spurious NotFound).
#[tokio::test]
async fn test_update_same_filename_replaces_in_place() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("inplace-update-123", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let dir = paths::memories_dir(&store.project_dir);
    let path = dir.join("inplace-update-123.md");
    assert!(path.exists(), "file must exist after create");

    let mut update = MemoryUpdate::new();
    update.content = Some("New content".to_string());
    store.update("inplace-update-123", update).await.unwrap();

    assert!(
        path.exists(),
        "same-filename update must keep the file at its original path"
    );
    assert_eq!(
        files_matching_id(&dir, "inplace-update-123").len(),
        1,
        "exactly one file for the id"
    );
    let retrieved = store.get("inplace-update-123").await.unwrap();
    assert_eq!(retrieved.content, "New content");
}

/// Issue-1 contract (b): a title-changing update renames the file —
/// afterwards exactly one file exists for the id and `get` returns the
/// new state.
#[tokio::test]
async fn test_update_title_change_leaves_single_file() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "aaaa1111-2222-3333-4444-555566667777";
    let mut memory = create_test_memory(id, Visibility::Shared);
    memory.title = Some("First Title".to_string());
    store.create(&memory).await.unwrap();

    let dir = paths::memories_dir(&store.project_dir);
    let old_path = dir.join(format!("first-title_{}.md", id));
    assert!(old_path.exists(), "slugged filename expected after create");

    let mut update = MemoryUpdate::new();
    update.title = Some("Second Title".to_string());
    update.content = Some("Renamed content".to_string());
    store.update(id, update).await.unwrap();

    assert!(!old_path.exists(), "old slug file must be removed");
    let new_path = dir.join(format!("second-title_{}.md", id));
    assert!(new_path.exists(), "new slug file must exist");
    assert_eq!(
        files_matching_id(&dir, id).len(),
        1,
        "exactly one file for the id after a rename-update"
    );

    let retrieved = store.get(id).await.unwrap();
    assert_eq!(retrieved.title.as_deref(), Some("Second Title"));
    assert_eq!(retrieved.content, "Renamed content");
}

/// Issue-1 contract (c): simulate the crash window of a rename-update —
/// BOTH the new and the stale old file exist for one id. Readers must
/// resolve to the newest file (not error as "ambiguous"), and `delete`
/// must sweep both files.
#[tokio::test]
async fn test_stale_duplicate_resolves_to_newest_and_delete_sweeps_both() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "cccc1111-2222-3333-4444-555566667777";
    let mut memory = create_test_memory(id, Visibility::Shared);
    memory.title = Some("New Title".to_string());
    memory.content = "Current content".to_string();
    store.create(&memory).await.unwrap();

    // Manually create the stale pre-rename file (old title, old content)
    // with an unambiguously older mtime.
    let dir = paths::memories_dir(&store.project_dir);
    let mut stale = memory.clone();
    stale.title = Some("Old Title".to_string());
    stale.content = "Stale content".to_string();
    let stale_path = dir.join(memory_file::memory_filename(&stale));
    std::fs::write(&stale_path, memory_file::write_memory_file(&stale).unwrap()).unwrap();
    age_file(&stale_path, 3600);
    assert_eq!(files_matching_id(&dir, id).len(), 2);

    // get resolves to the newest file instead of failing as ambiguous.
    let got = store.get(id).await.unwrap();
    assert_eq!(got.content, "Current content");

    // get_batch resolves the duplicate the same way.
    let batch = store.get_batch(&[id]).await.unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].1.content, "Current content");

    // delete removes BOTH files — no orphan left behind.
    store.delete(id).await.unwrap();
    assert!(files_matching_id(&dir, id).is_empty());
    assert!(store.get(id).await.is_err());
}

/// A subsequent update also cleans up a stale duplicate left by a
/// crashed rename-update.
#[tokio::test]
async fn test_update_cleans_up_stale_duplicate() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "dddd1111-2222-3333-4444-555566667777";
    let mut memory = create_test_memory(id, Visibility::Shared);
    memory.title = Some("New Title".to_string());
    store.create(&memory).await.unwrap();

    let dir = paths::memories_dir(&store.project_dir);
    let mut stale = memory.clone();
    stale.title = Some("Old Title".to_string());
    let stale_path = dir.join(memory_file::memory_filename(&stale));
    std::fs::write(&stale_path, memory_file::write_memory_file(&stale).unwrap()).unwrap();
    age_file(&stale_path, 3600);
    assert_eq!(files_matching_id(&dir, id).len(), 2);

    let mut update = MemoryUpdate::new();
    update.content = Some("Post-crash content".to_string());
    store.update(id, update).await.unwrap();

    assert_eq!(
        files_matching_id(&dir, id).len(),
        1,
        "update must sweep the stale duplicate"
    );
    assert!(!stale_path.exists());
    let got = store.get(id).await.unwrap();
    assert_eq!(got.content, "Post-crash content");
}

/// Reindex with a stale duplicate on disk: the newest file wins (its
/// content is what gets indexed), the duplicate counts once, and the
/// stale file is removed (reindex is the repair path).
#[tokio::test]
async fn test_reindex_dedupes_and_removes_stale_duplicate() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "eeee1111-2222-3333-4444-555566667777";
    let mut memory = create_test_memory(id, Visibility::Shared);
    memory.title = Some("New Title".to_string());
    memory.summary = "Current summary".to_string();
    store.create(&memory).await.unwrap();

    let dir = paths::memories_dir(&store.project_dir);
    let mut stale = memory.clone();
    stale.title = Some("Old Title".to_string());
    stale.summary = "Stale summary".to_string();
    let stale_path = dir.join(memory_file::memory_filename(&stale));
    std::fs::write(&stale_path, memory_file::write_memory_file(&stale).unwrap()).unwrap();
    age_file(&stale_path, 3600);

    let count = store.reindex().await.unwrap();
    assert_eq!(count, 1, "duplicate files for one id must index once");
    assert!(
        !stale_path.exists(),
        "reindex must remove the stale duplicate"
    );
    assert_eq!(files_matching_id(&dir, id).len(), 1);

    let got = store.get(id).await.unwrap();
    assert_eq!(got.summary, "Current summary");
    assert_eq!(store.list_ids().await.unwrap(), vec![id.to_string()]);
}

/// The first-embed stamp decides "unstamped" before its embed and stamps
/// after it; a fingerprint written in between (a reindex for a new
/// model) must survive. `set_embedding_fingerprint` overwrote it, which
/// flaked `server::tests::reindex_re_embeds_in_error_mode_despite_mismatch`.
#[tokio::test]
async fn test_stamp_embedding_fingerprint_if_unset_keeps_existing_stamp() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let fp = |model: &str| manifest::EmbeddingFingerprint {
        model: model.to_string(),
        dimensions: 384,
        composition: None,
    };

    assert_eq!(store.embedding_fingerprint().await.unwrap(), None);
    assert!(store
        .stamp_embedding_fingerprint_if_unset(fp("onnx/first"))
        .await
        .unwrap());
    assert_eq!(
        store.embedding_fingerprint().await.unwrap(),
        Some(fp("onnx/first"))
    );

    store
        .set_embedding_fingerprint(fp("onnx/reindexed"))
        .await
        .unwrap();
    assert!(!store
        .stamp_embedding_fingerprint_if_unset(fp("onnx/late-ingest"))
        .await
        .unwrap());
    assert_eq!(
        store.embedding_fingerprint().await.unwrap(),
        Some(fp("onnx/reindexed")),
        "a late first-embed stamp must not overwrite a newer fingerprint"
    );
}

/// Issue-3 regression: `set_embedding_fingerprint` racing concurrent
/// `create`s must not lose the fingerprint. Both paths do a
/// load-modify-save of manifest.toml; without the write lock a create
/// could load the pre-stamp manifest and save over the fingerprint.
#[tokio::test]
async fn test_set_embedding_fingerprint_survives_concurrent_creates() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let fingerprint = manifest::EmbeddingFingerprint {
        model: "onnx/race-test-model".to_string(),
        dimensions: 384,
        composition: None,
    };

    let mut tasks = Vec::new();
    for i in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            let mem = create_test_memory(&format!("fp-race-{}", i), Visibility::Shared);
            store.create(&mem).await.unwrap();
        }));
    }
    let stamp = {
        let store = store.clone();
        let fingerprint = fingerprint.clone();
        tokio::spawn(async move {
            store.set_embedding_fingerprint(fingerprint).await.unwrap();
        })
    };

    for task in tasks {
        task.await.unwrap();
    }
    stamp.await.unwrap();

    assert_eq!(
        store.embedding_fingerprint().await.unwrap(),
        Some(fingerprint),
        "a concurrent create clobbered the fingerprint (manifest RMW race)"
    );
    assert_eq!(store.count().await.unwrap(), 8, "all creates must land");
}

/// The core atomicity property of `update_with`: N concurrent
/// read-modify-write updates must all survive. Each task appends one
/// distinct tag inside the closure; because the memory is re-read under
/// the per-project write lock, no task can snapshot stale state and
/// overwrite another task's tag. (The old unlocked get-merge-update flow
/// would lose tags here.)
#[tokio::test]
async fn test_update_with_serializes_concurrent_updates() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("test-update-with-race", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .update_with("test-update-with-race", move |m| {
                        m.tags.push(format!("tag-{}", i));
                        Ok(())
                    })
                    .await
                    .unwrap();
            })
        })
        .collect();
    for task in tasks {
        task.await.unwrap();
    }

    let final_memory = store.get("test-update-with-race").await.unwrap();
    for i in 0..8 {
        assert!(
            final_memory.tags.contains(&format!("tag-{}", i)),
            "tag-{} was lost; tags = {:?} — update_with did not serialize",
            i,
            final_memory.tags
        );
    }
    assert_eq!(final_memory.tags.len(), 8);
}

/// `update_with` returns the memory exactly as persisted (closure applied,
/// `updated_at` bumped), and the same state is readable back from disk.
#[tokio::test]
async fn test_update_with_returns_persisted_memory() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("test-update-with-ret", Visibility::Shared);
    let created_at = memory.updated_at;
    store.create(&memory).await.unwrap();

    let returned = store
        .update_with("test-update-with-ret", |m| {
            m.summary = "Closure summary".to_string();
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(returned.summary, "Closure summary");
    assert!(returned.updated_at >= created_at);

    let reloaded = store.get("test-update-with-ret").await.unwrap();
    assert_eq!(reloaded.summary, "Closure summary");
    assert_eq!(reloaded.updated_at, returned.updated_at);
}

/// A failing closure must abort the update: nothing is written and the
/// error surfaces to the caller.
#[tokio::test]
async fn test_update_with_closure_error_writes_nothing() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("test-update-with-err", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let result = store
        .update_with("test-update-with-err", |m| {
            m.summary = "Should not persist".to_string();
            anyhow::bail!("merge failed")
        })
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("merge failed"));

    let reloaded = store.get("test-update-with-err").await.unwrap();
    assert_eq!(reloaded.summary, "Test summary");
}

/// `update_batch_with` is documented to match `update_with` per entry, and
/// `update_with` resolves a prefix id. The batched read keys off the exact
/// id, so a prefix used to fall straight through to "memory not found" —
/// silently, and only for the batched callers.
#[tokio::test]
async fn update_batch_with_resolves_a_prefix_id() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("abcdef012345", Visibility::Shared))
        .await
        .unwrap();

    let (updated, errors) = store
        .update_batch_with(&["abcdef".to_string()], |m| {
            m.summary = "Reached by prefix".to_string();
            Ok(())
        })
        .await
        .unwrap();

    assert!(errors.is_empty(), "prefix must resolve, got {errors:?}");
    assert_eq!(
        updated,
        vec!["abcdef012345".to_string()],
        "the report names the canonical id, not the prefix the caller passed"
    );
    assert_eq!(
        store.get("abcdef012345").await.unwrap().summary,
        "Reached by prefix"
    );
}

/// Two spellings of one memory in one batch must not apply the closure
/// twice — the closure is the caller's mutation, and running it on a
/// stale copy of the same memory would let the second write erase the
/// first.
#[tokio::test]
async fn update_batch_with_applies_a_duplicated_id_once() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("dupdupdup1234", Visibility::Shared))
        .await
        .unwrap();

    let mut calls = 0usize;
    let (updated, errors) = store
        .update_batch_with(&["dupdupdup1234".to_string(), "dupdup".to_string()], |m| {
            calls += 1;
            m.tags.push(format!("call-{calls}"));
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(calls, 1, "the closure must see each memory exactly once");
    assert_eq!(updated, vec!["dupdupdup1234".to_string()]);
    assert_eq!(
        errors.len(),
        1,
        "the second spelling has nothing left to do"
    );
    assert_eq!(
        store.get("dupdupdup1234").await.unwrap().tags,
        vec!["call-1"]
    );
}

/// An id that resolves and one that does not, in the same batch: the good
/// one is applied and the bad one is reported under **the spelling the
/// caller passed**, which is the only string it can act on.
#[tokio::test]
async fn update_batch_with_reports_an_unresolvable_id_by_the_caller_s_spelling() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory("realmemory0001", Visibility::Shared))
        .await
        .unwrap();

    let (updated, errors) = store
        .update_batch_with(&["realme".to_string(), "no-such-memory".to_string()], |m| {
            m.summary = "touched".to_string();
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(updated, vec!["realmemory0001".to_string()]);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "no-such-memory");
}

#[tokio::test]
async fn test_delete_removes_memory() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("test-delete-123", Visibility::Shared);

    store.create(&memory).await.unwrap();
    assert!(store.get("test-delete-123").await.is_ok());

    store.delete("test-delete-123").await.unwrap();

    let result = store.get("test-delete-123").await;
    assert!(result.is_err());
    match result {
        Err(StorageError::NotFound(_)) => {}
        _ => panic!("Expected NotFound error after delete"),
    }
}

#[tokio::test]
async fn test_delete_if_predicate_true_deletes() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("delete-if-true-123", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let deleted = store
        .delete_if("delete-if-true-123", |m| m.summary == "Test summary")
        .await
        .unwrap();
    assert!(deleted, "predicate true must delete and report true");
    assert!(matches!(
        store.get("delete-if-true-123").await,
        Err(StorageError::NotFound(_))
    ));
}

#[tokio::test]
async fn test_delete_if_predicate_false_keeps_memory() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("delete-if-false-123", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let deleted = store
        .delete_if("delete-if-false-123", |_| false)
        .await
        .unwrap();
    assert!(!deleted, "predicate false must keep the memory");
    assert!(store.get("delete-if-false-123").await.is_ok());
}

/// The predicate sees the LATEST persisted state, not whatever the caller
/// read before acquiring the lock — that re-read is the whole point.
#[tokio::test]
async fn test_delete_if_predicate_sees_fresh_state() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("delete-if-fresh-123", Visibility::Shared);
    store.create(&memory).await.unwrap();

    // Concurrent writer bumps criticality after the (hypothetical)
    // earlier unlocked read.
    let mut update = MemoryUpdate::new();
    update.criticality = Some(0.95);
    store.update("delete-if-fresh-123", update).await.unwrap();

    let deleted = store
        .delete_if("delete-if-fresh-123", |m| m.criticality < 0.9)
        .await
        .unwrap();
    assert!(!deleted, "predicate must judge the updated criticality");
    assert!(store.get("delete-if-fresh-123").await.is_ok());
}

#[tokio::test]
async fn test_delete_if_missing_id_is_skip_not_error() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let deleted = store
        .delete_if("never-existed-123", |_| {
            panic!("predicate must not run for a missing memory")
        })
        .await
        .unwrap();
    assert!(!deleted, "missing id must be Ok(false), not an error");
}

/// Stale index entry (row in LanceDB, data file gone) is also a skip.
#[tokio::test]
async fn test_delete_if_stale_index_entry_is_skip() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("delete-if-stale-123", Visibility::Shared);
    store.create(&memory).await.unwrap();

    // Remove the .md file but leave the index row.
    let memories_dir = paths::memories_dir(temp_dir.path());
    for entry in std::fs::read_dir(&memories_dir).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }

    let deleted = store
        .delete_if("delete-if-stale-123", |_| true)
        .await
        .unwrap();
    assert!(!deleted, "stale index entry must be skipped, not an error");
}

#[tokio::test]
async fn test_list_returns_all() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();

    let memory1 = create_test_memory("list-test-1", Visibility::Shared);
    let memory2 = create_test_memory("list-test-2", Visibility::Shared);
    let memory3 = create_test_memory("list-test-3", Visibility::Personal);

    store.create(&memory1).await.unwrap();
    store.create(&memory2).await.unwrap();
    store.create(&memory3).await.unwrap();

    let ids = store.list_ids().await.unwrap();
    assert_eq!(ids.len(), 3);

    assert!(ids.contains(&"list-test-1".to_string()));
    assert!(ids.contains(&"list-test-2".to_string()));
    assert!(ids.contains(&"list-test-3".to_string()));
}

#[tokio::test]
async fn test_reindex_rebuilds_from_files() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();

    let memory1 = create_test_memory("reindex-test-1", Visibility::Shared);
    let memory2 = create_test_memory("reindex-test-2", Visibility::Shared);

    store.create(&memory1).await.unwrap();
    store.create(&memory2).await.unwrap();

    assert_eq!(store.count().await.unwrap(), 2);

    // Clear LanceDB to simulate corruption
    store.lance_index.clear().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);

    // Reindex
    let count = store.reindex().await.unwrap();
    assert_eq!(count, 2);

    let ids = store.list_ids().await.unwrap();
    assert_eq!(ids.len(), 2);

    assert!(ids.contains(&"reindex-test-1".to_string()));
    assert!(ids.contains(&"reindex-test-2".to_string()));
}

/// Regression test for the reindex data-loss bug: rebuilding metadata
/// from the markdown files must NOT destroy existing embedding vectors.
/// Chunks for memories that no longer exist on disk are pruned, keeping
/// the chunks table consistent without losing live vectors.
#[tokio::test]
async fn test_reindex_preserves_chunks_and_prunes_orphans() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let memory = create_test_memory("reindex-chunks-live", Visibility::Shared);
    store.create(&memory).await.unwrap();
    store
        .upsert_chunks(
            "reindex-chunks-live",
            vec![vec![0.25f32; 384], vec![0.5f32; 384]],
        )
        .await
        .unwrap();

    // Chunks for a memory with no file on disk (deleted out-of-band).
    store
        .upsert_chunks("reindex-chunks-ghost", vec![vec![0.75f32; 384]])
        .await
        .unwrap();

    let count = store.reindex().await.unwrap();
    assert_eq!(count, 1);

    // The live memory's vectors must survive a metadata-only reindex.
    let chunks = store.export_chunks("reindex-chunks-live").await.unwrap();
    assert_eq!(chunks.len(), 2, "vectors must survive reindex");
    assert_eq!(chunks[0], vec![0.25f32; 384]);

    // Orphaned chunks must be pruned so the table stays consistent.
    assert_eq!(
        store.list_chunk_memory_ids().await.unwrap(),
        vec!["reindex-chunks-live".to_string()],
        "ghost chunks must be pruned, live chunks kept"
    );
}

#[tokio::test]
async fn clear_chunks_keeping_spares_only_the_named_ids() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let ids = ["keep-a", "keep-b", "drop-c", "drop-d"];
    for id in ids {
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
        store
            .upsert_chunks(id, vec![vec![0.5f32; 384]])
            .await
            .unwrap();
    }
    store
        .clear_chunks_keeping(&["keep-a".to_string(), "keep-b".to_string()])
        .await
        .unwrap();
    let mut left = store.list_chunk_memory_ids().await.unwrap();
    left.sort();
    assert_eq!(left, vec!["keep-a".to_string(), "keep-b".to_string()]);
    let flags: HashMap<String, bool> = store
        .list_for_filtering()
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.id, e.has_embedding))
        .collect();
    assert!(flags["keep-a"] && flags["keep-b"]);
    assert!(!flags["drop-c"] && !flags["drop-d"]);
}

/// The freshness-guarded upsert used by detached ingest tasks: a write
/// the predicate refuses or a deleted memory must not write chunks; one it
/// accepts must, and the predicate sees the memory re-read under the lock.
#[tokio::test]
async fn upsert_chunks_if_writes_when_predicate_holds_and_skips_deleted() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let memory = create_test_memory("guarded-chunks", Visibility::Shared);
    store.create(&memory).await.unwrap();

    // Predicate holds: written.
    let written = store
        .upsert_chunks_if("guarded-chunks", vec![vec![0.1f32; 384]], None, |m| {
            m.summary == memory.summary
        })
        .await
        .unwrap();
    assert!(written);

    // The memory moves on; the predicate sees the newer state...
    store
        .update_with("guarded-chunks", |m| {
            m.summary = "v2".to_string();
            Ok(())
        })
        .await
        .unwrap();
    let written = store
        .upsert_chunks_if("guarded-chunks", vec![vec![0.9f32; 384]], None, |m| {
            m.summary == "v2"
        })
        .await
        .unwrap();
    assert!(written);

    // ...so the OLD snapshot's late-arriving vectors are refused.
    let old_summary = memory.summary.clone();
    let written = store
        .upsert_chunks_if("guarded-chunks", vec![vec![0.1f32; 384]], None, move |m| {
            m.summary == old_summary
        })
        .await
        .unwrap();
    assert!(!written, "stale snapshot must not overwrite newer vectors");
    let chunks = store.export_chunks("guarded-chunks").await.unwrap();
    assert!(
        (chunks[0][0] - 0.9).abs() < f32::EPSILON,
        "newer vectors survive"
    );

    // Deleted memory: a late embed must not re-insert orphan chunks, even
    // with a predicate that would accept anything.
    store.delete("guarded-chunks").await.unwrap();
    let written = store
        .upsert_chunks_if("guarded-chunks", vec![vec![0.5f32; 384]], None, |_| true)
        .await
        .unwrap();
    assert!(!written, "deleted memory must not get orphan chunks");
    assert!(store
        .list_chunk_memory_ids()
        .await
        .unwrap()
        .iter()
        .all(|id| id != "guarded-chunks"));
}

#[tokio::test]
async fn test_list_includes_visibility() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();

    let shared = create_test_memory("vis-shared", Visibility::Shared);
    let personal = create_test_memory("vis-personal", Visibility::Personal);

    store.create(&shared).await.unwrap();
    store.create(&personal).await.unwrap();

    let entries = store.list_filterable().await.unwrap();
    let shared_entry = entries.iter().find(|e| e.id == "vis-shared").unwrap();
    let personal_entry = entries.iter().find(|e| e.id == "vis-personal").unwrap();

    assert_eq!(shared_entry.visibility, Visibility::Shared);
    assert_eq!(personal_entry.visibility, Visibility::Personal);
}

#[tokio::test]
async fn test_check_staleness_in_sync() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("staleness-sync-1", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let result = store
        .check_staleness(&IndexConfig::default())
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "Expected no staleness warning when in sync"
    );
}

#[tokio::test]
async fn test_check_staleness_detects_mismatch() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
        .await
        .unwrap();
    let memory = create_test_memory("staleness-mismatch-1", Visibility::Shared);
    store.create(&memory).await.unwrap();

    // Delete LanceDB entry but leave the .md file
    store.lance_index.clear().await.unwrap();

    let result = store
        .check_staleness(&IndexConfig::default())
        .await
        .unwrap();
    assert!(
        result.is_some(),
        "Expected staleness warning after clearing index"
    );
    let warning = result.unwrap();
    // Id-level detail replaces the bare counts: naming the memory that is
    // missing says strictly more than "1 on disk, 0 indexed", and saying
    // both says one thing twice.
    assert!(warning.contains("1 memory not indexed"), "{warning}");
    assert!(warning.contains("engramdb reindex"), "{warning}");
}

// --- get_batch tests ---

#[tokio::test]
async fn test_get_batch_returns_all() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut ids = Vec::new();
    for i in 0..5 {
        let mem = Memory::new(
            MemoryType::Decision,
            format!("Summary {}", i),
            format!("Content {}", i),
            Provenance::human(),
        );
        ids.push(store.create(&mem).await.unwrap());
    }

    let results = store.get_batch(&ids).await.unwrap();
    assert_eq!(results.len(), 5);
    for (id, _mem) in &results {
        assert!(ids.contains(id));
    }
}

#[tokio::test]
async fn test_get_batch_skips_missing() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut ids = Vec::new();
    for i in 0..3 {
        let mem = Memory::new(
            MemoryType::Decision,
            format!("Summary {}", i),
            format!("Content {}", i),
            Provenance::human(),
        );
        ids.push(store.create(&mem).await.unwrap());
    }
    ids.push("fake-id-1".to_string());
    ids.push("fake-id-2".to_string());

    let results = store.get_batch(&ids).await.unwrap();
    assert_eq!(results.len(), 3);
}

// --- batch_exists tests ---

#[tokio::test]
async fn test_batch_exists_all_present() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut ids = Vec::new();
    for i in 0..5 {
        let mem = Memory::new(
            MemoryType::Decision,
            format!("Summary {}", i),
            format!("Content {}", i),
            Provenance::human(),
        );
        ids.push(store.create(&mem).await.unwrap());
    }

    let existing = store.batch_exists(&ids).await.unwrap();
    assert_eq!(existing.len(), 5);
    for id in &ids {
        assert!(existing.contains(id));
    }
}

#[tokio::test]
async fn test_batch_exists_some_missing() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut ids = Vec::new();
    for i in 0..3 {
        let mem = Memory::new(
            MemoryType::Decision,
            format!("Summary {}", i),
            format!("Content {}", i),
            Provenance::human(),
        );
        ids.push(store.create(&mem).await.unwrap());
    }
    ids.push("fake-id-1".to_string());
    ids.push("fake-id-2".to_string());

    let existing = store.batch_exists(&ids).await.unwrap();
    assert_eq!(existing.len(), 3);
    assert!(!existing.contains("fake-id-1"));
    assert!(!existing.contains("fake-id-2"));
}

// --- Second-clone (shared project ID) guard tests ---

/// Create a fake git clone with a fixed remote URL so two directories
/// compute the same (remote-derived) project ID. Each test uses a
/// distinct `remote` so per-test global state never collides.
fn make_clone(root: &Path, name: &str, remote: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(
        dir.join(".git").join("config"),
        format!(
            "[remote \"origin\"]\n\turl = https://github.com/example/{}.git\n",
            remote
        ),
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn test_second_clone_detects_checkout_conflict() {
    let tmp = TempDir::new().unwrap();
    let a = make_clone(tmp.path(), "clone-a", "conflict-detect");
    let b = make_clone(tmp.path(), "clone-b", "conflict-detect");
    // `checkout_conflict` consults the global file registry (redirected to
    // a per-process temp dir by the test-isolation arm), so init through
    // it rather than an InMemoryRegistry.
    let registry = crate::registry::FileRegistry::global().unwrap();

    let store_a = MemoryStore::init(&a, &registry).await.unwrap();
    let store_b = MemoryStore::init(&b, &registry).await.unwrap();
    assert_eq!(
        store_a.project_id, store_b.project_id,
        "clones of one remote share a project ID by design"
    );

    assert_eq!(
        store_a.checkout_conflict().await,
        None,
        "the registered owner sees no conflict"
    );
    assert_eq!(
        store_b.checkout_conflict().await,
        Some(a.canonicalize().unwrap()),
        "the second clone must detect the registered checkout"
    );

    // The conflict is equally detectable from a plain `open`.
    let reopened_b = MemoryStore::open(&b).await.unwrap();
    assert_eq!(
        reopened_b.checkout_conflict().await,
        Some(a.canonicalize().unwrap())
    );
}

#[tokio::test]
async fn test_second_clone_init_does_not_steal_registration_until_owner_gone() {
    let tmp = TempDir::new().unwrap();
    let a = make_clone(tmp.path(), "clone-a", "conflict-repoint");
    let b = make_clone(tmp.path(), "clone-b", "conflict-repoint");
    let registry = crate::registry::FileRegistry::global().unwrap();

    let store_a = MemoryStore::init(&a, &registry).await.unwrap();
    MemoryStore::init(&b, &registry).await.unwrap();

    let loaded = registry.load().await.unwrap();
    let entry = loaded
        .projects
        .iter()
        .find(|e| e.project_id == store_a.project_id)
        .expect("project registered");
    assert_eq!(
        PathBuf::from(&entry.project_path),
        a.canonicalize().unwrap(),
        "registry must keep the first checkout while it exists"
    );

    // The first checkout disappears (deleted / moved clone): the next
    // init from B legitimately takes over the registration.
    async_fs::remove_dir_all(&a).await.unwrap();
    let store_b = MemoryStore::init(&b, &registry).await.unwrap();

    let loaded = registry.load().await.unwrap();
    let entry = loaded
        .projects
        .iter()
        .find(|e| e.project_id == store_b.project_id)
        .expect("project registered");
    assert_eq!(
        PathBuf::from(&entry.project_path),
        b.canonicalize().unwrap(),
        "registry must self-heal once the old checkout is gone"
    );
    assert_eq!(store_b.checkout_conflict().await, None);
}

/// CRITICAL data-loss guard: a reindex run from the second clone shares
/// the LanceDB index with the first clone but only sees its own memory
/// files — it must NOT clear the other clone's index rows or prune its
/// vectors as "orphans".
#[tokio::test]
async fn test_reindex_from_second_clone_preserves_other_checkouts_data() {
    let tmp = TempDir::new().unwrap();
    let a = make_clone(tmp.path(), "clone-a", "conflict-reindex");
    let b = make_clone(tmp.path(), "clone-b", "conflict-reindex");
    let registry = crate::registry::FileRegistry::global().unwrap();

    let store_a = MemoryStore::init(&a, &registry).await.unwrap();
    let store_b = MemoryStore::init(&b, &registry).await.unwrap();

    // Clone A's memory file lives only in A's tree; its index row and
    // vector live in the shared LanceDB table.
    let mem_a = create_test_memory("clone-a-mem", Visibility::Shared);
    store_a.create(&mem_a).await.unwrap();
    store_a
        .upsert_chunks("clone-a-mem", vec![vec![0.25f32; 384]])
        .await
        .unwrap();

    let mem_b = create_test_memory("clone-b-mem", Visibility::Shared);
    store_b.create(&mem_b).await.unwrap();

    let count = store_b.reindex().await.unwrap();
    assert_eq!(count, 1, "only this checkout's files are scanned");

    let ids = store_b.list_ids().await.unwrap();
    assert!(
        ids.contains(&"clone-a-mem".to_string()),
        "the other clone's index row must survive a guarded reindex"
    );
    assert!(ids.contains(&"clone-b-mem".to_string()));

    let chunks = store_b.export_chunks("clone-a-mem").await.unwrap();
    assert_eq!(
        chunks.len(),
        1,
        "the other clone's vectors must not be pruned as orphans"
    );
    assert_eq!(chunks[0], vec![0.25f32; 384]);
}

/// Control: once the conflicting checkout is gone (and the registry has
/// self-healed), reindex is destructive again — stale rows and orphan
/// chunks from the departed clone are cleaned up.
#[tokio::test]
async fn test_reindex_becomes_destructive_again_once_other_clone_gone() {
    let tmp = TempDir::new().unwrap();
    let a = make_clone(tmp.path(), "clone-a", "conflict-cleanup");
    let b = make_clone(tmp.path(), "clone-b", "conflict-cleanup");
    let registry = crate::registry::FileRegistry::global().unwrap();

    let store_a = MemoryStore::init(&a, &registry).await.unwrap();
    let store_b = MemoryStore::init(&b, &registry).await.unwrap();

    let mem_a = create_test_memory("clone-a-mem", Visibility::Shared);
    store_a.create(&mem_a).await.unwrap();
    store_a
        .upsert_chunks("clone-a-mem", vec![vec![0.25f32; 384]])
        .await
        .unwrap();
    let mem_b = create_test_memory("clone-b-mem", Visibility::Shared);
    store_b.create(&mem_b).await.unwrap();

    // Clone A disappears; re-init from B takes over the registration.
    async_fs::remove_dir_all(&a).await.unwrap();
    let store_b = MemoryStore::init(&b, &registry).await.unwrap();
    assert_eq!(store_b.checkout_conflict().await, None);

    let count = store_b.reindex().await.unwrap();
    assert_eq!(count, 1);

    let ids = store_b.list_ids().await.unwrap();
    assert_eq!(ids, vec!["clone-b-mem".to_string()]);
    assert_eq!(
        store_b.list_chunk_memory_ids().await.unwrap(),
        Vec::<String>::new(),
        "the departed clone's orphan chunks are pruned by a full reindex"
    );
}

// --- Global memory store tests ---

/// Handle returned by [`setup_global_store`]. Holds the live
/// `MemoryStore` alongside a process-wide lock guard that serializes
/// global-store tests within a single `cargo test` process. Derefs to
/// `MemoryStore` so existing call sites (`let store = ...;
/// store.create(...)`) keep working without change.
struct GlobalStoreHandle {
    store: MemoryStore,
    _lock: crate::test_support::GlobalTestLock,
}

impl std::ops::Deref for GlobalStoreHandle {
    type Target = MemoryStore;
    fn deref(&self) -> &MemoryStore {
        &self.store
    }
}

/// Initialize a fresh global store for testing.
///
/// Acquires the shared global-test lock and wipes the on-disk global
/// layout before each test, so concurrent `cargo test` runs don't race
/// on LanceDB table creation or leak state across tests.
async fn setup_global_store() -> GlobalStoreHandle {
    let lock = crate::test_support::acquire_global_test_lock().await;
    let store = MemoryStore::init_global().await.unwrap();
    GlobalStoreHandle { store, _lock: lock }
}

#[tokio::test]
async fn test_global_init_creates_structure() {
    let store = setup_global_store().await;

    assert!(store.is_global());
    assert_eq!(store.project_id, paths::GLOBAL_PROJECT_ID);

    let global_dir = paths::global_store_dir().unwrap();
    assert!(global_dir.join(".engramdb").exists());
    assert!(global_dir.join(".engramdb/memories").exists());
    assert!(global_dir.join(".engramdb/manifest.toml").exists());
    assert!(global_dir.join(".engramdb/config.toml").exists());
    assert!(paths::global_lancedb_dir().unwrap().exists());
}

#[tokio::test]
async fn test_global_open_auto_inits() {
    // open_global should auto-init if not present
    let store = setup_global_store().await;
    assert!(store.is_global());
    assert_eq!(store.project_id, paths::GLOBAL_PROJECT_ID);
}

#[tokio::test]
async fn test_global_create_and_get() {
    let store = setup_global_store().await;
    let memory = create_test_memory("global-test-001", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let retrieved = store.get("global-test-001").await.unwrap();
    assert_eq!(retrieved.id, "global-test-001");
    assert_eq!(retrieved.summary, "Test summary");
}

#[tokio::test]
async fn test_global_update() {
    let store = setup_global_store().await;
    let memory = create_test_memory("global-test-update", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let mut update = MemoryUpdate::new();
    update.summary = Some("Updated global summary".to_string());
    store.update("global-test-update", update).await.unwrap();

    let retrieved = store.get("global-test-update").await.unwrap();
    assert_eq!(retrieved.summary, "Updated global summary");
}

#[tokio::test]
async fn test_global_delete() {
    let store = setup_global_store().await;
    let memory = create_test_memory("global-test-delete", Visibility::Shared);
    store.create(&memory).await.unwrap();
    assert!(store.get("global-test-delete").await.is_ok());

    store.delete("global-test-delete").await.unwrap();

    let result = store.get("global-test-delete").await;
    assert!(result.is_err());
    match result {
        Err(StorageError::NotFound(_)) => {}
        _ => panic!("Expected NotFound error after delete"),
    }
}

#[tokio::test]
async fn test_global_reindex() {
    let store = setup_global_store().await;
    let memory = create_test_memory("global-test-reindex", Visibility::Shared);
    store.create(&memory).await.unwrap();

    let count = store.reindex().await.unwrap();
    assert!(count >= 1);

    // Memory should still be accessible after reindex
    let retrieved = store.get("global-test-reindex").await.unwrap();
    assert_eq!(retrieved.id, "global-test-reindex");
}

#[tokio::test]
async fn test_global_isolation_from_project() {
    // Global store should NOT contain project memories and vice versa.
    // Hold the global-test lock for the whole test; otherwise a parallel
    // global test can mutate `global_store`'s on-disk state mid-assertion.
    let _lock = crate::test_support::acquire_global_test_lock().await;
    let temp_dir = TempDir::new().unwrap();
    let project_store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let global_store = MemoryStore::init_global().await.unwrap();

    let project_mem = create_test_memory("project-only-mem", Visibility::Shared);
    project_store.create(&project_mem).await.unwrap();

    let global_mem = create_test_memory("global-only-mem", Visibility::Shared);
    global_store.create(&global_mem).await.unwrap();

    // Global store should NOT have the project memory
    assert!(global_store.get("project-only-mem").await.is_err());

    // Project store should NOT have the global memory
    assert!(project_store.get("global-only-mem").await.is_err());
}

#[tokio::test]
async fn test_global_is_not_global_for_regular_store() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    assert!(!store.is_global());
}

// ---- group stores (generalization of the global store) ----

#[tokio::test]
async fn test_group_init_open_roundtrip() {
    // Group stores live under `<ENGRAMDB_DATA_DIR>/groups/<id>/`, which the
    // ctor test-isolation arm redirects to a per-process temp dir — no
    // global-test lock needed (unlike the global store, whose fixed path is
    // shared across the process). A distinct group id per test name keeps
    // parallel group tests from colliding even within the process.
    let group_id = paths::compute_group_id("test-group-roundtrip");
    let store = MemoryStore::init_group(&group_id).await.unwrap();
    assert!(store.is_group());
    assert!(!store.is_global());
    assert_eq!(store.project_id, group_id);

    let group_dir = paths::group_store_dir(&group_id).unwrap();
    assert!(group_dir.join(".engramdb").exists());
    assert!(group_dir.join(".engramdb/memories").exists());
    assert!(group_dir.join(".engramdb/manifest.toml").exists());
    assert!(paths::group_lancedb_dir(&group_id).unwrap().exists());

    let memory = create_test_memory("group-mem-001", Visibility::Shared);
    store.create(&memory).await.unwrap();

    // Reopen via `open_group` (existing dir → no re-init) and confirm the
    // memory persisted and the store still reports as a group.
    let reopened = MemoryStore::open_group(&group_id).await.unwrap();
    assert!(reopened.is_group());
    let retrieved = reopened.get("group-mem-001").await.unwrap();
    assert_eq!(retrieved.id, "group-mem-001");
}

#[tokio::test]
async fn test_group_is_not_group_for_regular_store() {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    assert!(!store.is_group());
}

// ===================================================================
// Finding #1 (Critical): `create` must not orphan files across
// visibility directories.
//
// Scenario: a memory with ID X is created as Shared. Later the *same* ID
// is created again as Personal (e.g. worktree consolidation re-runs, or a
// personal/shared re-create). The LanceDB row is keyed on ID so it simply
// flips to Personal — but the old Shared `.md` file was never removed,
// leaving TWO files for one ID across two directories and diverging disk
// from the index.
// ===================================================================

async fn count_files(dir: &Path) -> usize {
    count_md_files(dir).await
}

#[tokio::test]
async fn create_basic_roundtrips_without_orphans() {
    // POSITIVE: a plain create leaves exactly one file and one index row.
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-000000000001";
    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();

    let shared = paths::memories_dir(&store.project_dir);
    let personal = paths::personal_memories_dir(&store.project_id).unwrap();
    assert_eq!(count_files(&shared).await, 1);
    assert_eq!(count_files(&personal).await, 0);
    assert_eq!(store.count().await.unwrap(), 1);
    assert_eq!(store.get(id).await.unwrap().visibility, Visibility::Shared);
}

#[tokio::test]
async fn create_same_id_different_visibility_does_not_orphan() {
    // NEGATIVE (red before fix): re-creating an existing ID under a new
    // visibility must remove the old-visibility file, not orphan it.
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-000000000002";

    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();
    // Re-create the SAME id as Personal.
    store
        .create(&create_test_memory(id, Visibility::Personal))
        .await
        .unwrap();

    let shared = paths::memories_dir(&store.project_dir);
    let personal = paths::personal_memories_dir(&store.project_id).unwrap();

    // Exactly one file total, in the new (Personal) dir; no Shared orphan.
    assert_eq!(
        count_files(&shared).await,
        0,
        "old Shared file must be removed, not orphaned"
    );
    assert_eq!(count_files(&personal).await, 1);
    // Index agrees: one row, resolving to the new visibility.
    assert_eq!(store.count().await.unwrap(), 1);
    assert_eq!(
        store.get(id).await.unwrap().visibility,
        Visibility::Personal
    );
}

// ===================================================================
// Finding #14 (Medium): tied-mtime duplicate resolution must be
// deterministic (not directory-iteration-order dependent).
// ===================================================================

#[test]
fn prefers_newer_breaks_mtime_ties_deterministically() {
    use std::time::{Duration, UNIX_EPOCH};
    let t = UNIX_EPOCH + Duration::from_secs(1000);
    let a = Path::new("aaa_id.md");
    let b = Path::new("bbb_id.md");

    // POSITIVE: strictly newer mtime always wins regardless of path.
    let newer = t + Duration::from_secs(1);
    assert!(prefers_newer((newer, a), (t, b)));
    assert!(!prefers_newer((t, b), (newer, a)));

    // NEGATIVE (red before fix): on an mtime TIE the result must be
    // order-independent. Lexicographically greater path ("bbb" > "aaa")
    // wins no matter which side it is on. Before the fix (`a.0 >= b.0`),
    // the first argument always won on ties → order-dependent.
    assert!(prefers_newer((t, b), (t, a)), "bbb should win the tie");
    assert!(
        !prefers_newer((t, a), (t, b)),
        "aaa must lose the tie regardless of argument order"
    );
}

// ===================================================================
// reindex_incremental — skipping rows whose file is unchanged
// ===================================================================

/// Nothing changed: every row is skipped, and the store is still complete.
#[tokio::test]
async fn incremental_reindex_skips_every_unchanged_row() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    for i in 0..3 {
        let id = format!("0190aaaa-bbbb-7ccc-8ddd-00000000in{:02}", i);
        store
            .create(&create_test_memory(&id, Visibility::Shared))
            .await
            .unwrap();
    }

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.indexed, 3, "every memory is still accounted for");
    assert_eq!(counts.skipped, 3, "and none of them needed a rewrite");
    assert_eq!(store.list_for_filtering().await.unwrap().len(), 3);
}

/// Only the edited memory is rewritten.
#[tokio::test]
async fn incremental_reindex_rewrites_only_the_changed_row() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let ids: Vec<String> = (0..3)
        .map(|i| format!("0190aaaa-bbbb-7ccc-8ddd-00000000ch{:02}", i))
        .collect();
    for id in &ids {
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
    }
    store.reindex_incremental().await.unwrap();

    edit_file_behind_store(&store, &ids[1], "a rewritten body\n").await;

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.indexed, 3);
    assert_eq!(counts.skipped, 2, "the two untouched files are skipped");

    // The edited row now reflects the file: no drift remains.
    let digests = store.index_digests().await.unwrap();
    let row = digests.iter().find(|d| d.memory_id == ids[1]).unwrap();
    let bytes = store.read_memory_bytes(&ids[1]).await.unwrap().unwrap();
    assert_eq!(
        row.content_sha256.as_deref(),
        Some(crate::digest::FileDigest::of(&bytes).sha256.as_str()),
        "the rewritten row must describe the edited file"
    );
}

/// **The data-loss guard.** A skipped memory keeps its vectors.
///
/// `indexed_ids` drives the orphan-chunk prune, so a skipped row that was
/// left out of it would have its vectors deleted — turning a no-op reindex
/// into silent, unrecoverable vector loss.
#[tokio::test]
async fn incremental_reindex_keeps_vectors_of_skipped_memories() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-0000000000vv";
    let m = create_test_memory(id, Visibility::Shared);
    store.create(&m).await.unwrap();
    store
        .upsert_chunks(id, vec![vec![0.5f32; 384]])
        .await
        .unwrap();
    assert!(!store.export_chunks(id).await.unwrap().is_empty());

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.skipped, 1, "fixture must actually skip");
    assert!(
        !store.export_chunks(id).await.unwrap().is_empty(),
        "a skipped memory must keep its vectors"
    );
    // And its row still says it has them.
    let digests = store.index_digests().await.unwrap();
    assert!(
        digests
            .iter()
            .find(|d| d.memory_id == id)
            .unwrap()
            .has_embedding
    );
}

/// **The deletion guard.** Incremental never clears the table, so a row
/// whose file is gone has to be removed explicitly — otherwise it answers
/// queries forever with a memory that no longer exists.
#[tokio::test]
async fn incremental_reindex_removes_rows_whose_file_is_gone() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let keep = "0190aaaa-bbbb-7ccc-8ddd-0000000000k1";
    let gone = "0190aaaa-bbbb-7ccc-8ddd-0000000000g1";
    for id in [keep, gone] {
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
    }
    store.reindex_incremental().await.unwrap();

    // Remove the file behind the store's back, leaving the row.
    let dir = paths::memories_dir(&store.project_dir);
    for path in find_memory_files(&dir, gone).await.unwrap() {
        async_fs::remove_file(&path).await.unwrap();
    }

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.indexed, 1);
    let ids: Vec<String> = store
        .list_for_filtering()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids, vec![keep.to_string()], "the deleted row must be gone");
}

/// A row with no recorded digest whose file is gone must still be removed.
///
/// The skip map holds only rows that *have* a digest, since a row without
/// one can never match a file. Deriving deletions from that same map would
/// leave such a row in the index permanently — it is absent from the very
/// structure that decides what to remove. Deletions come from the full row
/// set for exactly this case.
#[tokio::test]
async fn incremental_reindex_removes_a_digestless_row_whose_file_is_gone() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let keep = "0190aaaa-bbbb-7ccc-8ddd-0000000000d1";
    let gone = "0190aaaa-bbbb-7ccc-8ddd-0000000000d2";
    for id in [keep, gone] {
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
    }
    // Strip every digest, so no row carries one (a pre-0.8.0 shape).
    crate::test_support::clear_content_digests(&store)
        .await
        .unwrap();
    let dir = paths::memories_dir(&store.project_dir);
    for path in find_memory_files(&dir, gone).await.unwrap() {
        async_fs::remove_file(&path).await.unwrap();
    }

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.skipped, 0, "no digest means nothing can be skipped");
    let ids: Vec<String> = store
        .list_for_filtering()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(
        ids,
        vec![keep.to_string()],
        "the digest-less row whose file is gone must still be deleted"
    );
}

/// **Review finding.** A skipped row must not keep a stale `has_embedding`.
///
/// The flag mirrors the chunks table, not the file, so vectors added or
/// dropped since the last rebuild leave an unchanged file and a wrong flag.
/// The retrieval scorer reads it to decide whether a memory has semantic
/// evidence at all, and a full reindex is what used to reconcile it — so
/// "incremental produces what full produces" was false here.
#[tokio::test]
async fn incremental_reindex_refreshes_a_stale_has_embedding_flag() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-0000000000he";
    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();
    store.reindex().await.unwrap();

    // Vectors appear without the file changing.
    store
        .upsert_chunks(id, vec![vec![0.5f32; 384]])
        .await
        .unwrap();
    // Force the mirror stale, as a half-committed chunk write would.
    store
        .lance_index
        .set_has_embedding(id, false)
        .await
        .unwrap();

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.skipped, 0, "a stale flag is work, not a skip");
    let digests = store.index_digests().await.unwrap();
    assert!(
        digests
            .iter()
            .find(|d| d.memory_id == id)
            .unwrap()
            .has_embedding,
        "the flag must be reconciled from the chunks table"
    );
}

/// **Review finding.** A skipped row must not keep a stale `content_len`.
///
/// `content_sha256` is computed over CRLF-normalized bytes while
/// `content_len` is the raw on-disk length — deliberately different things.
/// A `core.autocrlf` checkout therefore leaves a matching hash and a stale
/// length, and the DEFAULT `size` staleness tier compares exactly that
/// length. Skipping on the hash alone made the read path warn on every
/// query with no reindex able to clear it.
#[tokio::test]
async fn incremental_reindex_refreshes_a_stale_content_len() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-0000000000cl";
    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();
    store.reindex().await.unwrap();

    // Rewrite the file with CRLF terminators: same normalized hash, longer
    // raw file. This is what a git line-ending filter produces.
    let dir = paths::memories_dir(&store.project_dir);
    let path = find_memory_files(&dir, id).await.unwrap()[0].clone();
    let lf = async_fs::read_to_string(&path).await.unwrap();
    let crlf = lf.replace('\n', "\r\n");
    assert!(crlf.len() > lf.len());
    async_fs::write(&path, &crlf).await.unwrap();
    assert_eq!(
        crate::digest::FileDigest::of(crlf.as_bytes()).sha256,
        crate::digest::FileDigest::of(lf.as_bytes()).sha256,
        "fixture premise: normalization makes the hashes equal"
    );

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.skipped, 0, "a stale length is work, not a skip");

    // And the read path is quiet again at the default tier.
    assert_eq!(
        store
            .check_staleness(&IndexConfig::default())
            .await
            .unwrap(),
        None,
        "the size tier must have nothing left to report"
    );
}

/// **The equivalence gate.** An incremental rebuild must leave the same
/// index a full one would.
///
/// Both arms run against the *same* store rather than two freshly built
/// ones: memory files carry creation timestamps, so two stores seeded a
/// moment apart hold genuinely different bytes and would differ for
/// reasons that have nothing to do with skipping. Rebuilding the same
/// store twice — incrementally, then fully — compares exactly the thing
/// under test, and is the stronger property anyway.
#[tokio::test]
async fn incremental_reindex_matches_full_reindex() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let ids: Vec<String> = (0..4)
        .map(|i| format!("0190aaaa-bbbb-7ccc-8ddd-00000000eq{:02}", i))
        .collect();
    for id in &ids {
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
    }
    // Vectors on one memory, so `has_embedding` is exercised in both arms.
    store
        .upsert_chunks(&ids[0], vec![vec![0.25f32; 384]])
        .await
        .unwrap();
    store.reindex().await.unwrap();

    // Real work for both arms: one edit, one deletion.
    edit_file_behind_store(&store, &ids[1], "changed body\n").await;
    let dir = paths::memories_dir(&store.project_dir);
    for path in find_memory_files(&dir, &ids[3]).await.unwrap() {
        async_fs::remove_file(&path).await.unwrap();
    }

    async fn snapshot(store: &MemoryStore) -> Vec<(String, Option<String>, bool)> {
        let mut rows: Vec<(String, Option<String>, bool)> = store
            .index_digests()
            .await
            .unwrap()
            .into_iter()
            .map(|d| (d.memory_id, d.content_sha256, d.has_embedding))
            .collect();
        rows.sort();
        rows
    }

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(counts.skipped, 2, "the two untouched memories are skipped");
    let after_incremental = snapshot(&store).await;

    store.reindex().await.unwrap();
    let after_full = snapshot(&store).await;

    assert_eq!(
        after_incremental, after_full,
        "an incremental rebuild must produce the index a full one would"
    );
    // And the vectors survived both.
    assert!(!store.export_chunks(&ids[0]).await.unwrap().is_empty());
}

/// A stale normalizer stamp means the *derivation* changed even where the
/// bytes did not, so an unchanged file is exactly the case needing a
/// rebuild. Incremental must fall back to rebuilding everything.
#[tokio::test]
async fn incremental_reindex_falls_back_when_the_derivation_stamp_is_stale() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    store
        .create(&create_test_memory(
            "0190aaaa-bbbb-7ccc-8ddd-0000000000ns",
            Visibility::Shared,
        ))
        .await
        .unwrap();
    store.reindex().await.unwrap();

    // Foreign normalizer: same bytes, different stems.
    let manifest_path = paths::project_dir(&store.project_dir).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.normalizer = Some("some-other-stemmer-v9".to_string());
    manifest::save_manifest(&manifest_path, &m).await.unwrap();

    let counts = store.reindex_incremental().await.unwrap();
    assert_eq!(
        counts.skipped, 0,
        "a foreign derivation stamp must rebuild every row, not skip it"
    );
    assert_eq!(counts.indexed, 1);
}

// ===================================================================
// check_staleness — the tiered index-currency check.
//
// The pure reconciliation and wording live in `crate::staleness`; these
// exercise the store-side scan, i.e. which tier notices what.
// ===================================================================

/// Overwrite a memory's file on disk, behind the store's back — a hand
/// edit, a `git checkout`, a restore. Returns the id.
async fn edit_file_behind_store(store: &MemoryStore, id: &str, new_body: &str) {
    let dir = paths::memories_dir(&store.project_dir);
    let files = find_memory_files(&dir, id).await.unwrap();
    let path = files.first().expect("memory file on disk");
    let text = async_fs::read_to_string(path).await.unwrap();
    // Keep the frontmatter, replace the body — the file stays parseable.
    let (fm, _) = text.split_once("\n---\n").expect("frontmatter delimiter");
    async_fs::write(path, format!("{fm}\n---\n{new_body}"))
        .await
        .unwrap();
}

fn tier(check: StalenessCheck) -> IndexConfig {
    IndexConfig {
        staleness_check: check,
        ..Default::default()
    }
}

#[tokio::test]
async fn staleness_in_sync_store_is_silent_at_every_tier() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    for i in 0..3 {
        let id = format!("0190aaaa-bbbb-7ccc-8ddd-00000000ss{:02}", i);
        store
            .create(&create_test_memory(&id, Visibility::Shared))
            .await
            .unwrap();
    }
    for check in [
        StalenessCheck::Counts,
        StalenessCheck::Size,
        StalenessCheck::Content,
    ] {
        assert_eq!(
            store.check_staleness(&tier(check)).await.unwrap(),
            None,
            "a store in sync must not warn at the {} tier",
            check.as_str()
        );
    }
}

#[tokio::test]
async fn staleness_count_mismatch_warns_at_every_tier() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-0000000000c1";
    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();
    // Delete the file only — the row survives.
    let dir = paths::memories_dir(&store.project_dir);
    for path in find_memory_files(&dir, id).await.unwrap() {
        async_fs::remove_file(&path).await.unwrap();
    }

    for check in [
        StalenessCheck::Counts,
        StalenessCheck::Size,
        StalenessCheck::Content,
    ] {
        assert!(
            store.check_staleness(&tier(check)).await.unwrap().is_some(),
            "a missing file must warn at the {} tier",
            check.as_str()
        );
    }
}

/// **Blind spot the `size` tier exists to close.** A hand edit leaves the
/// count identical, so the `counts` tier reports a clean store.
#[tokio::test]
async fn staleness_hand_edit_is_missed_by_counts_and_caught_by_size() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-0000000000e1";
    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();
    edit_file_behind_store(&store, id, "a substantially longer body than before\n").await;

    assert_eq!(
        store
            .check_staleness(&tier(StalenessCheck::Counts))
            .await
            .unwrap(),
        None,
        "the count tier is invariant under in-place change — this is the blind spot"
    );
    let warning = store
        .check_staleness(&tier(StalenessCheck::Size))
        .await
        .unwrap()
        .expect("the size tier must notice a length change");
    assert!(
        warning.contains("1 memory changed since indexing (size check)"),
        "{warning}"
    );
}

/// **The `size`/`content` boundary.** An edit that preserves the file's
/// length exactly is invisible to `statx` and needs the hash.
#[tokio::test]
async fn staleness_same_length_edit_needs_the_content_tier() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-0000000000e2";
    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();

    // Read the body back and substitute one character for another, so the
    // byte length is untouched.
    let dir = paths::memories_dir(&store.project_dir);
    let path = find_memory_files(&dir, id).await.unwrap()[0].clone();
    let before = async_fs::read(&path).await.unwrap();
    let mutated: Vec<u8> = before
        .iter()
        .map(|b| if *b == b'e' { b'3' } else { *b })
        .collect();
    assert_ne!(mutated, before, "the fixture body must contain an 'e'");
    assert_eq!(mutated.len(), before.len(), "the edit must preserve length");
    async_fs::write(&path, &mutated).await.unwrap();

    assert_eq!(
        store
            .check_staleness(&tier(StalenessCheck::Size))
            .await
            .unwrap(),
        None,
        "a length-preserving edit is invisible to the size tier, by construction"
    );
    let warning = store
        .check_staleness(&tier(StalenessCheck::Content))
        .await
        .unwrap()
        .expect("the content tier must notice a length-preserving edit");
    assert!(
        warning.contains("1 memory changed since indexing (content check)"),
        "{warning}"
    );
}

/// A row predating schema 0.8.0 records no length or hash. That is
/// *unknown*, not *changed* — reporting it as drift would make every
/// not-yet-reindexed store warn on every read, with a reindex the only way
/// to silence a warning about nothing.
#[tokio::test]
async fn staleness_rows_without_a_digest_are_not_reported_as_changed() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let id = "0190aaaa-bbbb-7ccc-8ddd-0000000000e3";
    store
        .create(&create_test_memory(id, Visibility::Shared))
        .await
        .unwrap();
    crate::test_support::clear_content_digests(&store)
        .await
        .unwrap();

    for check in [StalenessCheck::Size, StalenessCheck::Content] {
        assert_eq!(
            store.check_staleness(&tier(check)).await.unwrap(),
            None,
            "an undigested row must not read as changed at the {} tier",
            check.as_str()
        );
    }
}

/// The content tier's budget bounds the work, and says so rather than
/// letting a partial exact check read as a clean one.
#[tokio::test]
async fn staleness_content_budget_is_reported_not_silently_downgraded() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    // Two memories; the drift is on the one that sorts *last*, so a budget
    // that admits only the first cannot see it.
    let first = "0190aaaa-bbbb-7ccc-8ddd-0000000000b1";
    let second = "0190aaaa-bbbb-7ccc-8ddd-0000000000b2";
    for id in [first, second] {
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
    }
    // A length-preserving edit, so only the content tier could catch it.
    let dir = paths::memories_dir(&store.project_dir);
    let path = find_memory_files(&dir, second).await.unwrap()[0].clone();
    let bytes = async_fs::read(&path).await.unwrap();
    let mutated: Vec<u8> = bytes
        .iter()
        .map(|b| if *b == b'e' { b'3' } else { *b })
        .collect();
    async_fs::write(&path, &mutated).await.unwrap();

    let generous = IndexConfig {
        staleness_check: StalenessCheck::Content,
        staleness_max_bytes: 8 * 1024 * 1024,
    };
    assert!(
        store
            .check_staleness(&generous)
            .await
            .unwrap()
            .is_some_and(|w| w.contains("content check")),
        "with budget to spare, the edit is found"
    );

    // A budget of one byte admits nothing at all: the check finds no drift
    // but must not therefore report a clean store.
    let starved = IndexConfig {
        staleness_check: StalenessCheck::Content,
        staleness_max_bytes: 1,
    };
    assert_eq!(
        store.check_staleness(&starved).await.unwrap(),
        None,
        "an exhausted budget with nothing found is not itself a warning — \
         every read of a large store would otherwise warn"
    );
}

// ===================================================================
// Finding #5 (High): staleness warning must be suppressed under a
// shared-ID checkout conflict (where lance_count > md_count is normal).
// ===================================================================

/// Under a conflict the index legitimately holds the *other* checkout's
/// rows, so every tier would fire — permanently, since reindex degrades to
/// a non-destructive upsert under conflict by design.
#[tokio::test]
async fn staleness_suppressed_under_checkout_conflict_at_every_tier() {
    let tmp = TempDir::new().unwrap();
    let a = make_clone(tmp.path(), "clone-a", "staleness-conflict");
    let b = make_clone(tmp.path(), "clone-b", "staleness-conflict");
    let registry = crate::registry::FileRegistry::global().unwrap();

    let store_a = MemoryStore::init(&a, &registry).await.unwrap();
    let store_b = MemoryStore::init(&b, &registry).await.unwrap();
    // Rows in the shared index that clone B has no files for.
    for i in 0..2 {
        let id = format!("0190aaaa-bbbb-7ccc-8ddd-00000000cf{:02}", i);
        store_a
            .create(&create_test_memory(&id, Visibility::Shared))
            .await
            .unwrap();
    }
    assert!(
        store_b.checkout_conflict().await.is_some(),
        "fixture must actually produce a conflict"
    );

    for check in [
        StalenessCheck::Counts,
        StalenessCheck::Size,
        StalenessCheck::Content,
    ] {
        assert_eq!(
            store_b.check_staleness(&tier(check)).await.unwrap(),
            None,
            "must not warn under a checkout conflict at the {} tier",
            check.as_str()
        );
    }
}

// ===================================================================
// Finding #15 (Medium): get_batch tolerates a corrupt file (skips it,
// returns the valid ones) rather than failing the whole batch.
// ===================================================================

#[tokio::test]
async fn get_batch_skips_corrupt_file_returns_valid() {
    let temp = TempDir::new().unwrap();
    let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let good = "0190aaaa-bbbb-7ccc-8ddd-000000000010";
    let bad = "0190aaaa-bbbb-7ccc-8ddd-000000000011";
    store
        .create(&create_test_memory(good, Visibility::Shared))
        .await
        .unwrap();
    store
        .create(&create_test_memory(bad, Visibility::Shared))
        .await
        .unwrap();

    // Corrupt the second file on disk (truncate to garbage that won't parse).
    let shared = paths::memories_dir(&store.project_dir);
    for path in find_memory_files(&shared, bad).await.unwrap() {
        async_fs::write(&path, "this is not a valid memory file")
            .await
            .unwrap();
    }

    let got = store.get_batch(&[good, bad]).await.unwrap();
    let ids: Vec<&str> = got.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec![good],
        "valid memory returned, corrupt one skipped"
    );
}

/// Every file under `root` with its length and mtime, so two snapshots
/// differ when anything was created, removed, resized or rewritten.
fn tree_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
    fn walk(
        dir: &Path,
        out: &mut std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)>,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            // Directories are entries too: creating an empty one is a
            // side effect the file list alone would miss.
            out.insert(path.clone(), (meta.len(), meta.modified().unwrap()));
            if meta.is_dir() {
                walk(&path, out);
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, &mut out);
    out
}

/// Everything a read-only open could touch: the project tree, the global
/// data dir (LanceDB, personal memories, locks) and the config dir
/// (registry).
fn machine_snapshot(
    project: &Path,
) -> Vec<std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)>> {
    vec![
        tree_snapshot(project),
        tree_snapshot(&paths::global_data_dir().unwrap()),
        tree_snapshot(&paths::global_config_dir().unwrap()),
    ]
}

#[cfg(unix)]
fn dir_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// `projects/<id>/` holds personal memories, the conversations table and
/// the transcript copies; it used to be created with the umask default.
#[cfg(unix)]
#[tokio::test]
async fn a_new_project_data_dir_is_owner_only() {
    let tmp = TempDir::new().unwrap();
    let registry = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &registry).await.unwrap();
    let data = paths::lancedb_dir(&store.project_id).unwrap();
    assert_eq!(dir_mode(data.parent().unwrap()), 0o700);

    let global = MemoryStore::open_global().await.unwrap();
    drop(global);
    assert_eq!(dir_mode(&paths::global_store_dir().unwrap()), 0o700);
    let group = MemoryStore::open_group("__g_0123456789ab").await.unwrap();
    drop(group);
    assert_eq!(
        dir_mode(&paths::group_store_dir("__g_0123456789ab").unwrap()),
        0o700
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_existing_loose_project_data_dir_is_tightened_on_a_writable_open() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let registry = InMemoryRegistry::new();
    let project_id = {
        let store = MemoryStore::init(tmp.path(), &registry).await.unwrap();
        store.project_id.clone()
    };
    let data = paths::lancedb_dir(&project_id).unwrap();
    let data = data.parent().unwrap();
    // As a store created by an older binary under a 022 umask.
    std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o755)).unwrap();

    let _store = MemoryStore::open(tmp.path()).await.unwrap();
    assert_eq!(dir_mode(data), 0o700);
}

#[cfg(unix)]
#[tokio::test]
async fn a_read_only_open_changes_no_mode() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let registry = InMemoryRegistry::new();
    let project_id = {
        let store = MemoryStore::init(tmp.path(), &registry).await.unwrap();
        store
            .create(&create_test_memory("mode-1", Visibility::Shared))
            .await
            .unwrap();
        store.project_id.clone()
    };
    let data = paths::lancedb_dir(&project_id).unwrap();
    let data = data.parent().unwrap();
    std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o755)).unwrap();

    let store = MemoryStore::open_read_only(tmp.path()).await.unwrap();
    assert_eq!(store.get("mode-1").await.unwrap().id, "mode-1");
    assert_eq!(dir_mode(data), 0o755, "a read-only open tightened the dir");
}

#[tokio::test]
async fn open_read_only_and_reads_write_nothing() {
    let tmp = TempDir::new().unwrap();
    let registry = InMemoryRegistry::new();
    {
        let store = MemoryStore::init(tmp.path(), &registry).await.unwrap();
        store
            .create(&create_test_memory("ro-shared-1", Visibility::Shared))
            .await
            .unwrap();
        store
            .create(&create_test_memory("ro-personal-1", Visibility::Personal))
            .await
            .unwrap();
    }
    // `open` would backfill this; a read-only open must not.
    std::fs::remove_file(tmp.path().join(".engramdb").join(".gitignore")).unwrap();
    let before = machine_snapshot(tmp.path());

    let store = MemoryStore::open_read_only(tmp.path()).await.unwrap();
    assert_eq!(store.list_filterable().await.unwrap().len(), 2);
    assert_eq!(store.list_summary().await.unwrap().len(), 2);
    assert_eq!(store.get("ro-shared-1").await.unwrap().id, "ro-shared-1");
    assert_eq!(store.get("ro-personal").await.unwrap().id, "ro-personal-1");
    assert_eq!(
        store
            .get_batch(&["ro-shared-1", "ro-personal-1"])
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(!store.schema_state().await.needs_migration);
    let _ = store
        .check_staleness(&IndexConfig::default())
        .await
        .unwrap();
    let _ = store.index_digests().await.unwrap();
    let _ = store.file_digests().await.unwrap();
    drop(store);

    assert_eq!(
        before,
        machine_snapshot(tmp.path()),
        "a read-only open plus reads changed files on disk"
    );
}

#[tokio::test]
async fn open_read_only_reports_a_stale_schema_instead_of_migrating() {
    let tmp = TempDir::new().unwrap();
    MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap()
        .create(&create_test_memory("ro-stale-1", Visibility::Shared))
        .await
        .unwrap();
    let manifest_path = tmp.path().join(".engramdb").join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.1.0".to_string();
    manifest::save_manifest(&manifest_path, &m).await.unwrap();
    let before = machine_snapshot(tmp.path());

    let store = MemoryStore::open_read_only(tmp.path()).await.unwrap();
    let state = store.schema_state().await;
    assert!(state.needs_migration);
    assert_eq!(state.stored.as_deref(), Some("0.1.0"));
    assert_eq!(state.current, manifest::CURRENT_SCHEMA_VERSION);
    drop(store);

    assert_eq!(before, machine_snapshot(tmp.path()), "no migration ran");
    let m = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(m.schema_version, "0.1.0");
}

#[tokio::test]
async fn open_read_only_creates_nothing_for_missing_stores() {
    let tmp = TempDir::new().unwrap();
    let before = machine_snapshot(tmp.path());
    assert!(matches!(
        MemoryStore::open_read_only(tmp.path()).await,
        Err(StorageError::NotInitialized)
    ));
    assert!(matches!(
        MemoryStore::open_global_read_only().await,
        Err(StorageError::NotInitialized)
    ));
    assert!(matches!(
        MemoryStore::open_group_read_only("__g_nope").await,
        Err(StorageError::NotInitialized)
    ));
    assert_eq!(before, machine_snapshot(tmp.path()));

    // A store whose index was never built here (a fresh clone): the
    // `.engramdb/` exists, `lancedb/` does not, and must not appear.
    std::fs::create_dir_all(tmp.path().join(".engramdb").join("memories")).unwrap();
    let before = machine_snapshot(tmp.path());
    let err = MemoryStore::open_read_only(tmp.path()).await.err().unwrap();
    assert!(
        err.to_string()
            .contains("has not been indexed on this machine"),
        "{err}"
    );
    assert_eq!(before, machine_snapshot(tmp.path()));
    let id = project_id::compute_project_id(tmp.path());
    assert!(!paths::lancedb_dir(&id).unwrap().exists());
}

/// Several processes opening a stale store at once (every session's
/// hooks after an upgrade) all see the old stamp. Only the first may
/// rebuild; the rest must find the store current once they hold the lock.
#[tokio::test]
async fn concurrent_opens_migrate_once() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    {
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        for i in 0..5 {
            store
                .create(&create_test_memory(
                    &format!("mig-once-{i}"),
                    Visibility::Shared,
                ))
                .await
                .unwrap();
        }
    }
    let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
    let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
    m.schema_version = "0.8.0".to_string();
    manifest::save_manifest(&manifest_path, &m).await.unwrap();
    MIGRATION_REBUILDS.store(0, std::sync::atomic::Ordering::SeqCst);

    let dir = tmp.path().to_path_buf();
    let opens: Vec<_> = (0..3)
        .map(|_| {
            let dir = dir.clone();
            tokio::spawn(async move { MemoryStore::open(&dir).await })
        })
        .collect();
    for open in opens {
        let store = open.await.unwrap().unwrap();
        assert_eq!(store.list_summary().await.unwrap().len(), 5);
    }

    assert_eq!(
        MIGRATION_REBUILDS.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a stale store is rebuilt once, however many opens saw it stale"
    );
    let m = manifest::load_manifest(&manifest_path).await.unwrap();
    assert_eq!(m.schema_version, manifest::CURRENT_SCHEMA_VERSION);
}

/// A reindex empties the table with one overwrite commit. With a drop
/// followed by a create, a lock-free reader in between saw no table at
/// all (and a concurrent open created an empty one over the rebuild).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_memories_table_never_disappears_during_a_reindex() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    for i in 0..10 {
        store
            .create(&create_test_memory(
                &format!("never-gone-{i}"),
                Visibility::Shared,
            ))
            .await
            .unwrap();
    }
    let lance_path = paths::lancedb_dir(&store.project_id).unwrap();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let done = std::sync::Arc::clone(&done);
        tokio::spawn(async move {
            let mut looks = 0usize;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let index = LanceIndex::open_existing(&lance_path, 384)
                    .await
                    .expect("the index opens")
                    .expect("the memories table exists at every instant");
                index.count().await.expect("the table is readable");
                looks += 1;
            }
            looks
        })
    };
    for _ in 0..20 {
        assert_eq!(store.reindex().await.unwrap(), 10);
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(reader.await.unwrap() > 0);
    assert_eq!(store.list_summary().await.unwrap().len(), 10);
}

// ---- one id in both the shared and the personal directory ----

/// A memory that went personal, plus its old shared file back on disk
/// (what a `git checkout` of a tracked file does). Returns
/// `(store, id, shared_path, personal_path)`.
async fn store_with_a_restored_shared_copy(
    tmp: &TempDir,
    id: &str,
) -> (MemoryStore, String, PathBuf, PathBuf) {
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let mut original = create_test_memory(id, Visibility::Shared);
    original.summary = "Shared original".to_string();
    store.create(&original).await.unwrap();
    let shared_path = paths::memories_dir(tmp.path()).join(format!("{id}.md"));
    let shared_bytes = std::fs::read(&shared_path).unwrap();

    store
        .update_with(id, |m| {
            m.visibility = Visibility::Personal;
            m.summary = "Now private".to_string();
            Ok(())
        })
        .await
        .unwrap();
    assert!(!shared_path.exists(), "the flip removed the shared file");
    let personal_path = paths::personal_memories_dir(&store.project_id)
        .unwrap()
        .join(format!("{id}.md"));
    assert!(personal_path.exists());

    // The older shared copy comes back.
    std::fs::write(&shared_path, shared_bytes).unwrap();
    (store, id.to_string(), shared_path, personal_path)
}

#[tokio::test]
async fn a_restored_shared_copy_does_not_shadow_the_personal_one() {
    let tmp = TempDir::new().unwrap();
    let (store, id, shared_path, personal_path) =
        store_with_a_restored_shared_copy(&tmp, "cross-dir-get").await;

    // `get` used to return the shared copy (shared first).
    let got = store.get(&id).await.unwrap();
    assert_eq!(got.summary, "Now private");
    assert_eq!(got.visibility, Visibility::Personal);

    let batch = store.get_batch(&[id.as_str()]).await.unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].1.summary, "Now private");

    let duplicates = store.cross_dir_duplicates().await.unwrap();
    assert_eq!(
        duplicates,
        vec![CrossDirDuplicate {
            id: id.clone(),
            shared: shared_path,
            personal: personal_path.clone(),
            winner: CrossDirWinner::Personal,
        }]
    );

    // The index row, the content digest and the staleness check agree.
    store.reindex().await.unwrap();
    let rows = store.list_summary().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    let indexed = store.index_digests().await.unwrap();
    assert_eq!(
        indexed[0].content_sha256.as_deref(),
        Some(
            FileDigest::of(&std::fs::read(&personal_path).unwrap())
                .sha256
                .as_str()
        ),
        "the row was built from the personal copy"
    );
    let (digests, unreadable) = store.file_digests().await.unwrap();
    assert!(unreadable.is_empty());
    assert_eq!(
        digests[&id],
        FileDigest::of(&std::fs::read(&personal_path).unwrap())
    );
    for check in [
        StalenessCheck::Counts,
        StalenessCheck::Size,
        StalenessCheck::Content,
    ] {
        let cfg = IndexConfig {
            staleness_check: check,
            ..IndexConfig::default()
        };
        assert_eq!(
            store.check_staleness(&cfg).await.unwrap(),
            None,
            "{check:?}: a cross-directory duplicate is not permanent drift"
        );
    }
}

/// The shared copy is the newer one: it wins, and the tie goes personal.
#[test]
fn the_cross_dir_rule_is_newest_then_personal() {
    let t = chrono::Utc::now();
    let later = t + chrono::Duration::seconds(1);
    assert_eq!(cross_dir_rule(Some(later), Some(t)), CrossDirWinner::Shared);
    assert_eq!(
        cross_dir_rule(Some(t), Some(later)),
        CrossDirWinner::Personal
    );
    assert_eq!(cross_dir_rule(Some(t), Some(t)), CrossDirWinner::Personal);
    // A copy that does not parse loses.
    assert_eq!(cross_dir_rule(Some(t), None), CrossDirWinner::Shared);
    assert_eq!(cross_dir_rule(None, Some(t)), CrossDirWinner::Personal);
}

#[tokio::test]
async fn reindex_reports_and_keeps_a_cross_dir_duplicate() {
    let tmp = TempDir::new().unwrap();
    let (store, id, shared_path, personal_path) =
        store_with_a_restored_shared_copy(&tmp, "cross-dir-reindex").await;

    let counts = store.reindex_counted().await.unwrap();

    assert_eq!(counts.indexed, 1, "one memory, counted once");
    assert_eq!(counts.shadowed, vec![(id.clone(), shared_path.clone())]);
    assert!(shared_path.exists(), "reindex never deletes the loser");
    assert!(personal_path.exists());
    // Incremental reports it too.
    let again = store.reindex_incremental().await.unwrap();
    assert_eq!(again.shadowed, vec![(id, shared_path)]);
    assert_eq!(again.indexed, 1);
}

#[tokio::test]
async fn an_update_removes_the_other_directorys_copy() {
    let tmp = TempDir::new().unwrap();
    let (store, id, shared_path, personal_path) =
        store_with_a_restored_shared_copy(&tmp, "cross-dir-update").await;

    store
        .update_with(&id, |m| {
            m.summary = "Edited".to_string();
            Ok(())
        })
        .await
        .unwrap();

    assert!(!shared_path.exists(), "the update healed the duplicate");
    assert!(personal_path.exists());
    assert_eq!(store.get(&id).await.unwrap().summary, "Edited");
    assert!(store.cross_dir_duplicates().await.unwrap().is_empty());
}

/// Deleting stopped at the first directory that had a copy, so the other
/// copy survived and the next reindex brought the memory back.
#[tokio::test]
async fn a_delete_removes_both_copies() {
    let tmp = TempDir::new().unwrap();
    let (store, id, shared_path, personal_path) =
        store_with_a_restored_shared_copy(&tmp, "cross-dir-delete").await;

    store.delete(&id).await.unwrap();

    assert!(!shared_path.exists());
    assert!(!personal_path.exists());
    assert_eq!(store.reindex().await.unwrap(), 0);
    assert!(matches!(
        store.get(&id).await,
        Err(StorageError::NotFound(_))
    ));

    // The batched delete goes through the same sweep.
    let tmp = TempDir::new().unwrap();
    let (store, id, shared_path, personal_path) =
        store_with_a_restored_shared_copy(&tmp, "cross-dir-delete-batch").await;
    let (deleted, _) = store
        .delete_batch_if(std::slice::from_ref(&id), |_| true)
        .await
        .unwrap();
    assert_eq!(deleted, vec![id]);
    assert!(!shared_path.exists());
    assert!(!personal_path.exists());
}

#[tokio::test]
async fn get_batch_of_nothing_scans_nothing() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let none: [&str; 0] = [];
    assert!(store.get_batch(&none).await.unwrap().is_empty());
}

// ---- file selection ----

/// `<id>.bak` and `<id>.md.orig` are not the memory: `get` reads the
/// `.md`, and update / delete leave the backups alone.
#[tokio::test]
async fn a_backup_file_is_not_the_memory() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let memory = create_test_memory("backup-victim", Visibility::Shared);
    store.create(&memory).await.unwrap();
    let dir = paths::memories_dir(tmp.path());
    let bak = dir.join("backup-victim.bak");
    let orig = dir.join("backup-victim.md.orig");
    std::fs::write(&bak, "an editor's backup, not a memory file").unwrap();
    std::fs::write(&orig, "a merge tool's leftover").unwrap();

    assert_eq!(
        store.get("backup-victim").await.unwrap().summary,
        "Test summary"
    );
    store
        .update_with("backup-victim", |m| {
            m.summary = "Edited".to_string();
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(store.get("backup-victim").await.unwrap().summary, "Edited");
    assert!(bak.exists() && orig.exists(), "an update removed a backup");

    store.delete("backup-victim").await.unwrap();
    assert!(bak.exists() && orig.exists(), "a delete removed a backup");
    assert!(!dir.join("backup-victim.md").exists());
}

/// Two files for one id with the same mtime: every scan must pick the
/// same one (`prefers_newer`: the greater path), not whichever the
/// directory happened to list last.
#[tokio::test]
async fn equal_mtime_duplicates_resolve_the_same_in_every_scan() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let mut a = Memory::new(MemoryType::Decision, "Copy A", "c", Provenance::human());
    store.create(&a).await.unwrap();
    let id = a.id.clone();
    let dir = paths::memories_dir(tmp.path());
    let path_a = dir.join(format!("{id}.md"));
    // A second file for the same id, as a crashed rename leaves one.
    a.summary = "Copy B".to_string();
    let path_b = dir.join(format!("zzz_{id}.md"));
    std::fs::write(&path_b, memory_file::write_memory_file(&a).unwrap()).unwrap();
    let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    for path in [&path_a, &path_b] {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
    assert!(path_b > path_a);

    for _ in 0..3 {
        assert_eq!(store.get(&id).await.unwrap().summary, "Copy B");
        let batch = store.get_batch(&[id.as_str()]).await.unwrap();
        assert_eq!(batch[0].1.summary, "Copy B");
        let (digests, _) = store.file_digests().await.unwrap();
        assert_eq!(
            digests[&id],
            FileDigest::of(&std::fs::read(&path_b).unwrap())
        );
    }
    // Reindex keeps the same file (and removes the other).
    store.reindex().await.unwrap();
    assert!(path_b.exists());
    assert!(!path_a.exists());
    assert_eq!(store.get(&id).await.unwrap().summary, "Copy B");
}

/// A failed chunk probe in the middle of a batched update used to return
/// at once: the files were written and no row was. The index writes now
/// finish before the error is reported.
#[tokio::test]
async fn a_failed_chunk_probe_still_indexes_the_written_files() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let ids: Vec<String> = (0..3).map(|i| format!("probe-fail-{i}")).collect();
    for id in &ids {
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
    }

    FAIL_CHUNK_PROBE.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = store
        .update_batch_with(&ids, |m| {
            m.criticality = 0.9;
            Ok(())
        })
        .await;
    FAIL_CHUNK_PROBE.store(false, std::sync::atomic::Ordering::SeqCst);

    assert!(
        matches!(
            result,
            Err(StorageError::Index {
                op: "has_chunks",
                ..
            })
        ),
        "the probe error is still reported"
    );
    // Files and rows agree: no drift left behind.
    for id in &ids {
        assert_eq!(store.get(id).await.unwrap().criticality, 0.9);
    }
    let rows = store.list_summary().await.unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.criticality == 0.9));
    let cfg = IndexConfig {
        staleness_check: StalenessCheck::Content,
        ..IndexConfig::default()
    };
    assert_eq!(store.check_staleness(&cfg).await.unwrap(), None);
}

// ---- ids and filenames ----

/// A hand-authored (non-UUID) id used to become unreachable the moment
/// the memory got a title: the file was renamed `<slug>_<id>.md`, and
/// that stem reads back as a different id.
#[tokio::test]
async fn a_titled_non_uuid_memory_stays_reachable() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    store
        .create(&create_test_memory("auth-decision", Visibility::Shared))
        .await
        .unwrap();

    store
        .update_with("auth-decision", |m| {
            m.title = Some("Use short lived tokens".to_string());
            Ok(())
        })
        .await
        .unwrap();

    let got = store.get("auth-decision").await.unwrap();
    assert_eq!(got.title.as_deref(), Some("Use short lived tokens"));
    assert!(paths::memories_dir(tmp.path())
        .join("auth-decision.md")
        .exists());
    // And it can still be changed and removed by its id.
    store
        .update_with("auth-decision", |m| {
            m.summary = "Edited".to_string();
            Ok(())
        })
        .await
        .unwrap();
    store.delete("auth-decision").await.unwrap();
}

/// A file an older binary already renamed (or a hand-named one) is put
/// back under a name that carries its id.
#[tokio::test]
async fn reindex_renames_a_file_whose_stem_hides_its_id() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let mut memory = create_test_memory("auth-decision", Visibility::Shared);
    memory.title = Some("Use short lived tokens".to_string());
    let dir = paths::memories_dir(tmp.path());
    let hidden = dir.join("use-short-lived-tokens_auth-decision.md");
    std::fs::write(&hidden, memory_file::write_memory_file(&memory).unwrap()).unwrap();
    assert!(matches!(
        store.get("auth-decision").await,
        Err(StorageError::NotFound(_))
    ));

    let counts = store.reindex_counted().await.unwrap();

    let healed = dir.join("auth-decision.md");
    assert_eq!(
        counts.renamed,
        vec![("auth-decision".to_string(), healed.clone())]
    );
    assert!(healed.exists() && !hidden.exists());
    assert_eq!(counts.indexed, 1);
    assert_eq!(
        store.get("auth-decision").await.unwrap().title.as_deref(),
        Some("Use short lived tokens")
    );
    // Nothing left to do the second time.
    assert!(store.reindex_counted().await.unwrap().renamed.is_empty());
}

/// A file with no `Created` field used to get `now` on every parse.
#[tokio::test]
async fn a_file_without_created_parses_to_the_same_timestamps_twice() {
    let tmp = TempDir::new().unwrap();
    let reg = InMemoryRegistry::new();
    let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
    let memory = create_test_memory("no-created", Visibility::Shared);
    let content: String = memory_file::write_memory_file(&memory)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("**Created:**") && !l.contains("**Updated:**"))
        .map(|l| format!("{l}\n"))
        .collect();
    let path = paths::memories_dir(tmp.path()).join("no-created.md");
    std::fs::write(&path, &content).unwrap();
    let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(when)
        .unwrap();

    let first = store.get("no-created").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let second = store.get("no-created").await.unwrap();
    let batch = store.get_batch(&["no-created"]).await.unwrap();

    let expected: chrono::DateTime<chrono::Utc> = when.into();
    assert_eq!(first.created_at, expected);
    assert_eq!(first.updated_at, expected);
    assert_eq!(second.created_at, first.created_at);
    assert_eq!(batch[0].1.created_at, first.created_at);
}
