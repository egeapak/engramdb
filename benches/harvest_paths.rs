//! A/B measurements for the per-item loops on the harvest paths.
//!
//! Five of the six loops here ran serially over independent work: a
//! `update_with` per marked memory, a store open per project in the pin scan,
//! an index open per project in the machine-wide search, and a transcript
//! parse per session in the listing. Every group below pairs the shape the
//! code had (`*_PREVIOUS`, a local copy, kept only as the record of what was
//! replaced) with the shape it has now (`*_SHIPPED`, which calls the real
//! function), so the speedup is measured rather than asserted.
//!
//! The `*_PREVIOUS` arms are deliberate copies and are labelled as such. The
//! rule from the earlier pass applies: a *shipped* implementation is never
//! benchmarked through a copy — those arms call production directly, and a
//! copy that drifts from production is only ever the discarded candidate.
//!
//! | group | previous | shipped |
//! |-------|----------|---------|
//! | `harvest_link` | `update_with` per memory | `update_batch_with` once |
//! | `evidence_links` | serial store open + scan per path | bounded concurrency |
//! | `search_fanin` | serial index open, query re-embedded per project | bounded concurrency, embedded once |
//! | `list_sessions` | serial `summarize_session` per transcript | `rayon` over the transcripts |
//! | `index_embed_share` | — | how much of `harvest index --all` is the embed call |
//! | `index_all` | `index_session` per id (re-lists the scope each time) | one listing, one batched embed |
//!
//! `index_embed_share` is not an A/B: it splits the indexing pass into its
//! embedding half and everything else, which is what decided whether batching
//! the embeddings there could pay at all. `index_all` is the A/B that followed
//! from the answer.
//!
//! Run with: `cargo bench --bench harvest_paths`

mod helpers;

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

use engramdb::ops::harvest::SessionScope;
use engramdb::ops::{harvest_index, harvest_pin};
use engramdb::retrieval::engine::RetrievalEngine;
use engramdb::storage::conversation_index::ConversationIndex;
use engramdb::storage::{transcripts, InMemoryRegistry, MemoryStore};
use engramdb::types::EngramConfig;

use helpers::generate_memory;
use tempfile::TempDir;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to create tokio runtime")
}

/// Point every global-data-dir lookup — and Claude Code's transcript tree —
/// at throwaway directories.
///
/// The conversation index, the LanceDB directories and the write locks all
/// resolve through `paths::global_data_dir()`, which reads `ENGRAMDB_DATA_DIR`;
/// the transcript listing resolves through `CLAUDE_CONFIG_DIR`. Benches get no
/// `#[ctor]` test-isolation arm, so without this a bench run would seed
/// conversation tables into the developer's real store, `ConversationIndex::
/// exists` would see their projects rather than the fixture's, and
/// `index_sessions` would list their real conversations.
///
/// Leaked rather than dropped: it must outlive every group. Returns the Claude
/// home so a group can plant transcripts where `locate` will find them.
fn isolate_data_dir() -> &'static Path {
    use std::sync::OnceLock;
    static ROOT: OnceLock<&'static TempDir> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir: &'static TempDir = Box::leak(Box::new(
            TempDir::new().expect("failed to create bench dir"),
        ));
        std::env::set_var("ENGRAMDB_DATA_DIR", dir.path().join("data"));
        std::env::set_var("ENGRAMDB_CONFIG_DIR", dir.path().join("config"));
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path().join("claude"));
        dir
    })
    .path()
}

// ===========================================================================
// Group 1: recording a harvest's provenance links
// ===========================================================================

/// The shape `link_memories` had: one `update_with` per memory id, each of
/// which takes the per-project write lock, does a full directory scan to
/// resolve the id, writes, commits one index row and rescans the store to
/// refresh the manifest stats.
///
/// **Local copy of a replaced implementation — not production code.** The
/// shipped arm calls `harvest_pin::link_memories`.
async fn link_memories_previous(store: &MemoryStore, session_id: &str, memory_ids: &[String]) {
    for id in memory_ids {
        let mut was_new = false;
        let _ = store
            .update_with(id, |memory| {
                was_new = memory.link_source_session(session_id);
                Ok(())
            })
            .await;
    }
}

