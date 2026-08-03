//! Benchmarks for the two remaining cross-project scan loops.
//!
//! Both were left alone in the earlier batching pass on the argument that they
//! are one-shot and low-N. These groups exist to check that argument with a
//! measurement rather than an opinion, and in particular to see how each
//! behaves as the per-item constant grows — the cost that matters on a slow
//! disk, where every store open is a fresh set of file opens.
//!
//! 1. `aggregate_stats_projects` — `ops::projects::aggregate_stats` opens each
//!    registered project's store and pulls a full 7-column summary row for
//!    every memory, to produce a total and a per-type histogram. Swept over
//!    project count at a fixed size, and over memory count at a fixed project
//!    count, so the two factors can be told apart.
//!
//! 2. `worktree_consolidate` — `storage::worktree::consolidate_worktree_into_main`
//!    calls `get` (a full directory scan) once per migrated file against the
//!    main store, so it is O(W*M) in worktree files times main-store size.
//!    Swept over both.
//!
//! Timing note: every group uses `iter_batched` with the seeded fixtures built
//! in `setup` (untimed), because both operations mutate — consolidation
//! consumes its source files, so a re-run measures an empty directory.
//!
//! Run with: `cargo bench --bench project_scan`

mod helpers;

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

use engramdb::ops::projects::aggregate_stats;
use engramdb::storage::worktree::consolidate_worktree_into_main;
use engramdb::storage::{InMemoryRegistry, MemoryStore, RegistryBackend};

use helpers::generate_memory;
use tempfile::TempDir;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to create tokio runtime")
}

/// Build `projects` registered stores, each holding `per_project` memories.
///
/// Returns the temp dir root (kept alive by the caller) and the registry the
/// stores registered into — that registry is what `aggregate_stats` walks.
async fn seed_projects(projects: usize, per_project: usize) -> (TempDir, InMemoryRegistry) {
    let root = TempDir::new().expect("failed to create temp dir");
    let registry = InMemoryRegistry::new();

    for p in 0..projects {
        let dir = root.path().join(format!("project-{}", p));
        std::fs::create_dir_all(&dir).expect("failed to create project dir");
        let store = MemoryStore::init(&dir, &registry)
            .await
            .expect("failed to init store");
        for i in 0..per_project {
            // Offset by project so the type histogram is not identical across
            // projects — `generate_memory` cycles type by index.
            let memory = generate_memory(p * per_project + i);
            store.create(&memory).await.expect("failed to create");
        }
    }

    (root, registry)
}

/// `aggregate_stats` over a registry of N projects.
///
/// Two sweeps: project count at a fixed 100 memories each (isolates the
/// per-store-open cost) and memory count at a fixed 8 projects (isolates the
/// per-memory row cost).
fn bench_aggregate_stats(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("aggregate_stats_projects");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(20);

    for projects in [2usize, 8, 32] {
        let (root, registry) = rt.block_on(seed_projects(projects, 100));
        group.bench_with_input(
            BenchmarkId::new("by_project_count_100_memories", projects),
            &projects,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    aggregate_stats(&registry as &dyn RegistryBackend)
                        .await
                        .expect("aggregate_stats failed")
                });
            },
        );
        drop(root);
    }

    for per_project in [10usize, 100, 1_000] {
        let (root, registry) = rt.block_on(seed_projects(8, per_project));
        group.bench_with_input(
            BenchmarkId::new("by_memories_per_project_8_projects", per_project),
            &per_project,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    aggregate_stats(&registry as &dyn RegistryBackend)
                        .await
                        .expect("aggregate_stats failed")
                });
            },
        );
        drop(root);
    }

    group.finish();
}

/// Seed a main store with `main_count` memories and a sibling "worktree"
/// store holding `stray_count` memories to be consolidated into it.
///
/// The two directories are independent stores; consolidation is driven purely
/// by path, so this reproduces the migration loop without needing real git
/// worktree plumbing.
async fn seed_worktree(main_count: usize, stray_count: usize) -> (TempDir, std::path::PathBuf) {
    let root = TempDir::new().expect("failed to create temp dir");
    let registry = InMemoryRegistry::new();

    let main_dir = root.path().join("main");
    std::fs::create_dir_all(&main_dir).expect("failed to create main dir");
    let main_store = MemoryStore::init(&main_dir, &registry)
        .await
        .expect("failed to init main store");
    for i in 0..main_count {
        main_store
            .create(&generate_memory(i))
            .await
            .expect("failed to create");
    }

    let wt_dir = root.path().join("wt");
    std::fs::create_dir_all(&wt_dir).expect("failed to create worktree dir");
    let wt_store = MemoryStore::init(&wt_dir, &registry)
        .await
        .expect("failed to init worktree store");
    for i in 0..stray_count {
        // Disjoint index range so these are new IDs in main, exercising the
        // migrate path rather than the newest-wins drop path.
        wt_store
            .create(&generate_memory(100_000 + i))
            .await
            .expect("failed to create");
    }

    (root, wt_dir)
}

/// Consolidation cost as the main store grows (the O(W*M) factor) and as the
/// number of stray files grows (the O(W) factor).
fn bench_worktree_consolidate(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("worktree_consolidate");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    // Fixed 20 stray files, growing main store: isolates the per-file `get`
    // scan, which is linear in main-store size.
    for main_count in [100usize, 1_000] {
        group.bench_with_input(
            BenchmarkId::new("20_stray_by_main_size", main_count),
            &main_count,
            |b, &main_count| {
                // Sync `iter_batched`, not `to_async`: the setup closure has to
                // seed a fresh store per iteration (consolidation consumes its
                // source files), and `block_on` inside a closure criterion is
                // already driving from within the runtime panics with "cannot
                // start a runtime from within a runtime". Keeping the bencher
                // synchronous leaves the main thread outside the runtime, so
                // both setup and routine can drive it.
                b.iter_batched(
                    || rt.block_on(seed_worktree(main_count, 20)),
                    |(root, wt_dir)| {
                        let main_dir = root.path().join("main");
                        let n = rt
                            .block_on(consolidate_worktree_into_main(&wt_dir, &main_dir))
                            .expect("consolidate failed");
                        drop(root);
                        n
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    // Fixed 500-memory main store, growing stray count: the per-migrated-file
    // cost (create + chunk relocation + unlink).
    for stray in [5usize, 20, 100] {
        group.bench_with_input(
            BenchmarkId::new("500_main_by_stray_count", stray),
            &stray,
            |b, &stray| {
                b.iter_batched(
                    || rt.block_on(seed_worktree(500, stray)),
                    |(root, wt_dir)| {
                        let main_dir = root.path().join("main");
                        let n = rt
                            .block_on(consolidate_worktree_into_main(&wt_dir, &main_dir))
                            .expect("consolidate failed");
                        drop(root);
                        n
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_aggregate_stats, bench_worktree_consolidate);
criterion_main!(benches);
