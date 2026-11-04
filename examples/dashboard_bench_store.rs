//! Build the dashboard benchmark stores: deterministic projects of 60, 1k,
//! 2k and 10k memories with scopes, sub-scopes and relations of every kind
//! and status (see `examples/support/dashboard_store.rs`). No embedding
//! model is needed.
//!
//! ```bash
//! cargo run --example dashboard_bench_store -- <out dir> [sizes…]
//! # e.g. … -- /tmp/dash-bench s1k s10k      (default: s60 s1k s2k s10k)
//! ```
//!
//! Layout: `<out>/{data,conf,models}` is one machine (registry, global data,
//! an empty model cache) shared by every store, and each store is a project
//! at `<out>/<size>`. Run the dashboard on one with the same environment:
//!
//! ```bash
//! cd <out>/s1k && ENGRAMDB_DATA_DIR=<out>/data ENGRAMDB_CONFIG_DIR=<out>/conf \
//!   ENGRAMDB_MODEL_CACHE_DIR=<out>/models ENGRAMDB_OFFLINE=1 ENGRAMDB_IN_PROCESS=1 \
//!   engramdb dashboard --no-open
//! ```
//!
//! `crates/engram-dashboard/bench/browser/run.mjs` does exactly that.

#[path = "support/dashboard_store.rs"]
mod dashboard_store;

use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: dashboard_bench_store <out dir> [s60|s1k|s2k|s10k …]")
    })?);
    let wanted: Vec<String> = args.collect();
    std::fs::create_dir_all(&out)?;
    let out = std::fs::canonicalize(&out)?;
    dashboard_store::use_machine_dir(&out)?;
    for (name, n) in dashboard_store::SIZES {
        if !wanted.is_empty() && !wanted.iter().any(|w| w == name) {
            continue;
        }
        let t = std::time::Instant::now();
        let id = dashboard_store::build(&out.join(name), n).await?;
        println!(
            "{name}: {n} memories, project {id}, {:.1}s",
            t.elapsed().as_secs_f64()
        );
    }
    Ok(())
}
