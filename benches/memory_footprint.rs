//! Memory-footprint bench: does the daemon (and the MCP store path) give
//! memory back when projects are opened and then closed?
//!
//! This is not a Criterion bench. Criterion measures time; the question here
//! is resident memory (RSS) over a long-lived process, so this binary samples
//! RSS at fixed points of each scenario and prints a table.
//!
//! ```text
//! cargo bench --bench memory_footprint                # every scenario
//! cargo bench --bench memory_footprint -- open_close  # scenarios whose name contains "open_close"
//! cargo bench --bench memory_footprint -- --check     # exit 1 if a scenario fails its bound
//! ```
//!
//! Each daemon scenario runs a **fresh daemon in a child process** (this same
//! binary, re-executed with `ENGRAMDB_MEMBENCH_SOCKET` set) and reads that
//! child's RSS, so the numbers are the daemon's alone — the client half of
//! the bench lives in the parent and is not counted. The `mcp_*` scenario
//! measures the parent itself, because the MCP server opens stores in its
//! own process.
//!
//! Every run points `ENGRAMDB_DATA_DIR` / `ENGRAMDB_CONFIG_DIR` at a temp dir,
//! so the daemon's metrics snapshots and the bench projects never touch the
//! real user data. The model cache is the real one (the bench needs the
//! staged model); when the default ONNX embedding model is not available the
//! daemon scenarios are skipped with a message.
//!
//! Scenarios:
//!
//! - `daemon_request_churn` — one project, many small (query-sized) embed
//!   requests. RSS must plateau after warm-up; growth here is a true leak.
//! - `daemon_batch_shapes` — embed requests of growing shape, up to one
//!   memory with many 256-token chunks. Shows how much memory ONNX Runtime
//!   keeps after the largest request it has served.
//! - `daemon_same_config_projects` — several projects that share the default
//!   model selection. They must share one bundle: no growth per project.
//! - `daemon_open_close` — several projects with *different* model
//!   selections are opened, used, and closed, in cycles. After a close, RSS
//!   must return near the level before the first open.
//! - `mcp_store_open_close` — the MCP server's per-call path: open a project
//!   store, write, query, drop. Cycles over several projects must plateau.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engramdb::daemon::{remote_providers, DaemonHandle, ModelSelection};
use engramdb::embeddings::EmbeddingProvider;
use engramdb::types::EngramConfig;

/// Env var that turns this binary into the daemon child.
const ROLE_SOCKET_ENV: &str = "ENGRAMDB_MEMBENCH_SOCKET";

/// The daemon child's bundle window: a bundle nobody used or leased for this
/// long is unloaded. Production uses the daemon's idle timeout (15 minutes by
/// default); the bench shortens it so a "close" is observable in seconds.
const BUNDLE_WINDOW: Duration = Duration::from_secs(3);

/// How long a closed project is left alone before the "closed" sample: one
/// bundle window plus the sweeper's tick and some slack.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// How long to let the allocator and the daemon settle before a sample.
const SETTLE: Duration = Duration::from_millis(750);

// ---------------------------------------------------------------------------
// RSS sampling
// ---------------------------------------------------------------------------

/// Resident set size of `pid`, in MiB.
fn rss_mib(pid: u32) -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
            if let Some(kib) = status
                .lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse::<f64>().ok())
            {
                return kib / 1024.0;
            }
        }
    }
    // macOS (and a Linux without /proc): `ps` reports RSS in KiB.
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .map(|kib| kib / 1024.0)
        .unwrap_or(f64::NAN)
}

/// One labelled RSS sample.
struct Sample {
    label: String,
    mib: f64,
}

/// The samples of one scenario, plus its verdict.
struct Report {
    name: &'static str,
    samples: Vec<Sample>,
    /// `Some((passed, explanation))` when the scenario has a bound.
    verdict: Option<(bool, String)>,
}

impl Report {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            samples: Vec::new(),
            verdict: None,
        }
    }

    async fn sample(&mut self, pid: u32, label: impl Into<String>) -> f64 {
        tokio::time::sleep(SETTLE).await;
        let mib = rss_mib(pid);
        let label = label.into();
        eprintln!("  [{}] {label:<44} {mib:>9.1} MiB", self.name);
        self.samples.push(Sample { label, mib });
        mib
    }

    fn print(&self) {
        println!("\n## {}\n", self.name);
        println!("| step | RSS (MiB) |");
        println!("|---|---:|");
        for s in &self.samples {
            println!("| {} | {:.1} |", s.label, s.mib);
        }
        if let Some((ok, why)) = &self.verdict {
            println!("\n{}: {why}", if *ok { "PASS" } else { "FAIL" });
        }
    }
}

