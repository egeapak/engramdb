//! A/B measurement for the `projects discover` filesystem walk.
//!
//! `discover` is the one path in this crate whose advertised input is a home
//! directory, so its cost is dominated by directory reads: one `read_dir` per
//! directory, a `stat` per entry, a `.engramdb/` probe per directory, and two
//! more directory reads per project found. None of that shrinks with a better
//! algorithm — it is I/O latency, and a serial walk waits on one syscall at a
//! time.
//!
//! | group | previous | shipped |
//! |-------|----------|---------|
//! | `discover_walk` | serial depth-first walk | bounded-concurrency wave walk |
//!
//! Following the convention of `harvest_paths` and `parallel_simd`: the
//! `*_SHIPPED` arm calls production (`discover_projects_in`) directly, and the
//! `*_PREVIOUS` arm is a deliberate local copy of the shape the code had, kept
//! only as the record of what was replaced. The copy performs the same reads in
//! the same order the old code did but skips `classify` — classification is
//! pure CPU, runs identically in both shapes, and is not what this group is
//! measuring. Both arms therefore do the same I/O; only its scheduling differs.
//!
//! Sweeps are over directory *count* at fixed breadth (how the walk scales) and
//! over project *density* (how much the per-project `count_memories` reads add).
//!
//! Caveat worth reading before quoting a number: a bench machine's page cache
//! makes every one of these reads a memory hit, which is the case least
//! favourable to overlapping them. The gap this measures is the floor, not the
//! ceiling — the win grows with per-operation latency, and the target
//! environment (a real home directory, possibly on a network mount) has far
//! more of it than a warm tmpfs.
//!
//! Run with: `cargo bench --bench discover_walk`

use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use engramdb::ops::discover::{discover_projects_in, DiscoverOptions};
use engramdb::storage::{project_id, Registry};
use tempfile::TempDir;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to create tokio runtime")
}

/// Build a tree of `breadth^depth` directories, marking every `project_every`-th
/// leaf as an EngramDB project with `memories` memory files.
///
/// Returns the tempdir (kept alive by the caller) and the directory count.
fn build_tree(breadth: usize, depth: usize, project_every: usize, memories: usize) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let mut leaves = vec![tmp.path().to_path_buf()];
    for level in 0..depth {
        let mut next = Vec::new();
        for parent in &leaves {
            for i in 0..breadth {
                let dir = parent.join(format!("d{level}_{i}"));
                std::fs::create_dir_all(&dir).unwrap();
                next.push(dir);
            }
        }
        leaves = next;
    }
    for (i, leaf) in leaves.iter().enumerate() {
        if project_every == 0 || i % project_every != 0 {
            continue;
        }
        let memories_dir = leaf.join(".engramdb").join("memories");
        std::fs::create_dir_all(&memories_dir).unwrap();
        for m in 0..memories {
            std::fs::write(memories_dir.join(format!("m{m}.md")), "---\n---\nbody\n").unwrap();
        }
    }
    tmp
}

/// The walk as it was before the concurrency pass: a serial depth-first stack,
/// one directory read at a time, with the per-project memory counts read inline.
///
/// A deliberate copy — see the module docs.
async fn walk_serial_previous(root: &Path, opts: &DiscoverOptions) -> (usize, usize) {
    let mut scanned = 0usize;
    let mut projects = 0usize;
    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((canon, depth)) = stack.pop() {
        if !visited.insert(canon.clone()) {
            continue;
        }
        scanned += 1;

        if tokio::fs::metadata(canon.join(".engramdb"))
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            projects += 1;
            let pid = project_id::compute_project_id(&canon);
            // Both reads, serially, exactly as the old `count_memories` did.
            count_md(&canon.join(".engramdb").join("memories")).await;
            if let Ok(personal) = engramdb::storage::paths::personal_memories_dir(&pid) {
                count_md(&personal).await;
            }
        }

        let Ok(mut entries) = tokio::fs::read_dir(&canon).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if opts.skip_dirs.iter().any(|s| s == &name) {
                continue;
            }
            if !opts.include_hidden && name.starts_with('.') {
                continue;
            }
            if depth >= opts.max_depth {
                continue;
            }
            stack.push((entry.path(), depth + 1));
        }
    }
    (scanned, projects)
}

async fn count_md(dir: &Path) -> usize {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return 0;
    };
    let mut n = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().and_then(|s| s.to_str()) == Some("md") {
            n += 1;
        }
    }
    n
}

/// Scaling in directory count, at a fixed breadth and a fixed project density.
fn bench_walk_by_size(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("discover_walk");
    group.measurement_time(Duration::from_secs(10));

    // breadth^depth leaves: 4^3 = 64, 6^4 = 1296, 8^4 = 4096 leaf dirs.
    for (breadth, depth) in [(4usize, 3usize), (6, 4), (8, 4)] {
        let tmp = build_tree(breadth, depth, 8, 4);
        let root = tmp.path().to_path_buf();
        let opts = DiscoverOptions::default();
        let reg = Registry::default();
        let label = format!("{breadth}x{depth}");

        group.bench_with_input(
            BenchmarkId::new("serial_PREVIOUS", &label),
            &root,
            |b, root| {
                b.to_async(&rt)
                    .iter(|| async { walk_serial_previous(root, &opts).await });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("concurrent_SHIPPED", &label),
            &root,
            |b, root| {
                b.to_async(&rt).iter(|| async {
                    discover_projects_in(root, &reg, &opts, |_| {})
                        .await
                        .unwrap()
                });
            },
        );
    }
    group.finish();
}

/// Scaling in project density: every project found adds two more directory
/// reads, which the serial walk pays for inline.
fn bench_walk_by_density(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("discover_walk_density");
    group.measurement_time(Duration::from_secs(10));

    // 1296 leaves; every Nth is a project.
    for every in [64usize, 8, 1] {
        let tmp = build_tree(6, 4, every, 4);
        let root = tmp.path().to_path_buf();
        let opts = DiscoverOptions::default();
        let reg = Registry::default();
        let label = format!("1_in_{every}");

        group.bench_with_input(
            BenchmarkId::new("serial_PREVIOUS", &label),
            &root,
            |b, root| {
                b.to_async(&rt)
                    .iter(|| async { walk_serial_previous(root, &opts).await });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("concurrent_SHIPPED", &label),
            &root,
            |b, root| {
                b.to_async(&rt).iter(|| async {
                    discover_projects_in(root, &reg, &opts, |_| {})
                        .await
                        .unwrap()
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_walk_by_size, bench_walk_by_density);
criterion_main!(benches);
