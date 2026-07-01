//! The daemon process: loads each model once and serves inference.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, BufReader};

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
    /// Total number of `Ping` requests received since this process started.
    ping_count: AtomicU64,
    /// Timestamp of the most recent `Ping`, or `None` if no ping yet.
    last_ping: Mutex<Option<Instant>>,
    /// Signals `run_daemon` (and its background tasks) to wind down. Sent by
    /// the `Shutdown` handler and the idle watchdog. `run_daemon` *returns*
    /// when this fires; the process exit belongs to the binary front-end
    /// (`engramdb daemon run` exits when `run_daemon` returns), which keeps
    /// `run_daemon` fully drivable inside a test process.
    shutdown: tokio::sync::watch::Sender<bool>,
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

    /// Ask the accept loop (and background tasks) to stop.
    fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

/// Run the embedding daemon until idle-timeout or shutdown.
///
/// Startup is race-coordinated by the socket itself: only one process can be
/// bound to a given path. If a live daemon already owns the socket this
/// returns `Ok(())` (that daemon wins); a stale socket left by a crashed
/// daemon is detected (no listener answers) and reclaimed. Once serving, this
/// function returns after `idle_timeout` with no active connections, or when
/// a client sends `Shutdown` — the `engramdb daemon run` front-end then exits
/// the process (leaving the socket for the next daemon to reclaim), and the
/// next MCP process that needs a daemon respawns one. Returning instead of
/// calling `process::exit` here keeps `run_daemon` drivable in-process by
/// tests.
pub async fn run_daemon(socket: PathBuf, idle_timeout: Duration) -> anyhow::Result<()> {
    let listener: super::transport::Listener =
        match super::transport::bind_or_yield(&socket).await? {
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
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let ctx = Arc::new(Ctx {
        cache: ProviderCache::new(),
        counters: Arc::new(Counters::seeded(base)),
        start: Instant::now(),
        pid: std::process::id(),
        last_activity: Mutex::new(Instant::now()),
        ping_count: AtomicU64::new(0),
        last_ping: Mutex::new(None),
        shutdown: shutdown_tx,
    });
    let active = Arc::new(AtomicUsize::new(0));

    // Periodic persistence so an unclean exit (kill -9, crash) still leaves a
    // recent snapshot for `stats --daemon`.
    {
        let ctx = Arc::clone(&ctx);
        let mut shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(PERSIST_INTERVAL) => ctx.persist().await,
                    _ = shutdown.changed() => return,
                }
            }
        });
    }

    // Idle watchdog: persist a final snapshot, then signal shutdown (which
    // makes `run_daemon` return, and the daemon binary exit — leaving the
    // socket for the next daemon to reclaim) once nothing has used us for
    // `idle_timeout` and no connection is in flight.
    {
        let ctx = Arc::clone(&ctx);
        let active = Arc::clone(&active);
        let mut shutdown = shutdown_rx.clone();
        let tick = idle_timeout
            .min(Duration::from_secs(30))
            .max(Duration::from_secs(1));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tick) => {}
                    _ = shutdown.changed() => return,
                }
                if active.load(Ordering::SeqCst) == 0 {
                    let idle_for = ctx
                        .last_activity
                        .lock()
                        .map(|t| t.elapsed())
                        .unwrap_or_default();
                    if idle_for >= idle_timeout {
                        tracing::info!("engramdb daemon idle for {idle_for:?}; shutting down");
                        ctx.persist().await;
                        ctx.request_shutdown();
                        return;
                    }
                }
            }
        });
    }

    loop {
        let stream = tokio::select! {
            // Shutdown (requested by a client or the idle watchdog): stop
            // accepting and return. Dropping the listener refuses any further
            // connections; the socket *file* stays behind exactly as a killed
            // daemon would leave it, and the next daemon reclaims it.
            _ = shutdown_rx.changed() => {
                tracing::info!("engramdb daemon stopped accepting connections");
                return Ok(());
            }
            res = listener.accept() => match res {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("daemon accept failed: {e}");
                    continue;
                }
            },
        };
        // Defense layer 3 (Unix): verify the peer's kernel-reported uid via
        // `SO_PEERCRED` before serving anything. Layers 1+2 (0700 socket dir,
        // 0600 socket file — see `transport`) should already keep other users
        // out, but this check holds even if the socket path was relocated to
        // a directory with looser permissions. A rejected peer is dropped
        // before it can drive inference, read Status, probe arbitrary `dir`
        // config paths, or send an unauthenticated Shutdown.
        #[cfg(unix)]
        match stream.peer_cred() {
            Ok(cred) if peer_allowed(cred.uid(), super::current_euid()) => {}
            Ok(cred) => {
                tracing::warn!(
                    "daemon rejected connection from uid {} (serving uid {} only)",
                    cred.uid(),
                    super::current_euid()
                );
                continue;
            }
            Err(e) => {
                tracing::warn!("daemon rejected connection: peer credentials unavailable: {e}");
                continue;
            }
        }
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

