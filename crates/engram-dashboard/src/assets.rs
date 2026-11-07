//! The frontend: `web/` embedded at build time (see `build.rs`), or served
//! from disk with `--assets-dir` for development.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// One embedded file.
#[derive(Debug)]
pub struct Asset {
    /// Served path, relative, `/`-separated (`js/app.js`).
    pub path: &'static str,
    pub bytes: &'static [u8],
    pub mime: &'static str,
    /// A quoted strong ETag of the bytes.
    pub etag: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// Every embedded file.
pub fn embedded() -> &'static [Asset] {
    ASSETS
}

pub fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.path == path)
}

/// Must agree with `build.rs`'s copy (`mime_types_agree_with_build_rs`).
pub fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Clone)]
pub enum AssetSource {
    Embedded,
    Disk(PathBuf),
}

/// `/` → `index.html`; otherwise the path without its leading slash, or
/// `None` for anything that could leave the asset root (`..`, an absolute or
/// drive path, a backslash, NUL).
fn asset_path(uri_path: &str) -> Option<String> {
    let rel = uri_path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.contains('\\') || rel.contains('\0') {
        return None;
    }
    let p = Path::new(rel);
    if !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(rel.to_string())
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "Not found\n").into_response()
}

/// The router's fallback: static files for GET/HEAD, 405 otherwise.
pub async fn serve(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = req.uri().path();
    if path == "/api" || path.starts_with("/api/") {
        return crate::ApiError::not_found("unknown_endpoint", format!("no API endpoint {path}"))
            .into_response();
    }
    let Some(rel) = asset_path(path) else {
        return not_found();
    };
    match &state.assets {
        AssetSource::Embedded => {
            let Some(asset) = find(&rel) else {
                return not_found();
            };
            let fresh = req
                .headers()
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.split(',').any(|t| t.trim() == asset.etag));
            let mut res = if fresh {
                StatusCode::NOT_MODIFIED.into_response()
            } else {
                asset.bytes.into_response()
            };
            let h = res.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(asset.mime));
            h.insert(header::ETAG, HeaderValue::from_static(asset.etag));
            // Revalidate every time: a rebuilt binary must not be shadowed by
            // a cached module from the last one, and the 304 is cheap.
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            res
        }
        AssetSource::Disk(root) => {
            // Canonicalize both and compare by components, never by text: a
            // symlink inside the asset dir must not lead outside it. On the
            // blocking pool: these are synchronous syscalls.
            let (root, joined) = (root.clone(), root.join(&rel));
            let checked = tokio::task::spawn_blocking(move || {
                let root = std::fs::canonicalize(root).ok()?;
                let file = std::fs::canonicalize(joined).ok()?;
                (file.starts_with(&root) && file.is_file()).then_some(file)
            })
            .await;
            let Ok(Some(file)) = checked else {
                return not_found();
            };
            match tokio::fs::read(&file).await {
                Ok(bytes) => {
                    let mut res = bytes.into_response();
                    let h = res.headers_mut();
                    h.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static(mime_for(&rel)),
                    );
                    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                    res
                }
                Err(_) => not_found(),
            }
        }
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn asset_paths_cannot_escape() {
        assert_eq!(asset_path("/").as_deref(), Some("index.html"));
        assert_eq!(asset_path("/js/app.js").as_deref(), Some("js/app.js"));
        for bad in [
            "/../etc/passwd",
            "/js/../../x",
            "/./x",
            "/a\\b",
            "//etc/passwd",
        ] {
            // `//etc` trims to `etc/passwd`, which is a plain relative path
            // inside the root; it is simply not an asset.
            if bad == "//etc/passwd" {
                assert_eq!(asset_path(bad).as_deref(), Some("etc/passwd"));
                continue;
            }
            assert_eq!(asset_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn index_and_entry_module_are_embedded() {
        assert!(find("index.html").is_some());
        assert!(find("js/app.js").is_some());
        assert!(find("css/app.css").is_some());
    }

    #[test]
    fn mime_types_agree_with_build_rs() {
        for a in embedded() {
            assert_eq!(a.mime, mime_for(a.path), "{}", a.path);
        }
    }
}
