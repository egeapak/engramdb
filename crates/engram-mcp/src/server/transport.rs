//! The transports: stdio (`run_stdio`, with deferred startup and the
//! abandoned-probe filter), streamable HTTP (`run_sse`), and the shutdown
//! sequence they share.

use super::*;

// ---------------------------------------------------------------------------
// Server startup helpers
// ---------------------------------------------------------------------------

/// Start the MCP server with stdio transport.
///
/// **The handshake is answered before any startup work runs.** Claude Code
/// opens a stdio server with a `server/discover` probe and a short timeout.
/// The startup work in [`EngramDbServer::run_startup`] resolves the embedding
/// providers, which on a cold start spawns the shared daemon or loads ONNX
/// in-process — about 3 s. When that ran before `serve`, nothing read stdin
/// for those seconds, the probe timed out, and the session lost every tool
/// (see [`DiscoverProbeFilter`] for why the retry failed too). Now the work
/// runs in a background task, and tool calls and resource reads wait for it
/// (see [`serve_with_deferred_startup`]).
pub async fn run_stdio(
    dir: PathBuf,
    embedding_backend: Option<EmbeddingBackend>,
) -> anyhow::Result<()> {
    let stats_cfg = engramdb::types::EngramConfig::default().stats;
    let stats = engramdb::telemetry::StatsCollector::new(stats_cfg.clone());

    // Drain the collector's persistence channel into the per-project
    // LanceDB tables in the background. Capture the JoinHandle so we can
    // await it on shutdown — without this, tokio runtime teardown
    // cancels mid-`append_events` and tail events are lost.
    let flush_handle = stats.take_receiver().map(|rx| {
        engramdb::telemetry::persistence::spawn_flush_task(
            rx,
            stats_cfg.flush_interval_secs,
            stats_cfg.retention_days,
            Arc::downgrade(&stats),
        )
    });

    let mut server = EngramDbServer::new_with_stats(dir, embedding_backend, stats.clone())?;
    // Before the handshake, so the first `tools/list` already carries the
    // alwaysLoad `_meta`. Reading config.toml is cheap; the slow startup work
    // stays deferred.
    let always_load = server.configured_always_load().await;
    server.set_always_load(&always_load);
    let (stdin, stdout) = rmcp::transport::io::stdio();
    let transport = DiscoverProbeFilter::new(rmcp::transport::async_rw::AsyncRwTransport::<
        rmcp::RoleServer,
        _,
        _,
    >::new(stdin, stdout));
    let compaction = server.compaction_task();
    let (service, startup) = serve_with_deferred_startup(server, transport, |server| async move {
        server.run_startup().await
    })
    .await?;
    // `serve` consumes the server; the running service owns the remaining
    // `Arc<StatsCollector>` baked into it. `waiting` takes ownership and
    // consumes `service` on completion, so by the time it returns the rmcp
    // service has already dropped its clone; `finish_session` handles the
    // rest.
    service.waiting().await?;
    finish_session(startup, &compaction, stats, flush_handle).await;
    Ok(())
}

