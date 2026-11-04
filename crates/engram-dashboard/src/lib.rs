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
//! random 32-byte token per run, handed to the page in the URL fragment
//! (`/#token=…`, never sent to a server), kept in `sessionStorage` and sent
//! as `Authorization: Bearer` on every `/api` call — no cookie, because
//! cookies are shared by every port on 127.0.0.1; the static files are
//! public (they hold no data); `Host` must be a loopback name (or the
//! explicit `--bind` address) with the right port, which defeats DNS
//! rebinding; any non-GET needs a matching `Origin` and the per-run CSRF
//! header; a strict CSP with no inline script or style. The browser is
//! opened through a 0600 launch file, so the token never sits in a
//! process's argv.
//!
//! **No writes on GET.** Every store is opened with
//! [`engramdb::storage::MemoryStore::open_read_only`] and held behind
//! [`stores::ReadStore`], which exposes reads only, so a handler cannot write
//! through it by accident. A dashboard view records no usage event either: a
//! page view is not a `fetched` (the agent did not open the memory), and
//! counting it would let browsing inflate the signal the promotion job reads.
//! The one exception is opt-in: with `--allow-writes`, the two POST routes in
//! `writes.rs` confirm or reject a *proposed* edge, each on a writable store
//! opened for that request (never through the read-only cache, which it then
//! invalidates). Without the flag they answer 403.

pub mod api;
pub mod assets;
mod error;
mod graph;
pub mod projects;
mod relations;
pub mod security;
pub mod stores;
mod writes;

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
            if let Err(e) = tokio::signal::ctrl_c().await {
                // Without a signal handler the server must not stop on its
                // own: keep serving until the process is killed.
                tracing::warn!("cannot listen for Ctrl-C ({e}); stop the dashboard by killing it");
                std::future::pending::<()>().await;
            }
        })
        .await
    }
}

/// Whether `ip` is a loopback address; anything else gets a warning, since
/// the token is then the only thing between the network and the memories.
pub fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

/// The launch file [`open_browser`] wrote. Dropping it deletes the file and
/// its directory, so keep it alive until the dashboard stops.
#[derive(Debug)]
pub struct LaunchPage {
    dir: tempfile::TempDir,
}

impl LaunchPage {
    /// Write `url` into a fresh 0700 directory as a 0600 HTML file that
    /// redirects to it. The token never appears in a command line: argv is
    /// world-readable through `/proc/<pid>/cmdline` and a browser keeps its
    /// own argv for its whole life, while this file is readable by us alone.
    pub fn write(url: &str) -> std::io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("engramdb-dashboard-")
            .tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // `tempfile` honours the umask; tighten it before the file exists.
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let attr = html_attr_escape(url);
        let page = format!(
            "<!doctype html>\n<meta charset=\"utf-8\">\n\
             <meta http-equiv=\"refresh\" content=\"0;url={attr}\">\n\
             <title>EngramDB dashboard</title>\n\
             <p><a href=\"{attr}\">Open the EngramDB dashboard</a></p>\n"
        );
        use std::io::Write as _;
        opts.open(dir.path().join("open.html"))?
            .write_all(page.as_bytes())?;
        Ok(Self { dir })
    }

    pub fn path(&self) -> PathBuf {
        self.dir.path().join("open.html")
    }
}

fn html_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Try to open `url` in the user's browser, silently, through a
/// [`LaunchPage`]; returns it so the caller can keep it until exit, or `None`
/// when the launch file could not be written or the opener could not be
/// started (no opener installed, an unsupported platform). On `None` the
/// caller prints the sign-in URL instead — the one case it reaches the
/// terminal without `--no-open`. "Started" is all this can know: the opener
/// is detached with its output discarded (so a chatty `xdg-open` cannot
/// scribble over the terminal), and an opener that starts but finds no
/// display fails after this returns — the user then re-runs with `--no-open`.
pub fn open_browser(url: &str) -> Option<LaunchPage> {
    let page = match LaunchPage::write(url) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("cannot write the dashboard launch file: {e}");
            return None;
        }
    };
    let target = page.path();
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&target);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // `start` treats the first quoted argument as a window title.
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]).arg(&target);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&target);
        c
    };
    #[cfg(any(unix, target_os = "windows"))]
    {
        use std::process::Stdio;
        if let Err(e) = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            tracing::debug!("cannot launch a browser: {e}");
            return None;
        }
        Some(page)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = (target, page);
        None
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
