//! Timing harness for MCP memory creation.
//!
//! Reproduces the exact work the MCP `create` tool does per call
//! (`ops::build_engine` + `ops::create_memory` with `embed_async: true`)
//! and measures where the wall-clock time goes.
//!
//! Run with the ONNX embedding model present in the unified cache:
//!   cargo run --release --example timing_create

use std::path::Path;
use std::time::Instant;

use engramdb::ops::{self, CreateParams};
use engramdb::storage::{InMemoryRegistry, MemoryStore};
use engramdb::title::TitleStrategy;
use engramdb::types::{MemoryType, Provenance, Visibility};

fn params(n: usize, embed_async: bool) -> CreateParams {
    CreateParams {
        type_: MemoryType::Decision,
        content: format!(
            "Memory #{n}: we decided to cache the retrieval engine on the MCP \
             server so the ONNX embedding model is loaded once per process \
             instead of once per tool call. This is body text that gets chunked \
             and embedded by the background ingest task."
        ),
        summary: format!("Timing sample memory #{n}"),
        title: None,
        physical: vec!["src/mcp/server.rs".to_string()],
        logical: vec![],
        tags: vec!["timing".to_string()],
        criticality: 0.5,
        confidence: 0.8,
        details: None,
        visibility: Visibility::Shared,
        provenance: Provenance::agent("timing"),
        supersedes: vec![],
        decay_strategy: None,
        decay_half_life: None,
        decay_ttl: None,
        decay_floor: None,
        title_strategy: TitleStrategy::default(),
        embed_async,
    }
}

async fn ms<F, T>(f: F) -> (f64, T)
where
    F: std::future::Future<Output = T>,
{
    let t = Instant::now();
    let v = f.await;
    (t.elapsed().as_secs_f64() * 1000.0, v)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path();
    let registry = InMemoryRegistry::new();
    let store = MemoryStore::init(dir, &registry).await.unwrap();
    let config_path = dir.join(".engramdb").join("config.toml");

    println!("=== Scenario A: current MCP behavior (rebuild engine every call) ===");
    println!(
        "{:>5}  {:>14}  {:>14}  {:>14}",
        "call", "build_ms", "create_ms", "total_ms"
    );
    let runs = 6;
    let mut a_build = Vec::new();
    let mut a_create = Vec::new();
    for i in 0..runs {
        let (build_ms, engine) = ms(ops::build_engine(store.clone(), &config_path, None)).await;
        let (create_ms, res) = ms(ops::create_memory(&store, params(i, true), Some(&engine))).await;
        res.unwrap();
        let total = build_ms + create_ms;
        println!("{i:>5}  {build_ms:>14.1}  {create_ms:>14.1}  {total:>14.1}");
        if i > 0 {
            a_build.push(build_ms);
            a_create.push(create_ms);
        }
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    println!(
        "  warm mean (calls 1..{runs}): build={:.1}ms  create={:.1}ms  total={:.1}ms",
        mean(&a_build),
        mean(&a_create),
        mean(&a_build) + mean(&a_create)
    );

    println!();
    println!("=== Scenario B: proposed (build engine once, reuse for every call) ===");
    let (build_once_ms, engine) = ms(ops::build_engine(store.clone(), &config_path, None)).await;
    println!("  one-time engine build: {build_once_ms:.1}ms");
    println!("{:>5}  {:>14}", "call", "create_ms");
    let mut b_create = Vec::new();
    for i in 0..runs {
        let (create_ms, res) = ms(ops::create_memory(
            &store,
            params(100 + i, true),
            Some(&engine),
        ))
        .await;
        res.unwrap();
        println!("{i:>5}  {create_ms:>14.1}");
        if i > 0 {
            b_create.push(create_ms);
        }
    }
    println!("  warm mean create: {:.1}ms", mean(&b_create));

    println!();
    println!("=== Scenario C: new server behavior (providers cached, store+engine per call) ===");
    let cfg = engramdb::storage::config::load_config(&config_path)
        .await
        .unwrap_or_default();
    let (resolve_ms, providers) = ms(async {
        // Mirror the server: resolve the configured (auto cores/2) embedding
        // pool so this measures the real provider-resolve cost.
        let pool = cfg
            .embeddings
            .resolved_pool_size(engramdb::types::config::available_cores());
        tokio::task::spawn_blocking(move || ops::resolve_engine_providers(&cfg, None, pool))
            .await
            .unwrap()
    })
    .await;
    println!("  one-time provider resolve: {resolve_ms:.1}ms");
    println!(
        "{:>5}  {:>14}  {:>14}  {:>14}",
        "call", "open+asm_ms", "create_ms", "total_ms"
    );
    let mut c_total = Vec::new();
    for i in 0..runs {
        let cfg_i = engramdb::storage::config::load_config(&config_path)
            .await
            .unwrap_or_default();
        let (asm_ms, engine) = ms(async {
            let s = MemoryStore::open(Path::new(dir)).await.unwrap();
            ops::assemble_engine(s, cfg_i, providers.clone())
        })
        .await;
        let (create_ms, res) = ms(ops::create_memory(
            &store,
            params(300 + i, true),
            Some(&engine),
        ))
        .await;
        res.unwrap();
        let total = asm_ms + create_ms;
        println!("{i:>5}  {asm_ms:>14.1}  {create_ms:>14.1}  {total:>14.1}");
        if i > 0 {
            c_total.push(total);
        }
    }
    println!("  warm mean total per call: {:.1}ms", mean(&c_total));

    println!();
    println!("=== Cost the async path correctly defers (embed_async true vs false) ===");
    let (_, engine2) = ms(ops::build_engine(store.clone(), &config_path, None)).await;
    let (sync_ms, r) = ms(ops::create_memory(
        &store,
        params(200, false),
        Some(&engine2),
    ))
    .await;
    r.unwrap();
    let (async_ms, r) = ms(ops::create_memory(
        &store,
        params(201, true),
        Some(&engine2),
    ))
    .await;
    r.unwrap();
    println!("  create_memory inline embed (embed_async=false): {sync_ms:.1}ms");
    println!("  create_memory bg embed   (embed_async=true) : {async_ms:.1}ms");

    println!();
    println!("=== Breakdown of the synchronous prefix that blocks the agent ===");
    let cold_store = MemoryStore::open(Path::new(dir)).await.unwrap();
    let (open_ms, _) = ms(MemoryStore::open(Path::new(dir))).await;
    let (cfg_ms, _) = ms(engramdb::storage::config::load_config(&config_path)).await;
    let (title_ms, _) = ms(engramdb::title::generate_title(
        TitleStrategy::default(),
        "Timing sample memory",
    ))
    .await;
    let m = engramdb::types::Memory::new(
        MemoryType::Decision,
        "probe",
        "probe body",
        Provenance::agent("timing"),
    );
    let (store_create_ms, _) = ms(cold_store.create(&m)).await;
    println!("  MemoryStore::open       : {open_ms:.1}ms");
    println!("  load_config             : {cfg_ms:.1}ms");
    println!("  build_engine (model load): {build_once_ms:.1}ms   <-- repeated EVERY MCP call");
    println!("  generate_title (keyword): {title_ms:.1}ms");
    println!("  store.create (disk+index): {store_create_ms:.1}ms");
}