/// How long the shutdown sequence lets unfinished deferred startup work run.
///
/// Startup can be inside a schema migration (`reindex_with` carries an
/// unparseable file's row across the table clear in memory), and cancelling
/// it at an arbitrary await could lose that row; a bounded wait is cheap.
pub(crate) const SHUTDOWN_STARTUP_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// How long the shutdown sequence waits for background work tool calls
/// started (the query-triggered compaction and in-flight ingests).
pub(crate) const SHUTDOWN_DRAIN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long the shutdown sequence waits for the stats flush task.
const SHUTDOWN_FLUSH_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The shutdown sequence after the MCP session ended, in this order:
///
/// 1. **Startup** (≤ [`SHUTDOWN_STARTUP_WAIT`]): let unfinished deferred
///    startup finish; abort it only past the bound.
/// 2. **Drain** (≤ [`SHUTDOWN_DRAIN_WAIT`]): background work started by tool
///    calls — the query-triggered compaction and the detached ingest tasks
///    (`engramdb::retrieval::drain_ingests`), waited for together. This is the
///    one place further drains belong.
/// 3. **Stats flush** (≤ [`SHUTDOWN_FLUSH_WAIT`]): drop the last
///    `Arc<StatsCollector>` so the channel closes, then await the flush task.
///
/// The startup task and the compaction task each hold a server clone (and so
/// a stats `Arc`), which is why both end before step 3.
pub(crate) async fn finish_session(
    mut startup: tokio::task::JoinHandle<()>,
    compaction: &CompactionTask,
    stats: Arc<engramdb::telemetry::StatsCollector>,
    flush_handle: Option<tokio::task::JoinHandle<()>>,
) {
    // 1. Startup.
    if tokio::time::timeout(SHUTDOWN_STARTUP_WAIT, &mut startup)
        .await
        .is_err()
    {
        tracing::warn!(
            "EngramDB: startup work still running {SHUTDOWN_STARTUP_WAIT:?} after the session \
             ended; stopping it"
        );
        startup.abort();
        let _ = startup.await;
    }

    // 2. Drain: background work started by tool calls. The compaction and
    // the detached ingests (`create`/`update` with `embed_async`) run
    // concurrently, so both share the one bound.
    let ((), left) = tokio::join!(
        compaction.wait(SHUTDOWN_DRAIN_WAIT),
        engramdb::retrieval::drain_ingests(SHUTDOWN_DRAIN_WAIT),
    );
    if left > 0 {
        tracing::warn!(
            "EngramDB: {left} memory embedding(s) still running {SHUTDOWN_DRAIN_WAIT:?} after the              session ended; those memories have no vectors yet — run              `engramdb reindex --embeddings-only`"
        );
    }

    // 3. Stats flush.
    drop(stats);
    if let Some(h) = flush_handle {
        // Safety net: if some Arc clone unexpectedly outlives `stats` the
        // channel never closes and `h.await` would hang forever. Bound
        // the wait so a clean exit always happens; legitimate flushes
        // complete in milliseconds.
        let _ = tokio::time::timeout(SHUTDOWN_FLUSH_WAIT, h).await;
    }
}

/// Serve `server` on `transport` while `startup` runs in a background task.
///
/// `startup` gets a clone of the server and starts before the handshake, so
/// the two run concurrently: `initialize` (or `server/discover`), the list
/// calls and `ping` are answered at once, whatever `startup` is doing. Tool
/// calls and resource reads wait until `startup` finishes, so they still run
/// against a fully set-up project, exactly as when startup ran first. If
/// `startup` fails, those calls return its error instead of running.
///
/// Returns once the handshake is done, with the startup task's handle. The
/// caller aborts it when the session ends.
pub(super) async fn serve_with_deferred_startup<T, F, Fut>(
    mut server: EngramDbServer,
    transport: T,
    startup: F,
) -> anyhow::Result<(
    rmcp::service::RunningService<rmcp::RoleServer, EngramDbServer>,
    tokio::task::JoinHandle<()>,
)>
where
    T: rmcp::transport::Transport<rmcp::RoleServer> + 'static,
    F: FnOnce(EngramDbServer) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let (status_tx, status_rx) = tokio::sync::watch::channel(StartupStatus::Pending);
    server.startup = status_rx;
    let work = startup(server.clone());
    let startup_task = tokio::spawn(async move {
        let status = match work.await {
            Ok(()) => StartupStatus::Ready,
            Err(e) => {
                tracing::error!("EngramDB: startup failed: {e:#}");
                StartupStatus::Failed(format!("{e:#}"))
            }
        };
        // Fails only when every receiver is gone, i.e. nobody is waiting.
        let _ = status_tx.send(status);
    });
    match server.serve(transport).await {
        Ok(service) => Ok((service, startup_task)),
        Err(e) => {
            startup_task.abort();
            Err(e.into())
        }
    }
}

/// How long [`DiscoverProbeFilter`] waits after a `server/discover` opener for
/// an `initialize` that shows the probe was abandoned.
const ABANDONED_PROBE_WINDOW: std::time::Duration = std::time::Duration::from_millis(25);