/// Seed a store with `n` memories and hand back their ids.
///
/// The memories are the ones a harvest just saved, so the store is otherwise
/// small — which is the case that matters, and also the one where the
/// per-memory manifest rescan is *cheapest*. A bigger store makes the batched
/// version look better, not worse.
async fn seed_marked(n: usize) -> (TempDir, MemoryStore, Vec<String>) {
    let tmp = TempDir::new().expect("tempdir");
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .expect("init");
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        ids.push(store.create(&generate_memory(i)).await.expect("create"));
    }
    (tmp, store, ids)
}

fn bench_link_memories(c: &mut Criterion) {
    isolate_data_dir();
    let rt = runtime();
    let mut group = c.benchmark_group("harvest_link");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    // 4 is an ordinary harvest, 16 a productive session, 64 a batch review of
    // a backlog. `harvest mark` accepts an arbitrary list.
    for &n in &[4usize, 16, 64] {
        group.bench_with_input(
            BenchmarkId::new("update_with_loop_PREVIOUS", n),
            &n,
            |b, _| {
                b.iter_batched(
                    || rt.block_on(seed_marked(n)),
                    |(tmp, store, ids)| {
                        rt.block_on(link_memories_previous(&store, "bench-session", &ids));
                        // Handed back rather than dropped inside the timed region:
                        // tearing down the seeded store costs more than the
                        // operation and scales with the same axis.
                        (tmp, store)
                    },
                    BatchSize::PerIteration,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("update_batch_with_SHIPPED", n),
            &n,
            |b, _| {
                b.iter_batched(
                    || rt.block_on(seed_marked(n)),
                    |(tmp, store, ids)| {
                        rt.block_on(harvest_pin::link_memories(&store, "bench-session", &ids))
                            .expect("link_memories");
                        (tmp, store)
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

// ===========================================================================
// Group 2: the pin scan across a session scope
// ===========================================================================

/// The shape `evidence_links` had: open each scope path's store and scan it,
/// one after the other.
///
/// **Local copy of a replaced implementation — not production code.**
async fn evidence_links_previous(scope: &SessionScope) -> usize {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    let mut total = 0usize;
    for path in &scope.paths {
        let Ok(store) = MemoryStore::open(path).await else {
            continue;
        };
        if !seen.insert(engramdb::storage::project_id::compute_project_id(path)) {
            continue;
        }
        total += store
            .list_source_session_links()
            .await
            .expect("links")
            .len();
    }
    total
}

/// `projects` stores under one root, each holding `per_project` memories of
/// which the first `cited` cite a session.
///
/// Only some memories are cited on purpose: the scan projects two columns and
/// drops the uncited rows in the index, so a fixture where everything is cited
/// would overstate the per-row cost.
async fn seed_scope(projects: usize, per_project: usize, cited: usize) -> (TempDir, SessionScope) {
    let root = TempDir::new().expect("tempdir");
    let registry = InMemoryRegistry::new();
    let mut paths = Vec::with_capacity(projects);

    for p in 0..projects {
        let dir = root.path().join(format!("project-{p}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = MemoryStore::init(&dir, &registry).await.expect("init");
        let mut ids = Vec::new();
        for i in 0..per_project {
            ids.push(
                store
                    .create(&generate_memory(p * per_project + i))
                    .await
                    .expect("create"),
            );
        }
        ids.truncate(cited);
        harvest_pin::link_memories(&store, &format!("sess-{p}"), &ids)
            .await
            .expect("link");
        paths.push(dir);
    }

    let scope = SessionScope {
        root_project_id: engramdb::storage::project_id::compute_project_id(&paths[0]),
        root_dir: paths[0].clone(),
        paths,
    };
    (root, scope)
}

fn bench_evidence_links(c: &mut Criterion) {
    isolate_data_dir();
    let rt = runtime();
    let mut group = c.benchmark_group("evidence_links");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(20);

    // A scope is the root project plus its registered sub-projects and
    // worktrees: 2 is the common case, 32 a machine that links a lot.
    for &projects in &[2usize, 8, 32] {
        let (root, scope) = rt.block_on(seed_scope(projects, 50, 5));

        group.bench_with_input(
            BenchmarkId::new("serial_PREVIOUS", projects),
            &projects,
            |b, _| {
                b.to_async(&rt)
                    .iter(|| async { evidence_links_previous(&scope).await });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("concurrent_SHIPPED", projects),
            &projects,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    harvest_pin::evidence_links(&scope)
                        .await
                        .expect("evidence_links")
                });
            },
        );

        drop(root);
    }

    group.finish();
}

// ===========================================================================
// Group 3: listing the transcripts in a scope
// ===========================================================================

/// One realistic transcript: `turns` prompt/reply/tool-result triples, which
/// is what `summarize_session` has to deserialize in full to count them.
fn write_transcript(path: &Path, cwd: &str, turns: usize) {
    let mut f = std::fs::File::create(path).expect("create transcript");
    for t in 0..turns {
        let lines = [
            serde_json::json!({
                "type": "user", "cwd": cwd, "gitBranch": "main",
                "timestamp": "2026-08-01T10:00:00Z",
                "message": {"role": "user", "content":
                    format!("Turn {t}: the reindex path rebuilds the memories table from the \
                             .md files, so a relation held only in LanceDB does not survive it.")},
            }),
            serde_json::json!({
                "type": "assistant", "timestamp": "2026-08-01T10:01:00Z",
                "message": {"role": "assistant", "content": [
                    {"type": "text", "text": format!("Reply {t}: the schema version in the \
                        manifest is what triggers the backfill on open.")},
                    {"type": "tool_use", "id": format!("t{t}a"), "name": "Bash",
                     "input": {"command": "cargo nextest run --workspace"}},
                    {"type": "tool_use", "id": format!("t{t}b"), "name": "Read",
                     "input": {"file_path": "/repo/src/lib.rs"}},
                ]},
            }),
            serde_json::json!({
                "type": "user", "timestamp": "2026-08-01T10:02:00Z",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": format!("t{t}a"), "is_error": true,
                     "content": "error: could not find protoc"},
                    {"type": "tool_result", "tool_use_id": format!("t{t}b"), "is_error": false,
                     "content": "pub fn main() {}"},
                ]},
            }),
        ];
        for line in lines {
            writeln!(f, "{line}").expect("write");
        }
    }
}

/// A fake `~/.claude/projects` root holding `sessions` transcripts.
fn seed_transcripts(sessions: usize, turns: usize) -> TempDir {
    let root = TempDir::new().expect("tempdir");
    let dir = root.path().join("-repo-bench");
    std::fs::create_dir_all(&dir).expect("mkdir");
    for s in 0..sessions {
        write_transcript(
            &dir.join(format!("session-{s:04}.jsonl")),
            "/repo/bench",
            turns,
        );
    }
    root
}

/// The shape `list_sessions_in` had: `summarize_session` per file, in
/// sequence.
///
/// **Local copy of a replaced implementation — not production code.** Driven
/// with no project filter, which is what the shipped arm below is given too,
/// so the two do the same work.
fn list_sessions_serial_previous(root: &Path) -> Vec<transcripts::SessionSummary> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root).expect("read_dir").flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&dir).expect("read_dir").flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(summary) = transcripts::summarize_session(&path) else {
                continue;
            };
            out.push(summary);
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.ended_at));
    out
}

