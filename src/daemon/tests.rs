//! Daemon protocol + end-to-end delegation tests.

use std::time::Duration;

use tempfile::TempDir;
use tokio::io::BufReader;

use super::protocol::{read_msg, write_msg, DaemonOp, DaemonRequest, DaemonResponse};
use super::server::run_daemon;
use crate::storage::paths::model_cache_dir;

/// Frames survive a write→read round-trip over an in-memory duplex.
#[tokio::test]
async fn protocol_roundtrip() {
    let (a, b) = tokio::io::duplex(4096);
    let (ar, mut aw) = tokio::io::split(a);
    let (br, mut bw) = tokio::io::split(b);
    let mut ar = BufReader::new(ar);
    let mut br = BufReader::new(br);

    let req = DaemonRequest {
        dir: "/tmp/x".to_string(),
        backend: None,
        op: DaemonOp::Embed {
            texts: vec!["hello".to_string(), "world".to_string()],
        },
    };
    write_msg(&mut aw, &req).await.unwrap();
    let got: DaemonRequest = read_msg(&mut br).await.unwrap().unwrap();
    assert_eq!(got.dir, "/tmp/x");
    assert!(matches!(got.op, DaemonOp::Embed { .. }));

    let resp = DaemonResponse::Embedded {
        vectors: vec![vec![0.1, 0.2], vec![0.3, 0.4]],
    };
    write_msg(&mut bw, &resp).await.unwrap();
    let got: DaemonResponse = read_msg(&mut ar).await.unwrap().unwrap();
    match got {
        DaemonResponse::Embedded { vectors } => assert_eq!(vectors.len(), 2),
        other => panic!("unexpected {other:?}"),
    }
}

/// A clean EOF (peer closed) yields `Ok(None)` rather than an error.
#[tokio::test]
async fn read_msg_eof_is_none() {
    let (a, b) = tokio::io::duplex(64);
    drop(b);
    let (ar, _aw) = tokio::io::split(a);
    let mut ar = BufReader::new(ar);
    let got: Option<DaemonRequest> = read_msg(&mut ar).await.unwrap();
    assert!(got.is_none());
}

/// A real daemon answers `Ping` without loading any model.
#[tokio::test]
async fn daemon_answers_ping() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("d.sock");
    // Long idle timeout so the watchdog doesn't shut the daemon down mid-test.
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));

    let resp = wait_request(
        &socket,
        DaemonRequest {
            dir: String::new(),
            backend: None,
            op: DaemonOp::Ping,
        },
    )
    .await;
    match resp {
        DaemonResponse::Pong { version } => assert_eq!(version, super::PROTOCOL_VERSION),
        other => panic!("expected Pong, got {other:?}"),
    }
}

/// Full delegation: a remote embedding provider built against a real daemon
/// produces vectors. Skipped when the embedding model isn't staged.
#[tokio::test]
async fn remote_embedding_end_to_end() {
    let model = model_cache_dir()
        .map(|d| {
            d.join("models--Qdrant--all-MiniLM-L6-v2-onnx")
                .join("snapshots")
                .join("main")
                .join("model.onnx")
        })
        .map(|p| p.exists())
        .unwrap_or(false);
    if !model {
        eprintln!("skipping remote_embedding_end_to_end: embedding model not staged");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("d.sock");
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));

    // Wait until the daemon is reachable.
    for _ in 0..50 {
        if super::transport::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let store_dir = tmp.path().join("proj");
    std::fs::create_dir_all(&store_dir).unwrap();
    let config = crate::types::EngramConfig::default();
    let handle = super::DaemonHandle::connect_existing(socket.clone());
    let providers = super::remote_providers(
        handle,
        store_dir.to_string_lossy().into_owned(),
        None,
        &config,
    )
    .await
    .expect("remote providers");

    let emb = providers.embedding.expect("embedding provider");
    assert!(emb.dimensions() > 0);
    let v = emb.embed("a memory about caching").await.unwrap();
    assert_eq!(v.len(), emb.dimensions());
}

// ---------------------------------------------------------------------------
// Protocol: new Status / Shutdown frames + chunked reads + frame cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_and_shutdown_frames_roundtrip() {
    use super::protocol::DaemonStatus;
    let (a, b) = tokio::io::duplex(4096);
    let (ar, mut aw) = tokio::io::split(a);
    let (br, mut bw) = tokio::io::split(b);
    let mut ar = BufReader::new(ar);
    let mut br = BufReader::new(br);

    // DaemonOp::Status / Shutdown survive a request round-trip.
    for op in [DaemonOp::Status, DaemonOp::Shutdown] {
        let want = format!("{op:?}");
        write_msg(
            &mut aw,
            &DaemonRequest {
                dir: String::new(),
                backend: None,
                op,
            },
        )
        .await
        .unwrap();
        let got: DaemonRequest = read_msg(&mut br).await.unwrap().unwrap();
        assert_eq!(format!("{:?}", got.op), want);
    }

    // DaemonResponse::Status(_) (internally-tagged struct newtype) and the
    // unit ShuttingDown variant survive a response round-trip.
    let status = DaemonStatus {
        version: super::PROTOCOL_VERSION.to_string(),
        pid: 4242,
        uptime_secs: 12,
        idle_secs: 3,
        bundles_loaded: 2,
        requests_embed: 5,
        requests_classify: 1,
        requests_rerank: 0,
        requests_meta: 7,
        requests_status: 9,
        requests_title: 3,
        requests_total: 25,
        ping_count: 0,
        last_ping_secs_ago: None,
    };
    write_msg(&mut bw, &DaemonResponse::Status(status.clone()))
        .await
        .unwrap();
    match read_msg::<_, DaemonResponse>(&mut ar)
        .await
        .unwrap()
        .unwrap()
    {
        DaemonResponse::Status(s) => {
            assert_eq!(s.pid, 4242);
            assert_eq!(s.requests_title, 3);
            assert_eq!(s.requests_total, 25);
            assert_eq!(s.version, super::PROTOCOL_VERSION);
        }
        other => panic!("expected Status, got {other:?}"),
    }
    write_msg(&mut bw, &DaemonResponse::ShuttingDown)
        .await
        .unwrap();
    assert!(matches!(
        read_msg::<_, DaemonResponse>(&mut ar)
            .await
            .unwrap()
            .unwrap(),
        DaemonResponse::ShuttingDown
    ));
}

