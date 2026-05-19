//! The daemon process: loads each model once and serves inference.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};

use super::metrics::{self, Counters};
use super::protocol::{
    read_msg, write_msg, DaemonOp, DaemonRequest, DaemonResponse, DaemonStatus, NliWire,
    PROTOCOL_VERSION,
};
use crate::ops::ProviderCache;

/// Snapshot is persisted to the global store at least this often while the
/// daemon runs (plus on idle-exit and on graceful shutdown), so `stats
/// --daemon` stays reasonably fresh even without a clean shutdown.
const PERSIST_INTERVAL: Duration = Duration::from_secs(300);

/// Shared per-process daemon state.
struct Ctx {
    cache: ProviderCache,
    counters: Arc<Counters>,
    start: Instant,
    pid: u32,
    last_activity: Mutex<Instant>,
}

impl Ctx {
    async fn persist(&self) {
        metrics::persist(
            self.pid,
            self.start.elapsed().as_secs(),
            self.counters.snapshot(),
        )
        .await;
    }
}

/// Run the embedding daemon until idle-timeout or termination.
///
/// Startup is race-coordinated by the socket itself: only one process can be
/// bound to a given path. If a live daemon already owns the socket this
/// returns `Ok(())` (that daemon wins); a stale socket left by a crashed
/// daemon is detected (no listener answers) and reclaimed. Once serving, the
/// process exits after `idle_timeout` with no active connections — the next
/// MCP process that needs a daemon respawns one.
pub async fn run_daemon(socket: PathBuf, idle_timeout: Duration) -> anyhow::Result<()> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = match bind_or_yield(&socket).await? {
        Some(l) => l,
        None => {
            tracing::debug!("another engramdb daemon already owns {socket:?}; exiting");
            return Ok(());
        }
    };
    tracing::info!("engramdb daemon listening on {socket:?}");

    // Seed counters from the last persisted snapshot so request totals are
    // cumulative across daemon restarts.
    let base = metrics::load_latest()
        .await
        .map(|p| p.snapshot)
        .unwrap_or_default();
    let ctx = Arc::new(Ctx {
        cache: ProviderCache::new(),
        counters: Arc::new(Counters::seeded(base)),
        start: Instant::now(),
        pid: std::process::id(),
        last_activity: Mutex::new(Instant::now()),
    });
    let active = Arc::new(AtomicUsize::new(0));

    // Periodic persistence so an unclean exit (kill -9, crash) still leaves a
    // recent snapshot for `stats --daemon`.
    {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PERSIST_INTERVAL).await;
                ctx.persist().await;
            }
        });
    }

    // Idle watchdog: persist a final snapshot, then exit the process (leaving
    // the socket for the next daemon to reclaim) once nothing has used us for
    // `idle_timeout` and no connection is in flight.
    {
        let ctx = Arc::clone(&ctx);
        let active = Arc::clone(&active);
        let tick = idle_timeout
            .min(Duration::from_secs(30))
            .max(Duration::from_secs(1));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tick).await;
                if active.load(Ordering::SeqCst) == 0 {
                    let idle_for = ctx
                        .last_activity
                        .lock()
                        .map(|t| t.elapsed())
                        .unwrap_or_default();
                    if idle_for >= idle_timeout {
                        tracing::info!("engramdb daemon idle for {idle_for:?}; exiting");
                        ctx.persist().await;
                        std::process::exit(0);
                    }
                }
            }
        });
    }

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("daemon accept failed: {e}");
                continue;
            }
        };
        let ctx = Arc::clone(&ctx);
        let active = Arc::clone(&active);
        // Count the connection and stamp activity *before* spawning, so the
        // idle watchdog can't observe `active == 0` in the gap between
        // `accept` and the task's first instruction and exit the process out
        // from under a just-accepted client.
        active.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut t) = ctx.last_activity.lock() {
            *t = Instant::now();
        }
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, &ctx).await {
                tracing::debug!("daemon connection ended: {e}");
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// Bind the socket, reclaiming a stale one left by a crashed daemon.
///
/// Returns `Ok(None)` when a *live* daemon already owns the socket (this
/// process should exit), `Ok(Some(listener))` when we own it.
async fn bind_or_yield(socket: &Path) -> anyhow::Result<Option<UnixListener>> {
    match UnixListener::bind(socket) {
        Ok(l) => return Ok(Some(l)),
        Err(e) if e.kind() != ErrorKind::AddrInUse => return Err(e.into()),
        Err(_) => {}
    }
    // Path is occupied. If something answers, a live daemon owns it.
    if UnixStream::connect(socket).await.is_ok() {
        return Ok(None);
    }
    // No listener — the socket file is stale. Reclaim it atomically: bind a
    // private per-pid path, then `rename` it over the target. `rename` is
    // atomic and replaces the entry in-place, so there's never a window where
    // the target has no listener, and we can't unlink a socket a competing
    // daemon just bound at the target (we only ever touch our own temp path).
    let tmp = socket.with_extension(format!("tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let listener = UnixListener::bind(&tmp)?;
    if let Err(e) = std::fs::rename(&tmp, socket) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(Some(listener))
}

async fn handle_conn(stream: UnixStream, ctx: &Ctx) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    while let Some(req) = read_msg::<_, DaemonRequest>(&mut reader).await? {
        // Shutdown is terminal: ack, flush, persist, then exit the process.
        if let DaemonOp::Shutdown = req.op {
            write_msg(&mut write_half, &DaemonResponse::ShuttingDown).await?;
            tracing::info!("engramdb daemon shutting down on request");
            ctx.persist().await;
            std::process::exit(0);
        }

        let resp = dispatch(req, ctx).await;
        write_msg(&mut write_half, &resp).await?;
        // Stamp *after* serving so idle is measured from when work last
        // finished, not when it started — a slow inference call shouldn't
        // make the daemon look idle the moment it returns.
        if let Ok(mut t) = ctx.last_activity.lock() {
            *t = Instant::now();
        }
    }
    Ok(())
}

async fn dispatch(req: DaemonRequest, ctx: &Ctx) -> DaemonResponse {
    match req.op {
        DaemonOp::Shutdown => unreachable!("handled in handle_conn"),
        DaemonOp::Ping => {
            return DaemonResponse::Pong {
                version: PROTOCOL_VERSION.to_string(),
            }
        }
        DaemonOp::Status => {
            ctx.counters.incr_status();
            let s = ctx.counters.snapshot();
            let idle_secs = ctx
                .last_activity
                .lock()
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            return DaemonResponse::Status(DaemonStatus {
                version: PROTOCOL_VERSION.to_string(),
                pid: ctx.pid,
                uptime_secs: ctx.start.elapsed().as_secs(),
                idle_secs,
                bundles_loaded: ctx.cache.loaded_count().await,
                requests_embed: s.embed,
                requests_classify: s.classify,
                requests_rerank: s.rerank,
                requests_meta: s.meta,
                requests_status: s.status,
                requests_title: s.title,
                requests_total: s.total(),
            });
        }
        _ => {}
    }

    if req.dir.is_empty() {
        return DaemonResponse::Error {
            message: "missing store directory".to_string(),
        };
    }
    let config_path = Path::new(&req.dir).join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config(&config_path)
        .await
        .unwrap_or_default();
    // `req.backend` is the backend the client already resolved; trust it over
    // this daemon process's own environment so the provider-cache key (and
    // thus the loaded model) matches what the client expects.
    let providers = ctx.cache.get(&config, req.backend).await;

    match req.op {
        DaemonOp::Ping | DaemonOp::Status | DaemonOp::Shutdown => {
            unreachable!("handled above")
        }
        DaemonOp::Meta => {
            ctx.counters.incr_meta();
            match providers.embedding {
                Some(p) => DaemonResponse::Meta {
                    dimensions: p.dimensions(),
                    max_tokens: p.max_tokens(),
                    model_id: p.model_id(),
                },
                None => DaemonResponse::Error {
                    message: "embedding model unavailable".to_string(),
                },
            }
        }
        DaemonOp::Embed { texts } => {
            ctx.counters.incr_embed();
            match providers.embedding {
                Some(p) => {
                    let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                    match p.embed_batch(&refs).await {
                        Ok(vectors) => DaemonResponse::Embedded { vectors },
                        Err(e) => DaemonResponse::Error {
                            message: format!("embed failed: {e}"),
                        },
                    }
                }
                None => DaemonResponse::Error {
                    message: "embedding model unavailable".to_string(),
                },
            }
        }
        DaemonOp::Classify { pairs } => {
            ctx.counters.incr_classify();
            match providers.nli {
                Some(n) => {
                    let refs: Vec<(&str, &str)> = pairs
                        .iter()
                        .map(|(a, b)| (a.as_str(), b.as_str()))
                        .collect();
                    match n.classify_batch(&refs).await {
                        Ok(results) => DaemonResponse::Classified {
                            results: results
                                .into_iter()
                                .map(|r| NliWire {
                                    entailment: r.entailment,
                                    neutral: r.neutral,
                                    contradiction: r.contradiction,
                                })
                                .collect(),
                        },
                        Err(e) => DaemonResponse::Error {
                            message: format!("classify failed: {e}"),
                        },
                    }
                }
                None => DaemonResponse::Error {
                    message: "nli model unavailable".to_string(),
                },
            }
        }
        DaemonOp::Rerank { query, documents } => {
            ctx.counters.incr_rerank();
            match providers.reranker {
                Some(r) => match r.rerank(&query, &documents).await {
                    Ok(scores) => DaemonResponse::Reranked {
                        scores: scores.into_iter().map(|s| (s.index, s.score)).collect(),
                    },
                    Err(e) => DaemonResponse::Error {
                        message: format!("rerank failed: {e}"),
                    },
                },
                None => DaemonResponse::Error {
                    message: "reranker model unavailable".to_string(),
                },
            }
        }
        DaemonOp::Title { text } => {
            ctx.counters.incr_title();
            match providers.title {
                Some(t) => match t.generate(&text).await {
                    Ok(title) => DaemonResponse::Title { title },
                    Err(e) => DaemonResponse::Error {
                        message: format!("title generation failed: {e}"),
                    },
                },
                None => DaemonResponse::Error {
                    message: "title model unavailable".to_string(),
                },
            }
        }
    }
}