fn bench_list_sessions(c: &mut Criterion) {
    let mut group = c.benchmark_group("list_sessions");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(20);

    // 16 sessions is a week of work; 128 is what accumulates before Claude
    // Code prunes. 20 turns each keeps every file well under the 4 MiB record
    // cap while still being a real parse.
    for &sessions in &[16usize, 64, 128] {
        let root = seed_transcripts(sessions, 20);
        let path = root.path().to_path_buf();

        group.bench_with_input(
            BenchmarkId::new("serial_PREVIOUS", sessions),
            &sessions,
            |b, _| {
                b.iter(|| list_sessions_serial_previous(&path));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("rayon_SHIPPED", sessions),
            &sessions,
            |b, _| {
                b.iter(|| transcripts::list_sessions_in(&path, &[]).expect("list_sessions_in"));
            },
        );

        drop(root);
    }

    group.finish();
}

// ===========================================================================
// Group 4: the machine-wide conversation search
// ===========================================================================

/// Build `projects` registered projects, each with a conversation index
/// holding `per_project` rows, plus the registry that names them.
///
/// The rows are written through `harvest_index::index_session` so the vectors
/// come from the same provider the search will use.
async fn seed_conversation_indexes(
    projects: usize,
    per_project: usize,
    engine: &RetrievalEngine,
    dimensions: usize,
) -> (TempDir, engramdb::storage::Registry, Vec<String>) {
    let root = TempDir::new().expect("tempdir");
    let registry = InMemoryRegistry::new();
    let mut roots = Vec::new();

    for p in 0..projects {
        let dir = root.path().join(format!("project-{p}"));
        let transcripts_dir = dir.join("transcripts");
        std::fs::create_dir_all(&transcripts_dir).expect("mkdir");
        MemoryStore::init(&dir, &registry).await.expect("init");
        let root_id = engramdb::storage::project_id::compute_project_id(&dir);
        let scope = SessionScope {
            root_project_id: root_id.clone(),
            root_dir: dir.clone(),
            paths: vec![dir.clone()],
        };
        let index = ConversationIndex::open(&root_id, dimensions)
            .await
            .expect("open index");
        for s in 0..per_project {
            let session = format!("p{p}-s{s}");
            let path = transcripts_dir.join(format!("{session}.jsonl"));
            write_transcript(&path, dir.to_str().expect("utf8"), 4);
            harvest_index::index_transcript(&scope, &index, engine, &session, &path, true)
                .await
                .expect("index");
        }
        roots.push(root_id);
    }

    use engramdb::storage::RegistryBackend;
    let data = registry.load().await.expect("load registry");
    (root, data, roots)
}

/// The shape the CLI and the MCP tool both had: open each project's index in
/// sequence, and re-embed the query inside every per-project `search`.
///
/// **Local copy of a replaced implementation — not production code.**
async fn search_fanin_previous(
    registry: &engramdb::storage::Registry,
    own_root_id: &str,
    engine: &RetrievalEngine,
    dimensions: usize,
    query: &str,
    limit: usize,
) -> usize {
    let mut seen = vec![own_root_id.to_string()];
    let mut hits = Vec::new();
    for entry in &registry.projects {
        let root = engramdb::storage::resolve_root_project_id(registry, &entry.project_id);
        if seen.contains(&root) || !ConversationIndex::exists(&root) {
            continue;
        }
        seen.push(root.clone());
        let Ok(index) = ConversationIndex::open(&root, dimensions).await else {
            continue;
        };
        // The re-embed is the point: this is `harvest_index::search`, which
        // embeds `query` before every per-project lookup.
        if let Ok(found) = harvest_index::search(&index, engine, query, limit, None).await {
            hits.extend(found);
        }
    }
    hits.len()
}

#[cfg(feature = "onnxruntime")]
fn bench_search_fanin(c: &mut Criterion) {
    use engramdb::embeddings::OnnxProvider;

    isolate_data_dir();
    // The real model, not a stub: the whole claim being measured is that the
    // query was embedded once per project, and a stub embedder makes that
    // free. Skipped with a notice when the model or the runtime is missing,
    // so a bench run never fails on a machine without them.
    let Some(provider) = OnnxProvider::try_new() else {
        eprintln!("search_fanin: embedding model unavailable, skipping");
        return;
    };
    let dimensions = {
        use engramdb::embeddings::EmbeddingProvider;
        provider.dimensions()
    };

    let rt = runtime();
    let engine_dir = TempDir::new().expect("tempdir");
    let engine = rt.block_on(async {
        let store = MemoryStore::init(engine_dir.path(), &InMemoryRegistry::new())
            .await
            .expect("init");
        RetrievalEngine::new(store, EngramConfig::default())
            .with_embedding_provider(Arc::new(provider))
    });

    let mut group = c.benchmark_group("search_fanin");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &projects in &[4usize, 16] {
        let (root, registry, roots) =
            rt.block_on(seed_conversation_indexes(projects, 4, &engine, dimensions));
        let own = roots[0].clone();

        group.bench_with_input(
            BenchmarkId::new("serial_reembed_PREVIOUS", projects),
            &projects,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    search_fanin_previous(
                        &registry,
                        &own,
                        &engine,
                        dimensions,
                        "protoc build failure",
                        10,
                    )
                    .await
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("concurrent_embed_once_SHIPPED", projects),
            &projects,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    harvest_index::search_other_projects(
                        &registry,
                        &own,
                        &engine,
                        dimensions,
                        "protoc build failure",
                        10,
                        None,
                    )
                    .await
                    .expect("search_other_projects")
                });
            },
        );

        drop(root);
    }

    group.finish();
}

