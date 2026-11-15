//! Pins the whole output of text queries, so a change to how the engine
//! *reaches* its scores (which rows it loads, from where) can be proven not to
//! change what it returns.
//!
//! The golden file was captured from the engine before any of the query-path
//! performance work. Every query in the matrix is rendered to text — id order,
//! `score.to_bits()`, the whole `ScoreBreakdown`, `total`,
//! `retrieval_quality`, `dropped_below_threshold`, and whether each returned
//! memory carries content and details — and compared byte for byte.
//!
//! Scores are independent of the clock: every timestamp is fixed in the past,
//! and each decay shape is one whose factor is constant at that age (a step
//! not yet reached, a linear or exponential decay already at its floor).
//!
//! Regenerate (only when a change is *meant* to alter output) with
//! `ENGRAMDB_PIN_REGEN=1 cargo nextest run -p engramdb --lib -E
//! 'test(text_query_output_is_pinned)'`.

use super::*;
use crate::retrieval::reranker::{RerankScore, Reranker};
use crate::storage::InMemoryRegistry;
use crate::types::{
    Challenge, Decay, DecayStrategy, Link, LinkKind, LinkOrigin, LinkStatus, Memory, MemoryType,
    Provenance, Visibility,
};
use chrono::TimeZone;
use std::fmt::Write as _;

const GOLDEN_PATH: &str = "src/retrieval/testdata/text_query_pinned.golden";
const DIMS: usize = 384;

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Deterministic vectors from a hash of the text, with a keyword component so
/// semantic neighbours are not pure noise.
struct HashEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for HashEmbedding {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let lower = text.to_lowercase();
        let mut v: Vec<f32> = (0..DIMS)
            .map(|i| {
                let h = fnv(format!("{i}:{lower}").as_bytes());
                ((h % 1000) as f32 / 1000.0) - 0.5
            })
            .collect();
        for (i, w) in WORDS.iter().enumerate() {
            if lower.contains(w) {
                v[i % DIMS] += 1.0;
            }
        }
        Ok(v)
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }
    fn dimensions(&self) -> usize {
        DIMS
    }
    fn max_tokens(&self) -> usize {
        256
    }
    fn model_id(&self) -> String {
        "onnx/hash-pin-stub".to_string()
    }
}

/// Scores a document from a hash of (query, document).
struct HashReranker;

#[async_trait::async_trait]
impl Reranker for HashReranker {
    async fn rerank(&self, query: &str, documents: &[String]) -> anyhow::Result<Vec<RerankScore>> {
        Ok(documents
            .iter()
            .enumerate()
            .map(|(index, d)| RerankScore {
                index,
                score: ((fnv(format!("{query}|{d}").as_bytes()) % 2000) as f32 / 100.0) - 10.0,
            })
            .collect())
    }
}

const WORDS: [&str; 12] = [
    "token", "refresh", "rotation", "cache", "index", "daemon", "lancedb", "parser", "retry",
    "timeout", "schema", "hook",
];
const SCOPES: [&str; 5] = ["auth.oauth", "auth", "storage.index", "daemon", "hooks.pre"];
const PATHS: [&str; 4] = [
    "src/auth/token.rs",
    "src/storage/index.rs",
    "src/daemon/server.rs",
    "src/hooks/mod.rs",
];
const TYPES: [MemoryType; 5] = [
    MemoryType::Decision,
    MemoryType::Hazard,
    MemoryType::Convention,
    MemoryType::Context,
    MemoryType::Decision,
];

fn id(n: usize) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