/// A stdio transport wrapper that drops a `server/discover` probe the client
/// has already given up on.
///
/// **This worked around rmcp 3.4.0 behavior**, which rmcp 3.8 fixed (see
/// `rmcp_answers_an_abandoned_discover_probe`): the session lock described
/// next is gone, but rmcp still *answers* the stale probe, and that response
/// alone makes Claude Code drop the connection — so the filter stays.
///
/// Under rmcp 3.4.0, when the first request on a
/// connection is anything but `initialize`, rmcp's `serve_server` marks the
/// whole session as the 2026-07-28 inline lifecycle
/// (`Peer::require_request_metadata`), and nothing clears that mark. A later
/// `initialize` on the same connection is answered, but every request after
/// it is then rejected unless it carries the modern per-request `_meta`
/// ("request _meta is missing or has malformed required fields:
/// io.modelcontextprotocol/protocolVersion, …").
///
/// That is exactly what a client in "probe, then fall back" mode produces
/// when the server is slow to read stdin. Claude Code (and rmcp's own
/// `ClientLifecycleMode::Auto`) sends `server/discover`; on a timeout it sends
/// a legacy `initialize` on the same pipe. A server that only then starts
/// reading finds the stale probe first, answers it (the client reports
/// "Received a response for an unknown message ID" and drops the
/// connection), and is left expecting modern `_meta` that the legacy client
/// never sends.
///
/// So when the first message is a `server/discover` request, this wrapper
/// waits up to [`ABANDONED_PROBE_WINDOW`] for the next message. If that is an
/// `initialize`, the client has abandoned the probe: it is dropped unanswered
/// (the client stopped waiting for its response) and the session starts on
/// the legacy lifecycle the client chose. Anything else is delivered in
/// order behind the probe, so a client that really uses `server/discover`
/// pays at most the window once.
///
/// Serving the handshake before the slow startup work (see [`run_stdio`])
/// removes the usual cause of a late probe; this covers the rest (an
/// overloaded machine, a stalled disk).
pub(super) struct DiscoverProbeFilter<T> {
    inner: T,
    first_checked: bool,
    queued: Option<ClientJsonRpcMessage>,
    eof: bool,
}

impl<T> DiscoverProbeFilter<T> {
    pub(super) fn new(inner: T) -> Self {
        Self {
            inner,
            first_checked: false,
            queued: None,
            eof: false,
        }
    }
}

fn is_client_request(msg: &ClientJsonRpcMessage, pred: impl Fn(&ClientRequest) -> bool) -> bool {
    matches!(msg, JsonRpcMessage::Request(req) if pred(&req.request))
}

impl<T> rmcp::transport::Transport<rmcp::RoleServer> for DiscoverProbeFilter<T>
where
    T: rmcp::transport::Transport<rmcp::RoleServer>,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        if let Some(msg) = self.queued.take() {
            return Some(msg);
        }
        if self.eof {
            return None;
        }
        let msg = self.inner.receive().await?;
        if std::mem::replace(&mut self.first_checked, true)
            || !is_client_request(&msg, |r| matches!(r, ClientRequest::DiscoverRequest(_)))
        {
            return Some(msg);
        }
        // `receive` is cancel-safe for rmcp's stdio transport (a partial line
        // stays buffered), so timing it out loses nothing.
        match tokio::time::timeout(ABANDONED_PROBE_WINDOW, self.inner.receive()).await {
            Ok(Some(next))
                if is_client_request(&next, |r| {
                    matches!(r, ClientRequest::InitializeRequest(_))
                }) =>
            {
                tracing::info!(
                    "EngramDB: dropping an abandoned server/discover probe; \
                     the client fell back to initialize"
                );
                return Some(next);
            }
            Ok(Some(next)) => self.queued = Some(next),
            Ok(None) => self.eof = true,
            Err(_elapsed) => {}
        }
        Some(msg)
    }

    fn close(&mut self) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

/// Configuration for the streamable-HTTP transport.
///
/// **Every request that carries an `Origin` header is refused** (403). The
/// MCP specification requires servers to validate `Origin`, and this server
/// has no authentication while serving the user's memories and conversation
/// history. Browsers always send `Origin` on a cross-site request; MCP
/// clients (Claude Code, other CLI and desktop clients) send none, so they are
/// unaffected. rmcp leaves this check off by default for backward
/// compatibility; `enforce_origin_validation` with no allowed origins turns it
/// on. The `Host` check that rmcp does enable by default (loopback names only)
/// stays in place and is the DNS-rebinding guard; this one also covers a page
/// that reaches `127.0.0.1` directly.
///
/// To let a specific browser-based client in, list its origin with
/// `with_allowed_origins` rather than removing the check.
pub(super) fn http_server_config(
) -> rmcp::transport::streamable_http_server::StreamableHttpServerConfig {
    rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default()
        .enforce_origin_validation()
}

