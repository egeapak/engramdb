//! `engramdb dashboard`: serve the read-only web dashboard (see the
//! `engram-dashboard` crate for the security model).

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::Result;
use engram_dashboard::{Dashboard, DashboardOptions};

use crate::output::{outln, OutputFormatter};

/// Flags of `engramdb dashboard`.
pub struct DashboardParams {
    pub port: Option<u16>,
    pub bind: IpAddr,
    pub no_open: bool,
    pub assets_dir: Option<PathBuf>,
    pub allow_writes: bool,
}

pub async fn run_dashboard(
    dir: &Path,
    params: DashboardParams,
    formatter: &OutputFormatter,
) -> Result<()> {
    if !engram_dashboard::is_loopback(params.bind) {
        formatter.print_warning(&format!(
            "binding {} makes the dashboard reachable from other machines; the URL's \
             token is then the only thing guarding your memories, and the connection \
             is not encrypted",
            params.bind
        ));
    }
    if let Some(assets) = &params.assets_dir {
        if !assets.join("index.html").is_file() {
            anyhow::bail!(
                "--assets-dir {} has no index.html (point it at crates/engram-dashboard/web)",
                assets.display()
            );
        }
    }
    let dashboard = Dashboard::bind(DashboardOptions {
        dir: dir.to_path_buf(),
        bind: params.bind,
        port: params.port,
        assets_dir: params.assets_dir.clone(),
        allow_writes: params.allow_writes,
    })
    .await?;
    let url = dashboard.url().to_string();
    if formatter.is_json() {
        outln!(
            formatter,
            "{}",
            serde_json::json!({
                "url": url,
                "address": dashboard.local_addr().to_string(),
                "current_project": dashboard.current().map(|c| c.id.clone()),
                "allow_writes": params.allow_writes,
            })
        );
    } else {
        outln!(formatter, "EngramDB dashboard: {url}");
        match dashboard.current() {
            Some(c) => outln!(
                formatter,
                "Current project: {} ({})",
                c.root.display(),
                c.id
            ),
            None => outln!(
                formatter,
                "Started outside any project; showing every store on this machine."
            ),
        }
        if params.assets_dir.is_some() {
            outln!(formatter, "Serving web files from disk (no caching).");
        }
        if params.allow_writes {
            outln!(
                formatter,
                "Writes enabled (--allow-writes): proposed edges can be confirmed or rejected \
                 from the page. Press Ctrl-C to stop."
            );
        } else {
            outln!(formatter, "Read-only view. Press Ctrl-C to stop.");
        }
    }
    // Held until the server stops: dropping it deletes the launch file.
    let _launch = if params.no_open {
        None
    } else {
        engram_dashboard::open_browser(&url)
    };
    dashboard.serve().await
}
