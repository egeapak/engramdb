//! Router-level tests: the security guard, the API against real temp
//! stores, the embedded assets, and the "no writes on GET" property.
//!
//! Stores, the registry and the data dir are the per-process temp dirs the
//! `engram-test-support` ctor sets up, so each nextest process gets its own
//! machine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use engramdb::ops::usage::{MemoryEvent, StoreRef, Surface, UsageKind};
use engramdb::storage::{paths, FileRegistry, MemoryStore, RegistryBackend};
use engramdb::types::{Challenge, Memory, MemoryType, Provenance, Status};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::security::{Security, CSRF_HEADER};
use crate::{AppState, CurrentProject};

const TOKEN: &str = "test-token-0123456789abcdef";
const CSRF: &str = "test-csrf";
const HOST: &str = "127.0.0.1:7341";

fn security() -> Security {
    Security::with_secrets(HOST.parse().unwrap(), TOKEN.into(), CSRF.into())
}

fn app(current: Option<CurrentProject>) -> axum::Router {
    app_with(current, None, false)
}

fn app_with(
    current: Option<CurrentProject>,
    assets: Option<PathBuf>,
    writes: bool,
) -> axum::Router {
    crate::router(AppState::new(security(), assets, writes, current))
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

fn req(method: Method, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
}

fn authed(uri: &str) -> Request<Body> {
    req(Method::GET, uri)
        .header(header::AUTHORIZATION, bearer())
        .body(Body::empty())
        .unwrap()
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }
    fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(&name)
            .unwrap_or_else(|| panic!("no {name} header"))
            .to_str()
            .unwrap()
    }
}

async fn send(app: &axum::Router, r: Request<Body>) -> Res {
    let res = app.clone().oneshot(r).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), 16 << 20)
        .await
        .unwrap()
        .to_vec();
    Res {
        status,
        headers,
        body,
    }
}

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let r = send(app, authed(uri)).await;
    let v = r.json();
    (r.status, v)
}

// ---------------------------------------------------------------------------
// Fixture: one project with a handful of memories and some usage.
// ---------------------------------------------------------------------------

struct Fixture {
    _tmp: TempDir,
    dir: PathBuf,
    id: String,
    current: CurrentProject,
    ids: Vec<String>,
}

fn memory(type_: MemoryType, summary: &str, logical: &[&str], tags: &[&str]) -> Memory {
    let mut m = Memory::new(
        type_,
        summary,
        format!("Body of **{summary}**."),
        Provenance::human(),
    );
    m.logical = logical.iter().map(|s| s.to_string()).collect();
    m.tags = tags.iter().map(|s| s.to_string()).collect();
    m
}

async fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let registry = FileRegistry::global().unwrap();
    let store = MemoryStore::init(&dir, &registry).await.unwrap();

    let a = memory(
        MemoryType::Decision,
        "Use flock for writes",
        &["storage.lock"],
        &["locking"],
    );
    let b = memory(
        MemoryType::Hazard,
        "Never compare paths textually",
        &["storage.paths"],
        &["paths"],
    );
    let mut c = memory(MemoryType::Convention, "Hooks fail open", &["hooks"], &[]);
    c.status = Status::Challenged;
    c.challenges = vec![Challenge::new(
        "<script>alert(1)</script> contradicts the exit-2 rule",
    )];
    let mut d = memory(
        MemoryType::Decision,
        "Ship load-dynamic only",
        &[],
        &["build"],
    );
    d.supersedes = vec![a.id.clone()];
    let mut e = memory(MemoryType::Context, "Old context", &["build"], &[]);
    e.invalidated_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
    let ids: Vec<String> = [&a, &b, &c, &d, &e].iter().map(|m| m.id.clone()).collect();
    for m in [&a, &b, &c, &d, &e] {
        store.create(m).await.unwrap();
    }

    // Usage: `a` fetched twice and retrieved once, `b` injected once.
    let owner = StoreRef::of(&store);
    let ev = |id: &str, kind| MemoryEvent::new(id, &store.project_id, kind, Surface::Cli);
    engramdb::ops::usage::write_events(
        &owner,
        &[
            ev(&a.id, UsageKind::Fetched),
            ev(&a.id, UsageKind::Fetched),
            ev(&a.id, UsageKind::Returned),
            ev(&b.id, UsageKind::Injected),
        ],
    );

    let current = CurrentProject::resolve(&dir).expect("fixture is a project");
    assert_eq!(current.id, store.project_id);
    Fixture {
        id: store.project_id.clone(),
        _tmp: tmp,
        dir,
        current,
        ids,
    }
}

// ---------------------------------------------------------------------------
// Security
// ---------------------------------------------------------------------------