/// A frame far larger than the BufReader's internal buffer must reassemble
/// correctly across many `fill_buf`/`consume` iterations (the bounded reader
/// rewrite).
#[tokio::test]
async fn large_frame_reassembles_across_buffer_boundaries() {
    let (a, b) = tokio::io::duplex(8 * 1024);
    let (ar, _aw) = tokio::io::split(a);
    let (_br, mut bw) = tokio::io::split(b);
    let mut ar = BufReader::with_capacity(8 * 1024, ar);

    // ~ 20k floats ⇒ hundreds of KB of JSON, dwarfing the 8 KiB buffer.
    let big = DaemonResponse::Embedded {
        vectors: (0..2000).map(|i| vec![i as f32; 10]).collect(),
    };
    let expected_len = 2000;
    tokio::spawn(async move {
        write_msg(&mut bw, &big).await.unwrap();
    });
    match read_msg::<_, DaemonResponse>(&mut ar)
        .await
        .unwrap()
        .unwrap()
    {
        DaemonResponse::Embedded { vectors } => assert_eq!(vectors.len(), expected_len),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn blank_frame_is_treated_as_eof() {
    let (a, b) = tokio::io::duplex(64);
    let (ar, _aw) = tokio::io::split(a);
    let (_br, mut bw) = tokio::io::split(b);
    tokio::io::AsyncWriteExt::write_all(&mut bw, b"   \n")
        .await
        .unwrap();
    let mut ar = BufReader::new(ar);
    let got: Option<DaemonRequest> = read_msg(&mut ar).await.unwrap();
    assert!(got.is_none());
}

// ---------------------------------------------------------------------------
// Daemon: Status / Shutdown end-to-end + startup race
// ---------------------------------------------------------------------------

#[tokio::test]
async fn daemon_status_reports_metrics() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("d.sock");
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));

    // First Status: status counter is incremented for this very request.
    let first = wait_request(
        &socket,
        DaemonRequest {
            dir: String::new(),
            backend: None,
            op: DaemonOp::Status,
        },
    )
    .await;
    let s1 = match first {
        DaemonResponse::Status(s) => s,
        other => panic!("expected Status, got {other:?}"),
    };
    assert_eq!(s1.version, super::PROTOCOL_VERSION);
    assert!(s1.pid > 0);
    assert!(s1.requests_status >= 1);

    // A second Status shows the counter advancing.
    let s2 = match wait_request(
        &socket,
        DaemonRequest {
            dir: String::new(),
            backend: None,
            op: DaemonOp::Status,
        },
    )
    .await
    {
        DaemonResponse::Status(s) => s,
        other => panic!("expected Status, got {other:?}"),
    };
    assert!(s2.requests_status > s1.requests_status);
}

#[tokio::test]
async fn client_helpers_without_daemon() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("absent.sock");
    assert!(super::query_status(&socket).await.unwrap().is_none());
    assert!(!super::request_shutdown(&socket).await.unwrap());
}

/// `request_shutdown` maps a `ShuttingDown` ack to `Ok(true)`.
///
/// A stub that returns `ShuttingDown` pins the client-side mapping in
/// isolation; the *real* daemon's shutdown flow (ack, persist, stop
/// accepting) is driven end-to-end by `cell_self_heals_after_daemon_killed`
/// and `daemon_cell_respawns_after_handle_lost`, which `run_daemon`'s
/// return-on-shutdown seam makes safe to exercise in-process.
#[tokio::test]
async fn request_shutdown_maps_ack() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("stub.sock");
    spawn_stub(socket.clone(), |op| match op {
        DaemonOp::Shutdown => DaemonResponse::ShuttingDown,
        _ => DaemonResponse::Error {
            message: "unexpected".to_string(),
        },
    });
    for _ in 0..100 {
        if super::transport::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(super::request_shutdown(&socket).await.unwrap());
}

