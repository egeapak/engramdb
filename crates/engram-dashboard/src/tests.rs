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

fn cookie() -> String {
    format!("{}={TOKEN}", security().cookie_name())
}

fn req(method: Method, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
}

fn authed(uri: &str) -> Request<Body> {
    req(Method::GET, uri)
        .header(header::COOKIE, cookie())
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
async fn requests_without_the_token_are_refused() {
    let app = app(None);
    for uri in ["/", "/api/projects", "/js/app.js"] {
        let r = send(&app, req(Method::GET, uri).body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    let r = send(
        &app,
        req(Method::GET, "/api/session")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], "unauthorized");
    // A wrong token, by every carrier.
    let wrong = format!("{}=nope", security().cookie_name());
    for r in [
        req(Method::GET, "/?token=nope")
            .body(Body::empty())
            .unwrap(),
        req(Method::GET, "/api/session")
            .header(header::COOKIE, wrong)
            .body(Body::empty())
            .unwrap(),
        req(Method::GET, "/api/session")
            .header(header::AUTHORIZATION, "Bearer nope")
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(send(&app, r).await.status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn the_url_token_is_swapped_for_a_strict_cookie_and_stripped() {
    let app = app(None);
    let r = send(
        &app,
        req(Method::GET, &format!("/?token={TOKEN}&x=1"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.header(header::LOCATION), "/?x=1");
    let c = r.header(header::SET_COOKIE);
    assert!(c.starts_with(&cookie()), "{c}");
    assert!(
        c.contains("HttpOnly") && c.contains("SameSite=Strict"),
        "{c}"
    );
    assert_eq!(r.header(header::REFERRER_POLICY), "no-referrer");

    // The cookie then authorizes pages and the API.
    let r = send(&app, authed("/")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&r.body).contains("<title>"));

    // The token also works directly on the API (curl), with no redirect.
    let r = send(
        &app,
        req(Method::GET, &format!("/api/session?token={TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &app,
        req(Method::GET, "/api/session")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["csrf"], CSRF);
}

#[tokio::test]
async fn foreign_hosts_are_refused_even_with_the_cookie() {
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
            b.header(header::COOKIE, cookie())
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
                .header(header::COOKIE, cookie())
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
        let mut b = req(Method::POST, "/api/projects").header(header::COOKIE, cookie());
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
    // Both right: past the guard, where there is no write route yet.
    let r = send(&app, post(Some(&me), Some(CSRF))).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    // The same for a page path.
    let r = send(
        &app,
        req(Method::DELETE, "/")
            .header(header::COOKIE, cookie())
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
            .header(header::COOKIE, cookie())
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
    assert_eq!(v["usage"]["reads_last_7_days"], 4);
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
    assert_eq!(v["next_cursor"], "1");
    let (_, v2) = get_json(&app, &format!("{base}?sort=fetched&limit=1&cursor=1")).await;
    assert_eq!(ids(&v2), [fx.ids[1].clone()], "ties break by id");

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
/// in an embedded module, resolved against the module's directory.
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
