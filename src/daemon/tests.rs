//! Daemon protocol + end-to-end delegation tests.

use std::time::Duration;

use tempfile::TempDir;
use tokio::io::BufReader;
use tokio::net::UnixStream;

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
    // Long idle timeout: the watchdog calls process::exit, which would kill
    // the test binary if it fired.
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
        if UnixStream::connect(&socket).await.is_ok() {
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

/// Send one request over a fresh connection, retrying connect until the
/// daemon is up.
async fn wait_request(socket: &std::path::Path, req: DaemonRequest) -> DaemonResponse {
    for _ in 0..100 {
        if let Ok(stream) = UnixStream::connect(socket).await {
            let (r, mut w) = stream.into_split();
            write_msg(&mut w, &req).await.unwrap();
            let mut r = BufReader::new(r);
            return read_msg(&mut r).await.unwrap().unwrap();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("daemon never came up");
}