/// ~80 memories: above `PARALLEL_SCORE_MIN`, so the rayon branch runs.
async fn fixture() -> (tempfile::TempDir, MemoryStore) {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let t0 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    let mut memories = Vec::new();
    for n in 0..80usize {
        let w = |k: usize| WORDS[(n * 7 + k * 5) % WORDS.len()];
        let summary = format!("{} {} {} note {n}", w(0), w(1), w(2));
        let content = format!(
            "The {} path uses {} and {}; see item {n}.",
            w(3),
            w(4),
            w(5)
        );
        let mut m = Memory::new(
            TYPES[n % TYPES.len()],
            &summary,
            content,
            Provenance::human(),
        );
        m.id = id(n);
        m.visibility = Visibility::Shared;
        m.criticality = 0.3 + (n % 7) as f64 * 0.1;
        m.confidence = 0.5 + (n % 5) as f64 * 0.1;
        m.logical = vec![SCOPES[n % SCOPES.len()].to_string()];
        m.physical = vec![PATHS[n % PATHS.len()].to_string()];
        m.tags = vec![w(6).to_string()];
        if n % 9 == 0 {
            m.details = Some(format!("details for {n}"));
        }
        m.decay = match n % 5 {
            0 => None,
            // Not yet reached: factor 1.0.
            1 => Some(Decay {
                strategy: DecayStrategy::Step,
                half_life: None,
                ttl: Some(chrono::Duration::days(3650 * 3)),
                floor: 0.0,
            }),
            // Long past: the floor.
            2 => Some(Decay {
                strategy: DecayStrategy::Step,
                half_life: None,
                ttl: Some(chrono::Duration::days(1)),
                floor: 0.4,
            }),
            3 => Some(Decay {
                strategy: DecayStrategy::Linear,
                half_life: None,
                ttl: Some(chrono::Duration::days(30)),
                floor: 0.3,
            }),
            _ => Some(Decay {
                strategy: DecayStrategy::Exponential,
                half_life: Some(chrono::Duration::hours(1)),
                ttl: None,
                floor: 0.25,
            }),
        };
        let at = t0 + chrono::Duration::hours(n as i64);
        m.created_at = at;
        m.updated_at = at;
        m.accessed_at = at;
        memories.push(m);
    }
    // One challenged, one expired, one invalidated.
    memories[4].challenges = vec![Challenge {
        timestamp: t0,
        agent_id: None,
        evidence: "it changed".to_string(),
        source_file: None,
    }];
    memories[5].expires_at = Some(t0 + chrono::Duration::days(2));
    memories[6].invalidated_at = Some(t0 + chrono::Duration::days(3));
    // Edges for expansion.
    let link = |to: usize, kind: LinkKind| {
        let mut l = Link::new(id(to), kind, LinkOrigin::Human);
        l.status = LinkStatus::Confirmed;
        l
    };
    memories[10].links = vec![link(0, LinkKind::Supports)];
    memories[11].links = vec![link(0, LinkKind::RelatedTo)];
    memories[12].links = vec![link(1, LinkKind::DerivedFrom)];
    memories[13].links = vec![link(2, LinkKind::RelatedTo)];
    memories[20].links = vec![
        link(3, LinkKind::Supports),
        link(7, LinkKind::RelatedTo),
        link(33, LinkKind::RelatedTo),
    ];
    // From frequent hits to memories that are rarely hits, so expansion adds.
    memories[41].links = vec![link(60, LinkKind::DerivedFrom)];
    memories[55].links = vec![link(61, LinkKind::RelatedTo)];
    memories[62].links = vec![link(76, LinkKind::Supports)];
    for m in &memories {
        store.create(m).await.unwrap();
    }
    let engine = RetrievalEngine::new(store.clone(), EngramConfig::default())
        .with_embedding_provider(Arc::new(HashEmbedding));
    let (embedded, failed) = engine.embed_memories(&memories).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(embedded, memories.len());
    // Two rows on the stems fallback.
    store
        .null_keyword_stems_for_test(&[id(1), id(2)])
        .await
        .unwrap();
    (tmp, store)
}

fn render(out: &mut String, label: &str, r: &RetrievalResult) {
    writeln!(
        out,
        "## {label}\ntotal={} quality={} dropped={}",
        r.total, r.retrieval_quality, r.dropped_below_threshold
    )
    .unwrap();
    for sm in &r.memories {
        writeln!(
            out,
            "{} {} content={} details={} {}",
            sm.memory.id,
            sm.score.to_bits(),
            !sm.memory.content.is_empty(),
            sm.memory.details.is_some(),
            serde_json::to_string(&sm.score_breakdown).unwrap()
        )
        .unwrap();
    }
}

