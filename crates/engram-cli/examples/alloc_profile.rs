//! Allocation, heap, RSS and CPU profile of EngramDB's hot paths.
//!
//! Built for the allocation audit (`docs/contributors/allocation-audit.md`):
//! every scenario runs in its own process, behind a counting global
//! allocator, and reports per operation
//!
//! - allocation count and bytes allocated (the allocation rate),
//! - peak live heap above the scenario's starting point,
//! - peak RSS of the measured phase (`VmHWM` after a `clear_refs` reset),
//! - CPU time (user + sys, the whole process: tokio workers and LanceDB's
//!   threads included) and wall time.
//!
//! No model is loaded: queries run on the keyword path, hooks use keyword
//! situations. So the numbers are EngramDB's own code plus LanceDB, never ONNX.
//!
//! ```bash
//! # once: the persistent stores (100 / 1,000 / 5,000 memories)
//! cargo run --profile bench -p engram-cli --example alloc_profile -- <machine> build
//! # every scenario, 7 reps each, results as JSON
//! cargo run --profile bench -p engram-cli --example alloc_profile -- <machine> all out.json
//! # one scenario
//! cargo run --profile bench -p engram-cli --example alloc_profile -- <machine> run q-filter-1k
//! # interleaved A/B of several builds (3 rounds, scenarios matching a filter)
//! alloc_profile <machine> ab 3 <outdir> q-,hook- <bin A> <bin B>
//! # compare two result files
//! cargo run --profile bench -p engram-cli --example alloc_profile -- <machine> compare a.json b.json
//! ```
//!
//! `<machine>` holds the data/config dirs and the stores, so nothing touches
//! the real ones. The counting allocator lives in this example only; it never
//! reaches the shipped binary.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::{Duration, TimeZone, Utc};

use engram_cli::commands::hook::{
    execute_hook, HookEnv, SituationModel, StoreCache, StoreSource, UsageSink,
};
use engramdb::daemon::protocol::{read_msg, write_msg, DaemonOp, DaemonRequest, DaemonResponse};
use engramdb::daemon::{HookCall, ModelSelection};
use engramdb::retrieval::engine::{RetrievalEngine, RetrievalMode, RetrievalQuery};
use engramdb::storage::memory_file::{memory_filename, parse_memory_file, write_memory_file};
use engramdb::storage::{paths, FileRegistry, MemoryStore};
use engramdb::types::{
    EngramConfig, Link, LinkKind, LinkOrigin, LinkStatus, Memory, MemoryType, Provenance,
    Visibility,
};

// ---------------------------------------------------------------- allocator

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

