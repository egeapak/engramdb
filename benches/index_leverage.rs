//! Benchmarks for the three LanceDB "leverage" options.
//!
//! Each group answers one question with a measurement instead of an opinion:
//!
//! 1. `leverage_scalar_index` — does building Bitmap/BTree indexes on the
//!    columns the retrieval pushdown filters on make the pushdown faster?
//! 2. `leverage_typed_filter` — does swapping the hand-escaped SQL predicate
//!    for a typed Datafusion expression cost anything at query time?
//! 3. `leverage_tag_pushdown` — tags are filtered in Rust today. Does pushing
//!    them into LanceDB (with an FM substring index) beat that?
//! 4. `leverage_keyword` — what does the current in-Rust keyword scorer
//!    actually cost, i.e. how much is on the table for an FTS swap?
//!
//! Seeding is untimed and shared per group. Every group runs at
//! [`SCALE_COUNT`], matching `benchmarks.rs`'s `scale_1k` group so numbers are
//! comparable across the two files.
//!
//! Run with: `cargo bench --bench index_leverage`

mod helpers;

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

use engramdb::retrieval::filters::{build_filter_expr, build_filter_predicate};
use engramdb::retrieval::{apply_index_filters, SearchFilters};
use engramdb::search::{keyword_search, keyword_search_stems};
use engramdb::storage::lance_index::LanceIndex;
use engramdb::storage::MemoryStore;
use engramdb::types::{KeywordStems, Memory, MemoryType};

use chrono::Utc;
use helpers::{generate_memory, setup_store};

/// Same scale as `benchmarks.rs::scale_1k`, so the two files' numbers line up.
const SCALE_COUNT: usize = 1_000;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to create tokio runtime")
}

/// The pushdown predicate the retrieval engine actually builds for a typical
/// filtered query: two low-cardinality enum `IN`s, a criticality floor, and
/// the two liveness range checks.
fn bench_filter_args() -> (Vec<MemoryType>, f64, chrono::DateTime<chrono::Utc>) {
    (
        vec![MemoryType::Decision, MemoryType::Convention],
        0.6,
        Utc::now(),
    )
}

fn sql_predicate() -> Option<String> {
    let (types, min_crit, now) = bench_filter_args();
    build_filter_predicate(Some(&types), None, Some(min_crit), Some(now), Some(now))
}

fn expr_predicate() -> Option<lancedb::expr::DfExpr> {
    let (types, min_crit, now) = bench_filter_args();
    build_filter_expr(Some(&types), None, Some(min_crit), Some(now), Some(now))
}

/// Seed a store of `SCALE_COUNT` memories and compact it, so timed scans
/// measure steady state rather than accumulated table versions.
async fn seeded_store() -> (tempfile::TempDir, MemoryStore) {
    let (td, store) = setup_store(SCALE_COUNT).await;
    store.optimize().await.expect("failed to optimize");
    (td, store)
}

// ===========================================================================
// Group 1: do scalar indexes speed up the retrieval pushdown?
// ===========================================================================

fn leverage_scalar_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("leverage_scalar_index");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));
    let rt = runtime();

    // Two independent stores with identical contents: one left as the code
    // ships today (vector index only), one with the scalar plan built.
    let (_td_plain, plain) = rt.block_on(seeded_store());
    let (_td_indexed, indexed) = rt.block_on(async {
        let (td, s) = seeded_store().await;
        let built = s
            .create_scalar_indexes()
            .await
            .expect("failed to build scalar indexes");
        assert_eq!(
            built.len(),
            LanceIndex::SCALAR_INDEX_PLAN.len(),
            "expected every planned column to be indexed on a fresh store"
        );
        s.optimize().await.expect("failed to optimize after index");
        (td, s)
    });

    let predicate = sql_predicate();
    assert!(predicate.is_some(), "bench predicate must be non-trivial");

    group.bench_function("pushdown_no_index", |b| {
        b.to_async(&rt).iter(|| async {
            plain
                .list_for_filtering_where(predicate.clone())
                .await
                .unwrap()
        });
    });

    group.bench_function("pushdown_with_scalar_index", |b| {
        b.to_async(&rt).iter(|| async {
            indexed
                .list_for_filtering_where(predicate.clone())
                .await
                .unwrap()
        });
    });

    // Control: the unfiltered full scan both paths degrade to when the query
    // carries no scalar signal. An index cannot help here — if it appears to,
    // the numbers above are noise.
    group.bench_function("full_scan_control", |b| {
        b.to_async(&rt)
            .iter(|| async { plain.list_for_filtering().await.unwrap() });
    });

    group.finish();
}

// ===========================================================================
// Group 2: does the typed predicate cost anything vs the SQL string?
// ===========================================================================

