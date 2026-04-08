mod helpers;

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use engramdb::retrieval::{
    apply_index_filters, DetailLevel, RetrievalEngine, RetrievalQuery, SearchFilters,
};
use engramdb::scope::{logical, physical};
use engramdb::scoring::trust_weight_from_config;
use engramdb::scoring::{composite_score, decay_factor, effective_relevance, ScoringContext};
use engramdb::storage::MemoryStore;
use engramdb::types::{Decay, DecayStrategy, EngramConfig, Memory, MemoryType, ProvenanceSource};

use chrono::Utc;

use helpers::{default_config, generate_memory, sample_hook_json, setup_store};

// ---------------------------------------------------------------------------
// Shared tokio runtime for async benchmarks
// ---------------------------------------------------------------------------

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to create tokio runtime")
}

// ===========================================================================
// Group 1: Scope Benchmarks — Physical & Logical Scope Matching
// ===========================================================================

fn scope_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("scope");

    // --- Physical proximity ---

    // Exact match (best case)
    group.bench_function("physical_proximity/exact_match", |b| {
        let patterns = vec!["src/api/auth/handlers.rs".to_string()];
        b.iter(|| physical::proximity(&patterns, "src/api/auth/handlers.rs", 0.82, 0.3));
    });

    // 5 patterns with one glob match (typical case)
    group.bench_function("physical_proximity/glob_5_patterns", |b| {
        let patterns = vec![
            "src/types/memory.rs".to_string(),
            "src/storage/store.rs".to_string(),
            "src/api/**".to_string(),
            "tests/integration.rs".to_string(),
            "docs/architecture.md".to_string(),
        ];
        b.iter(|| physical::proximity(&patterns, "src/api/auth/handlers.rs", 0.82, 0.3));
    });

    // 20 patterns mixed (worst case)
    group.bench_function("physical_proximity/glob_20_patterns", |b| {
        let patterns: Vec<String> = (0..20)
            .map(|i| match i % 4 {
                0 => format!("src/module_{}/file.rs", i),
                1 => format!("src/module_{}/**", i),
                2 => format!("tests/test_{}.rs", i),
                _ => "/".to_string(),
            })
            .collect();
        b.iter(|| physical::proximity(&patterns, "src/module_5/subdir/file.rs", 0.82, 0.3));
    });

    // --- Physical matches (GlobSet rebuild) ---

    group.bench_function("physical_matches/5_patterns", |b| {
        let patterns = vec![
            "src/api/**".to_string(),
            "src/types/*.rs".to_string(),
            "/".to_string(),
            "tests/**".to_string(),
            "src/storage/store.rs".to_string(),
        ];
        b.iter(|| physical::matches(&patterns, "src/api/auth/handlers.rs"));
    });

    // --- Logical proximity ---

    // 3 scopes (typical)
    group.bench_function("logical_proximity/3_scopes", |b| {
        let memory_scopes = vec![
            "api.auth".to_string(),
            "storage.lance".to_string(),
            "cli.commands".to_string(),
        ];
        let current_scopes = vec!["api.auth.oauth".to_string(), "cli.output".to_string()];
        b.iter(|| logical::proximity(&memory_scopes, &current_scopes));
    });

    // 10 scopes (stress test)
    group.bench_function("logical_proximity/10_scopes", |b| {
        let memory_scopes: Vec<String> = (0..10)
            .map(|i| format!("domain_{}.sub_{}", i % 5, i))
            .collect();
        let current_scopes: Vec<String> = (0..10)
            .map(|i| format!("domain_{}.sub_{}", i % 5, i + 1))
            .collect();
        b.iter(|| logical::proximity(&memory_scopes, &current_scopes));
    });

    group.finish();
}

// ===========================================================================
// Group 2: Scoring Benchmarks — Composite Scoring Pipeline
// ===========================================================================