#[inline]
fn grow(n: u64) {
    let live = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(live, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        grow(l.size() as u64);
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        grow(l.size() as u64);
        System.alloc_zeroed(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as u64, Relaxed);
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // A realloc is an allocation for the rate: it may move and copy.
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(new as u64, Relaxed);
        if new >= l.size() {
            grow((new - l.size()) as u64);
        } else {
            LIVE.fetch_sub((l.size() - new) as u64, Relaxed);
        }
        System.realloc(p, l, new)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// ---------------------------------------------------------------- process stats

/// User + system CPU of the whole process, in microseconds.
#[cfg(unix)]
fn cpu_us() -> u64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let tv = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// CPU time is read with `getrusage`, which only Unix has. On other
/// platforms the harness still compiles and reports 0.
#[cfg(not(unix))]
fn cpu_us() -> u64 {
    0
}

/// Reset the kernel's peak-RSS mark to the current RSS (Linux).
fn reset_hwm() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

/// Peak RSS since the last [`reset_hwm`], in KiB.
fn hwm_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------- machine

fn use_machine_dir(machine: &Path) -> Result<()> {
    for d in ["data", "conf", "models"] {
        std::fs::create_dir_all(machine.join(d))?;
    }
    // Edition 2021: `set_var` is safe; this runs before any thread reads env.
    std::env::set_var("ENGRAMDB_DATA_DIR", machine.join("data"));
    std::env::set_var("ENGRAMDB_CONFIG_DIR", machine.join("conf"));
    std::env::set_var(
        "ENGRAMDB_REGISTRY_PATH",
        machine.join("conf").join("registry.json"),
    );
    std::env::set_var("ENGRAMDB_MODEL_CACHE_DIR", machine.join("models"));
    std::env::set_var("ENGRAMDB_OFFLINE", "1");
    std::env::set_var("ENGRAMDB_IN_PROCESS", "1");
    std::env::set_var("ENGRAMDB_DISABLE_AUTO_MAINTENANCE", "1");
    Ok(())
}

// ---------------------------------------------------------------- data

/// Deterministic LCG.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const PATHS: &[&str] = &[
    "src/main.rs",
    "src/lib.rs",
    "src/retrieval/engine.rs",
    "src/retrieval/filters.rs",
    "src/scoring/mod.rs",
    "src/scoring/decay.rs",
    "src/scope/physical.rs",
    "src/scope/logical.rs",
    "src/daemon/server.rs",
    "src/daemon/protocol.rs",
    "src/ops/create.rs",
    "src/ops/query.rs",
    "crates/engram-storage/src/store.rs",
    "crates/engram-storage/src/lance_index.rs",
    "crates/engram-storage/src/memory_file/mod.rs",
    "crates/engram-cli/src/commands/hook.rs",
    "crates/engram-mcp/src/server.rs",
    "crates/engram-dashboard/src/layout/grid.rs",
    "docs/architecture.md",
    "src/**/*.rs",
    "crates/*/src/**",
    "tests/",
];
const LOGICAL: &[&str] = &[
    "storage",
    "storage.index",
    "storage.lock",
    "retrieval",
    "retrieval.engine",
    "retrieval.rerank",
    "scoring.decay",
    "daemon.socket",
    "daemon.cache",
    "harvest.ledger",
    "cli.hooks",
    "mcp.server",
    "dashboard.graph",
    "models.onnx",
];
const WORDS: &[&str] = &[
    "index",
    "lock",
    "vector",
    "schema",
    "retry",
    "socket",
    "token",
    "prune",
    "cache",
    "rank",
    "hook",
    "pool",
    "ledger",
    "digest",
    "budget",
    "edge",
    "decay",
    "filter",
    "query",
    "embedding",
    "chunk",
    "fragment",
    "compaction",
    "migration",
    "daemon",
    "session",
    "transcript",
    "scope",
    "logical",
    "physical",
    "glob",
    "pattern",
    "matcher",
    "reranker",
    "contradiction",
    "summary",
    "manifest",
    "registry",
    "worktree",
    "harvest",
    "always",
    "never",
    "because",
    "when",
    "the",
    "store",
    "writes",
    "reads",
    "under",
    "lock",
    "before",
    "after",
    "returns",
    "keeps",
    "drops",
];

fn words(rng: &mut Rng, n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        if i > 0 {
            s.push(if i % 14 == 0 { '.' } else { ' ' });
            if i % 14 == 0 {
                s.push(' ');
            }
        }
        s.push_str(WORDS[rng.below(WORDS.len())]);
    }
    s.push('.');
    s
}

fn memory_id(n: usize, i: usize) -> String {
    format!(
        "0c{:06x}-{:04x}-7000-8000-{:012x}",
        n,
        (i >> 16) & 0xffff,
        i
    )
}

const TYPES: [MemoryType; 6] = [
    MemoryType::Decision,
    MemoryType::Hazard,
    MemoryType::Convention,
    MemoryType::Context,
    MemoryType::Debug,
    MemoryType::Intent,
];

/// A store's memories: realistic content lengths, 1–3 physical scopes,
/// 1–2 logical, tags and ~0.5 links per memory. Fixed per `n`.
fn generate(n: usize) -> Vec<Memory> {
    let mut rng = Rng(0x5eed ^ n as u64);
    let t0 = Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap();
    let mut mems: Vec<Memory> = (0..n)
        .map(|i| {
            let summary = format!("{} memory {i}", words(&mut rng, 6).trim_end_matches('.'));
            let len = 50 + rng.below(90);
            let content = words(&mut rng, len);
            let mut m = Memory::new(
                TYPES[rng.below(TYPES.len())],
                summary,
                content,
                Provenance::agent("bench"),
            );
            m.id = memory_id(n, i);
            m.title = Some(format!("M {i}"));
            m.physical = (0..1 + rng.below(3))
                .map(|_| PATHS[rng.below(PATHS.len())].to_string())
                .collect();
            m.logical = (0..1 + rng.below(2))
                .map(|_| LOGICAL[rng.below(LOGICAL.len())].to_string())
                .collect();
            m.tags = vec![format!("tag-{}", i % 7), format!("area-{}", i % 3)];
            m.criticality = (rng.below(10) as f64) / 10.0;
            if rng.below(3) == 0 {
                m.details = Some(words(&mut rng, 30));
            }
            m.visibility = Visibility::Shared;
            let at = t0 + Duration::minutes(i as i64);
            m.created_at = at;
            m.updated_at = at;
            m.accessed_at = at;
            m
        })
        .collect();
    for _ in 0..n / 2 {
        let s = rng.below(n);
        let t = rng.below(n);
        if s == t {
            continue;
        }
        let kind = match rng.below(3) {
            0 => LinkKind::RelatedTo,
            1 => LinkKind::Supports,
            _ => LinkKind::DerivedFrom,
        };
        let mut link = Link::new(mems[t].id.clone(), kind, LinkOrigin::parse_lenient("human"));
        link.status = LinkStatus::Confirmed;
        link.confidence = 0.9;
        link.at = Some(t0 + Duration::days(2));
        mems[s].links.push(link);
    }
    mems
}

