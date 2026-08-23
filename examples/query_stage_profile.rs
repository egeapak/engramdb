//! Where does a query actually spend its time?
//!
//! Times the four externally-observable stages of the read path against a real
//! store, so the cost of a daemon-side memory cache can be priced before it is
//! built. The two stages a cache would eliminate — the index projection and the
//! per-candidate file read+parse — are measured separately from the two it
//! would not (query embedding, vector search).
//!
//! Point it at an existing store:
//!   ENGRAMDB_DATA_DIR=… cargo run --release --example query_stage_profile -- <project-dir>
//!
//! `ORT_DYLIB_PATH` must resolve an ONNX Runtime, as for any embedding path.

use anyhow::Result;
use engramdb::embeddings::{EmbeddingProvider, OnnxProvider};

use engramdb::storage::MemoryStore;
use std::path::PathBuf;
use std::time::Instant;

fn stat(label: &str, mut ms: Vec<f64>, extra: &str) {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| ms[((ms.len() as f64 - 1.0) * q) as usize];
    println!(
        "  {label:<34} p50 {:>8.2} ms   p90 {:>8.2} ms   min {:>8.2} ms   {extra}",
        p(0.5),
        p(0.9),
        ms[0]
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let dir: PathBuf = std::env::args().nth(1).unwrap_or_else(|| ".".into()).into();
    let reps: usize = std::env::var("REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let t = Instant::now();
    let store = MemoryStore::open(&dir).await?;
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let provider = OnnxProvider::try_new().ok_or_else(|| {
        anyhow::anyhow!("no ONNX embedding provider — check ORT_DYLIB_PATH and the model cache")
    })?;
    let model_ms = t.elapsed().as_secs_f64() * 1000.0;

    let all = store.list_for_filtering().await?;
    println!("\nstore: {} rows in {}\n", all.len(), dir.display());
    println!(
        "  {:<34} {:>11.2} ms   (once per process)",
        "store open", open_ms
    );
    println!(
        "  {:<34} {:>11.2} ms   (once per process; what the daemon amortizes today)",
        "embedding model load", model_ms
    );
    println!();

    let queries = [
        "how does the caching layer handle the hot path in retrieval",
        "what convention governs the database indexing path",
        "why is the daemon on the hot loop for parsing",
    ];

    let (mut embed, mut index_scan, mut vsearch, mut getbatch) = (vec![], vec![], vec![], vec![]);

    // one warm pass so page cache and any lazy init are not in the numbers
    let warm = provider.embed(queries[0]).await?;
    let _ = store.vector_search(warm.clone(), 30, None).await?;
    let warm_ids: Vec<String> = all.iter().take(30).map(|e| e.id.clone()).collect();
    let _ = store.get_batch(&warm_ids).await?;

    for i in 0..reps {
        let q = queries[i % queries.len()];

        let t = Instant::now();
        let vector = provider.embed(q).await?;
        embed.push(t.elapsed().as_secs_f64() * 1000.0);

        // Stage 1: the whole post-predicate index projection streamed into Rust.
        let t = Instant::now();
        let rows = store.list_for_filtering().await?;
        index_scan.push(t.elapsed().as_secs_f64() * 1000.0);

        // Stage 4: k-NN over the chunks table.
        let t = Instant::now();
        let _ = store.vector_search(vector, 30, None).await?;
        vsearch.push(t.elapsed().as_secs_f64() * 1000.0);

        // Stage 3: two dirent scans + one file read+parse per candidate.
        let ids: Vec<String> = rows.iter().take(30).map(|e| e.id.clone()).collect();
        let t = Instant::now();
        let _ = store.get_batch(&ids).await?;
        getbatch.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    println!("per-query stages ({reps} reps, {} rows):", all.len());
    stat(
        "embed (ONNX forward pass)",
        embed,
        "daemon amortizes the LOAD, not this",
    );
    stat(
        "list_for_filtering (index scan)",
        index_scan,
        "<- a cache eliminates",
    );
    stat("vector_search (k-NN)", vsearch, "");
    stat(
        "get_batch(30) (read+parse .md)",
        getbatch,
        "<- a cache eliminates",
    );

    // How get_batch scales, since it is the stage a cache targets.
    println!("\nget_batch by candidate count:");
    for k in [1usize, 10, 30, 100, 300] {
        if k > all.len() {
            break;
        }
        let ids: Vec<String> = all.iter().take(k).map(|e| e.id.clone()).collect();
        let mut ms = vec![];
        for _ in 0..reps.min(10) {
            let t = Instant::now();
            let _ = store.get_batch(&ids).await?;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "  k={k:<5} p50 {:>7.2} ms   ({:>5.3} ms/memory)",
            ms[ms.len() / 2],
            ms[ms.len() / 2] / k as f64
        );
    }
    // The index projection is the dominant term, so break it down: is the cost
    // per-row (Arrow -> Rust for 20 columns) or fixed (table open + plan)?
    println!("\nindex-scan breakdown:");

    let mut ms = vec![];
    for _ in 0..10 {
        let t = Instant::now();
        let _ = store.count().await?;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "  {:<40} p50 {:>8.2} ms   (table open + plan, no rows)",
        "count()",
        ms[ms.len() / 2]
    );

    let mut ms = vec![];
    for _ in 0..10 {
        let t = Instant::now();
        let _ = store.list_ids().await?;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "  {:<40} p50 {:>8.2} ms   (1 column x N rows)",
        "list_ids()",
        ms[ms.len() / 2]
    );

    let mut ms = vec![];
    for _ in 0..10 {
        let t = Instant::now();
        let _ = store.list_for_filtering().await?;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "  {:<40} p50 {:>8.2} ms   (20 columns x N rows)",
        "list_for_filtering()",
        ms[ms.len() / 2]
    );

    // Does the vector search cost scale with the restrict list, or is it fixed?
    let vector = provider.embed(queries[0]).await?;
    for (label, restrict) in [
        ("vector_search(restrict=None)", None),
        (
            "vector_search(restrict=all ids)",
            Some(all.iter().map(|e| e.id.clone()).collect::<Vec<_>>()),
        ),
    ] {
        let mut ms = vec![];
        for _ in 0..10 {
            let t = Instant::now();
            let _ = store
                .vector_search(vector.clone(), 30, restrict.as_deref())
                .await?;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("  {label:<40} p50 {:>8.2} ms", ms[ms.len() / 2]);
    }

    Ok(())
}