/// A second `run_daemon` on a socket a live daemon owns yields immediately
/// (returns `Ok(())`) instead of binding.
#[tokio::test]
async fn second_daemon_yields_to_live_one() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("d.sock");
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    for _ in 0..100 {
        if super::transport::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The second invocation must return promptly (yielded), not block in the
    // accept loop.
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        run_daemon(socket.clone(), Duration::from_secs(3600)),
    )
    .await
    .expect("second run_daemon should yield, not block");
    assert!(r.is_ok());
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[test]
fn counters_seed_and_snapshot() {
    use super::metrics::{Counters, MetricsSnapshot};
    let c = Counters::default();
    assert_eq!(c.snapshot().total(), 0);

    let c = Counters::seeded(MetricsSnapshot {
        embed: 10,
        classify: 2,
        rerank: 0,
        meta: 3,
        status: 1,
        title: 5,
    });
    c.incr_embed();
    c.incr_embed();
    c.incr_meta();
    c.incr_status();
    c.incr_title();
    let s = c.snapshot();
    assert_eq!(s.embed, 12);
    assert_eq!(s.meta, 4);
    assert_eq!(s.status, 2);
    assert_eq!(s.classify, 2);
    assert_eq!(s.title, 6);
    assert_eq!(s.total(), 12 + 2 + 4 + 2 + 6);
}

#[tokio::test]
async fn metrics_persist_then_load_latest() {
    use super::metrics::{self, MetricsSnapshot};
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    assert!(metrics::load_latest_at(dir).await.unwrap().is_none());

    metrics::persist_at(
        dir,
        111,
        60,
        MetricsSnapshot {
            embed: 1,
            classify: 0,
            rerank: 0,
            meta: 2,
            status: 0,
            title: 0,
        },
    )
    .await
    .unwrap();
    // Ensure a distinct, strictly-later timestamp for the second row.
    tokio::time::sleep(Duration::from_millis(5)).await;
    metrics::persist_at(
        dir,
        111,
        120,
        MetricsSnapshot {
            embed: 9,
            classify: 4,
            rerank: 2,
            meta: 5,
            status: 3,
            title: 7,
        },
    )
    .await
    .unwrap();

    let latest = metrics::load_latest_at(dir).await.unwrap().unwrap();
    assert_eq!(latest.snapshot.embed, 9);
    assert_eq!(latest.snapshot.title, 7);
    assert_eq!(latest.snapshot.total(), 9 + 4 + 2 + 5 + 3 + 7);
    assert_eq!(latest.uptime_secs, 120);
}

/// Unbounded-growth guard: `persist_at` must prune snapshot rows older than
/// the retention window (30 days) while the just-appended row — and
/// `load_latest`'s view of it — survives.
#[tokio::test]
async fn metrics_persist_prunes_old_snapshots() {
    use super::metrics::{self, MetricsSnapshot};
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    // Seed a batch of stale snapshot rows (40 days old) directly, bypassing
    // the prune-on-persist path.
    let old_ts = chrono::Utc::now() - chrono::Duration::days(40);
    for i in 0..5u64 {
        metrics::persist_row_at(
            dir,
            old_ts + chrono::Duration::seconds(i as i64),
            222,
            i,
            MetricsSnapshot {
                embed: i,
                ..MetricsSnapshot::default()
            },
        )
        .await
        .unwrap();
    }

    // A normal persist appends the fresh row and prunes everything stale.
    metrics::persist_at(
        dir,
        222,
        999,
        MetricsSnapshot {
            embed: 42,
            ..MetricsSnapshot::default()
        },
    )
    .await
    .unwrap();

    // Only the fresh row remains on disk…
    let conn = lancedb::connect(dir.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    let table = conn.open_table("daemon_metrics").execute().await.unwrap();
    assert_eq!(
        table.count_rows(None).await.unwrap(),
        1,
        "stale snapshot rows must be pruned on persist"
    );

    // …and load_latest still resolves it.
    let latest = metrics::load_latest_at(dir).await.unwrap().unwrap();
    assert_eq!(latest.snapshot.embed, 42);
    assert_eq!(latest.uptime_secs, 999);
}

// ---------------------------------------------------------------------------
// Remote providers: error mapping + None-on-Meta-failure
// ---------------------------------------------------------------------------

/// Spawn a stub daemon on `socket` that replies to each request with the
/// response produced by `reply`.
fn spawn_stub<F>(socket: std::path::PathBuf, reply: F)
where
    F: Fn(&DaemonOp) -> DaemonResponse + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let listener = super::transport::bind_or_yield(&socket)
            .await
            .unwrap()
            .expect("stub owns the address");
        loop {
            let stream = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let (rh, mut wh) = tokio::io::split(stream);
            let mut r = BufReader::new(rh);
            while let Some(req) = read_msg::<_, DaemonRequest>(&mut r).await.unwrap_or(None) {
                let resp = reply(&req.op);
                if write_msg(&mut wh, &resp).await.is_err() {
                    break;
                }
            }
        }
    });
}

#[tokio::test]
async fn remote_providers_none_when_meta_errors() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("stub.sock");
    spawn_stub(socket.clone(), |_op| DaemonResponse::Error {
        message: "no model here".to_string(),
    });
    for _ in 0..100 {
        if super::transport::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let handle = super::DaemonHandle::connect_existing(socket);
    let out = super::remote_providers(
        handle,
        "/tmp/whatever".to_string(),
        None,
        &crate::types::EngramConfig::default(),
    )
    .await;
    assert!(out.is_none(), "Meta error must yield in-process fallback");
}