#[cfg(not(feature = "onnxruntime"))]
fn bench_search_fanin(_c: &mut Criterion) {}

// ===========================================================================
// Group 5: what share of `harvest index --all` is the embedding call
// ===========================================================================

/// A provider that answers instantly with a fixed vector.
///
/// Not a candidate implementation — a *subtractor*. Running `index_sessions`
/// against it and against the real model gives the embedding share of the
/// pass, which is the ceiling on what batching the embeddings could recover.
struct NullEmbedder(usize);

#[async_trait::async_trait]
impl engramdb::embeddings::EmbeddingProvider for NullEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.1; self.0])
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(vec![vec![0.1; self.0]; texts.len()])
    }
    fn dimensions(&self) -> usize {
        self.0
    }
    fn model_id(&self) -> String {
        "null-bench".into()
    }
    fn max_tokens(&self) -> usize {
        512
    }
}

/// A scope whose transcripts live in a directory the bench controls, with the
/// session ids that `index_sessions` will be handed.
fn seed_index_scope(sessions: usize, turns: usize) -> (TempDir, SessionScope, Vec<String>) {
    let root = TempDir::new().expect("tempdir");
    let dir = root.path().join("project");
    let transcripts_dir = dir.join("transcripts");
    std::fs::create_dir_all(&transcripts_dir).expect("mkdir");
    let mut ids = Vec::new();
    for s in 0..sessions {
        let session = format!("session-{s:04}");
        write_transcript(
            &transcripts_dir.join(format!("{session}.jsonl")),
            dir.to_str().expect("utf8"),
            turns,
        );
        ids.push(session);
    }
    let scope = SessionScope {
        root_project_id: engramdb::storage::project_id::compute_project_id(&dir),
        root_dir: dir.clone(),
        paths: vec![dir],
    };
    (root, scope, ids)
}

