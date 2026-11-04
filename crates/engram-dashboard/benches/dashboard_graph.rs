//! The dashboard's graph and relations handlers, in process, against the
//! deterministic benchmark stores (`examples/support/dashboard_store.rs`):
//! the whole router (security layers included) driven with `oneshot`, the
//! way `src/tests.rs` does, so a figure is the server's share of a tab
//! click with no socket and no browser. Budgets and results:
//! `docs/contributors/dashboard-performance.md`.
//!
//! ```bash
//! cargo bench -p engram-dashboard --bench dashboard_graph
//! DASHBOARD_BENCH_SIZES=s1k,s2k cargo bench -p engram-dashboard --bench dashboard_graph
//! ```
//!
//! The stores are built once per run in a temp dir (10k takes a few
//! seconds). Every request after the first finds its store in the
//! dashboard's own read-only store cache, as a page's second request would.

#[path = "../../../examples/support/dashboard_store.rs"]
mod dashboard_store;

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use criterion::{criterion_group, criterion_main, Criterion};
use engram_dashboard::security::Security;
use engram_dashboard::{AppState, CurrentProject};
use tower::ServiceExt;

const TOKEN: &str = "bench-token";
const HOST: &str = "127.0.0.1:7341";

async fn get(app: &axum::Router, uri: &str) -> usize {
    let req = Request::get(uri)
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{uri}");
    axum::body::to_bytes(res.into_body(), 64 << 20)
        .await
        .unwrap()
        .len()
}

fn bench(c: &mut Criterion) {
    let tmp = tempfile::TempDir::new().unwrap();
    dashboard_store::use_machine_dir(tmp.path()).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let only = std::env::var("DASHBOARD_BENCH_SIZES").ok();
    for (name, n) in dashboard_store::SIZES {
        if only
            .as_deref()
            .is_some_and(|o| !o.split(',').any(|s| s == name))
        {
            continue;
        }
        let dir = tmp.path().join(name);
        let id = rt.block_on(dashboard_store::build(&dir, n)).unwrap();
        let current = CurrentProject::resolve(&std::fs::canonicalize(&dir).unwrap());
        let security = Security::with_secrets(HOST.parse().unwrap(), TOKEN.into(), "csrf".into());
        let app = engram_dashboard::router(AppState::new(security, None, false, current));
        let base = format!("/api/projects/{id}");
        // What the page asks for (views/graph.js, views/relations.js).
        let all = "include_proposed=true&include_rejected=true&include_invalidated=false";
        let cases = [
            (
                "graph_overview",
                format!("{base}/graph?{all}&summary_over=150"),
            ),
            ("graph_full", format!("{base}/graph?{all}&limit=2000")),
            (
                "graph_top_scope",
                format!("{base}/graph?{all}&top_scope=storage"),
            ),
            (
                "relations_first_page",
                format!("{base}/relations?limit=100"),
            ),
        ];
        let mut group = c.benchmark_group(format!("dashboard/{name}"));
        group
            .sample_size(20)
            .measurement_time(Duration::from_secs(8))
            .warm_up_time(Duration::from_secs(2));
        for (case, uri) in &cases {
            // The first request also opens the store; time the steady state.
            let bytes = rt.block_on(get(&app, uri));
            eprintln!("{name} {case}: {bytes} bytes");
            group.bench_function(*case, |b| b.to_async(&rt).iter(|| get(&app, uri)));
        }
        group.finish();
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