#[tokio::test]
async fn text_query_output_is_pinned() {
    let (_tmp, store) = fixture().await;
    let keyword = RetrievalEngine::new(store.clone(), EngramConfig::default());
    let semantic = RetrievalEngine::new(store.clone(), EngramConfig::default())
        .with_embedding_provider(Arc::new(HashEmbedding));
    let mut rerank_config = EngramConfig::default();
    rerank_config.rerank.enabled = true;
    let reranked = RetrievalEngine::new(store.clone(), rerank_config)
        .with_embedding_provider(Arc::new(HashEmbedding))
        .with_reranker(Arc::new(HashReranker));
    let engines = [
        ("keyword", &keyword),
        ("semantic", &semantic),
        ("rerank", &reranked),
    ];

    let mut out = String::new();
    for text in ["token refresh rotation", "lancedb index cache"] {
        for mode in [RetrievalMode::Filter, RetrievalMode::Rank] {
            for (name, engine) in engines {
                for max_results in [3usize, 50] {
                    for detail in [
                        DetailLevel::Summary,
                        DetailLevel::Content,
                        DetailLevel::Full,
                    ] {
                        for expand in [0usize, 2] {
                            for include_expired in [false, true] {
                                let q = RetrievalQuery {
                                    mode,
                                    query: Some(text.to_string()),
                                    max_results: Some(max_results),
                                    detail_level: detail,
                                    expand_links: Some(expand),
                                    include_expired: Some(include_expired),
                                    ..Default::default()
                                };
                                let r = engine.query(&q).await.unwrap();
                                let label = format!(
                                    "{text:?} {mode:?} {name} max={max_results} \
                                     {detail:?} expand={expand} expired={include_expired}"
                                );
                                render(&mut out, &label, &r);
                            }
                        }
                    }
                }
            }
        }
    }

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_PATH);
    if std::env::var_os("ENGRAMDB_PIN_REGEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &out).unwrap();
        return;
    }
    let golden = std::fs::read_to_string(&path).expect("golden file (see module docs)");
    if golden != out {
        let first = golden
            .lines()
            .zip(out.lines())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| golden.lines().count().min(out.lines().count()));
        let context = |s: &str| {
            s.lines()
                .skip(first.saturating_sub(3))
                .take(6)
                .collect::<Vec<_>>()
                .join("\n")
        };
        panic!(
            "text query output changed at line {}\n--- golden\n{}\n--- now\n{}",
            first + 1,
            context(&golden),
            context(&out)
        );
    }
}

fn files_requested() -> usize {
    super::FILES_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

fn text_filter(text: &str, max_results: usize) -> RetrievalQuery {
    RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some(text.to_string()),
        max_results: Some(max_results),
        expand_links: Some(0),
        ..Default::default()
    }
}

/// A text query scores from the index and reads only the files it returns,
/// plus the rows whose stems it has to derive from the file.
#[tokio::test]
async fn text_query_reads_only_survivor_files() {
    let (_tmp, store) = fixture().await;
    let engine = RetrievalEngine::new(store, EngramConfig::default());
    let before = files_requested();
    let r = engine
        .query(&text_filter("token refresh rotation", 3))
        .await
        .unwrap();
    assert_eq!(r.memories.len(), 3);
    assert!(r.total > 3, "more candidates than survivors: {}", r.total);
    let fallback = [id(1), id(2)];
    let survivors_beyond_fallback = r
        .memories
        .iter()
        .filter(|sm| !fallback.contains(&sm.memory.id))
        .count();
    assert_eq!(
        files_requested() - before,
        fallback.len() + survivors_beyond_fallback,
        "the two stem-less rows and the survivors, nothing else"
    );
}