fn scoring_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("scoring");

    let now = Utc::now();
    let config = EngramConfig::default();

    // --- Decay factor (all 4 strategies) ---

    group.bench_function("decay_factor/none", |b| {
        let created = now - chrono::Duration::days(5);
        let decay = Some(Decay::none());
        b.iter(|| decay_factor(created, now, &decay));
    });

    group.bench_function("decay_factor/linear", |b| {
        let created = now - chrono::Duration::days(5);
        let decay = Some(Decay::linear(chrono::Duration::days(10)));
        b.iter(|| decay_factor(created, now, &decay));
    });

    group.bench_function("decay_factor/exponential", |b| {
        let created = now - chrono::Duration::days(7);
        let decay = Some(Decay::exponential(chrono::Duration::days(7)));
        b.iter(|| decay_factor(created, now, &decay));
    });

    group.bench_function("decay_factor/step", |b| {
        let created = now - chrono::Duration::days(5);
        let decay = Some(Decay {
            strategy: DecayStrategy::Step,
            half_life: None,
            ttl: Some(chrono::Duration::days(10)),
            floor: 0.2,
        });
        b.iter(|| decay_factor(created, now, &decay));
    });

    // --- Effective relevance ---

    group.bench_function("effective_relevance", |b| {
        let memory = generate_memory(0);
        b.iter(|| effective_relevance(&memory, now));
    });

    // --- Trust weight ---

    group.bench_function("trust_weight", |b| {
        let weights = config.trust_weights.clone();
        b.iter(|| {
            trust_weight_from_config(ProvenanceSource::Human, &weights);
            trust_weight_from_config(ProvenanceSource::Agent, &weights);
            trust_weight_from_config(ProvenanceSource::Inferred, &weights);
            trust_weight_from_config(ProvenanceSource::Imported, &weights);
        });
    });

    // --- Composite score ---

    group.bench_function("composite_score/scope_only", |b| {
        let memory = generate_memory(0);
        let logical = vec!["api.auth".to_string()];
        let context = ScoringContext::scope_only(Some("src/main.rs"), &logical);
        b.iter(|| composite_score(&memory, &context, &config, now));
    });

    group.bench_function("composite_score/with_query", |b| {
        let memory = generate_memory(0);
        let logical = vec!["api.auth".to_string()];
        let context = ScoringContext::with_semantic(
            Some("src/main.rs"),
            &logical,
            "authentication handler",
            0.85,
        );
        b.iter(|| composite_score(&memory, &context, &config, now));
    });

    // --- Batch scoring ---

    for count in [100, 200] {
        group.bench_with_input(
            BenchmarkId::new("batch_scoring", count),
            &count,
            |b, &count| {
                let memories: Vec<Memory> = (0..count).map(generate_memory).collect();
                let logical = vec!["api.auth".to_string()];
                let context = ScoringContext::scope_only(Some("src/main.rs"), &logical);
                b.iter(|| {
                    for memory in &memories {
                        composite_score(memory, &context, &config, now);
                    }
                });
            },
        );
    }

    group.finish();
}

// ===========================================================================
// Group 3: Storage Benchmarks — MemoryStore I/O (async)
// ===========================================================================