/// Drive the whole `harvest index --all` body for one provider.
async fn index_all_with(
    scope: &SessionScope,
    root: &Path,
    engine: &RetrievalEngine,
    dimensions: usize,
    ids: &[String],
) {
    let index = ConversationIndex::open_at(&root.join("lancedb"), dimensions)
        .await
        .expect("open index");
    for id in ids {
        let path = scope
            .root_dir
            .join("transcripts")
            .join(format!("{id}.jsonl"));
        harvest_index::index_transcript(scope, &index, engine, id, &path, true)
            .await
            .expect("index");
    }
}

#[cfg(feature = "onnxruntime")]
fn bench_index_embed_share(c: &mut Criterion) {
    use engramdb::embeddings::{EmbeddingProvider, OnnxProvider};

    isolate_data_dir();
    let Some(provider) = OnnxProvider::try_new() else {
        eprintln!("index_embed_share: embedding model unavailable, skipping");
        return;
    };
    let dimensions = provider.dimensions();
    let rt = runtime();

    let engine_dir = TempDir::new().expect("tempdir");
    let (real_engine, null_engine) = rt.block_on(async {
        let a = MemoryStore::init(&engine_dir.path().join("a"), &InMemoryRegistry::new())
            .await
            .expect("init");
        let b = MemoryStore::init(&engine_dir.path().join("b"), &InMemoryRegistry::new())
            .await
            .expect("init");
        (
            RetrievalEngine::new(a, EngramConfig::default())
                .with_embedding_provider(Arc::new(provider)),
            RetrievalEngine::new(b, EngramConfig::default())
                .with_embedding_provider(Arc::new(NullEmbedder(dimensions))),
        )
    });

    let mut group = c.benchmark_group("index_embed_share");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &sessions in &[8usize, 32] {
        group.bench_with_input(
            BenchmarkId::new("real_embedder", sessions),
            &sessions,
            |b, _| {
                b.iter_batched(
                    || {
                        let (root, scope, ids) = seed_index_scope(sessions, 20);
                        let db = TempDir::new().expect("tempdir");
                        (root, db, scope, ids)
                    },
                    |(root, db, scope, ids)| {
                        rt.block_on(index_all_with(
                            &scope,
                            db.path(),
                            &real_engine,
                            dimensions,
                            &ids,
                        ));
                        (root, db)
                    },
                    BatchSize::PerIteration,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("null_embedder", sessions),
            &sessions,
            |b, _| {
                b.iter_batched(
                    || {
                        let (root, scope, ids) = seed_index_scope(sessions, 20);
                        let db = TempDir::new().expect("tempdir");
                        (root, db, scope, ids)
                    },
                    |(root, db, scope, ids)| {
                        rt.block_on(index_all_with(
                            &scope,
                            db.path(),
                            &null_engine,
                            dimensions,
                            &ids,
                        ));
                        (root, db)
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

#[cfg(not(feature = "onnxruntime"))]
fn bench_index_embed_share(_c: &mut Criterion) {}

// ===========================================================================
// Group 6: `harvest index --all`, end to end
// ===========================================================================

/// A scope whose transcripts sit where Claude Code would have put them, so
/// `index_sessions` resolves them through the real `locate`.
///
/// `seed_index_scope` deliberately does not do this — it hands paths straight
/// to `index_transcript` to isolate the embedding share from the lookup. This
/// one measures the command as a user runs it, lookup included.
fn seed_claude_home(
    claude: &Path,
    sessions: usize,
    turns: usize,
) -> (TempDir, SessionScope, Vec<String>) {
    let root = TempDir::new().expect("tempdir");
    let dir = root.path().join("project");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dir = dir.canonicalize().expect("canonicalize");

    let encoded = transcripts::encode_project_dir(&dir);
    let tx_dir = claude.join("projects").join(&encoded);
    // A fresh subdirectory per fixture would be ideal, but `list_sessions_for`
    // walks the whole projects root — so the previous iteration's transcripts
    // are cleared instead, or the Nth iteration would list N times the
    // transcripts and the sweep would measure the fixture, not the code.
    let _ = std::fs::remove_dir_all(&tx_dir);
    std::fs::create_dir_all(&tx_dir).expect("mkdir");

    let mut ids = Vec::new();
    for s in 0..sessions {
        let session = format!("session-{s:04}");
        write_transcript(
            &tx_dir.join(format!("{session}.jsonl")),
            dir.to_str().expect("utf8"),
            turns,
        );
        ids.push(session);
    }
    let scope = SessionScope {
        root_project_id: engramdb::storage::project_id::compute_project_id(&dir),
        root_dir: dir.clone(),
        paths: vec![dir],
    };
    (root, scope, ids)
}

/// The shape `index_sessions` had: `index_session` per id, each of which calls
/// `locate` — and `locate` lists (and therefore re-parses) every transcript
/// under the scope, every time.
///
/// **Local copy of a replaced implementation — not production code.** The
/// per-session `index_session` it calls is still production, so what this
/// isolates is exactly the loop that was hoisted.
async fn index_all_previous(
    scope: &SessionScope,
    index: &ConversationIndex,
    engine: &RetrievalEngine,
    ids: &[String],
) {
    for id in ids {
        let _ = harvest_index::index_session(scope, index, engine, id, true).await;
    }
}

#[cfg(feature = "onnxruntime")]
fn bench_index_all(c: &mut Criterion) {
    use engramdb::embeddings::{EmbeddingProvider, OnnxProvider};

    let bench_root = isolate_data_dir();
    let claude = bench_root.join("claude");
    let Some(provider) = OnnxProvider::try_new() else {
        eprintln!("index_all: embedding model unavailable, skipping");
        return;
    };
    let dimensions = provider.dimensions();
    let rt = runtime();

    let engine_dir = TempDir::new().expect("tempdir");
    let engine = rt.block_on(async {
        let store = MemoryStore::init(engine_dir.path(), &InMemoryRegistry::new())
            .await
            .expect("init");
        RetrievalEngine::new(store, EngramConfig::default())
            .with_embedding_provider(Arc::new(provider))
    });

    let mut group = c.benchmark_group("index_all");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    for &sessions in &[8usize, 32] {
        let mut arm = |name: &str, batched: bool| {
            group.bench_with_input(BenchmarkId::new(name, sessions), &sessions, |b, _| {
                b.iter_batched(
                    || {
                        let (root, scope, ids) = seed_claude_home(&claude, sessions, 20);
                        let db = TempDir::new().expect("tempdir");
                        let index = rt
                            .block_on(ConversationIndex::open_at(
                                &db.path().join("db"),
                                dimensions,
                            ))
                            .expect("open index");
                        (root, db, scope, ids, index)
                    },
                    |(root, db, scope, ids, index)| {
                        if batched {
                            rt.block_on(harvest_index::index_sessions(
                                &scope, &index, &engine, &ids, true,
                            ))
                            .expect("index_sessions");
                        } else {
                            rt.block_on(index_all_previous(&scope, &index, &engine, &ids));
                        }
                        (root, db)
                    },
                    BatchSize::PerIteration,
                );
            });
        };
        arm("per_session_locate_and_embed_PREVIOUS", false);
        arm("one_listing_one_batch_SHIPPED", true);
    }

    group.finish();
}

#[cfg(not(feature = "onnxruntime"))]
fn bench_index_all(_c: &mut Criterion) {}

criterion_group!(
    benches,
    bench_link_memories,
    bench_evidence_links,
    bench_list_sessions,
    bench_search_fanin,
    bench_index_embed_share,
    bench_index_all
);
criterion_main!(benches);