fn store_dir(machine: &Path, n: usize) -> PathBuf {
    machine.join(format!("s{n}"))
}

/// Write `n` memory files and index them once (no model), then compact.
async fn build_store(dir: &Path, n: usize) -> Result<MemoryStore> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    let dir = std::fs::canonicalize(dir)?;
    let registry = FileRegistry::global()?;
    let store = MemoryStore::init(&dir, &registry).await?;
    let mdir = paths::memories_dir(&dir);
    std::fs::create_dir_all(&mdir)?;
    for m in generate(n) {
        std::fs::write(mdir.join(memory_filename(&m)), write_memory_file(&m)?)?;
    }
    let indexed = store.reindex().await?;
    anyhow::ensure!(indexed == n, "indexed {indexed} of {n}");
    store.optimize().await?;
    Ok(store)
}

/// A synthetic Claude Code transcript: `turns` user/assistant exchanges with
/// tool calls, a failed tool call every few turns and long tool output.
fn transcript(dir: &Path, turns: usize) -> Result<PathBuf> {
    use std::io::Write;
    let path = dir.join("session.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&path)?);
    let mut rng = Rng(42);
    let cwd = dir.to_string_lossy();
    let t0 = Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap();
    for t in 0..turns {
        let ts = |k: i64| (t0 + Duration::seconds(t as i64 * 60 + k)).to_rfc3339();
        let (ul, al) = (20 + rng.below(40), 40 + rng.below(80));
        let user = serde_json::json!({"type":"user","cwd":cwd,"sessionId":"bench","timestamp":ts(0),
            "message":{"role":"user","content":words(&mut rng, ul)}});
        writeln!(f, "{user}")?;
        let id = format!("toolu_{t}");
        let asst = serde_json::json!({"type":"assistant","cwd":cwd,"sessionId":"bench","timestamp":ts(5),
            "message":{"role":"assistant","content":[
                {"type":"text","text":words(&mut rng, al)},
                {"type":"tool_use","id":id,"name": if t % 2 == 0 {"Bash"} else {"Read"},
                 "input": if t % 2 == 0 {serde_json::json!({"command":"cargo test -p engram-types"})}
                          else {serde_json::json!({"file_path":"src/retrieval/engine.rs"})}}]}});
        writeln!(f, "{asst}")?;
        let failed = t % 5 == 0;
        let result = serde_json::json!({"type":"user","cwd":cwd,"sessionId":"bench","timestamp":ts(9),
            "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"is_error":failed,
                "content": words(&mut rng, if failed {30} else {300})}]}});
        writeln!(f, "{result}")?;
    }
    f.flush()?;
    Ok(path)
}