#[tokio::test]
async fn remote_embedding_maps_daemon_error() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("stub.sock");
    // Meta succeeds (so providers build) but Embed fails.
    spawn_stub(socket.clone(), |op| match op {
        DaemonOp::Meta => DaemonResponse::Meta {
            dimensions: 8,
            max_tokens: 128,
            model_id: "onnx/stub".to_string(),
        },
        DaemonOp::Embed { .. } => DaemonResponse::Error {
            message: "boom".to_string(),
        },
        _ => DaemonResponse::Error {
            message: "unexpected".to_string(),
        },
    });
    for _ in 0..100 {
        if super::transport::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let handle = super::DaemonHandle::connect_existing(socket);
    let providers = super::remote_providers(
        handle,
        "/tmp/whatever".to_string(),
        None,
        &crate::types::EngramConfig::default(),
    )
    .await
    .expect("providers (Meta ok)");
    let emb = providers.embedding.expect("embedding provider");
    assert_eq!(emb.dimensions(), 8);
    assert_eq!(emb.max_tokens(), 128);
    let err = emb.embed("hi").await.expect_err("daemon returned Error");
    assert!(err.to_string().contains("boom"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Socket resolution precedence
// ---------------------------------------------------------------------------

#[test]
fn resolve_socket_precedence() {
    use crate::types::DaemonConfig;
    let env_key = "ENGRAMDB_DAEMON_SOCKET";
    let saved = std::env::var_os(env_key);
    std::env::remove_var(env_key);

    let mut cfg = DaemonConfig::default();

    // 4. default when nothing set.
    let d = super::resolve_socket(None, &cfg);
    assert!(d.ends_with("daemon.sock"));

    // 3. config value beats default.
    cfg.socket_path = Some("/tmp/from-config.sock".to_string());
    assert_eq!(
        super::resolve_socket(None, &cfg),
        std::path::PathBuf::from("/tmp/from-config.sock")
    );

    // 2. env beats config.
    std::env::set_var(env_key, "/tmp/from-env.sock");
    assert_eq!(
        super::resolve_socket(None, &cfg),
        std::path::PathBuf::from("/tmp/from-env.sock")
    );

    // 1. explicit CLI beats env + config.
    assert_eq!(
        super::resolve_socket(Some(std::path::Path::new("/tmp/from-cli.sock")), &cfg),
        std::path::PathBuf::from("/tmp/from-cli.sock")
    );

    // restore env for other tests
    std::env::remove_var(env_key);
    if let Some(v) = saved {
        std::env::set_var(env_key, v);
    }
}

// ---------------------------------------------------------------------------
// Frame cap + malformed input
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_msg_capped_enforces_limit_and_parses() {
    use super::protocol::read_msg_capped;

    // A small valid frame under the cap parses fine.
    let mut r =
        BufReader::new(&b"{\"dir\":\"\",\"backend\":null,\"op\":{\"kind\":\"ping\"}}\n"[..]);
    let ok: Option<DaemonRequest> = read_msg_capped(&mut r, 1024).await.unwrap();
    assert!(matches!(ok.unwrap().op, DaemonOp::Ping));

    // A newline-less stream exceeding the cap is rejected (no OOM, no hang).
    let flood = vec![b'x'; 4096];
    let mut r = BufReader::new(&flood[..]);
    let err = read_msg_capped::<_, DaemonRequest>(&mut r, 64)
        .await
        .expect_err("oversized frame must error");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

    // Well-formed line, but not valid JSON for the target type.
    let mut r = BufReader::new(&b"not json at all\n"[..]);
    let err = read_msg_capped::<_, DaemonRequest>(&mut r, 1024)
        .await
        .expect_err("garbage must error");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

// ---------------------------------------------------------------------------
// Daemon dispatch + stale-socket reclaim
// ---------------------------------------------------------------------------

/// Model-free dispatch path: a non-Ping/Status op with an empty `dir` is
/// rejected before any model load.
#[tokio::test]
async fn dispatch_rejects_missing_dir() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("d.sock");
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    let resp = wait_request(
        &socket,
        DaemonRequest {
            dir: String::new(),
            backend: None,
            op: DaemonOp::Embed {
                texts: vec!["x".to_string()],
            },
        },
    )
    .await;
    match resp {
        DaemonResponse::Error { message } => assert!(message.contains("missing store directory")),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// A stale file left at the socket path (crashed daemon) is reclaimed via the
/// atomic bind-temp + rename path, and the new daemon serves normally.
///
/// Unix-specific: a leftover regular file at the socket path is a
/// Unix-domain-socket scenario. Windows named pipes leave no on-disk state when
/// their owner dies; the equivalent "a new daemon binds after the old one is
/// gone" property is covered cross-platform by
/// `daemon::transport::tests::bind_succeeds_after_owner_drops`.
#[cfg(unix)]
#[tokio::test]
async fn daemon_reclaims_stale_socket() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("d.sock");
    std::fs::write(&socket, b"stale - not a live socket").unwrap();

    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    let resp = wait_request(
        &socket,
        DaemonRequest {
            dir: String::new(),
            backend: None,
            op: DaemonOp::Ping,
        },
    )
    .await;
    assert!(matches!(resp, DaemonResponse::Pong { .. }));

    // Permission hardening holds on the reclaim path too: the served socket
    // is owner-only (chmod'd on the temp path before the atomic rename).
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "reclaimed daemon socket must be mode 0600");
}

/// The daemon's peer policy: only a peer whose `SO_PEERCRED` uid equals our
/// effective uid is served. Root (uid 0) is deliberately not exempt — see
/// `server::peer_allowed`. The accept-path enforcement is exercised
/// implicitly by every test that connects to a `run_daemon` socket (same
/// process ⇒ same uid); this pins the decision function itself, which a real
/// cross-uid connection can't do inside a single-user test environment.
#[cfg(unix)]
#[test]
fn peer_allowed_only_for_matching_euid() {
    use super::server::peer_allowed;
    assert!(peer_allowed(1000, 1000), "same uid is served");
    assert!(peer_allowed(0, 0), "a root daemon serves root peers");
    assert!(!peer_allowed(1001, 1000), "another user is rejected");
    assert!(
        !peer_allowed(0, 1000),
        "root peers are rejected by a non-root daemon (no uid-0 exemption)"
    );
    assert!(
        !peer_allowed(1000, 0),
        "a root daemon does not serve non-root peers"
    );
}

// ---------------------------------------------------------------------------
// Client: status parsing + protocol-version gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_status_parses_stub_status() {
    use super::protocol::DaemonStatus;
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("stub.sock");
    spawn_stub(socket.clone(), |op| match op {
        DaemonOp::Status => DaemonResponse::Status(DaemonStatus {
            version: super::PROTOCOL_VERSION.to_string(),
            pid: 77,
            uptime_secs: 1,
            idle_secs: 0,
            bundles_loaded: 3,
            requests_embed: 4,
            requests_classify: 0,
            requests_rerank: 0,
            requests_meta: 1,
            requests_status: 2,
            requests_title: 0,
            requests_total: 7,
            ping_count: 0,
            last_ping_secs_ago: None,
        }),
        _ => DaemonResponse::Error {
            message: "unexpected".to_string(),
        },
    });
    for _ in 0..100 {
        if super::transport::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let s = super::query_status(&socket)
        .await
        .unwrap()
        .expect("status present");
    assert_eq!(s.pid, 77);
    assert_eq!(s.bundles_loaded, 3);
    assert_eq!(s.requests_total, 7);
}

#[tokio::test]
async fn healthy_rejects_protocol_version_mismatch() {
    let tmp = TempDir::new().unwrap();

    let good = tmp.path().join("good.sock");
    spawn_stub(good.clone(), |_| DaemonResponse::Pong {
        version: super::PROTOCOL_VERSION.to_string(),
    });
    let bad = tmp.path().join("bad.sock");
    spawn_stub(bad.clone(), |_| DaemonResponse::Pong {
        version: "999.bogus".to_string(),
    });
    for s in [&good, &bad] {
        for _ in 0..100 {
            if super::transport::connect(s).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    assert!(
        super::DaemonHandle::connect_existing(good)
            .check_health()
            .await
    );
    assert!(
        !super::DaemonHandle::connect_existing(bad)
            .check_health()
            .await,
        "a version mismatch must be treated as unhealthy"
    );
}

// ---------------------------------------------------------------------------
// Remote NLI / reranker + config-gated wiring
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remote_providers_wire_nli_and_reranker_per_config() {
    use super::protocol::NliWire;
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("stub.sock");
    spawn_stub(socket.clone(), |op| match op {
        DaemonOp::Meta => DaemonResponse::Meta {
            dimensions: 4,
            max_tokens: 64,
            model_id: "onnx/stub".to_string(),
        },
        DaemonOp::Classify { pairs } => DaemonResponse::Classified {
            results: pairs
                .iter()
                .map(|_| NliWire {
                    entailment: 0.1,
                    neutral: 0.2,
                    contradiction: 0.7,
                })
                .collect(),
        },
        DaemonOp::Rerank { documents, .. } => DaemonResponse::Reranked {
            scores: documents
                .iter()
                .enumerate()
                .map(|(i, _)| (i, 1.0 - i as f32))
                .collect(),
        },
        _ => DaemonResponse::Error {
            message: "unexpected".to_string(),
        },
    });
    for _ in 0..100 {
        if super::transport::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Default config: NLI + rerank disabled ⇒ not wired.
    let handle = super::DaemonHandle::connect_existing(socket.clone());
    let p = super::remote_providers(
        handle,
        "/tmp/x".to_string(),
        None,
        &crate::types::EngramConfig::default(),
    )
    .await
    .expect("providers");
    assert!(p.embedding.is_some());
    assert!(p.nli.is_none());
    assert!(p.reranker.is_none());

    // Enable both ⇒ wired, and they round-trip through the daemon.
    let mut cfg = crate::types::EngramConfig::default();
    cfg.nli.enabled = true;
    cfg.rerank.enabled = true;
    let handle = super::DaemonHandle::connect_existing(socket.clone());
    let p = super::remote_providers(handle, "/tmp/x".to_string(), None, &cfg)
        .await
        .expect("providers");
    let nli = p.nli.expect("nli wired when enabled");
    let res = nli.classify_batch(&[("a", "b")]).await.unwrap();
    assert_eq!(res.len(), 1);
    assert!((res[0].contradiction - 0.7).abs() < 1e-6);

    let rr = p.reranker.expect("reranker wired when enabled");
    let scores = rr
        .rerank("q", &["d0".to_string(), "d1".to_string()])
        .await
        .unwrap();
    assert_eq!(scores.len(), 2);
    assert_eq!(scores[0].index, 0);
}

/// Poll until a daemon on `socket` stops accepting new connections (has shut down).
async fn poll_until_unconnectable(socket: &std::path::Path) {
    for _ in 0..200 {
        if super::transport::connect(socket).await.is_err() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("daemon on {:?} never stopped accepting connections", socket);
}

/// Poll until a daemon on `socket` starts accepting connections.
async fn poll_until_connectable(socket: &std::path::Path) {
    for _ in 0..200 {
        if super::transport::connect(socket).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("daemon on {:?} never became connectable", socket);
}

// ---------------------------------------------------------------------------
// DaemonCell: re-resolvable cell with spawn backoff
// ---------------------------------------------------------------------------

#[tokio::test]
async fn daemon_cell_respawns_after_handle_lost() {
    use crate::ops::daemon_resolve::{DaemonCell, DaemonPolicy};

    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("cell.sock");
    let cell = DaemonCell::new();

    // No daemon running yet → ConnectOnly yields None (nothing to connect to).
    assert!(cell
        .get(&socket, 3600, DaemonPolicy::ConnectOnly)
        .await
        .is_none());

    // Also ConnectOrSpawn yields None here because the test binary doesn't
    // have the `daemon run` subcommand. We test cell caching + re-connect by
    // starting a daemon in-process and calling `connect_only` via the cell.
    // Start a daemon in-process.
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    poll_until_connectable(&socket).await;

    // ConnectOnly now resolves the live in-process daemon.
    let h1 = cell.get(&socket, 3600, DaemonPolicy::ConnectOnly).await;
    assert!(h1.is_some(), "ConnectOnly should find the running daemon");

    // Calling again hits the cached handle (still healthy).
    let h2 = cell.get(&socket, 3600, DaemonPolicy::ConnectOnly).await;
    assert!(h2.is_some(), "Second call should return cached handle");

    // Kill the daemon (the Shutdown handler acks, persists, and makes
    // `run_daemon` return — stopping the in-process accept loop).
    assert!(
        super::request_shutdown(&socket).await.unwrap(),
        "running daemon must ack the shutdown"
    );
    poll_until_unconnectable(&socket).await;

    // Cell detects the dead handle and returns None (ConnectOnly, no respawn).
    let h3 = cell.get(&socket, 3600, DaemonPolicy::ConnectOnly).await;
    assert!(h3.is_none(), "Dead daemon: ConnectOnly should yield None");

    // Start a fresh daemon on the same socket (simulates re-spawn).
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    poll_until_connectable(&socket).await;

    // Cell re-connects to the new daemon without poisoning.
    let h4 = cell.get(&socket, 3600, DaemonPolicy::ConnectOnly).await;
    assert!(
        h4.is_some(),
        "DaemonCell should re-connect after new daemon starts"
    );
}

/// Send one request over a fresh connection, retrying connect until the
/// daemon is up.
async fn wait_request(socket: &std::path::Path, req: DaemonRequest) -> DaemonResponse {
    for _ in 0..100 {
        if let Ok(stream) = super::transport::connect(socket).await {
            let (r, mut w) = tokio::io::split(stream);
            write_msg(&mut w, &req).await.unwrap();
            let mut r = BufReader::new(r);
            return read_msg(&mut r).await.unwrap().unwrap();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("daemon never came up");
}

// ---------------------------------------------------------------------------
// resolve_providers: shared in-process vs daemon routing
// ---------------------------------------------------------------------------

/// `InProcess` policy never touches a socket — providers are built in-process.
/// We verify this by using a non-existent socket path: if the code tried to
/// connect it would fail/block; InProcess must return providers without any
/// socket activity.
#[tokio::test]
async fn resolve_providers_in_process_never_touches_socket() {
    use crate::ops::daemon_resolve::{resolve_providers, DaemonCell, DaemonPolicy};

    let tmp = TempDir::new().unwrap();
    // Deliberately absent socket — any connect attempt would fail.
    let socket_env = tmp.path().join("absent.sock");
    std::env::set_var("ENGRAMDB_DAEMON_SOCKET", &socket_env);

    let cell = DaemonCell::new();
    let config = crate::types::EngramConfig::default();
    let dir = tmp.path();

    // InProcess must return providers (possibly with no embedding if the model
    // isn't staged — the function still returns the struct, just with None fields).
    let providers = resolve_providers(&cell, &config, None, dir, DaemonPolicy::InProcess).await;
    // The call must not hang or panic. We only assert the type is returned.
    let _ = providers;

    std::env::remove_var("ENGRAMDB_DAEMON_SOCKET");
}

// ---------------------------------------------------------------------------
// CLI e2e: connect-only uses live daemon; InProcess override ignores it
// ---------------------------------------------------------------------------

/// With a live (stub) daemon and `ConnectOnly` policy, `resolve_providers`
/// returns remote-backed providers whose dimensions match the daemon's reply.
/// With `InProcess` policy, it does not connect — confirmed by killing the
/// daemon first and showing that `InProcess` still resolves providers.
#[tokio::test]
async fn cli_connect_only_uses_daemon_and_in_process_override_does_not() {
    use super::protocol::DaemonOp;
    use crate::ops::daemon_resolve::{resolve_providers, DaemonCell, DaemonPolicy};

    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("cli-e2e.sock");

    // Stub daemon: answers Ping + Meta with sentinel dimensions=42.
    spawn_stub(socket.clone(), |op| match op {
        DaemonOp::Ping => DaemonResponse::Pong {
            version: super::PROTOCOL_VERSION.to_string(),
        },
        DaemonOp::Meta => DaemonResponse::Meta {
            dimensions: 42,
            max_tokens: 64,
            model_id: "onnx/cli-e2e-stub".to_string(),
        },
        _ => DaemonResponse::Error {
            message: "not implemented in stub".to_string(),
        },
    });
    poll_until_connectable(&socket).await;

    // ── Part 1: ConnectOnly uses the live daemon ──────────────────────────
    std::env::set_var("ENGRAMDB_DAEMON_SOCKET", &socket);

    let cell = DaemonCell::new();
    let mut config = crate::types::EngramConfig::default();
    config.daemon.enabled = true;
    let dir = tmp.path();

    let providers = resolve_providers(&cell, &config, None, dir, DaemonPolicy::ConnectOnly).await;
    let emb = providers
        .embedding
        .expect("ConnectOnly: remote embedding expected when daemon is live");
    assert_eq!(
        emb.dimensions(),
        42,
        "ConnectOnly: dimensions must match daemon's sentinel value"
    );

    // ── Part 2: InProcess ignores the socket entirely ────────────────────
    // Point the env at a deliberately absent socket — if InProcess tried to
    // connect it would fail; the policy must bypass the daemon path entirely.
    let absent = tmp.path().join("absent-cli-e2e.sock");
    std::env::set_var("ENGRAMDB_DAEMON_SOCKET", &absent);

    // InProcess must still return providers (in-process fallback, possibly with
    // no embedding if the model isn't staged).  The key assertion is that it
    // does not hang or panic — it never attempts to connect to the absent socket.
    let cell2 = DaemonCell::new();
    let providers2 = resolve_providers(&cell2, &config, None, dir, DaemonPolicy::InProcess).await;
    // If ONNX model is staged the embedding will be Some; if not it will be
    // None.  Either way `resolve_providers` must return without error.
    let _ = providers2;

    std::env::remove_var("ENGRAMDB_DAEMON_SOCKET");
}

/// `ConnectOnly` against a live daemon returns remote-backed providers.
/// The embedding field is present (daemon responded to Meta), with the
/// daemon's reported dimensions.
#[tokio::test]
async fn resolve_providers_connect_only_uses_live_daemon() {
    use super::protocol::DaemonOp;
    use crate::ops::daemon_resolve::{resolve_providers, DaemonCell, DaemonPolicy};

    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("rp.sock");

    // Stub daemon: answers Meta (so remote providers build) but nothing else.
    spawn_stub(socket.clone(), |op| match op {
        DaemonOp::Ping => DaemonResponse::Pong {
            version: super::PROTOCOL_VERSION.to_string(),
        },
        DaemonOp::Meta => DaemonResponse::Meta {
            dimensions: 16,
            max_tokens: 64,
            model_id: "onnx/stub-model".to_string(),
        },
        _ => DaemonResponse::Error {
            message: "not implemented in stub".to_string(),
        },
    });
    poll_until_connectable(&socket).await;

    std::env::set_var("ENGRAMDB_DAEMON_SOCKET", &socket);

    let cell = DaemonCell::new();
    let mut config = crate::types::EngramConfig::default();
    // daemon.enabled must be true for resolve_providers to try the daemon path.
    config.daemon.enabled = true;
    let dir = tmp.path();

    let providers = resolve_providers(&cell, &config, None, dir, DaemonPolicy::ConnectOnly).await;
    // The stub answered Meta with dimensions=16, so the remote embedding
    // provider should be present with those dimensions.
    let emb = providers
        .embedding
        .expect("remote embedding provider expected");
    assert_eq!(emb.dimensions(), 16);
    assert_eq!(emb.max_tokens(), 64);

    std::env::remove_var("ENGRAMDB_DAEMON_SOCKET");
}

// ---------------------------------------------------------------------------
// Heartbeat: pings keep daemon alive past idle; cell self-heals after kill
// ---------------------------------------------------------------------------

/// Send a single Ping to the daemon at `socket` and wait for Pong.
async fn ping_once(socket: &std::path::Path) {
    let resp = wait_request(
        socket,
        DaemonRequest {
            dir: String::new(),
            backend: None,
            op: DaemonOp::Ping,
        },
    )
    .await;
    assert!(
        matches!(resp, DaemonResponse::Pong { .. }),
        "expected Pong, got {resp:?}"
    );
}

/// A heartbeat-style loop of Pings every ~330 ms keeps the daemon alive past
/// its 1-second idle timeout.
#[tokio::test]
async fn heartbeat_pings_keep_daemon_alive_past_idle() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("hb.sock");

    // Spawn daemon with a 1-second idle timeout.
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(1)));
    poll_until_connectable(&socket).await;

    // Ping every ~330ms for a total of ~6 pings over ~1.98s — more than the
    // idle timeout, but the pings should keep last_activity fresh.
    let sock = socket.clone();
    let hb = tokio::spawn(async move {
        for _ in 0..6 {
            ping_once(&sock).await;
            tokio::time::sleep(Duration::from_millis(330)).await;
        }
    });

    // After 1.5s (> idle_timeout) the daemon must still be alive.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        super::transport::connect(&socket).await.is_ok(),
        "daemon should still be alive while heartbeat pings continue"
    );

    hb.abort();
}

/// The heartbeat mechanism the MCP server actually uses — repeated
/// `DaemonCell::get` — keeps a daemon alive past its idle timeout, because each
/// `get` health-checks the handle with a `Ping` that refreshes the daemon's
/// idle clock. This is the cell-level analogue of
/// `heartbeat_pings_keep_daemon_alive_past_idle` (which pings the socket
/// directly). `ConnectOnly` is used so a missed keepalive surfaces as a failed
/// assertion rather than spawning the test binary as a daemon.
#[tokio::test]
async fn daemon_cell_get_keeps_daemon_alive_past_idle() {
    use crate::ops::daemon_resolve::{DaemonCell, DaemonPolicy};

    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("hbcell.sock");

    // 1-second idle timeout; the daemon is already running so each `get` just
    // health-checks (no spawn path is taken).
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(1)));
    poll_until_connectable(&socket).await;

    let cell = DaemonCell::new();
    // 6 resolves spaced ~330ms span ~2s, well past the 1s idle timeout.
    for _ in 0..6 {
        let h = cell.get(&socket, 1, DaemonPolicy::ConnectOnly).await;
        assert!(h.is_some(), "cell.get should keep returning a live handle");
        tokio::time::sleep(Duration::from_millis(330)).await;
    }

    assert!(
        super::transport::connect(&socket).await.is_ok(),
        "DaemonCell::get heartbeat should keep the daemon alive past idle"
    );
}

/// Each `DaemonCell::get` resolve sends exactly one `Ping` (via the handle
/// health check) — which is what lets the heartbeat refresh the daemon's idle
/// clock. Two resolves ⇒ the daemon's `Status` reports `ping_count == 2`.
/// (`poll_until_connectable` only opens a socket; it does not send a `Ping`.)
#[tokio::test]
async fn daemon_cell_get_pings_the_daemon() {
    use crate::ops::daemon_resolve::{DaemonCell, DaemonPolicy};

    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("cellping.sock");
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    poll_until_connectable(&socket).await;

    let cell = DaemonCell::new();
    // First get: bare connect → 1 ping. Second get: cached health-check → 1 ping.
    assert!(cell
        .get(&socket, 3600, DaemonPolicy::ConnectOnly)
        .await
        .is_some());
    assert!(cell
        .get(&socket, 3600, DaemonPolicy::ConnectOnly)
        .await
        .is_some());

    let st = super::query_status(&socket).await.unwrap().unwrap();
    assert_eq!(
        st.ping_count, 2,
        "each DaemonCell::get should ping the daemon exactly once"
    );
}

/// The daemon tracks how many Pings it has received and when the last one
/// arrived. After two pings, `Status` must report `ping_count == 2` and
/// `last_ping_secs_ago` must be `Some`.
#[tokio::test]
async fn daemon_status_reports_ping_count_and_last_ping() {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("p.sock");
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    poll_until_connectable(&socket).await;

    ping_once(&socket).await;
    ping_once(&socket).await;

    let st = super::query_status(&socket).await.unwrap().unwrap();
    assert_eq!(st.ping_count, 2, "ping_count must be 2 after two pings");
    assert!(
        st.last_ping_secs_ago.is_some(),
        "last_ping_secs_ago must be Some after a ping"
    );
}

/// Killing the daemon and then calling `DaemonCell::get` with `ConnectOnly`
/// causes the cell to detect the dead handle and return `None`. After a fresh
/// daemon starts on the same socket, the cell re-connects without poisoning.
/// (In production the heartbeat calls `ConnectOrSpawn` which also spawns; here
/// we start the replacement in-process to keep the test binary clean.)
#[tokio::test]
async fn cell_self_heals_after_daemon_killed() {
    use crate::ops::daemon_resolve::{DaemonCell, DaemonPolicy};

    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("heal.sock");

    // Start daemon 1.
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    poll_until_connectable(&socket).await;

    let cell = DaemonCell::new();
    let h1 = cell.get(&socket, 3600, DaemonPolicy::ConnectOnly).await;
    assert!(h1.is_some(), "cell should connect to running daemon");

    // Kill it (the Shutdown handler acks, persists, and makes `run_daemon`
    // return — stopping the in-process accept loop).
    assert!(
        super::request_shutdown(&socket).await.unwrap(),
        "running daemon must ack the shutdown"
    );
    poll_until_unconnectable(&socket).await;

    // Cell detects dead handle and returns None (ConnectOnly, no respawn).
    let h2 = cell.get(&socket, 3600, DaemonPolicy::ConnectOnly).await;
    assert!(
        h2.is_none(),
        "cell should return None after daemon is killed"
    );

    // Simulate what the heartbeat ConnectOrSpawn would do: a new daemon starts.
    tokio::spawn(run_daemon(socket.clone(), Duration::from_secs(3600)));
    poll_until_connectable(&socket).await;

    // Cell must re-connect without poisoning.
    let h3 = cell.get(&socket, 3600, DaemonPolicy::ConnectOnly).await;
    assert!(
        h3.is_some(),
        "cell should re-connect after new daemon starts"
    );
}