/// Start the MCP server with streamable HTTP transport.
pub async fn run_sse(
    dir: PathBuf,
    port: u16,
    embedding_backend: Option<EmbeddingBackend>,
) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpService,
    };
    use std::sync::Arc;

    // One process-global stats collector shared across every per-connection
    // server instance. Hydrate once and spawn a single flush task.
    let stats_cfg = engramdb::types::EngramConfig::default().stats;
    let stats = engramdb::telemetry::StatsCollector::new(stats_cfg.clone());
    let flush_handle = stats.take_receiver().map(|rx| {
        engramdb::telemetry::persistence::spawn_flush_task(
            rx,
            stats_cfg.flush_interval_secs,
            stats_cfg.retention_days,
            Arc::downgrade(&stats),
        )
    });

    // One process-wide model cache + daemon handle shared across every
    // per-connection server, so the embedding model loads once (or the
    // daemon is probed/spawned once) for the whole process rather than per
    // HTTP connection.
    let provider_cache = ops::ProviderCache::new();
    let daemon: Arc<engramdb::daemon::DaemonCell> = Arc::new(engramdb::daemon::DaemonCell::new());

    // Resolve hierarchy eagerly once: subsequent per-connection server
    // instances share the registry, so registration only happens once.
    //
    // The embedding-model-change policy is evaluated here as well. The
    // per-connection servers built by the factory closure below are
    // short-lived and never run startup logic, so without this every SSE
    // connection would silently skip the check (no embedding notice)
    // and `auto` mode would never re-embed. Mirror `run_stdio`: compute the
    // warning (and auto-reindex if requested) exactly once, then seed every
    // per-connection server with the resulting warning. The warmup server
    // shares the process-wide model cache / daemon so the check (and any
    // auto-reindex) reuse the same providers the connections will.
    //
    // Unlike stdio, this runs before the listener binds: an HTTP client is
    // not probing a pipe on a short timeout, and a connection made during
    // startup would only queue.
    let embedding_warning = {
        let warmup = EngramDbServer::new_with_stats(dir.clone(), embedding_backend, stats.clone())?
            .with_shared_model_caches(provider_cache.clone(), daemon.clone());
        warmup.run_startup().await?;
        (
            warmup.embedding_notice.get(),
            warmup.configured_always_load().await,
            // Shared by every connection, so the one-time registration
            // notice reaches the first session that can see it, once.
            warmup.registration_notice.clone(),
        )
    };
    let (embedding_warning, always_load, registration_notice) = embedding_warning;

    let config = http_server_config();
    let ct = config.cancellation_token.clone();
    let service = StreamableHttpService::new(
        {
            let stats = stats.clone();
            let embedding_warning = embedding_warning.clone();
            let provider_cache = provider_cache.clone();
            let daemon = daemon.clone();
            let always_load = always_load.clone();
            let registration_notice = registration_notice.clone();
            move || {
                let mut server =
                    EngramDbServer::new_with_stats(dir.clone(), embedding_backend, stats.clone())
                        .map(|s| s.with_shared_model_caches(provider_cache.clone(), daemon.clone()))
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                // A notice per connection: each session's instructions
                // carry the warning, not only the first one's.
                server.embedding_notice = Arc::new(StartupNotice::new(embedding_warning.clone()));
                server.registration_notice = registration_notice.clone();
                server.set_always_load(&always_load);
                Ok(server)
            }
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("EngramDB MCP server listening on {}", addr);

    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await?;

    // In-flight ingests (`create`/`update` embed in the background), bounded
    // like the stdio shutdown's drain step.
    let left = engramdb::retrieval::drain_ingests(SHUTDOWN_DRAIN_WAIT).await;
    if left > 0 {
        tracing::warn!(
            "EngramDB: {left} memory embedding(s) still running at shutdown; those memories \
             have no vectors yet — run `engramdb reindex --embeddings-only`"
        );
    }

    // Drop the collector — closing the channel triggers the flush task to
    // drain any buffered events and exit cleanly. Then await the task so
    // tokio runtime teardown doesn't cancel it mid-`append_events`.
    //
    // Safety net: if any per-connection `EngramDbServer` Arc clone outlives
    // `axum::serve` returning (e.g. via `LocalSessionManager` retention),
    // the channel never closes and the await would hang. Bound the wait.
    drop(stats);
    if let Some(h) = flush_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
    }
    Ok(())
}