/// A survivor whose file has gone (a drifted store) is dropped, uncounted,
/// and replaced by the next candidate, so the answer is not short.
#[tokio::test]
async fn text_query_refills_when_a_survivor_file_is_missing() {
    let (tmp, store) = fixture().await;
    let engine = RetrievalEngine::new(store, EngramConfig::default());
    let q = text_filter("token refresh rotation", 3);
    let full = engine.query(&q).await.unwrap();
    let ids: Vec<String> = full.memories.iter().map(|m| m.memory.id.clone()).collect();
    let wider = engine
        .query(&text_filter("token refresh rotation", 4))
        .await
        .unwrap();
    let fourth = wider.memories[3].memory.id.clone();

    let path = crate::storage::paths::memory_path(tmp.path(), &ids[0])
        .await
        .expect("memory file");
    std::fs::remove_file(path).unwrap();

    let r = engine.query(&q).await.unwrap();
    let got: Vec<String> = r.memories.iter().map(|m| m.memory.id.clone()).collect();
    assert_eq!(got, vec![ids[1].clone(), ids[2].clone(), fourth]);
    assert_eq!(r.total, full.total - 1, "the missing file is not counted");
}

/// A row with no stored stems still matches: its stems come from the file.
#[tokio::test]
async fn text_query_falls_back_to_the_file_for_rows_without_stems() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut m = Memory::new(
        MemoryType::Decision,
        "zebra crossings need a stem",
        "only the file knows this word",
        Provenance::human(),
    );
    m.id = id(900);
    store.create(&m).await.unwrap();
    store.null_keyword_stems_for_test(&[id(900)]).await.unwrap();
    assert!(store
        .list_for_filtering()
        .await
        .unwrap()
        .iter()
        .all(|e| e.keyword_stems.is_none()));

    let engine = RetrievalEngine::new(store, EngramConfig::default());
    let r = engine.query(&text_filter("zebra", 5)).await.unwrap();
    assert_eq!(r.memories.len(), 1, "{r:?}");
    assert_eq!(r.memories[0].memory.id, id(900));
    assert_eq!(r.retrieval_quality, "keyword_only");
}

/// `skip_load` returns the unskipped result minus the skipped ids — same
/// order, same scores, same `total` — on both the text and the scope-only
/// path, and the text path reads none of the skipped files.
#[tokio::test]
async fn skip_load_keeps_rank_positions() {
    let (_tmp, store) = fixture().await;
    let engine = RetrievalEngine::new(store, EngramConfig::default());
    let scope_only = RetrievalQuery {
        mode: RetrievalMode::Rank,
        path: Some(PATHS[0].to_string()),
        max_results: Some(10),
        expand_links: Some(0),
        rank_fallback: Some(0),
        ..Default::default()
    };
    for base in [text_filter("token refresh rotation", 10), scope_only] {
        let before = files_requested();
        let full = engine.query(&base).await.unwrap();
        let read_full = files_requested() - before;
        assert!(full.memories.len() >= 4, "{}", full.memories.len());
        let skip: std::collections::HashSet<String> = [0usize, 2]
            .iter()
            .map(|&i| full.memories[i].memory.id.clone())
            .collect();
        let before = files_requested();
        let skipped = engine
            .query(&RetrievalQuery {
                skip_load: Some(skip.clone()),
                ..base.clone()
            })
            .await
            .unwrap();
        let read = files_requested() - before;
        let expect: Vec<(String, u64)> = full
            .memories
            .iter()
            .filter(|sm| !skip.contains(&sm.memory.id))
            .map(|sm| (sm.memory.id.clone(), sm.score.to_bits()))
            .collect();
        let got: Vec<(String, u64)> = skipped
            .memories
            .iter()
            .map(|sm| (sm.memory.id.clone(), sm.score.to_bits()))
            .collect();
        assert_eq!(got, expect, "a skipped id gave its place to another");
        assert_eq!(skipped.total, full.total);
        assert_eq!(
            skipped.dropped_below_threshold,
            full.dropped_below_threshold
        );
        if base.query.is_some() {
            // Two fewer reads: the stem-less rows' files are read either way.
            assert_eq!(read, read_full - 2, "skipped files were read");
        }
    }
}
