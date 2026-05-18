//! The daemon process: loads each model once and serves inference.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};

use super::protocol::{
    read_msg, write_msg, DaemonOp, DaemonRequest, DaemonResponse, NliWire, PROTOCOL_VERSION,
};
use crate::ops::ProviderCache;

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

    let cache = ProviderCache::new();
    let active = Arc::new(AtomicUsize::new(0));
    let last_activity = Arc::new(Mutex::new(Instant::now()));

    // Idle watchdog: exit the process (leaving the socket for the next
    // daemon to reclaim) once nothing has used us for `idle_timeout` and no
    // connection is in flight.
    {
        let active = Arc::clone(&active);
        let last_activity = Arc::clone(&last_activity);
        let tick = idle_timeout
            .min(Duration::from_secs(30))
            .max(Duration::from_secs(1));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tick).await;
                if active.load(Ordering::SeqCst) == 0 {
                    let idle_for = last_activity
                        .lock()
                        .map(|t| t.elapsed())
                        .unwrap_or_default();
                    if idle_for >= idle_timeout {
                        tracing::info!("engramdb daemon idle for {idle_for:?}; exiting");
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
        let cache = cache.clone();
        let active = Arc::clone(&active);
        let last_activity = Arc::clone(&last_activity);
        tokio::spawn(async move {
            active.fetch_add(1, Ordering::SeqCst);
            if let Err(e) = handle_conn(stream, &cache, &last_activity).await {
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
    // No listener — the socket file is stale. Reclaim it.
    let _ = std::fs::remove_file(socket);
    match UnixListener::bind(socket) {
        Ok(l) => Ok(Some(l)),
        // Lost the reclaim race to another daemon that bound first.
        Err(e) if e.kind() == ErrorKind::AddrInUse => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn handle_conn(
    stream: UnixStream,
    cache: &ProviderCache,
    last_activity: &Mutex<Instant>,
) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    while let Some(req) = read_msg::<_, DaemonRequest>(&mut reader).await? {
        if let Ok(mut t) = last_activity.lock() {
            *t = Instant::now();
        }
        let resp = dispatch(req, cache).await;
        write_msg(&mut write_half, &resp).await?;
    }
    Ok(())
}

async fn dispatch(req: DaemonRequest, cache: &ProviderCache) -> DaemonResponse {
    if let DaemonOp::Ping = req.op {
        return DaemonResponse::Pong {
            version: PROTOCOL_VERSION.to_string(),
        };
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
    let providers = cache.get(&config, req.backend).await;

    match req.op {
        DaemonOp::Ping => unreachable!("handled above"),
        DaemonOp::Meta => match providers.embedding {
            Some(p) => DaemonResponse::Meta {
                dimensions: p.dimensions(),
                max_tokens: p.max_tokens(),
            },
            None => DaemonResponse::Error {
                message: "embedding model unavailable".to_string(),
            },
        },
        DaemonOp::Embed { texts } => match providers.embedding {
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
        },
        DaemonOp::Classify { pairs } => match providers.nli {
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
        },
        DaemonOp::Rerank { query, documents } => match providers.reranker {
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
        },
    }
}