/// A synthetic scope-force input of `n` memories.
fn force_input(n: usize) -> engram_dashboard::layout::ForceInput {
    use engram_dashboard::layout::{radius, ForceEdge, ForceInput, ForceNode};
    let mut rng = Rng(7 + n as u64);
    let tops = [
        "storage",
        "retrieval",
        "daemon",
        "harvest",
        "cli",
        "mcp",
        "scoring",
    ];
    let subs = ["", ".index", ".engine", ".cache", ".hooks"];
    let mut nodes: Vec<ForceNode> = (0..n)
        .map(|i| {
            let ti = rng.below(tops.len()).min(rng.below(tops.len()));
            let top = tops[ti];
            let scope = format!("{top}{}", subs[rng.below(subs.len())]);
            ForceNode {
                id: memory_id(n, i),
                top_scope: top.to_string(),
                slot: Some(ti as u32),
                scope,
                degree: 0.0,
                r: radius(rng.below(20) as u64),
                label_len: 10 + rng.below(40),
                weight: 1.0,
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut edges = Vec::new();
    for _ in 0..(n * 7 / 10) {
        let s = rng.below(n) as i64;
        let t = rng.below(n) as i64;
        if s != t {
            edges.push(ForceEdge {
                s,
                t,
                far_scope: None,
            });
            nodes[s as usize].degree += 1.0;
            nodes[t as usize].degree += 1.0;
        }
    }
    ForceInput {
        nodes,
        edges,
        aspect: 1.6,
        width: Some(1400.0),
    }
}

// ---------------------------------------------------------------- scenarios

const QUERIES: &[&str] = &[
    "compaction fragment lock",
    "daemon socket retry budget",
    "how does the reranker scope pattern work",
    "ledger digest transcript session",
];

/// What one repetition of a scenario measured.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Rep {
    allocs: f64,
    bytes: f64,
    peak_heap: f64,
    cpu_us: f64,
    wall_us: f64,
}

/// A scenario's per-operation cost, as `ops` operations per repetition.
struct Measure {
    ops: usize,
}

impl Measure {
    async fn rep<F, Fut>(&self, mut f: F) -> Result<Rep>
    where
        F: FnMut(usize) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let a0 = ALLOCS.load(Relaxed);
        let b0 = BYTES.load(Relaxed);
        let live0 = LIVE.load(Relaxed);
        PEAK.store(live0, Relaxed);
        let c0 = cpu_us();
        let t0 = Instant::now();
        for i in 0..self.ops {
            f(i).await?;
        }
        let wall = t0.elapsed().as_secs_f64() * 1e6;
        let cpu = cpu_us() - c0;
        let n = self.ops as f64;
        Ok(Rep {
            allocs: (ALLOCS.load(Relaxed) - a0) as f64 / n,
            bytes: (BYTES.load(Relaxed) - b0) as f64 / n,
            peak_heap: PEAK.load(Relaxed).saturating_sub(live0) as f64,
            cpu_us: cpu as f64 / n,
            wall_us: wall / n,
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ScenarioResult {
    name: String,
    ops: usize,
    reps: Vec<Rep>,
    /// Peak RSS of the measured phase, KiB.
    hwm_kib: u64,
}

const SCENARIOS: &[&str] = &[
    "q-filter-100",
    "q-filter-1k",
    "q-filter-5k",
    "q-rank-100",
    "q-rank-1k",
    "q-rank-5k",
    "q-path-100",
    "q-path-1k",
    "q-path-5k",
    "create-1k",
    "update-1k",
    "reindex-1k",
    "hook-pre-1k",
    "hook-pre-5k",
    "hook-prompt-1k",
    "hook-post-1k",
    "hook-start-1k",
    "memfile-roundtrip",
    "transcript-digest",
    "defang",
    "daemon-frame",
    "layout-1k",
];

fn size_of_suffix(name: &str) -> usize {
    if name.ends_with("-5k") {
        5000
    } else if name.ends_with("-100") {
        100
    } else {
        1000
    }
}

async fn run_scenario(machine: &Path, name: &str, reps: usize) -> Result<ScenarioResult> {
    let n = size_of_suffix(name);
    let mut results = Vec::new();
    let kind = name.rsplit_once('-').map_or(name, |(k, _)| k);
    // Setup outside the measured phase; then one warm-up rep (not kept).
    macro_rules! go {
        ($ops:expr, $body:expr) => {{
            let m = Measure { ops: $ops };
            let mut body = $body;
            m.rep(&mut body).await?;
            reset_hwm();
            for _ in 0..reps {
                results.push(m.rep(&mut body).await?);
            }
            ($ops, hwm_kib())
        }};
    }
    let (ops, hwm) = match kind {
        "q-filter" | "q-rank" | "q-path" => {
            let dir = std::fs::canonicalize(store_dir(machine, n))?;
            let store = MemoryStore::open(&dir).await?;
            let engine = RetrievalEngine::new(store, EngramConfig::default());
            let engine = &engine;
            let ops = match n {
                100 => 60,
                1000 => 20,
                _ => 6,
            };
            go!(ops, |i: usize| {
                let q = match kind {
                    "q-filter" => RetrievalQuery {
                        mode: RetrievalMode::Filter,
                        query: Some(QUERIES[i % QUERIES.len()].to_string()),
                        ..Default::default()
                    },
                    "q-rank" => RetrievalQuery {
                        mode: RetrievalMode::Rank,
                        query: Some(QUERIES[i % QUERIES.len()].to_string()),
                        ..Default::default()
                    },
                    _ => RetrievalQuery {
                        mode: RetrievalMode::Rank,
                        path: Some(PATHS[i % 18].to_string()),
                        ..Default::default()
                    },
                };
                async move {
                    let r = engine.query(&q).await?;
                    std::hint::black_box(r);
                    Ok(())
                }
            })
        }
        "create" | "update" | "reindex" => {
            let scratch = machine.join(format!("scratch-{kind}"));
            let store = build_store(&scratch, n).await?;
            let store = &store;
            let ids: Vec<String> = (0..n).map(|i| memory_id(n, i)).collect();
            let ids = &ids;
            let mut rng = Rng(99);
            let texts: Vec<String> = (0..400).map(|_| words(&mut rng, 80)).collect();
            let texts = &texts;
            let ops = if kind == "reindex" { 2 } else { 20 };
            let r = go!(ops, |i: usize| async move {
                match kind {
                    "create" => {
                        engramdb::ops::create_memory(
                            store,
                            create_params(&texts[i % texts.len()], i),
                            None,
                            engramdb::ops::usage::Surface::Cli,
                        )
                        .await?;
                    }
                    "update" => {
                        engramdb::ops::update_memory(
                            store,
                            &ids[(i * 37) % ids.len()],
                            update_params(&texts[(i + 7) % texts.len()]),
                            None,
                            engramdb::ops::usage::Surface::Cli,
                        )
                        .await?;
                    }
                    _ => {
                        std::hint::black_box(store.reindex().await?);
                    }
                }
                Ok(())
            });
            let _ = std::fs::remove_dir_all(&scratch);
            r
        }
        "hook-pre" | "hook-prompt" | "hook-post" | "hook-start" => {
            let dir = std::fs::canonicalize(store_dir(machine, n))?;
            let cache = StoreCache::default();
            let collected = std::sync::Mutex::new(Vec::new());
            let env = HookEnv {
                stores: StoreSource::Cached(&cache),
                situation: SituationModel::KeywordOnly,
                usage: UsageSink::Collect(&collected),
                registry: None,
            };
            let (env, dir, collected) = (&env, &dir, &collected);
            let mut rng = Rng(5);
            let prompts: Vec<String> = (0..16).map(|_| words(&mut rng, 25)).collect();
            let prompts = &prompts;
            let call = match kind {
                "hook-pre" => HookCall::PreToolUse,
                "hook-prompt" => HookCall::UserPromptSubmit,
                "hook-post" => HookCall::PostToolUse,
                _ => HookCall::SessionStart {
                    min_criticality: 0.6,
                },
            };
            let call = &call;
            let ops = if n == 5000 { 10 } else { 40 };
            go!(ops, |i: usize| {
                let input = match kind {
                    "hook-pre" => serde_json::json!({
                        "hook_event_name": "PreToolUse",
                        "tool_name": "Read",
                        "tool_input": {"file_path": dir.join(PATHS[i % 18]).to_string_lossy()},
                        "cwd": dir.to_string_lossy(),
                    }),
                    "hook-prompt" => serde_json::json!({
                        "hook_event_name": "UserPromptSubmit",
                        "prompt": prompts[i % prompts.len()],
                        "cwd": dir.to_string_lossy(),
                    }),
                    "hook-post" => serde_json::json!({
                        "hook_event_name": "PostToolUse",
                        "tool_name": "Edit",
                        "tool_input": {"file_path": dir.join(PATHS[i % 18]).to_string_lossy()},
                        "tool_response": {"success": true},
                        "cwd": dir.to_string_lossy(),
                    }),
                    _ => serde_json::json!({
                        "hook_event_name": "SessionStart",
                        "source": "startup",
                        "cwd": dir.to_string_lossy(),
                    }),
                }
                .to_string();
                async move {
                    let out = execute_hook(call, dir, &input, env).await?;
                    std::hint::black_box(out);
                    collected.lock().unwrap().clear();
                    Ok(())
                }
            })
        }
        "memfile" => {
            let texts: Vec<String> = generate(1000)
                .iter()
                .map(|m| write_memory_file(m).unwrap())
                .collect();
            let texts = &texts;
            go!(1000, |i: usize| async move {
                let m = parse_memory_file(&texts[i])?;
                std::hint::black_box(write_memory_file(&m)?);
                Ok(())
            })
        }
        "transcript" => {
            let tmp = machine.join("scratch-transcript");
            std::fs::create_dir_all(&tmp)?;
            let path = transcript(&tmp, 400)?;
            let path = &path;
            go!(5, |_i: usize| async move {
                let d = engramdb::ops::harvest::digest_session(path, Default::default())?;
                let text = engramdb::ops::harvest::index_text(&d);
                let md = engramdb::ops::harvest::render_digest_markdown(&d);
                std::hint::black_box((text, md));
                Ok(())
            })
        }
        "defang" => {
            let mems = generate(200);
            let mems = &mems;
            go!(200, |i: usize| async move {
                let m = &mems[i];
                for s in [&m.summary, &m.content, m.title.as_ref().unwrap()]
                    .into_iter()
                    .chain(m.physical.iter())
                    .chain(m.logical.iter())
                    .chain(m.tags.iter())
                    .chain(std::iter::once(&m.id))
                {
                    std::hint::black_box(engramdb::ops::harvest::defang_memory_text(s));
                }
                std::hint::black_box(engramdb::ops::harvest::defang_prose(&m.content));
                Ok(())
            })
        }
        "daemon" => {
            let config = EngramConfig::default();
            let models = ModelSelection::from_config(&config, None);
            let mut rng = Rng(3);
            let req = DaemonRequest {
                models: Some(models),
                op: DaemonOp::Embed {
                    texts: (0..8).map(|_| words(&mut rng, 60)).collect(),
                },
            };
            let resp = DaemonResponse::Embedded {
                vectors: (0..8)
                    .map(|k| {
                        (0..384)
                            .map(|j| ((j * 31 + k) % 97) as f32 / 97.0)
                            .collect()
                    })
                    .collect(),
            };
            let (req, resp) = (&req, &resp);
            go!(500, |_i: usize| async move {
                let mut wire = Vec::new();
                write_msg(&mut wire, req).await?;
                write_msg(&mut wire, resp).await?;
                let mut r = tokio::io::BufReader::new(&wire[..]);
                let a: Option<DaemonRequest> = read_msg(&mut r).await?;
                let b: Option<DaemonResponse> = read_msg(&mut r).await?;
                std::hint::black_box((a, b));
                Ok(())
            })
        }
        "layout" => {
            let input = force_input(n);
            let input = &input;
            go!(2, |_i: usize| async move {
                std::hint::black_box(engram_dashboard::layout::scope_force::layout(input));
                std::hint::black_box(engram_dashboard::layout::grid::layout(input));
                Ok(())
            })
        }
        _ => bail!("unknown scenario {name}"),
    };
    Ok(ScenarioResult {
        name: name.to_string(),
        ops,
        reps: results,
        hwm_kib: hwm,
    })
}

fn create_params(content: &str, i: usize) -> engramdb::ops::CreateParams {
    engramdb::ops::CreateParams {
        type_: MemoryType::Decision,
        content: content.to_string(),
        summary: format!("Bench create {i}"),
        title: Some(format!("Bench {i}")),
        physical: vec![PATHS[i % PATHS.len()].to_string()],
        logical: vec![LOGICAL[i % LOGICAL.len()].to_string()],
        tags: vec!["bench".to_string()],
        criticality: 0.5,
        confidence: 0.8,
        details: None,
        visibility: Visibility::Shared,
        provenance: Provenance::agent("bench"),
        supersedes: vec![],
        audience: None,
        epistemic: None,
        premise: None,
        invalidated_by: vec![],
        origin_task: None,
        generality: None,
        valid_from: None,
        decay_strategy: None,
        decay_half_life: None,
        decay_ttl: None,
        decay_floor: None,
        title_strategy: engramdb::title::TitleStrategy::None,
        embed_async: false,
    }
}

fn update_params(content: &str) -> engramdb::ops::UpdateParams {
    engramdb::ops::UpdateParams {
        type_: None,
        content: Some(content.to_string()),
        summary: None,
        title: None,
        physical: None,
        logical: None,
        tags: None,
        tags_add: None,
        tags_remove: None,
        criticality: None,
        confidence: None,
        details: None,
        visibility: None,
        status: None,
        supersedes: None,
        audience: None,
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        clear_validity: false,
        clear_invalidated: false,
        decay_strategy: None,
        decay_half_life: None,
        decay_ttl: None,
        decay_floor: None,
        embed_async: false,
    }
}

// ---------------------------------------------------------------- report

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Median and half the min–max spread, relative to the median.
fn stat(r: &ScenarioResult, f: impl Fn(&Rep) -> f64) -> (f64, f64) {
    let v: Vec<f64> = r.reps.iter().map(f).collect();
    let m = median(v.clone());
    let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let spread = if m > 0.0 {
        (hi - lo) / 2.0 / m * 100.0
    } else {
        0.0
    };
    (m, spread)
}

fn print_table(results: &[ScenarioResult]) {
    println!(
        "| scenario | ops/rep | allocs/op | KiB alloc/op | peak heap KiB | peak RSS MiB | CPU µs/op (±%) | wall µs/op |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    for r in results {
        let (a, _) = stat(r, |x| x.allocs);
        let (b, _) = stat(r, |x| x.bytes);
        let (p, _) = stat(r, |x| x.peak_heap);
        let (c, cs) = stat(r, |x| x.cpu_us);
        let (w, _) = stat(r, |x| x.wall_us);
        println!(
            "| {} | {} | {:.0} | {:.1} | {:.0} | {:.1} | {:.0} (±{:.0}%) | {:.0} |",
            r.name,
            r.ops,
            a,
            b / 1024.0,
            p / 1024.0,
            r.hwm_kib as f64 / 1024.0,
            c,
            cs,
            w
        );
    }
}

fn pct(a: f64, b: f64) -> String {
    if a == 0.0 {
        return "–".into();
    }
    format!("{:+.1}%", (b - a) / a * 100.0)
}

fn compare(a: &[ScenarioResult], b: &[ScenarioResult]) {
    println!("| scenario | allocs/op | Δ | KiB/op | Δ | peak heap KiB | Δ | peak RSS MiB | Δ | CPU µs/op | Δ | wall µs/op | Δ |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for x in a {
        let Some(y) = b.iter().find(|y| y.name == x.name) else {
            continue;
        };
        let f = |r: &ScenarioResult, g: fn(&Rep) -> f64| stat(r, g).0;
        let (a0, a1) = (f(x, |r| r.allocs), f(y, |r| r.allocs));
        let (b0, b1) = (f(x, |r| r.bytes), f(y, |r| r.bytes));
        let (p0, p1) = (f(x, |r| r.peak_heap), f(y, |r| r.peak_heap));
        let (h0, h1) = (x.hwm_kib as f64, y.hwm_kib as f64);
        let (c0, c1) = (f(x, |r| r.cpu_us), f(y, |r| r.cpu_us));
        let (w0, w1) = (f(x, |r| r.wall_us), f(y, |r| r.wall_us));
        println!(
            "| {} | {:.0} → {:.0} | {} | {:.1} → {:.1} | {} | {:.0} → {:.0} | {} | {:.1} → {:.1} | {} | {:.0} → {:.0} | {} | {:.0} → {:.0} | {} |",
            x.name,
            a0, a1, pct(a0, a1),
            b0 / 1024.0, b1 / 1024.0, pct(b0, b1),
            p0 / 1024.0, p1 / 1024.0, pct(p0, p1),
            h0 / 1024.0, h1 / 1024.0, pct(h0, h1),
            c0, c1, pct(c0, c1),
            w0, w1, pct(w0, w1),
        );
    }
}

/// Run one scenario in a fresh process of `exe`.
fn run_child(exe: &Path, machine: &Path, name: &str) -> Result<ScenarioResult> {
    let o = std::process::Command::new(exe)
        .arg(machine)
        .arg("run")
        .arg(name)
        .arg("--json")
        .output()?;
    if !o.status.success() {
        bail!(
            "{name} failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    serde_json::from_slice(&o.stdout)
        .with_context(|| format!("{name}: {}", String::from_utf8_lossy(&o.stdout)))
}

/// `size_of` / `align_of` of the types the hot paths hold many of.
fn print_sizes() {
    macro_rules! row {
        ($($t:ty),* $(,)?) => {$(
            println!(
                "| `{}` | {} | {} |",
                stringify!($t),
                std::mem::size_of::<$t>(),
                std::mem::align_of::<$t>()
            );
        )*};
    }
    println!("| type | size | align |");
    println!("|---|---:|---:|");
    row!(
        engramdb::types::Memory,
        engramdb::storage::IndexForFiltering,
        engramdb::types::KeywordStems,
        engramdb::scoring::ScoreBreakdown,
        engramdb::retrieval::engine::ScoredMemory,
        engramdb::types::Link,
        engramdb::types::Challenge,
        engramdb::types::Provenance,
        engramdb::types::Decay,
        engramdb::storage::transcripts::Event,
        engramdb::daemon::protocol::DaemonRequest,
        engramdb::daemon::protocol::DaemonOp,
        engramdb::daemon::protocol::DaemonResponse,
        engram_dashboard::layout::ForceNode,
        engram_dashboard::layout::ForceEdge,
    );
}

// ---------------------------------------------------------------- main

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let machine = PathBuf::from(
        args.next()
            .context("usage: alloc_profile <machine> <cmd>")?,
    );
    let cmd = args.next().unwrap_or_else(|| "all".into());
    let rest: Vec<String> = args.collect();
    std::fs::create_dir_all(&machine)?;
    let machine = std::fs::canonicalize(&machine)?;
    use_machine_dir(&machine)?;
    let reps: usize = std::env::var("ALLOC_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    match cmd.as_str() {
        "build" => rt.block_on(async {
            for n in [100, 1000, 5000] {
                let t = Instant::now();
                build_store(&store_dir(&machine, n), n).await?;
                eprintln!("built s{n} in {:.1}s", t.elapsed().as_secs_f64());
            }
            Ok(())
        }),
        "run" => {
            let name = rest.first().context("run <scenario>")?;
            let r = rt.block_on(run_scenario(&machine, name, reps))?;
            if rest.get(1).map(String::as_str) == Some("--json") {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                print_table(std::slice::from_ref(&r));
            }
            Ok(())
        }
        "ab" => {
            // Interleaved A/B: `ab <rounds> <outdir> <filter> <bin>...`.
            // Each round runs every scenario once per binary, alternating,
            // so drift in machine load lands on every binary alike.
            let rounds: usize = rest
                .first()
                .context("ab <rounds> <outdir> <filter> <bin>...")?
                .parse()?;
            let outdir = PathBuf::from(&rest[1]);
            let filter = rest[2].clone();
            let bins: Vec<PathBuf> = rest[3..].iter().map(PathBuf::from).collect();
            std::fs::create_dir_all(&outdir)?;
            let names: Vec<&str> = SCENARIOS
                .iter()
                .copied()
                .filter(|s| filter.split(',').any(|f| s.contains(f)))
                .collect();
            let mut merged: Vec<Vec<ScenarioResult>> = bins.iter().map(|_| Vec::new()).collect();
            let mut hwms: Vec<Vec<Vec<u64>>> =
                bins.iter().map(|_| vec![Vec::new(); names.len()]).collect();
            for round in 0..rounds {
                for (k, name) in names.iter().enumerate() {
                    let order: Vec<usize> = if round % 2 == 0 {
                        (0..bins.len()).collect()
                    } else {
                        (0..bins.len()).rev().collect()
                    };
                    for b in order {
                        let r = run_child(&bins[b], &machine, name)?;
                        hwms[b][k].push(r.hwm_kib);
                        match merged[b].iter_mut().find(|m| m.name == r.name) {
                            Some(m) => m.reps.extend(r.reps),
                            None => merged[b].push(r),
                        }
                    }
                }
                eprintln!("round {} of {rounds}", round + 1);
            }
            for (b, results) in merged.iter_mut().enumerate() {
                for (k, r) in results.iter_mut().enumerate() {
                    r.hwm_kib = median(hwms[b][k].iter().map(|&v| v as f64).collect()) as u64;
                }
                let name = bins[b].file_name().unwrap().to_string_lossy().to_string();
                std::fs::write(
                    outdir.join(format!("{name}.json")),
                    serde_json::to_string_pretty(results)?,
                )?;
            }
            for b in 1..bins.len() {
                println!(
                    "\n{} → {}\n",
                    bins[0].file_name().unwrap().to_string_lossy(),
                    bins[b].file_name().unwrap().to_string_lossy()
                );
                compare(&merged[0], &merged[b]);
            }
            Ok(())
        }
        "all" => {
            let out = rest.first().context("all <out.json> [filter]")?;
            let filter = rest.get(1).cloned().unwrap_or_default();
            let exe = std::env::current_exe()?;
            let mut all = Vec::new();
            for name in SCENARIOS.iter().filter(|s| s.contains(filter.as_str())) {
                all.push(run_child(&exe, &machine, name)?);
                eprintln!("done {name}");
            }
            std::fs::write(out, serde_json::to_string_pretty(&all)?)?;
            print_table(&all);
            Ok(())
        }
        "sizes" => {
            print_sizes();
            Ok(())
        }
        "show" => {
            let a: Vec<ScenarioResult> = serde_json::from_str(&std::fs::read_to_string(&rest[0])?)?;
            print_table(&a);
            Ok(())
        }
        "compare" => {
            let a: Vec<ScenarioResult> = serde_json::from_str(&std::fs::read_to_string(&rest[0])?)?;
            let b: Vec<ScenarioResult> = serde_json::from_str(&std::fs::read_to_string(&rest[1])?)?;
            compare(&a, &b);
            Ok(())
        }
        other => bail!("unknown command {other}"),
    }
}