/// Access policy for an accepted daemon connection: the peer's uid (from
/// `SO_PEERCRED`) must equal this process's effective uid. Root is
/// deliberately **not** exempt — a uid-0 peer of a non-root daemon is
/// rejected like any other mismatch. Root can already reach the models, data,
/// and the daemon process itself directly, so an exemption would buy nothing
/// while complicating the policy to two cases; a root client that genuinely
/// needs a daemon simply spawns one as itself (auto-spawn makes that free).
#[cfg(unix)]
pub(crate) fn peer_allowed(peer_uid: u32, my_euid: u32) -> bool {
    peer_uid == my_euid
}

/// Serve a single client connection. Generic over the transport stream so the
/// same dispatch loop runs over a Unix domain socket or a Windows named pipe.
async fn handle_conn<S>(stream: S, ctx: &Ctx) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    while let Some(req) = read_msg::<_, DaemonRequest>(&mut reader).await? {
        // Shutdown is terminal: ack, flush, persist, then signal the accept
        // loop so `run_daemon` returns (the daemon binary's main then exits).
        if let DaemonOp::Shutdown = req.op {
            write_msg(&mut write_half, &DaemonResponse::ShuttingDown).await?;
            tracing::info!("engramdb daemon shutting down on request");
            ctx.persist().await;
            ctx.request_shutdown();
            return Ok(());
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
            // Stamp ping stats BEFORE returning the Pong so Status queries
            // issued immediately after a Ping observe the updated counters.
            ctx.ping_count.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut t) = ctx.last_ping.lock() {
                *t = Some(Instant::now());
            }
            return DaemonResponse::Pong {
                version: PROTOCOL_VERSION.to_string(),
            };
        }
        DaemonOp::Status => {
            ctx.counters.incr_status();
            let s = ctx.counters.snapshot();
            let idle_secs = ctx
                .last_activity
                .lock()
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            let ping_count = ctx.ping_count.load(Ordering::Relaxed);
            let last_ping_secs_ago = ctx
                .last_ping
                .lock()
                .ok()
                .and_then(|guard| *guard)
                .map(|t| t.elapsed().as_secs());
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
                ping_count,
                last_ping_secs_ago,
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
        // Per-op counters are incremented only on a *successful* response, so
        // `stats --daemon` (persisted) reflects served work, not failed or
        // model-unavailable attempts (finding #11).
        DaemonOp::Meta => match providers.embedding {
            Some(p) => {
                ctx.counters.incr_meta();
                DaemonResponse::Meta {
                    dimensions: p.dimensions(),
                    max_tokens: p.max_tokens(),
                    model_id: p.model_id(),
                }
            }
            None => DaemonResponse::Error {
                message: "embedding model unavailable".to_string(),
            },
        },
        DaemonOp::Embed { texts } => match providers.embedding {
            Some(p) => {
                let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                match p.embed_batch(&refs).await {
                    Ok(vectors) => {
                        ctx.counters.incr_embed();
                        DaemonResponse::Embedded { vectors }
                    }
                    Err(e) => DaemonResponse::Error {
                        message: format!("embed failed: {e}"),
                    },
                }
            }
            None => DaemonResponse::Error {
                message: "embedding model unavailable".to_string(),
            },
        },
        DaemonOp::Classify { pairs } => match providers.nli {
            Some(n) => {
                let refs: Vec<(&str, &str)> = pairs
                    .iter()
                    .map(|(a, b)| (a.as_str(), b.as_str()))
                    .collect();
                match n.classify_batch(&refs).await {
                    Ok(results) => {
                        ctx.counters.incr_classify();
                        DaemonResponse::Classified {
                            results: results
                                .into_iter()
                                .map(|r| NliWire {
                                    entailment: r.entailment,
                                    neutral: r.neutral,
                                    contradiction: r.contradiction,
                                })
                                .collect(),
                        }
                    }
                    Err(e) => DaemonResponse::Error {
                        message: format!("classify failed: {e}"),
                    },
                }
            }
            None => DaemonResponse::Error {
                message: "nli model unavailable".to_string(),
            },
        },
        DaemonOp::Rerank { query, documents } => match providers.reranker {
            Some(r) => match r.rerank(&query, &documents).await {
                Ok(scores) => {
                    ctx.counters.incr_rerank();
                    DaemonResponse::Reranked {
                        scores: scores.into_iter().map(|s| (s.index, s.score)).collect(),
                    }
                }
                Err(e) => DaemonResponse::Error {
                    message: format!("rerank failed: {e}"),
                },
            },
            None => DaemonResponse::Error {
                message: "reranker model unavailable".to_string(),
            },
        },
        DaemonOp::Title { text } => match providers.title {
            Some(t) => match t.generate(&text).await {
                Ok(title) => {
                    ctx.counters.incr_title();
                    DaemonResponse::Title { title }
                }
                Err(e) => DaemonResponse::Error {
                    message: format!("title generation failed: {e}"),
                },
            },
            None => DaemonResponse::Error {
                message: "title model unavailable".to_string(),
            },
        },
    }
}