#[tokio::test]
async fn api_requests_without_the_bearer_token_are_refused() {
    let app = app(None);
    for uri in ["/api/session", "/api/projects", "/api/nope"] {
        let r = send(&app, req(Method::GET, uri).body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(r.json()["error"]["code"], "unauthorized");
    }
    // A wrong token, and the right one by every carrier but the header: a
    // query string lands in history and logs, and a cookie is shared with
    // every other server on 127.0.0.1, so neither is accepted.
    for r in [
        req(Method::GET, "/api/session")
            .header(header::AUTHORIZATION, "Bearer nope")
            .body(Body::empty())
            .unwrap(),
        req(Method::GET, &format!("/api/session?token={TOKEN}"))
            .body(Body::empty())
            .unwrap(),
        req(Method::GET, "/api/session")
            .header(header::COOKIE, format!("engramdb_dashboard_7341={TOKEN}"))
            .body(Body::empty())
            .unwrap(),
        req(Method::GET, "/api/session")
            .header(header::AUTHORIZATION, TOKEN)
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(send(&app, r).await.status, StatusCode::UNAUTHORIZED);
    }
    let r = send(&app, authed("/api/session")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["csrf"], CSRF);
}

#[tokio::test]
async fn static_files_are_public_and_never_set_a_cookie_or_redirect() {
    let app = app(None);
    // The page must load before it can read the fragment, so the files are
    // served without the token; they are the same for everyone.
    for uri in ["/", "/index.html", "/js/app.js", "/css/app.css"] {
        let r = send(&app, req(Method::GET, uri).body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
        assert!(r.headers.get(header::SET_COOKIE).is_none(), "{uri}");
    }
    // A stray `?token=` on a page is neither honoured nor traded for a
    // cookie: no redirect, so no `Location` to abuse as an open redirect.
    for uri in [
        format!("/?token={TOKEN}"),
        format!("//evil.example/?token={TOKEN}"),
    ] {
        let r = send(&app, req(Method::GET, &uri).body(Body::empty()).unwrap()).await;
        assert!(r.headers.get(header::LOCATION).is_none(), "{uri}");
        assert!(r.headers.get(header::SET_COOKIE).is_none(), "{uri}");
    }
    let page = send(&app, req(Method::GET, "/").body(Body::empty()).unwrap()).await;
    assert!(!String::from_utf8_lossy(&page.body).contains(TOKEN));
    assert_eq!(page.header(header::REFERRER_POLICY), "no-referrer");
}

#[test]
fn the_launch_url_carries_the_token_in_the_fragment_only() {
    let url = security().launch_url();
    let (before, frag) = url.split_once('#').expect("a fragment");
    assert_eq!(before, format!("http://{HOST}/"));
    assert_eq!(frag, format!("token={TOKEN}"));
}

#[tokio::test]
async fn foreign_hosts_are_refused_even_with_the_token() {
    let app = app(None);
    for host in [
        Some("evil.example:7341"),
        Some("127.0.0.1:9999"),
        Some("localhost.evil.example:7341"),
        None,
    ] {
        let mut b = Request::builder().uri("/api/session");
        if let Some(h) = host {
            b = b.header(header::HOST, h);
        }
        let r = send(
            &app,
            b.header(header::AUTHORIZATION, bearer())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{host:?}");
        assert_eq!(r.json()["error"]["code"], "host_not_allowed");
        // Refusals carry the security headers too.
        assert!(r
            .header(header::CONTENT_SECURITY_POLICY)
            .contains("default-src 'self'"));
    }
    for host in ["localhost:7341", "[::1]:7341"] {
        let r = send(
            &app,
            Request::builder()
                .uri("/api/session")
                .header(header::HOST, host)
                .header(header::AUTHORIZATION, bearer())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn non_get_requests_need_origin_and_csrf() {
    let app = app(None);
    let post = |origin: Option<&str>, csrf: Option<&str>| {
        let mut b = req(Method::POST, "/api/projects").header(header::AUTHORIZATION, bearer());
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        if let Some(c) = csrf {
            b = b.header(CSRF_HEADER, c);
        }
        b.body(Body::empty()).unwrap()
    };
    let me = format!("http://{HOST}");
    for (origin, csrf) in [
        (None, Some(CSRF)),
        (Some("http://evil.example"), Some(CSRF)),
        (Some("http://localhost:7341"), Some(CSRF)), // not the Host the request named
        (Some(me.as_str()), None),
        (Some(me.as_str()), Some("wrong")),
    ] {
        let r = send(&app, post(origin, csrf)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{origin:?} {csrf:?}");
        assert_eq!(r.json()["error"]["code"], "csrf");
    }
    // Both right: past the guard, where this path has no POST route.
    let r = send(&app, post(Some(&me), Some(CSRF))).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    // The same for a page path.
    let r = send(
        &app,
        req(Method::DELETE, "/")
            .header(header::AUTHORIZATION, bearer())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let app = app(None);
    for uri in ["/", "/js/app.js", "/api/session", "/api/nope", "/nope"] {
        let r = send(&app, authed(uri)).await;
        let csp = r.header(header::CONTENT_SECURITY_POLICY);
        assert!(csp.contains("default-src 'self'"), "{uri}: {csp}");
        assert!(
            csp.contains("script-src 'self'") && !csp.contains("unsafe-inline"),
            "{csp}"
        );
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert_eq!(r.header(header::X_FRAME_OPTIONS), "DENY", "{uri}");
        assert_eq!(r.header(header::REFERRER_POLICY), "no-referrer", "{uri}");
        assert_eq!(r.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff", "{uri}");
    }
    let r = send(&app, authed("/api/session")).await;
    assert_eq!(r.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(r.header(header::CONTENT_TYPE), "application/json");
    let r = send(&app, authed("/api/nope")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["error"]["code"], "unknown_endpoint");
}

#[tokio::test]
async fn embedded_assets_have_types_and_etags() {
    let app = app(None);
    let r = send(&app, authed("/js/app.js")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r
        .header(header::CONTENT_TYPE)
        .starts_with("text/javascript"));
    let etag = r.header(header::ETAG).to_string();
    let r = send(
        &app,
        req(Method::GET, "/js/app.js")
            .header(header::IF_NONE_MATCH, &etag)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    for bad in ["/../Cargo.toml", "/js/../../Cargo.toml", "/nope.js"] {
        assert_eq!(
            send(&app, authed(bad)).await.status,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn dev_mode_serves_from_disk_without_caching_and_stays_inside() {
    let tmp = TempDir::new().unwrap();
    let web = tmp.path().join("web");
    std::fs::create_dir_all(web.join("js")).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html><title>dev</title>").unwrap();
    std::fs::write(web.join("js/app.js"), "export {}").unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "no").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(tmp.path().join("secret.txt"), web.join("leak.txt")).unwrap();
    let app = app_with(None, Some(web), false);
    let r = send(&app, authed("/")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&r.body).contains("dev"));
    assert_eq!(r.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(
        send(&app, authed("/../secret.txt")).await.status,
        StatusCode::NOT_FOUND
    );
    #[cfg(unix)]
    assert_eq!(
        send(&app, authed("/leak.txt")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn session_reports_the_write_mode() {
    let (_, v) = get_json(&app(None), "/api/session").await;
    assert_eq!(v["allow_writes"], false);
    let (_, v) = get_json(&app_with(None, None, true), "/api/session").await;
    assert_eq!(v["allow_writes"], true);
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

fn find<'a>(projects: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap_or_else(|| panic!("no project {id} in {projects}"))
}

#[tokio::test]
async fn projects_lists_every_store_with_links_and_the_current_one() {
    let fx = fixture().await;
    let registry = FileRegistry::global().unwrap();

    // A worktree-like child with no store of its own, under the fixture.
    let child_dir = fx.dir.parent().unwrap().join("child");
    std::fs::create_dir_all(&child_dir).unwrap();
    registry
        .update_with_parent(&child_dir, "child-id-000001", Some(&fx.id))
        .await
        .unwrap();
    // A project whose directory is gone.
    let gone = fx.dir.parent().unwrap().join("gone");
    std::fs::create_dir_all(gone.join(".engramdb")).unwrap();
    registry.update(&gone, "gone-id-0000001").await.unwrap();
    std::fs::remove_dir_all(&gone).unwrap();
    // A group nobody has written to.
    let group = registry.create_group("team").await.unwrap();
    registry.subscribe(&fx.id, &group).await.unwrap();

    let (status, v) = get_json(&app(Some(fx.current.clone())), "/api/projects").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["current"], fx.id.as_str());
    assert_eq!(v["projects"][0]["id"], fx.id.as_str(), "current first");

    let me = find(&v, &fx.id);
    assert_eq!(me["current"], true);
    assert_eq!(me["available"], true);
    assert_eq!(me["memory_count"], 5);
    assert_eq!(me["children"], serde_json::json!(["child-id-000001"]));
    assert_eq!(me["subscriptions"], serde_json::json!([group.clone()]));
    assert_eq!(me["schema"]["needs_migration"], false);

    let child = find(&v, "child-id-000001");
    assert_eq!(child["available"], false);
    assert_eq!(child["parent_id"], fx.id.as_str());
    assert!(child["unavailable_reason"]
        .as_str()
        .unwrap()
        .contains("sub-project"));
    // Children come right after their parent.
    assert_eq!(v["projects"][1]["id"], "child-id-000001");

    let gone = find(&v, "gone-id-0000001");
    assert_eq!(gone["available"], false);
    assert!(gone["unavailable_reason"]
        .as_str()
        .unwrap()
        .contains("no longer exists"));

    let g = find(&v, &group);
    assert_eq!(g["kind"], "group");
    assert_eq!(g["available"], false);
    assert_eq!(g["subscribers"], serde_json::json!([fx.id.clone()]));

    let global = find(&v, paths::GLOBAL_PROJECT_ID);
    assert_eq!(global["kind"], "global");

    // Unavailable stores answer 409, unknown ones 404.
    let app = app(Some(fx.current.clone()));
    let (s, v) = get_json(&app, "/api/projects/gone-id-0000001/stats").await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "unavailable");
    let (s, v) = get_json(&app, "/api/projects/child-id-000001/memories").await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, v) = get_json(&app, "/api/projects/nope/stats").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "unknown_project");
    let (s, _) = get_json(
        &app,
        &format!("/api/projects/{}/stats", paths::GLOBAL_PROJECT_ID),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a global store never created is unavailable"
    );
}

#[tokio::test]
async fn stats_has_counts_usage_series_and_attention() {
    let fx = fixture().await;
    let (s, v) = get_json(
        &app(Some(fx.current.clone())),
        &format!("/api/projects/{}/stats", fx.id),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 5);
    assert_eq!(v["live"], 4);
    assert_eq!(v["invalidated"], 1);
    assert_eq!(v["usage"]["fetched"], 2);
    assert_eq!(v["usage"]["returned"], 1);
    assert_eq!(v["usage"]["injected"], 1);
    assert_eq!(v["usage"]["used_memories"], 2);
    // Per signal, never one summed "reads" number.
    assert_eq!(
        v["usage"]["last_7_days"],
        serde_json::json!({"returned": 1, "injected": 1, "fetched": 2})
    );
    assert_eq!(
        v["usage"]["previous_7_days"],
        serde_json::json!({"returned": 0, "injected": 0, "fetched": 0})
    );
    assert!(v["usage"].get("reads_last_7_days").is_none());
    assert_eq!(v["series"].as_array().unwrap().len(), 30);
    assert_eq!(v["project"]["id"], fx.id.as_str());
    let att = v["attention"].as_array().unwrap();
    let ch = att
        .iter()
        .find(|a| a["reason"] == "challenged")
        .expect("challenged item");
    assert_eq!(ch["id"], fx.ids[2].as_str());
    assert_eq!(ch["count"], 1);
    // Untrusted text comes back verbatim, as data.
    assert_eq!(
        ch["detail"],
        "<script>alert(1)</script> contradicts the exit-2 rule"
    );
    let lines: Vec<&str> = v["by_line"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(lines, ["storage", "", "hooks"]);
    // Slots rank lines over the whole store (the invalidated `build` line
    // included): storage 2, then build and hooks 1 each, by name.
    let slots: Vec<serde_json::Value> = v["by_line"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["slot"].clone())
        .collect();
    assert_eq!(
        slots,
        [
            serde_json::json!(0),
            serde_json::Value::Null,
            serde_json::json!(2)
        ]
    );
    assert_eq!(ch["line_slot"], 2);
    assert!(v.get("rebuilding").is_none());
}

#[tokio::test]
async fn a_line_has_one_slot_on_every_endpoint() {
    let fx = fixture().await;
    let app = app(Some(fx.current.clone()));
    let base = format!("/api/projects/{}", fx.id);
    let (_, list) = get_json(&app, &format!("{base}/memories?status=all")).await;
    let (_, stats) = get_json(&app, &format!("{base}/stats")).await;
    let facet = |name: &str| {
        list["facets"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["name"] == name)
            .map(|l| l["slot"].clone())
    };
    for m in list["items"].as_array().unwrap() {
        let id = m["id"].as_str().unwrap();
        let (_, d) = get_json(&app, &format!("{base}/memories/{id}")).await;
        assert_eq!(m["line_slot"], d["line_slot"], "{id}: list vs detail");
        if let Some(line) = m["line"].as_str() {
            assert_eq!(
                Some(m["line_slot"].clone()),
                facet(line),
                "{line}: list vs facet"
            );
            if let Some(b) = stats["by_line"]
                .as_array()
                .unwrap()
                .iter()
                .find(|l| l["name"] == line)
            {
                assert_eq!(m["line_slot"], b["slot"], "{line}: list vs stats");
            }
        } else {
            assert!(m.get("line_slot").is_none());
        }
    }
}

#[tokio::test]
async fn facets_count_what_the_status_filter_admits() {
    let fx = fixture().await;
    let app = app(Some(fx.current.clone()));
    let base = format!("/api/projects/{}/memories", fx.id);
    let (_, v) = get_json(&app, &format!("{base}?status=invalidated")).await;
    assert_eq!(
        v["facets"]["types"],
        serde_json::json!([{"name": "context", "count": 1}])
    );
    let (_, v) = get_json(&app, &format!("{base}?status=challenged")).await;
    assert_eq!(
        v["facets"]["types"],
        serde_json::json!([{"name": "convention", "count": 1}])
    );
}

#[tokio::test]
async fn paging_is_keyed_so_a_new_row_never_repeats_one() {
    let fx = fixture().await;
    let app = app(Some(fx.current.clone()));
    let base = format!("/api/projects/{}/memories?sort=criticality&limit=2", fx.id);
    let (_, p1) = get_json(&app, &base).await;
    let first: Vec<String> = p1["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    let cursor = p1["next_cursor"].as_str().unwrap().to_string();

    // A row that sorts first lands between the two page loads. An offset
    // cursor would now hand back page 1's last row again.
    let store = MemoryStore::open(&fx.dir).await.unwrap();
    let mut top = memory(MemoryType::Decision, "Now the most critical", &[], &[]);
    top.criticality = 1.0;
    store.create(&top).await.unwrap();

    let (s, p2) = get_json(&app, &format!("{base}&cursor={cursor}")).await;
    assert_eq!(s, StatusCode::OK, "{p2}");
    let second: Vec<String> = p2["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(second.len(), 2);
    for id in &second {
        assert!(!first.contains(id), "{id} repeated across pages");
        assert_ne!(id, &top.id, "a row before the cursor is not on page 2");
    }
    // A cursor is bound to its sort.
    let (s, _) = get_json(
        &app,
        &format!(
            "/api/projects/{}/memories?sort=created&cursor={cursor}",
            fx.id
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_store_with_a_genuinely_old_table_answers_409_not_400() {
    let fx = fixture().await;
    // The pre-0.3.0 shape: the epistemic columns are not in the table.
    engramdb::storage::test_support::downgrade_dropping(
        &fx.dir,
        &[
            "epistemic",
            "verified_at",
            "generality",
            "origin_task",
            "valid_from",
            "invalidated_at",
            "watch_paths",
        ],
        "0.2.0",
    )
    .await;
    let app = app(Some(fx.current.clone()));
    let base = format!("/api/projects/{}", fx.id);
    for path in ["stats", "memories", "doctor"] {
        let (s, v) = get_json(&app, &format!("{base}/{path}")).await;
        match s {
            StatusCode::CONFLICT => {
                assert_eq!(v["error"]["code"], "schema_behind", "{path}: {v}");
                assert!(
                    v["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("engramdb reindex"),
                    "{path}: {v}"
                );
            }
            // A read that does not touch the missing columns still works,
            // and says the schema is behind.
            StatusCode::OK => assert_eq!(v["schema"]["needs_migration"], true, "{path}: {v}"),
            other => panic!("{path}: {other} {v}"),
        }
    }
    let (s, v) = get_json(&app, &format!("{base}/memories")).await;
    assert_eq!(s, StatusCode::CONFLICT, "listing needs the columns: {v}");
    // Still not migrated.
    let m =
        engramdb::storage::manifest::load_manifest(&fx.dir.join(".engramdb").join("manifest.toml"))
            .await
            .unwrap();
    assert_eq!(m.schema_version, "0.2.0");
}

#[tokio::test]
async fn memories_filter_sort_and_page() {
    let fx = fixture().await;
    let app = app(Some(fx.current.clone()));
    let base = format!("/api/projects/{}/memories", fx.id);

    let (s, v) = get_json(&app, &base).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 5);
    assert_eq!(v["matched"], 4, "invalidated hidden by default");
    let a = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == fx.ids[0].as_str())
        .unwrap();
    assert_eq!(a["usage"]["fetched"], 2);
    assert_eq!(a["usage"]["returned"], 1);
    assert_eq!(a["line"], "storage");
    assert_eq!(a["type"], "decision");
    assert!(a["usage"]["last_read_at"].is_string());

    let ids = |v: &serde_json::Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, v) = get_json(&app, &format!("{base}?sort=fetched&limit=1")).await;
    assert_eq!(ids(&v), [fx.ids[0].clone()]);
    let cursor = v["next_cursor"].as_str().unwrap().to_string();
    let (_, v2) = get_json(
        &app,
        &format!("{base}?sort=fetched&limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(ids(&v2), [fx.ids[1].clone()], "ties break by id");
    // Walk to the end: every live row once, then no cursor.
    let mut seen = ids(&v);
    let mut next = Some(cursor);
    while let Some(c) = next {
        let (_, p) = get_json(&app, &format!("{base}?sort=fetched&limit=1&cursor={c}")).await;
        seen.extend(ids(&p));
        next = p["next_cursor"].as_str().map(str::to_string);
    }
    assert_eq!(seen.len(), 4);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 4);

    let (_, v) = get_json(&app, &format!("{base}?kind=hazard")).await;
    assert_eq!(ids(&v), [fx.ids[1].clone()]);
    let (_, v) = get_json(&app, &format!("{base}?status=challenged")).await;
    assert_eq!(ids(&v), [fx.ids[2].clone()]);
    let (_, v) = get_json(&app, &format!("{base}?status=invalidated")).await;
    assert_eq!(ids(&v), [fx.ids[4].clone()]);
    let (_, v) = get_json(&app, &format!("{base}?status=all")).await;
    assert_eq!(v["matched"], 5);
    let (_, v) = get_json(&app, &format!("{base}?q=PATHS")).await;
    assert_eq!(ids(&v), [fx.ids[1].clone()]);
    let (_, v) = get_json(&app, &format!("{base}?tag=build")).await;
    assert_eq!(ids(&v), [fx.ids[3].clone()]);
    let (_, v) = get_json(&app, &format!("{base}?scope=storage")).await;
    assert_eq!(v["matched"], 2);
    let (_, v) = get_json(&app, &format!("{base}?line=hooks,-")).await;
    let mut got = ids(&v);
    got.sort();
    let mut want = vec![fx.ids[2].clone(), fx.ids[3].clone()];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(v["facets"]["types"][0]["name"], "decision");

    for bad in [
        "kind=nope",
        "sort=nope",
        "dir=up",
        "status=nope",
        "cursor=x",
        "limit=x",
    ] {
        let (s, v) = get_json(&app, &format!("{base}?{bad}")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
}

#[tokio::test]
async fn memory_detail_has_usage_links_and_timeline() {
    let fx = fixture().await;
    let app = app(Some(fx.current.clone()));
    let (s, v) = get_json(
        &app,
        &format!("/api/projects/{}/memories/{}", fx.id, fx.ids[3]),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["memory"]["id"], fx.ids[3].as_str());
    assert_eq!(v["supersedes"][0]["id"], fx.ids[0].as_str());
    assert_eq!(v["supersedes"][0]["found"], true);
    assert_eq!(v["supersedes"][0]["summary"], "Use flock for writes");
    assert_eq!(v["series"].as_array().unwrap().len(), 30);
    assert_eq!(v["timeline"][0]["kind"], "created");

    // A prefix resolves; usage is exact for the memory.
    let prefix = &fx.ids[0][..28];
    let (s, v) = get_json(&app, &format!("/api/projects/{}/memories/{prefix}", fx.id)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["usage"]["fetched"], 2);
    let kinds: Vec<&str> = v["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"last_fetched") && kinds.contains(&"created"),
        "{kinds:?}"
    );

    let (s, v) = get_json(&app, &format!("/api/projects/{}/memories/ffffffff", fx.id)).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "unknown_memory");
}

#[tokio::test]
async fn doctor_reports_a_healthy_store() {
    let fx = fixture().await;
    let (s, v) = get_json(
        &app(Some(fx.current.clone())),
        &format!("/api/projects/{}/doctor", fx.id),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["result"]["healthy"], true, "{v}");
    assert_eq!(v["result"]["indexed"], 5);
    assert_eq!(v["schema"]["needs_migration"], false);
}

#[tokio::test]
async fn stores_are_cached_and_a_stale_schema_is_reported_not_fixed() {
    let fx = fixture().await;
    let manifest_path = fx.dir.join(".engramdb").join("manifest.toml");
    let mut m = engramdb::storage::manifest::load_manifest(&manifest_path)
        .await
        .unwrap();
    m.schema_version = "0.1.0".into();
    engramdb::storage::manifest::save_manifest(&manifest_path, &m)
        .await
        .unwrap();

    let state = AppState::new(security(), None, false, Some(fx.current.clone()));
    let app = crate::router(state.clone());
    for _ in 0..2 {
        let (s, v) = get_json(&app, &format!("/api/projects/{}/stats", fx.id)).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["schema"]["needs_migration"], true);
        assert_eq!(v["schema"]["stored"], "0.1.0");
    }
    assert_eq!(state.stores.open_count().await, 1);
    let m = engramdb::storage::manifest::load_manifest(&manifest_path)
        .await
        .unwrap();
    assert_eq!(m.schema_version, "0.1.0", "the dashboard never migrates");
}

#[tokio::test]
async fn the_cache_evicts_at_capacity_and_never_caches_a_failure() {
    use crate::projects::{Location, StoreKind};
    use crate::stores::StoreCache;
    let fx = fixture().await;
    let loc = |id: &str, root: &Path| Location {
        id: id.to_string(),
        kind: StoreKind::Project,
        root: root.to_path_buf(),
    };
    let other = TempDir::new().unwrap();
    let other_dir = std::fs::canonicalize(other.path()).unwrap();
    let cache = StoreCache::new(1);

    // Not a store yet: refused, and the refusal is not remembered...
    let b = loc("b", &other_dir);
    assert!(cache.get(&b).await.is_err());
    assert_eq!(cache.open_count().await, 0);
    // ...so once it becomes one, the next request opens it.
    MemoryStore::init(&other_dir, &FileRegistry::global().unwrap())
        .await
        .unwrap();
    let opened_b = cache.get(&b).await.unwrap();
    assert_eq!(opened_b.root(), other_dir.as_path());

    // Capacity 1: opening the fixture evicts `b`, and `b` reopens fine.
    let a = loc(&fx.id, &fx.dir);
    let opened_a = cache.get(&a).await.unwrap();
    assert_eq!(cache.open_count().await, 1);
    assert_eq!(opened_a.root(), fx.dir.as_path());
    assert!(!std::sync::Arc::ptr_eq(
        &opened_b,
        &cache.get(&b).await.unwrap()
    ));
    assert_eq!(cache.open_count().await, 1);
}

#[tokio::test]
async fn group_and_global_stores_with_memories_are_listed_and_readable() {
    let fx = fixture().await;
    let registry = FileRegistry::global().unwrap();
    let group = registry.create_group("team").await.unwrap();
    let gs = MemoryStore::init_group(&group).await.unwrap();
    gs.create(&memory(MemoryType::Convention, "Team rule", &["team"], &[]))
        .await
        .unwrap();
    let global = MemoryStore::init_global().await.unwrap();
    for n in ["Global one", "Global two"] {
        global
            .create(&memory(MemoryType::Context, n, &[], &[]))
            .await
            .unwrap();
    }

    let app = app(Some(fx.current.clone()));
    let (_, v) = get_json(&app, "/api/projects").await;
    let g = find(&v, &group);
    assert_eq!(g["available"], true, "{g}");
    assert_eq!(g["memory_count"], 1);
    let gl = find(&v, paths::GLOBAL_PROJECT_ID);
    assert_eq!(gl["available"], true, "{gl}");
    assert_eq!(gl["memory_count"], 2);

    for (id, n) in [(group.as_str(), 1), (paths::GLOBAL_PROJECT_ID, 2)] {
        let (s, v) = get_json(&app, &format!("/api/projects/{id}/stats")).await;
        assert_eq!(s, StatusCode::OK, "{id}: {v}");
        assert_eq!(v["total"], n, "{id}");
        assert_eq!(v["project"]["id"], id);
        let (s, v) = get_json(&app, &format!("/api/projects/{id}/memories")).await;
        assert_eq!(s, StatusCode::OK, "{id}: {v}");
        assert_eq!(v["matched"], n, "{id}");
    }
}

#[tokio::test]
async fn the_current_project_of_a_linked_worktree_is_its_main_checkout() {
    let tmp = TempDir::new().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&main, &["init", "-q"]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let wt = base.join("wt");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    MemoryStore::init(&main, &FileRegistry::global().unwrap())
        .await
        .unwrap();

    let cur = CurrentProject::resolve(&wt).expect("the worktree routes to main");
    assert_eq!(cur.root, main);
    assert_eq!(
        cur.id,
        engramdb::storage::project_id::compute_project_id(&main)
    );
    // And nothing was created in the worktree.
    assert!(!wt.join(".engramdb").exists());
}

#[cfg(unix)]
#[test]
fn the_launch_page_is_private_and_removed_on_drop() {
    use std::os::unix::fs::PermissionsExt as _;
    let url = security().launch_url();
    let page = crate::LaunchPage::write(&url).unwrap();
    let path = page.path();
    let dir = path.parent().unwrap().to_path_buf();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&path), 0o600);
    let html = std::fs::read_to_string(&path).unwrap();
    assert!(
        html.contains(&format!("content=\"0;url={url}\"")),
        "the redirect target"
    );
    drop(page);
    assert!(!dir.exists(), "deleted with the guard");
}

// ---------------------------------------------------------------------------
// No writes on GET
// ---------------------------------------------------------------------------

type Snapshot = BTreeMap<PathBuf, (u64, std::time::SystemTime)>;

fn tree(root: &Path, out: &mut Snapshot) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        out.insert(p.clone(), (meta.len(), meta.modified().unwrap()));
        if meta.is_dir() {
            tree(&p, out);
        }
    }
}

fn machine(project: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    tree(project, &mut out);
    tree(&paths::global_data_dir().unwrap(), &mut out);
    tree(&paths::global_config_dir().unwrap(), &mut out);
    out
}

#[tokio::test]
async fn no_endpoint_writes_anything() {
    let fx = fixture().await;
    // `open` would backfill this file; the dashboard must not.
    std::fs::remove_file(fx.dir.join(".engramdb").join(".gitignore")).unwrap();
    let before = machine(&fx.dir);
    let app = app(Some(fx.current.clone()));
    let id = &fx.id;
    for uri in [
        "/".to_string(),
        "/js/app.js".into(),
        "/api/session".into(),
        "/api/projects".into(),
        format!("/api/projects/{id}/stats"),
        format!("/api/projects/{id}/memories"),
        format!("/api/projects/{id}/memories?sort=fetched&q=a"),
        format!("/api/projects/{id}/memories/{}", fx.ids[0]),
        format!("/api/projects/{id}/memories/{}", fx.ids[2]),
        format!("/api/projects/{id}/doctor"),
        format!("/api/projects/{id}/graph"),
        format!("/api/projects/{id}/graph?include_invalidated=true&kinds=contradicts"),
        format!("/api/projects/{}/stats", paths::GLOBAL_PROJECT_ID),
    ] {
        let r = send(&app, authed(&uri)).await;
        assert!(
            r.status.is_success() || r.status == StatusCode::CONFLICT,
            "{uri}: {}",
            r.status
        );
    }
    assert_eq!(before, machine(&fx.dir), "a GET changed something on disk");
}

// ---------------------------------------------------------------------------
// Frontend guard rails
// ---------------------------------------------------------------------------

/// Every relative `import … from '…'` / `import('…')` / `export … from '…'`
/// / `new URL('…', import.meta.url)` (a module worker) in an embedded
/// module, resolved against the module's directory.
fn imports(path: &str, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let dir = match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    };
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("//") || t.starts_with('*') {
            continue;
        }
        let is_static =
            (t.starts_with("import ") || t.starts_with("export ")) && t.contains(" from ");
        let candidates: Vec<&str> = if is_static {
            vec![t.rsplit(" from ").next().unwrap()]
        } else if let Some(i) = t.find("import(") {
            vec![&t[i + 7..]]
        } else if t.starts_with("import '") || t.starts_with("import \"") {
            vec![&t[7..]]
        } else if let Some(i) = t.find("new URL(").filter(|_| t.contains("import.meta.url")) {
            // A module worker: `new Worker(new URL('./w.js', import.meta.url))`.
            vec![&t[i + 8..]]
        } else {
            vec![]
        };
        for c in candidates {
            let c = c.trim();
            let q = c.chars().next().unwrap_or(' ');
            if q != '\'' && q != '"' {
                continue;
            }
            let spec: String = c[1..].chars().take_while(|ch| *ch != q).collect();
            out.push(resolve(dir, &spec));
        }
    }
    out
}

fn resolve(dir: &str, spec: &str) -> String {
    assert!(
        spec.starts_with("./") || spec.starts_with("../"),
        "bare or absolute import {spec:?}: no bundler, no CDN"
    );
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in spec.split('/') {
        match seg {
            "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[test]
fn every_import_resolves_to_an_embedded_file() {
    let assets = crate::assets::embedded();
    let mut checked = 0;
    for a in assets.iter().filter(|a| a.path.ends_with(".js")) {
        let src = std::str::from_utf8(a.bytes).unwrap();
        for target in imports(a.path, src) {
            assert!(
                crate::assets::find(&target).is_some(),
                "{} imports {target}, which is not embedded",
                a.path
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 10,
        "the import scan found only {checked} imports"
    );
    // The layout worker is reached only through `new URL(…)`, so the scan
    // must see it or a renamed worker would 404 at run time.
    let graph = crate::assets::find("js/views/graph.js").expect("graph view embedded");
    let graph_imports = imports(
        "js/views/graph.js",
        std::str::from_utf8(graph.bytes).unwrap(),
    );
    assert!(
        graph_imports
            .iter()
            .any(|t| t == "js/graph/layout-worker.js"),
        "{graph_imports:?}"
    );
    // And the page's own references.
    let index = std::str::from_utf8(crate::assets::find("index.html").unwrap().bytes).unwrap();
    for needle in ["src=\"", "href=\""] {
        for part in index.split(needle).skip(1) {
            let target: String = part.chars().take_while(|c| *c != '"').collect();
            if target.starts_with('#') {
                continue;
            }
            assert!(
                crate::assets::find(&target).is_some(),
                "index.html references {target}"
            );
        }
    }
}

#[test]
fn the_frontend_never_parses_html_or_inlines_script_or_style() {
    for a in crate::assets::embedded() {
        let src = String::from_utf8_lossy(a.bytes);
        for banned in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
        ] {
            // The DOM helper names the APIs it refuses in a comment.
            let hits = src
                .lines()
                .filter(|l| l.contains(banned) && !l.trim_start().starts_with("//"))
                .count();
            assert_eq!(hits, 0, "{} uses {banned}", a.path);
        }
        if a.path.ends_with(".html") {
            assert!(
                !src.contains("style="),
                "{}: inline style attribute",
                a.path
            );
            assert!(!src.contains("<style"), "{}: inline <style>", a.path);
            for script in src.split("<script").skip(1) {
                assert!(script.contains("src="), "{}: inline <script>", a.path);
            }
            assert!(
                !src.contains("http://") && !src.contains("https://"),
                "{}: external URL",
                a.path
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Edges: the graph endpoint and the confirm/reject writes
// ---------------------------------------------------------------------------

use engramdb::ops::links::{self as ops_links, LinkKind, LinkOrigin, LinkParams, LinkStatus};

/// The base fixture plus edges of every review state: a proposed NLI
/// contradiction `b ⊣ c`, a confirmed `b supports d`, a proposed similarity
/// `c related_to d`, and `b derived_from` a memory that was then deleted.
struct EdgeFixture {
    fx: Fixture,
    gone: String,
}

async fn edge_fixture() -> EdgeFixture {
    let fx = fixture().await;
    let store = MemoryStore::open(&fx.dir).await.unwrap();
    let edge = |src: &str, kind, dst: &str, origin, status, confidence| LinkParams {
        src: src.to_string(),
        kind,
        dst: dst.to_string(),
        dst_store: None,
        confidence: Some(confidence),
        origin,
        status: Some(status),
        note: None,
    };
    let (b, c, d) = (&fx.ids[1], &fx.ids[2], &fx.ids[3]);
    let gone = memory(MemoryType::Context, "Deleted later", &[], &[]);
    store.create(&gone).await.unwrap();
    for p in [
        edge(
            b,
            LinkKind::Contradicts,
            c,
            LinkOrigin::Nli,
            LinkStatus::Proposed,
            0.91,
        ),
        edge(
            b,
            LinkKind::Supports,
            d,
            LinkOrigin::Human,
            LinkStatus::Confirmed,
            1.0,
        ),
        edge(
            c,
            LinkKind::RelatedTo,
            d,
            LinkOrigin::Similarity,
            LinkStatus::Proposed,
            0.84,
        ),
        edge(
            b,
            LinkKind::DerivedFrom,
            &gone.id,
            LinkOrigin::Human,
            LinkStatus::Confirmed,
            1.0,
        ),
    ] {
        ops_links::link(&store, p, Surface::Cli).await.unwrap();
    }
    store.delete(&gone.id).await.unwrap();
    EdgeFixture { fx, gone: gone.id }
}

fn edge_of<'a>(edges: &'a serde_json::Value, kind: &str) -> &'a serde_json::Value {
    edges
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} edge in {edges}"))
}

#[tokio::test]
async fn a_store_without_an_edges_table_still_has_a_graph() {
    // No `link` ever ran, so there is no edges table: the graph is the
    // memories plus the one edge a memory file itself declares
    // (`d.supersedes = [a]`), never an error.
    let fx = fixture().await;
    let (s, v) = get_json(
        &app(Some(fx.current.clone())),
        &format!("/api/projects/{}/graph", fx.id),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let edges = v["edges"].as_array().unwrap();
    assert!(edges.iter().all(|e| e["kind"] == "supersedes"), "{v}");
    assert_eq!(v["counts"]["proposed"], 0);
    // Invalidated memories are hidden by default (the fixture has one).
    assert_eq!(v["shown_nodes"], 4);
    assert_eq!(v["truncated"], false);
    let (_, v) = get_json(
        &app(Some(fx.current.clone())),
        &format!("/api/projects/{}/graph?include_invalidated=true", fx.id),
    )
    .await;
    assert_eq!(v["shown_nodes"], 5);
}

#[tokio::test]
async fn the_graph_has_nodes_edges_states_and_bounds() {
    let ef = edge_fixture().await;
    let fx = &ef.fx;
    let app = app(Some(fx.current.clone()));
    let base = format!("/api/projects/{}/graph", fx.id);
    let (s, v) = get_json(&app, &base).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let c = edge_of(&v["edges"], "contradicts");
    assert_eq!(c["status"], "proposed");
    assert_eq!(c["origin"], "nli");
    assert!((c["confidence"].as_f64().unwrap() - 0.91).abs() < 1e-9);
    assert_eq!(c["state"], "live");
    assert_eq!(edge_of(&v["edges"], "supports")["status"], "confirmed");
    assert_eq!(edge_of(&v["edges"], "related_to")["origin"], "similarity");
    let dangling = edge_of(&v["edges"], "derived_from");
    assert_eq!(dangling["state"], "dangling");
    assert_eq!(dangling["dst"], ef.gone.as_str());
    assert_eq!(v["counts"]["proposed"], 2);
    assert_eq!(v["counts"]["proposed_contradictions"], 1);
    assert_eq!(v["counts"]["dangling"], 1);

    let node = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == fx.ids[1].as_str())
        .unwrap();
    assert_eq!(node["degree"], 3);
    assert!(node["line"].is_string(), "{node}");
    assert!(node["line_slot"].is_u64(), "{node}");

    // Filters: kinds, proposed.
    let (_, v) = get_json(&app, &format!("{base}?kinds=supports")).await;
    assert_eq!(v["edges"].as_array().unwrap().len(), 1);
    let (_, v) = get_json(&app, &format!("{base}?include_proposed=false")).await;
    assert!(v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["status"] != "proposed"));

    // Bounded: the best-connected memory survives a limit of 1, and the
    // page says it is partial.
    let (_, v) = get_json(&app, &format!("{base}?limit=1")).await;
    assert_eq!(v["truncated"], true);
    assert_eq!(v["shown_nodes"], 1);
    assert_eq!(v["nodes"][0]["id"], fx.ids[1].as_str());
    for bad in ["kinds=nope", "limit=many", "include_proposed=maybe"] {
        let (s, v) = get_json(&app, &format!("{base}?{bad}")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
}

#[tokio::test]
async fn the_graph_and_detail_views_write_nothing() {
    let ef = edge_fixture().await;
    let fx = &ef.fx;
    std::fs::remove_file(fx.dir.join(".engramdb").join(".gitignore")).ok();
    let before = machine(&fx.dir);
    // Writes enabled: a GET still writes nothing.
    let app = app_with(Some(fx.current.clone()), None, true);
    for uri in [
        format!("/api/projects/{}/graph", fx.id),
        format!(
            "/api/projects/{}/graph?include_invalidated=true&include_rejected=true",
            fx.id
        ),
        format!("/api/projects/{}/memories/{}", fx.id, fx.ids[1]),
        format!("/api/projects/{}/stats", fx.id),
    ] {
        let r = send(&app, authed(&uri)).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
    }
    assert_eq!(before, machine(&fx.dir), "a GET changed something on disk");
}

#[tokio::test]
async fn detail_and_attention_carry_the_proposals() {
    let ef = edge_fixture().await;
    let fx = &ef.fx;
    let app = app(Some(fx.current.clone()));
    let (s, v) = get_json(
        &app,
        &format!("/api/projects/{}/memories/{}", fx.id, fx.ids[2]),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // `c` holds the related_to edge and is the target of the contradiction
    // (symmetric, so it may be stored on either side: look in both).
    let all: Vec<&serde_json::Value> = v["edges"]["outbound"]
        .as_array()
        .unwrap()
        .iter()
        .chain(v["edges"]["inbound"].as_array().unwrap())
        .collect();
    assert!(all
        .iter()
        .any(|e| e["kind"] == "contradicts" && e["other_id"] == fx.ids[1].as_str()));
    assert!(all
        .iter()
        .any(|e| e["kind"] == "related_to" && e["other_id"] == fx.ids[3].as_str()));

    let (_, v) = get_json(&app, &format!("/api/projects/{}/stats", fx.id)).await;
    let att = v["attention"].as_array().unwrap();
    assert_eq!(att[0]["reason"], "proposed_contradiction", "{att:?}");
    assert_eq!(att[0]["edge"]["kind"], "contradicts");
    assert_eq!(att[0]["edge"]["origin"], "nli");
    assert!(att.iter().any(|a| a["reason"] == "proposed_edge"));
    assert_eq!(v["edges"]["proposed_contradictions"], 1);
    assert_eq!(v["edges"]["proposed"], 2);
}

fn post_edge(path: &str, body: serde_json::Value) -> Request<Body> {
    req(Method::POST, path)
        .header(header::AUTHORIZATION, bearer())
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(CSRF_HEADER, CSRF)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn edge_status(dir: &Path, src: &str, kind: LinkKind, dst: &str) -> Option<LinkStatus> {
    let store = MemoryStore::open_read_only(dir).await.unwrap();
    for id in [src, dst] {
        let m = store.get(id).await.unwrap();
        let other = if id == src { dst } else { src };
        if let Some(l) = m.links.iter().find(|l| l.kind == kind && l.to == other) {
            return Some(l.status.clone());
        }
    }
    None
}

#[tokio::test]
async fn writes_are_refused_unless_enabled_and_same_origin() {
    let ef = edge_fixture().await;
    let fx = &ef.fx;
    let uri = format!("/api/projects/{}/edges/confirm", fx.id);
    let body = serde_json::json!({
        "src": fx.ids[2], "kind": "related_to", "dst": fx.ids[3],
    });

    // Read-only dashboard: 403 read_only, nothing written.
    let ro = app(Some(fx.current.clone()));
    let r = send(&ro, post_edge(&uri, body.clone())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["code"], "read_only");
    let r = send(
        &ro,
        post_edge(
            &format!("/api/projects/{}/edges/reject", fx.id),
            body.clone(),
        ),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], "read_only");

    // Writes enabled, but no CSRF header / a foreign Origin: the guard.
    let rw = app_with(Some(fx.current.clone()), None, true);
    let no_csrf = req(Method::POST, &uri)
        .header(header::AUTHORIZATION, bearer())
        .header(header::ORIGIN, format!("http://{HOST}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let foreign = req(Method::POST, &uri)
        .header(header::AUTHORIZATION, bearer())
        .header(header::ORIGIN, "http://evil.example")
        .header(CSRF_HEADER, CSRF)
        .body(Body::from(body.to_string()))
        .unwrap();
    let no_token = req(Method::POST, &uri)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(CSRF_HEADER, CSRF)
        .body(Body::from(body.to_string()))
        .unwrap();
    for (r, code) in [
        (no_csrf, StatusCode::FORBIDDEN),
        (foreign, StatusCode::FORBIDDEN),
        (no_token, StatusCode::UNAUTHORIZED),
    ] {
        assert_eq!(send(&rw, r).await.status, code);
    }
    assert_eq!(
        edge_status(&fx.dir, &fx.ids[2], LinkKind::RelatedTo, &fx.ids[3]).await,
        Some(LinkStatus::Proposed),
        "a refused write changed the edge"
    );

    // Malformed requests are 400, before any store is opened for writing.
    for bad in [
        serde_json::json!({"src": fx.ids[1], "kind": "supersedes", "dst": fx.ids[2]}),
        serde_json::json!({"src": fx.ids[1], "kind": "nope", "dst": fx.ids[2]}),
        serde_json::json!({"src": fx.ids[1], "kind": "contradicts", "dst": fx.ids[2], "verdict": "maybe"}),
        serde_json::json!({"src": fx.ids[2], "kind": "related_to", "dst": fx.ids[3], "verdict": "both_stand"}),
        serde_json::json!({"src": fx.ids[2], "kind": "related_to", "dst": fx.ids[3], "extra": 1}),
    ] {
        let r = send(&rw, post_edge(&uri, bad.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    // An edge that does not exist is 404.
    let r = send(
        &rw,
        post_edge(
            &uri,
            serde_json::json!({"src": fx.ids[3], "kind": "related_to", "dst": fx.ids[0]}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json());
    assert_eq!(r.json()["error"]["code"], "unknown_edge");
}

fn dashboard_events(dir: &Path, kind: UsageKind) -> Vec<MemoryEvent> {
    engramdb::telemetry::usage::load_memory_events(dir)
        .unwrap()
        .events
        .into_iter()
        .filter(|e| e.kind == kind && e.surface == Surface::Dashboard)
        .collect()
}

#[tokio::test]
async fn confirm_and_reject_write_through_ops_and_refresh_the_cache() {
    let ef = edge_fixture().await;
    let fx = &ef.fx;
    let (b, c, d) = (&fx.ids[1], &fx.ids[2], &fx.ids[3]);
    let state = AppState::new(security(), None, true, Some(fx.current.clone()));
    let app = crate::router(state.clone());
    let graph = format!("/api/projects/{}/graph", fx.id);
    let (_, before) = get_json(&app, &graph).await;
    assert_eq!(before["counts"]["proposed"], 2);
    assert_eq!(state.stores.open_count().await, 1);

    // Reject the similarity proposal: kept as rejected.
    let r = send(
        &app,
        post_edge(
            &format!("/api/projects/{}/edges/reject", fx.id),
            serde_json::json!({"src": c, "kind": "related_to", "dst": d}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json());
    assert_eq!(
        edge_status(&fx.dir, c, LinkKind::RelatedTo, d).await,
        Some(LinkStatus::Rejected)
    );
    // The write dropped the cached read-only handle.
    assert_eq!(state.stores.open_count().await, 0);
    let (_, after) = get_json(&app, &graph).await;
    assert_eq!(after["counts"]["proposed"], 1);
    assert!(after["edges"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["kind"] != "related_to"));
    assert!(!dashboard_events(&fx.dir, UsageKind::Linked).is_empty());

    // Supersede: `c` wins over `b`; only `b` closes, the contradiction
    // becomes `c supersedes b`, and `b` records `resolved`.
    let r = send(
        &app,
        post_edge(
            &format!("/api/projects/{}/edges/confirm", fx.id),
            serde_json::json!({
                "src": b, "kind": "contradicts", "dst": c,
                "verdict": "supersedes", "winner": c,
            }),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json());
    let store = MemoryStore::open_read_only(&fx.dir).await.unwrap();
    assert!(store.get(b).await.unwrap().invalidated_at.is_some());
    assert!(store.get(c).await.unwrap().invalidated_at.is_none());
    assert_eq!(
        edge_status(&fx.dir, b, LinkKind::Contradicts, c).await,
        None
    );
    let resolved = dashboard_events(&fx.dir, UsageKind::Resolved);
    assert!(resolved.iter().any(|e| &e.memory_id == b), "{resolved:?}");
    let (_, after) = get_json(&app, &graph).await;
    assert_eq!(after["counts"]["proposed_contradictions"], 0);
}

#[tokio::test]
async fn both_stand_confirms_the_contradiction() {
    let ef = edge_fixture().await;
    let fx = &ef.fx;
    let (b, c) = (&fx.ids[1], &fx.ids[2]);
    let app = app_with(Some(fx.current.clone()), None, true);
    let r = send(
        &app,
        post_edge(
            &format!("/api/projects/{}/edges/confirm", fx.id),
            serde_json::json!({"src": c, "kind": "contradicts", "dst": b, "verdict": "both_stand"}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json());
    assert_eq!(
        edge_status(&fx.dir, b, LinkKind::Contradicts, c).await,
        Some(LinkStatus::Confirmed)
    );
    let store = MemoryStore::open_read_only(&fx.dir).await.unwrap();
    assert!(store.get(b).await.unwrap().invalidated_at.is_none());
    assert!(store.get(c).await.unwrap().invalidated_at.is_none());
    // A contradiction must be confirmed with a verdict.
    let r = send(
        &app,
        post_edge(
            &format!("/api/projects/{}/edges/confirm", fx.id),
            serde_json::json!({"src": b, "kind": "contradicts", "dst": c}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}
