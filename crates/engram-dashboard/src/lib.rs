//! `engramdb dashboard`: a local, read-only web view of every EngramDB store
//! on the machine.
//!
//! A front end like `engram-mcp`: it depends on the core (`engramdb`) and is
//! never re-exported from it. It runs as **its own process**, never inside
//! the daemon — the daemon is a model host on a 0700 Unix socket with a
//! narrow protocol, and a TCP listener there would widen its attack surface
//! and tie its lifetime to a browser tab. Nothing here loads a model.
//!
//! **Security model** (see [`security`]): bound to 127.0.0.1 by default; a
//! random 32-byte token per run, printed in the URL and swapped on first use
//! for an `HttpOnly; SameSite=Strict` cookie by a redirect that strips it
//! from the address bar; every request needs the cookie (or the token);
//! `Host` must be a loopback name (or the explicit `--bind` address) with
//! the right port, which defeats DNS rebinding; any non-GET needs a matching
//! `Origin` and the per-run CSRF header; a strict CSP with no inline script
//! or style.
//!
//! **No writes on GET.** Every store is opened with
//! [`engramdb::storage::MemoryStore::open_read_only`] and held behind
//! [`stores::ReadStore`], which exposes reads only, so a handler cannot write
//! through it by accident. A dashboard view records no usage event either: a
//! page view is not a `fetched` (the agent did not open the memory), and
//! counting it would let browsing inflate the signal the promotion job reads.
//! `--allow-writes` is accepted and shown, and gates the phase-4 edge
//! confirm/reject; there is no write endpoint yet.

pub mod api;
pub mod assets;
mod error;
pub mod projects;
pub mod security;
pub mod stores;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;

pub use error::ApiError;

/// The port tried first when `--port` is not given. Fixed so a bookmarked
/// URL keeps working across runs (the token does not, by design); taken by
/// another process, the dashboard falls back to a free port.
pub const DEFAULT_PORT: u16 = 7341;

/// What `engramdb dashboard` was asked for.
#[derive(Debug, Clone)]
pub struct DashboardOptions {
    /// The directory the command ran in; its project is shown as current.
    pub dir: PathBuf,
    pub bind: IpAddr,
    /// `None`: [`DEFAULT_PORT`], falling back to a free one. `Some(0)`: a
    /// free port. `Some(n)`: exactly `n`, or fail.
    pub port: Option<u16>,
    /// Serve `web/` from this directory instead of the embedded copy, with
    /// `Cache-Control: no-store` (frontend development).
    pub assets_dir: Option<PathBuf>,
    pub allow_writes: bool,
}

impl Default for DashboardOptions {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: None,
            assets_dir: None,
            allow_writes: false,
        }
    }
}

/// The project of the directory the dashboard started in, resolved the way
/// every other command resolves it — a linked git worktree routes to its
/// main worktree — but without `resolve_project_root`'s side effects (it
/// initializes the main store, consolidates and registers the worktree).
/// Identified by project id, never by path text.
#[derive(Debug, Clone)]
pub struct CurrentProject {
    pub id: String,
    pub root: PathBuf,
}

impl CurrentProject {
    pub fn resolve(dir: &std::path::Path) -> Option<Self> {
        let root =
            engramdb::storage::detect_worktree_main(dir).unwrap_or_else(|| dir.to_path_buf());
        if !engramdb::storage::paths::project_dir(&root).is_dir() {
            return None;
        }
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        Some(Self {
            id: engramdb::storage::project_id::compute_project_id(&root),
            root,
        })
    }
}

/// Shared, immutable per-run state plus the store cache.
pub struct AppState {
    pub(crate) security: security::Security,
    pub(crate) assets: assets::AssetSource,
    pub(crate) allow_writes: bool,
    pub(crate) current: Option<CurrentProject>,
    pub(crate) stores: stores::StoreCache,
}

impl AppState {
    pub fn new(
        security: security::Security,
        assets_dir: Option<PathBuf>,
        allow_writes: bool,
        current: Option<CurrentProject>,
    ) -> Arc<Self> {
        Arc::new(Self {
            security,
            assets: match assets_dir {
                Some(dir) => assets::AssetSource::Disk(dir),
                None => assets::AssetSource::Embedded,
            },
            allow_writes,
            current,
            stores: stores::StoreCache::new(stores::DEFAULT_CAPACITY),
        })
    }
}

/// The whole application: API and assets behind the security layers.
pub fn router(state: Arc<AppState>) -> axum::Router {
    let app = api::routes().fallback(assets::serve);
    security::layer(app, state.clone()).with_state(state)
}

/// A bound, not yet serving, dashboard. Binding first lets the caller print
/// the URL (and tests learn the port) before the server runs.
pub struct Dashboard {
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    url: String,
    addr: SocketAddr,
}

impl Dashboard {
    pub async fn bind(opts: DashboardOptions) -> anyhow::Result<Self> {
        let listener = match opts.port {
            Some(port) => tokio::net::TcpListener::bind(SocketAddr::new(opts.bind, port))
                .await
                .with_context(|| {
                    format!("cannot listen on {}", SocketAddr::new(opts.bind, port))
                })?,
            None => {
                match tokio::net::TcpListener::bind(SocketAddr::new(opts.bind, DEFAULT_PORT)).await
                {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::debug!("port {DEFAULT_PORT} unavailable ({e}); using a free port");
                        tokio::net::TcpListener::bind(SocketAddr::new(opts.bind, 0))
                            .await
                            .with_context(|| format!("cannot listen on {}", opts.bind))?
                    }
                }
            }
        };
        let addr = listener.local_addr()?;
        let security = security::Security::new(addr)?;
        let url = security.launch_url();
        let current = CurrentProject::resolve(&opts.dir);
        let state = AppState::new(security, opts.assets_dir, opts.allow_writes, current);
        Ok(Self {
            listener,
            state,
            url,
            addr,
        })
    }

    /// The URL to open, token included. Print it to the user only.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The current project, if the start directory has one.
    pub fn current(&self) -> Option<&CurrentProject> {
        self.state.current.as_ref()
    }

    /// Serve until Ctrl-C (or the future given resolves).
    pub async fn serve_until(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        axum::serve(self.listener, router(self.state))
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok(())
    }

    pub async fn serve(self) -> anyhow::Result<()> {
        self.serve_until(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }
}

/// Whether `ip` is a loopback address; anything else gets a warning, since
/// the token is then the only thing between the network and the memories.
pub fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

/// Try to open `url` in the user's browser, silently. A failure (no display,
/// no opener installed, a headless box over SSH) is not an error: the URL
/// has been printed. The opener is detached with its output discarded, so a
/// chatty `xdg-open` cannot scribble over the terminal.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // `start` treats the first quoted argument as a window title.
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]).arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    #[cfg(any(unix, target_os = "windows"))]
    {
        use std::process::Stdio;
        let _ = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

// Test isolation: see `engram-test-support`. The API tests open real stores
// and the registry, which must be the per-process temp dirs.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn arm_test_isolation() {
    engram_test_support::arm();
}

#[cfg(test)]
mod tests;
