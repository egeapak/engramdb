//! Stub daemons for tests, here (not in `tests.rs`) so the CLI and MCP
//! crates' tests can reach them through the core's `test-support` feature,
//! which the shipped binary never enables.

use std::time::Duration;

use tokio::io::BufReader;

use super::protocol::{read_msg, write_msg, DaemonOp, DaemonRequest, DaemonResponse};

/// A stub daemon whose reply may be `None`: drop the connection
/// without answering (a daemon that died mid-request). Every op received is
/// logged.
pub fn spawn_recording_stub<F>(
    socket: std::path::PathBuf,
    reply: F,
) -> std::sync::Arc<std::sync::Mutex<Vec<String>>>
where
    F: Fn(&DaemonOp) -> Option<DaemonResponse> + Send + Sync + 'static,
{
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&log);
    tokio::spawn(async move {
        let listener = super::transport::bind_or_yield(&socket)
            .await
            .unwrap()
            .expect("stub owns the address");
        while let Ok(stream) = listener.accept().await {
            let (rh, mut wh) = tokio::io::split(stream);
            let mut r = BufReader::new(rh);
            while let Some(req) = read_msg::<_, DaemonRequest>(&mut r).await.unwrap_or(None) {
                seen.lock().unwrap().push(format!("{:?}", req.op));
                let Some(resp) = reply(&req.op) else {
                    break;
                };
                if write_msg(&mut wh, &resp).await.is_err() {
                    break;
                }
            }
        }
    });
    log
}

pub fn current_pong() -> DaemonResponse {
    DaemonResponse::Pong {
        version: super::PROTOCOL_VERSION.to_string(),
        build: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

pub async fn wait_connectable(socket: &std::path::Path) {
    for _ in 0..200 {
        if super::transport::connect(socket).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing listening on {}", socket.display());
}
