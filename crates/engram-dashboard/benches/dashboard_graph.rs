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
//!
//! The `*_layout` cases ask for the server-side layout (`src/layout/`) the
//! way the page does, on a router whose layout cache is off
//! (`ENGRAMDB_DASHBOARD_LAYOUT_CACHE=0`), so each iteration lays out from
//! scratch: their difference from the same case without `layout=` is the
//! layout's cost. `*_layout_cached` is the same request with the cache on
//! (a memory hit after the first).

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
    // The layout cache's disk half, in the temp dir, never the user's cache.
    std::env::set_var(
        "ENGRAMDB_DASHBOARD_CACHE_DIR",
        tmp.path().join("cache").join("layouts"),
    );
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
        let app = engram_dashboard::router(AppState::new(
            security.clone(),
            None,
            false,
            current.clone(),
        ));
        std::env::set_var("ENGRAMDB_DASHBOARD_LAYOUT_CACHE", "0");
        let cold = engram_dashboard::router(AppState::new(security, None, false, current));
        std::env::remove_var("ENGRAMDB_DASHBOARD_LAYOUT_CACHE");
        let base = format!("/api/projects/{id}");
        // What the page asks for (views/graph.js, views/relations.js).
        let all = "include_proposed=true&include_rejected=true&include_invalidated=false";
        let lay = "layout_aspect=0.6&layout_width=1280";
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
        // (case, uri, cold): the server-side layouts, as the page asks.
        let layout_cases = [
            (
                "graph_overview_layout",
                format!("{base}/graph?{all}&summary_over=150&layout=scope-overview,scope-force&{lay}"),
            ),
            (
                "graph_top_scope_layout",
                format!("{base}/graph?{all}&top_scope=storage&layout=scope-force,opened&layout_open_over=150&{lay}"),
            ),
            (
                "graph_show_all_layout",
                format!("{base}/graph?{all}&layout=scope-force&{lay}"),
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
        for (case, uri) in &layout_cases {
            let bytes = rt.block_on(get(&cold, uri));
            eprintln!("{name} {case}: {bytes} bytes");
            group.bench_function(*case, |b| b.to_async(&rt).iter(|| get(&cold, uri)));
            let cached = format!("{case}_cached");
            rt.block_on(get(&app, uri));
            group.bench_function(cached.as_str(), |b| b.to_async(&rt).iter(|| get(&app, uri)));
        }
        group.finish();
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