fn leverage_typed_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("leverage_typed_filter");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));
    let rt = runtime();

    let (_td, store) = rt.block_on(seeded_store());

    let sql = sql_predicate();
    let expr = expr_predicate();
    assert!(expr.is_some(), "expr predicate must be non-trivial");

    // Predicate construction only — the part that differs on the hot path per
    // query. Both are trivial, but a regression here would be silent.
    group.bench_function("build_sql_predicate", |b| {
        b.iter(|| std::hint::black_box(sql_predicate()));
    });
    group.bench_function("build_expr_predicate", |b| {
        b.iter(|| std::hint::black_box(expr_predicate()));
    });

    // End-to-end: same logical filter, two encodings.
    group.bench_function("query_via_sql_string", |b| {
        b.to_async(&rt)
            .iter(|| async { store.list_for_filtering_where(sql.clone()).await.unwrap() });
    });
    group.bench_function("query_via_typed_expr", |b| {
        b.to_async(&rt).iter(|| async {
            store
                .list_for_filtering_where_expr(expr.clone().unwrap())
                .await
                .unwrap()
        });
    });

    group.finish();
}

// ===========================================================================
// Group 3: tag filtering — Rust-side today vs pushed down with an FM index
// ===========================================================================

fn leverage_tag_pushdown(c: &mut Criterion) {
    let mut group = c.benchmark_group("leverage_tag_pushdown");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));
    let rt = runtime();

    let (_td_plain, plain) = rt.block_on(seeded_store());
    let (_td_fm, fm) = rt.block_on(async {
        let (td, s) = seeded_store().await;
        assert!(
            s.create_tag_search_index()
                .await
                .expect("failed to build FM index"),
            "FM index should be newly built on a fresh store"
        );
        s.optimize().await.expect("failed to optimize after index");
        (td, s)
    });

    // `generate_memory` assigns tag-{i%5}, so this selects ~1/5 of the store.
    let tag = "tag-3";
    let tag_pred = LanceIndex::tag_contains_predicate(tag);
    let filters = SearchFilters {
        tags: Some(vec![tag.to_string()]),
        ..Default::default()
    };

    // Today: stream every row into Rust, then filter.
    group.bench_function("tags_filtered_in_rust", |b| {
        b.to_async(&rt).iter(|| async {
            let entries = plain.list_for_filtering().await.unwrap();
            apply_index_filters(entries, &filters)
        });
    });

    // Pushed down as a substring predicate, no FM index present.
    group.bench_function("tags_pushed_down_no_index", |b| {
        b.to_async(&rt).iter(|| async {
            plain
                .list_for_filtering_where(Some(tag_pred.clone()))
                .await
                .unwrap()
        });
    });

    // Pushed down with the FM index built.
    group.bench_function("tags_pushed_down_fm_index", |b| {
        b.to_async(&rt).iter(|| async {
            fm.list_for_filtering_where(Some(tag_pred.clone()))
                .await
                .unwrap()
        });
    });

    group.finish();
}

// ===========================================================================
// Group 4: what the in-Rust keyword scorer costs (the FTS-swap budget)
// ===========================================================================

fn leverage_keyword(c: &mut Criterion) {
    let mut group = c.benchmark_group("leverage_keyword");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    // Pure CPU cost of the current scorer, isolated from I/O. This is the
    // ceiling on what replacing it with a LanceDB FTS index could save.
    let memories: Vec<Memory> = (0..SCALE_COUNT).map(generate_memory).collect();
    let query = "authentication handler validates JWT tokens";

    group.bench_function("keyword_search_1k", |b| {
        b.iter(|| std::hint::black_box(keyword_search(query, &memories)));
    });

    // The same scorer over the candidate set a selective filter would leave —
    // the realistic input size on a filtered query.
    let narrowed: Vec<Memory> = memories.iter().take(SCALE_COUNT / 5).cloned().collect();
    group.bench_function("keyword_search_200", |b| {
        b.iter(|| std::hint::black_box(keyword_search(query, &narrowed)));
    });

    // The same work with the stems already available — what the engine does
    // once a store has migrated to schema 0.5.0 and the write path has
    // populated `keyword_stems`. The delta against the two arms above is
    // exactly what precomputing buys.
    let stems_1k: Vec<KeywordStems> = memories
        .iter()
        .map(|m| KeywordStems::compute(&m.summary, &m.tags, &m.content))
        .collect();
    let stems_200: Vec<KeywordStems> = stems_1k.iter().take(SCALE_COUNT / 5).cloned().collect();

    group.bench_function("stored_stems_1k", |b| {
        b.iter(|| std::hint::black_box(keyword_search_stems(query, &stems_1k)));
    });
    group.bench_function("stored_stems_200", |b| {
        b.iter(|| std::hint::black_box(keyword_search_stems(query, &stems_200)));
    });

    group.finish();
}

criterion_group!(
    leverage,
    leverage_scalar_index,
    leverage_typed_filter,
    leverage_tag_pushdown,
    leverage_keyword,
);
criterion_main!(leverage);