// ---------------------------------------------------------------------------
// Test data
// ---------------------------------------------------------------------------

const WORDS: &[&str] = &[
    "memory",
    "daemon",
    "project",
    "embedding",
    "session",
    "retrieval",
    "scope",
    "vector",
    "index",
    "query",
    "storage",
    "config",
    "hook",
    "model",
    "runtime",
    "allocator",
    "release",
    "cache",
    "bundle",
    "socket",
    "request",
    "worktree",
    "registry",
    "decay",
    "challenge",
];

/// A deterministic text of about `words` words (~1.2 tokens per word).
fn text(seed: usize, words: usize) -> String {
    (0..words)
        .map(|i| WORDS[(seed * 31 + i * 7) % WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

fn texts(seed: usize, count: usize, words: usize) -> Vec<String> {
    (0..count).map(|i| text(seed + i, words)).collect()
}

/// A project config: the default model selection with the heavy optional
/// models off (NLI and the reranker are not staged by default) and keyword
/// titles, so the only model is the embedding model.
fn project_config(pool_size: Option<usize>) -> EngramConfig {
    let mut c = EngramConfig::default();
    c.nli.enabled = false;
    c.rerank.enabled = false;
    c.title.strategy = engramdb::title::TitleStrategy::Keyword;
    c.embeddings.pool_size = pool_size;
    c
}

// ---------------------------------------------------------------------------
// Daemon child process
// ---------------------------------------------------------------------------

/// The daemon role: serve on the socket until the parent sends `Shutdown`.
fn run_daemon_role(socket: PathBuf) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("daemon runtime");
    rt.block_on(async {
        let options = engramdb::daemon::server::DaemonOptions {
            idle_timeout: Duration::from_secs(3600),
            bundle_idle_timeout: BUNDLE_WINDOW,
        };
        engramdb::daemon::server::run_daemon_with(socket, options)
            .await
            .expect("daemon run");
    });
}

/// A daemon child for one scenario. Shut down and reaped on drop.
struct DaemonChild {
    child: Child,
    socket: PathBuf,
    // Keeps the private socket folder alive for the child's lifetime.
    _dir: tempfile::TempDir,
}

impl DaemonChild {
    async fn start() -> Self {
        let dir = tempfile::TempDir::new().expect("socket dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
                .expect("chmod socket dir");
        }
        let socket = dir.path().join("membench.sock");
        let child = Command::new(std::env::current_exe().expect("current exe"))
            .env(ROLE_SOCKET_ENV, &socket)
            .spawn()
            .expect("spawn daemon child");
        let deadline = Instant::now() + Duration::from_secs(20);
        while tokio::net::UnixStream::connect(&socket).await.is_err() {
            assert!(Instant::now() < deadline, "daemon child did not bind");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Self {
            child,
            socket,
            _dir: dir,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Open a "project" against this daemon: what an MCP session does on its
    /// first model-backed tool call (a `Meta` that loads the bundle).
    async fn open_project(&self, config: &EngramConfig) -> Arc<dyn EmbeddingProvider> {
        let handle = DaemonHandle::connect_or_spawn(self.socket.clone(), None)
            .await
            .expect("daemon reachable");
        let providers = remote_providers(handle, ModelSelection::from_config(config, None), config)
            .await
            .expect("daemon has an embedding model");
        providers.embedding.expect("embedding provider")
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        let socket = self.socket.clone();
        // Best effort: a graceful shutdown, then make sure the child is gone.
        let _ = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
                .map(|rt| rt.block_on(engramdb::daemon::request_shutdown(&socket)))
        })
        .join();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn embed(provider: &Arc<dyn EmbeddingProvider>, batch: &[String]) {
    let refs: Vec<&str> = batch.iter().map(String::as_str).collect();
    provider.embed_batch(&refs).await.expect("embed");
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Allowed RSS growth over a reference point before a bound fails, in MiB.
const TOLERANCE_MIB: f64 = 64.0;

async fn daemon_request_churn() -> Report {
    let mut r = Report::new("daemon_request_churn");
    let d = DaemonChild::start().await;
    r.sample(d.pid(), "daemon started, nothing loaded").await;
    let p = d.open_project(&project_config(None)).await;
    embed(&p, &texts(0, 1, 12)).await;
    r.sample(d.pid(), "project opened (model loaded)").await;

    // Warm-up: let every pool session see the request shapes once.
    for i in 0..200 {
        embed(&p, &texts(i, 1 + i % 4, 8 + i % 24)).await;
    }
    let warm = r.sample(d.pid(), "after 200 warm-up requests").await;
    let mut last = warm;
    for round in 1..=5 {
        for i in 0..400 {
            embed(&p, &texts(round * 1000 + i, 1 + i % 4, 8 + i % 24)).await;
        }
        last = r
            .sample(d.pid(), format!("after {} more requests", round * 400))
            .await;
    }
    let growth = last - warm;
    r.verdict = Some((
        growth <= TOLERANCE_MIB,
        format!("{growth:+.1} MiB over 2000 requests after warm-up (bound {TOLERANCE_MIB} MiB)"),
    ));
    r
}

async fn daemon_batch_shapes() -> Report {
    let mut r = Report::new("daemon_batch_shapes");
    let d = DaemonChild::start().await;
    r.sample(d.pid(), "daemon started, nothing loaded").await;
    let p = d.open_project(&project_config(None)).await;
    embed(&p, &texts(0, 1, 12)).await;
    let opened = r.sample(d.pid(), "project opened (model loaded)").await;

    // (count, words): a query; a small memory; the engine's EMBED_BATCH_CHUNK
    // of full-window chunks; one large memory whose chunks go in one request.
    let shapes = [(1, 12), (8, 60), (32, 120), (64, 220), (256, 220)];
    for (count, words) in shapes {
        // Send the shape several times so every pool session serves it.
        for i in 0..8 {
            embed(&p, &texts(i * 7, count, words)).await;
        }
        r.sample(
            d.pid(),
            format!("after 8 × batch of {count} × ~{words} words"),
        )
        .await;
    }
    for i in 0..300 {
        embed(&p, &texts(i, 1, 12)).await;
    }
    // The project is still open here: this is what a long-lived session pays
    // for the largest request it ever made.
    let after = r
        .sample(d.pid(), "after 300 small requests (still open)")
        .await;
    tokio::time::sleep(CLOSE_WAIT).await;
    r.sample(
        d.pid(),
        format!("after {CLOSE_WAIT:?} idle (project closed)"),
    )
    .await;
    let kept = after - opened;
    let bound = TOLERANCE_MIB * 4.0;
    r.verdict = Some((
        kept <= bound,
        format!(
            "{kept:+.1} MiB held by an open project after its large requests ended (bound {bound} MiB)"
        ),
    ));
    r
}

async fn daemon_same_config_projects() -> Report {
    let mut r = Report::new("daemon_same_config_projects");
    let d = DaemonChild::start().await;
    r.sample(d.pid(), "daemon started, nothing loaded").await;
    let first = d.open_project(&project_config(None)).await;
    embed(&first, &texts(0, 1, 12)).await;
    let one = r.sample(d.pid(), "1 project opened").await;
    let mut providers = vec![first];
    for n in 2..=8 {
        let p = d.open_project(&project_config(None)).await;
        embed(&p, &texts(n, 4, 20)).await;
        providers.push(p);
    }
    let eight = r.sample(d.pid(), "8 projects opened (same config)").await;
    let growth = eight - one;
    r.verdict = Some((
        growth <= TOLERANCE_MIB,
        format!(
            "{growth:+.1} MiB for 7 more projects sharing one config (bound {TOLERANCE_MIB} MiB)"
        ),
    ));
    r
}

async fn daemon_open_close() -> Report {
    let mut r = Report::new("daemon_open_close");
    let d = DaemonChild::start().await;
    let initial = r.sample(d.pid(), "daemon started, nothing loaded").await;
    // Three projects whose configs differ in a model-affecting field
    // (`[embeddings].pool_size`), so each needs its own bundle.
    let configs = [
        project_config(Some(1)),
        project_config(Some(2)),
        project_config(Some(3)),
    ];
    let mut closed = initial;
    for cycle in 1..=3 {
        {
            let mut open = Vec::new();
            for c in &configs {
                let p = d.open_project(c).await;
                embed(&p, &texts(cycle, 16, 60)).await;
                open.push(p);
            }
            r.sample(d.pid(), format!("cycle {cycle}: 3 projects opened"))
                .await;
            // A session that ends sends nothing: it stops renewing its
            // lease, and the daemon unloads the bundle one window later.
        }
        tokio::time::sleep(CLOSE_WAIT).await;
        closed = r
            .sample(
                d.pid(),
                format!("cycle {cycle}: projects closed, {CLOSE_WAIT:?} idle"),
            )
            .await;
    }
    let held = closed - initial;
    r.verdict = Some((
        held <= TOLERANCE_MIB,
        format!("{held:+.1} MiB above the initial level after the last close (bound {TOLERANCE_MIB} MiB)"),
    ));
    r
}

async fn mcp_store_open_close() -> Report {
    use engramdb::storage::{InMemoryRegistry, MemoryStore};
    use engramdb::types::{Memory, MemoryType, Provenance};

    let mut r = Report::new("mcp_store_open_close");
    let pid = std::process::id();
    let root = tempfile::TempDir::new().expect("projects dir");
    let registry = InMemoryRegistry::new();
    let projects: Vec<PathBuf> = (0..6)
        .map(|i| {
            let p = root.path().join(format!("project-{i}"));
            std::fs::create_dir_all(&p).expect("project dir");
            p
        })
        .collect();

    async fn use_project(dir: &Path, registry: &InMemoryRegistry, round: usize) {
        let store = MemoryStore::init(dir, registry).await.expect("init store");
        for i in 0..5 {
            let m = Memory::new(
                MemoryType::Context,
                &text(round * 100 + i, 8),
                &text(round * 100 + i, 60),
                Provenance::human(),
            );
            store.create(&m).await.expect("create memory");
        }
        let engine = engramdb::ops::assemble_engine(
            store,
            project_config(None),
            engramdb::ops::EngineProviders::default(),
        );
        let q = engramdb::retrieval::RetrievalQuery {
            query: Some("daemon memory".to_string()),
            ..Default::default()
        };
        let _ = engine.query(&q).await;
    }

    let initial = r.sample(pid, "before any project is opened").await;
    let mut first_cycle = initial;
    let mut last = initial;
    for cycle in 1..=5 {
        for dir in &projects {
            use_project(dir, &registry, cycle).await;
        }
        last = r
            .sample(pid, format!("cycle {cycle}: 6 projects opened and closed"))
            .await;
        if cycle == 1 {
            first_cycle = last;
        }
    }
    let growth = last - first_cycle;
    r.verdict = Some((
        growth <= TOLERANCE_MIB,
        format!("{growth:+.1} MiB from cycle 1 to cycle 5 (bound {TOLERANCE_MIB} MiB)"),
    ));
    r
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    if let Some(socket) = std::env::var_os(ROLE_SOCKET_ENV) {
        run_daemon_role(PathBuf::from(socket));
        return;
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.iter().any(|a| a == "--check");
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let selected = |name: &str| filters.is_empty() || filters.iter().any(|f| name.contains(*f));

    // Isolate every data and config path before anything reads them. The
    // child inherits these, so the daemon persists its metrics in the temp
    // dir too.
    let isolated = tempfile::TempDir::new().expect("isolation dir");
    std::env::set_var("ENGRAMDB_DATA_DIR", isolated.path().join("data"));
    std::env::set_var("ENGRAMDB_CONFIG_DIR", isolated.path().join("config"));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("bench runtime");

    let model_available = engramdb::embeddings::OnnxProvider::try_new().is_some();
    let mut reports = Vec::new();
    rt.block_on(async {
        type Scenario = fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Report>>>;
        let daemon: [(&str, Scenario); 4] = [
            ("daemon_request_churn", || Box::pin(daemon_request_churn())),
            ("daemon_batch_shapes", || Box::pin(daemon_batch_shapes())),
            ("daemon_same_config_projects", || {
                Box::pin(daemon_same_config_projects())
            }),
            ("daemon_open_close", || Box::pin(daemon_open_close())),
        ];
        for (name, run) in daemon {
            if !selected(name) {
                continue;
            }
            if !model_available {
                eprintln!("[{name}] SKIP — the default ONNX embedding model is not available");
                continue;
            }
            reports.push(run().await);
        }
        if selected("mcp_store_open_close") {
            reports.push(mcp_store_open_close().await);
        }
    });

    println!("# memory_footprint");
    for r in &reports {
        r.print();
    }
    let failed: Vec<&str> = reports
        .iter()
        .filter(|r| matches!(r.verdict, Some((false, _))))
        .map(|r| r.name)
        .collect();
    if !failed.is_empty() {
        println!("\nFailed bounds: {}", failed.join(", "));
        if check {
            std::process::exit(1);
        }
    }
}
