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
    // The address without the token: everything before the `#` fragment.
    let address_url = url.split('#').next().unwrap_or(&url).to_string();

    // Launch first, so what is printed can depend on whether it worked.
    // Held until the server stops: dropping it deletes the launch file.
    let launch = if params.no_open {
        None
    } else {
        engram_dashboard::open_browser(&url)
    };
    // The sign-in URL (with the token) reaches the terminal only when the
    // user needs it to get in: with `--no-open`, or — the one declared
    // exception — when the browser could not be launched, since the token is
    // otherwise unrecoverable and the server would serve nobody. When the
    // browser was launched, the token travelled through the private launch
    // file alone and the terminal shows the bare address.
    let show_token = launch.is_none();
    let launch_failed = !params.no_open && launch.is_none();

    if formatter.is_json() {
        let mut obj = serde_json::json!({
            "address": dashboard.local_addr().to_string(),
            "address_url": address_url,
            "opened_browser": launch.is_some(),
            "current_project": dashboard.current().map(|c| c.id.clone()),
            "allow_writes": params.allow_writes,
        });
        if show_token {
            obj["url"] = serde_json::Value::String(url.clone());
        }
        outln!(formatter, "{}", obj);
    } else {
        if show_token {
            if launch_failed {
                outln!(
                    formatter,
                    "Could not open a browser; open this sign-in URL yourself:"
                );
            }
            outln!(formatter, "EngramDB dashboard: {url}");
        } else {
            outln!(
                formatter,
                "EngramDB dashboard at {address_url} — opened in your browser"
            );
        }
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
    let _launch = launch;
    dashboard.serve().await
}