fn storage_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    let rt = runtime();

    // --- Store open (cold open) ---

    group.bench_function("store_open", |b| {
        b.iter_batched(
            || {
                let (temp_dir, _store) = rt.block_on(setup_store(10));
                temp_dir
            },
            |temp_dir| {
                rt.block_on(async {
                    let _store = MemoryStore::open(temp_dir.path()).await.unwrap();
                });
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // --- Store get (single memory by ID) ---

    group.bench_function("store_get", |b| {
        let (temp_dir, store, target_id) = {
            let (td, s) = rt.block_on(setup_store(100));
            let ids = rt.block_on(s.list_ids()).unwrap();
            let id = ids[50].clone();
            (td, s, id)
        };

        b.to_async(&rt)
            .iter(|| async { store.get(&target_id).await.unwrap() });

        drop(temp_dir);
    });

    // --- Store list (12 columns — list_filterable) ---

    for count in [10, 100] {
        group.bench_with_input(
            BenchmarkId::new("store_list", count),
            &count,
            |b, &count| {
                let (temp_dir, store) = rt.block_on(setup_store(count));

                b.to_async(&rt)
                    .iter(|| async { store.list_filterable().await.unwrap() });

                drop(temp_dir);
            },
        );
    }

    // --- Store list_for_filtering (6 columns — lightweight) ---

    for count in [10, 100] {
        group.bench_with_input(
            BenchmarkId::new("store_list_for_filtering", count),
            &count,
            |b, &count| {
                let (temp_dir, store) = rt.block_on(setup_store(count));

                b.to_async(&rt)
                    .iter(|| async { store.list_for_filtering().await.unwrap() });

                drop(temp_dir);
            },
        );
    }

    // --- Store create (single memory) ---

    group.bench_function("store_create", |b| {
        let mut idx = 0usize;
        b.iter_batched(
            || {
                let (td, s) = rt.block_on(setup_store(0));
                let memory = generate_memory(idx);
                idx = idx.wrapping_add(1);
                (td, s, memory)
            },
            |(temp_dir, store, memory)| {
                rt.block_on(async { store.create(&memory).await.unwrap() });
                drop(temp_dir);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // --- Store get_batch (batch get all items from 100-memory store) ---

    group.bench_function("store_get_batch/100", |b| {
        let (temp_dir, store, ids) = {
            let (td, s) = rt.block_on(setup_store(100));
            let ids = rt.block_on(s.list_ids()).unwrap();
            (td, s, ids)
        };

        b.to_async(&rt).iter(|| {
            let store = store.clone();
            let ids = ids.clone();
            async move { store.get_batch(&ids).await.unwrap() }
        });

        drop(temp_dir);
    });

    // --- Store batch_exists (existence check all items from 100-memory store) ---

    group.bench_function("store_batch_exists/100", |b| {
        let (temp_dir, store, ids) = {
            let (td, s) = rt.block_on(setup_store(100));
            let ids = rt.block_on(s.list_ids()).unwrap();
            (td, s, ids)
        };

        b.to_async(&rt).iter(|| {
            let store = store.clone();
            let ids = ids.clone();
            async move { store.batch_exists(&ids).await.unwrap() }
        });

        drop(temp_dir);
    });

    group.finish();
}

// ===========================================================================
// Group 4: Retrieval Benchmarks — Full RetrievalEngine
// ===========================================================================

fn retrieval_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("retrieval");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    let rt = runtime();

    // --- Retrieve scope_only ---

    for count in [10, 100] {
        group.bench_with_input(
            BenchmarkId::new("retrieve_scope_only", count),
            &count,
            |b, &count| {
                let (temp_dir, engine, query) = {
                    let (td, store) = rt.block_on(setup_store(count));
                    let config = default_config();
                    let e = RetrievalEngine::new(store, config);
                    let q = RetrievalQuery {
                        path: Some("src/main.rs".to_string()),
                        logical: vec!["api.auth".to_string()],
                        max_results: Some(5),
                        detail_level: DetailLevel::Summary,
                        ..Default::default()
                    };
                    (td, e, q)
                };

                b.to_async(&rt)
                    .iter(|| async { engine.retrieve(&query).await.unwrap() });

                drop(temp_dir);
            },
        );
    }

    // --- Index filters ---

    for count in [50, 100] {
        group.bench_with_input(
            BenchmarkId::new("index_filters", count),
            &count,
            |b, &count| {
                let (temp_dir, entries) = {
                    let (td, store) = rt.block_on(setup_store(count));
                    let e = rt.block_on(store.list_filterable()).unwrap();
                    (td, e)
                };
                let filters = SearchFilters {
                    types: Some(vec![MemoryType::Decision, MemoryType::Convention]),
                    min_criticality: Some(0.5),
                    ..Default::default()
                };

                b.iter_batched(
                    || entries.clone(),
                    |e| apply_index_filters(e, &filters),
                    criterion::BatchSize::SmallInput,
                );

                drop(temp_dir);
            },
        );
    }

    group.finish();
}

// ===========================================================================
// Group 5: Hook Path Benchmarks — PreToolUse Critical Path
// ===========================================================================

fn hook_path_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("hook_path");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    let rt = runtime();

    // --- 5a. In-process hook simulation ---

    for count in [10, 100] {
        group.bench_with_input(
            BenchmarkId::new("hook_inprocess", count),
            &count,
            |b, &count| {
                let temp_dir = {
                    let (td, _) = rt.block_on(setup_store(count));
                    td
                };

                b.to_async(&rt).iter(|| {
                    let path = temp_dir.path().to_path_buf();
                    async move {
                        let store = MemoryStore::open(&path).await.unwrap();
                        let config = default_config();
                        let engine = RetrievalEngine::new(store, config);
                        let query = RetrievalQuery {
                            path: Some("src/main.rs".to_string()),
                            max_results: Some(10),
                            detail_level: DetailLevel::Summary,
                            ..Default::default()
                        };
                        engine.retrieve(&query).await.unwrap()
                    }
                });
            },
        );
    }

    // --- 5b. CLI subprocess benchmark ---
    // Only runs if the engramdb binary exists (requires `cargo build --release` first).

    let binary = find_engramdb_binary();
    if binary.exists() {
        for count in [10, 100] {
            group.bench_with_input(
                BenchmarkId::new("hook_subprocess", count),
                &count,
                |b, &count| {
                    let (temp_dir, json_input) = {
                        let (td, _) = rt.block_on(setup_store(count));
                        let json = sample_hook_json(td.path(), "src/main.rs");
                        (td, json)
                    };

                    b.iter(|| {
                        use std::io::Write;
                        let mut child = std::process::Command::new(&binary)
                            .args(["hook", "pre-tool-use", "--dir"])
                            .arg(temp_dir.path())
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                            .expect("failed to spawn engramdb binary");
                        if let Some(ref mut stdin) = child.stdin {
                            stdin.write_all(json_input.as_bytes()).unwrap();
                        }
                        child.wait_with_output().unwrap();
                    });
                },
            );
        }
    }

    group.finish();
}

/// Find the engramdb binary, preferring release build.
fn find_engramdb_binary() -> std::path::PathBuf {
    let release = std::path::PathBuf::from("target/release/engramdb");
    if release.exists() {
        return release;
    }
    let debug = std::path::PathBuf::from("target/debug/engramdb");
    if debug.exists() {
        return debug;
    }
    // Fall back to hoping it's on PATH
    std::path::PathBuf::from("engramdb")
}

// ===========================================================================
// Performance budget test
// ===========================================================================

#[cfg(test)]
#[allow(unused_imports)]
mod budget_tests {
    use engramdb::retrieval::{DetailLevel, RetrievalEngine, RetrievalQuery};
    use engramdb::storage::{InMemoryRegistry, MemoryStore};
    use std::time::Duration;

    use crate::helpers::{default_config, setup_store};

    #[tokio::test]
    async fn hook_path_performance_budget() {
        // Setup 100-memory store
        let (temp_dir, _) = setup_store(100).await;

        let start = std::time::Instant::now();

        // Full hook path: open → build engine → retrieve
        let store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let config = default_config();
        let engine = RetrievalEngine::new(store, config);
        let query = RetrievalQuery {
            path: Some("src/main.rs".to_string()),
            max_results: Some(10),
            detail_level: DetailLevel::Summary,
            ..Default::default()
        };
        let _result = engine.retrieve(&query).await.unwrap();

        let elapsed = start.elapsed();

        // Generous budget for CI (real target is < 50ms)
        assert!(
            elapsed < Duration::from_millis(200),
            "Hook path took {:?}, exceeds 200ms budget",
            elapsed
        );
    }
}

// ===========================================================================
// Group 6: Ops Benchmarks — doctor, compress
// ===========================================================================

fn ops_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("ops");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    let rt = runtime();

    // --- Doctor health check on 100 memories ---

    group.bench_function("doctor/100", |b| {
        let (temp_dir, store) = rt.block_on(setup_store(100));

        b.to_async(&rt)
            .iter(|| async { engramdb::ops::doctor(&store).await.unwrap() });

        drop(temp_dir);
    });

    // --- Compress candidates listing on 100 memories ---

    group.bench_function("compress_candidates/100", |b| {
        let (temp_dir, store) = rt.block_on(setup_store(100));

        b.to_async(&rt).iter(|| async {
            engramdb::ops::compress_candidates(&store, None, Some(0.4))
                .await
                .unwrap()
        });

        drop(temp_dir);
    });

    group.finish();
}

// ===========================================================================
// Criterion registration
// ===========================================================================

criterion_group!(
    benches,
    scope_benchmarks,
    scoring_benchmarks,
    storage_benchmarks,
    retrieval_benchmarks,
    hook_path_benchmarks,
    ops_benchmarks,
);
criterion_main!(benches);
