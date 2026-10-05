//! Daemon-pool throughput: mirrors `engram_models::embeddings::pool`.
//!
//! The daemon / MCP server builds `pool_size` independent sessions
//! (`cores/2` by default), each behind its own mutex, and hands requests to
//! them round-robin. Embedding sessions keep the engine's default thread
//! count (fastembed never sets intra-op threads). Concurrent callers each
//! send one text per request, as an agent's `query` or `create` does.
//!
//! For every (pool size, caller count, workload) point, `C` caller threads
//! run for a fixed window and we record aggregate requests per second and
//! the per-request latency each caller saw, including time spent waiting
//! for a free session.

use crate::common::{embed, rss_mib, Engine};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokenizers::Tokenizer;

#[derive(Serialize, Deserialize, Clone)]
pub struct PoolPoint {
    pub pool: usize,
    pub callers: usize,
    pub workload: String,
    pub requests: usize,
    pub req_per_sec: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub cores_busy: f64,
}

#[derive(Serialize, Deserialize)]
pub struct PoolReport {
    pub engine: String,
    pub threads_per_session: String,
    /// `(pool size, RSS after building the pool in MiB)`.
    pub rss_by_pool: Vec<(usize, f64)>,
    pub points: Vec<PoolPoint>,
}

type Member = Arc<Mutex<Box<dyn Engine>>>;

struct Pool {
    members: Vec<Member>,
    next: AtomicUsize,
}

impl Pool {
    fn pick(&self) -> &Member {
        &self.members[self.next.fetch_add(1, Ordering::Relaxed) % self.members.len()]
    }
}

fn run_point(
    pool: &Arc<Pool>,
    tok: &Arc<Tokenizer>,
    texts: &Arc<Vec<String>>,
    callers: usize,
    window: Duration,
) -> Result<(usize, f64, Vec<f64>, f64)> {
    let cpu0 = crate::common::cpu_secs();
    let start = Instant::now();
    let deadline = start + window;
    let handles: Vec<_> = (0..callers)
        .map(|c| {
            let (pool, tok, texts) = (pool.clone(), tok.clone(), texts.clone());
            std::thread::spawn(move || -> Result<Vec<f64>> {
                let mut lat = Vec::new();
                // Callers start at different texts so they are not in lockstep.
                let mut i = c * 7;
                while Instant::now() < deadline {
                    let text = texts[i % texts.len()].as_str();
                    i += 1;
                    let t = Instant::now();
                    let member = pool.pick();
                    let mut engine = member.lock().unwrap();
                    embed(engine.as_mut(), &tok, &[text])?;
                    drop(engine);
                    lat.push(t.elapsed().as_secs_f64() * 1e3);
                }
                Ok(lat)
            })
        })
        .collect();
    let mut all = Vec::new();
    for h in handles {
        all.extend(h.join().expect("caller thread panicked")?);
    }
    let wall = start.elapsed().as_secs_f64();
    let cores_busy = (crate::common::cpu_secs() - cpu0) / wall;
    Ok((all.len(), all.len() as f64 / wall, all, cores_busy))
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Builds pools of each size by reusing members (member `k` of a pool of 4
/// is the same session as member `k` of a pool of 2), so a sweep loads the
/// model `max(pool_sizes)` times, not once per point.
pub struct Sweep {
    pub pool_sizes: Vec<usize>,
    pub caller_counts: Vec<usize>,
    pub window: Duration,
}

pub fn run(
    name: &str,
    load: &dyn Fn() -> Result<Box<dyn Engine>>,
    tok: Tokenizer,
    queries: Vec<String>,
    docs: Vec<String>,
    sweep: &Sweep,
) -> Result<PoolReport> {
    let (pool_sizes, caller_counts, window) =
        (&sweep.pool_sizes, &sweep.caller_counts, sweep.window);
    let tok = Arc::new(tok);
    let workloads = [("query", Arc::new(queries)), ("doc", Arc::new(docs))];
    let mut members: Vec<Member> = Vec::new();
    let mut rss_by_pool = Vec::new();
    let mut points = Vec::new();
    for &p in pool_sizes.iter() {
        while members.len() < p {
            let mut engine = load()?;
            // Warm each session once so the first measured request is not
            // paying lazy allocation.
            embed(engine.as_mut(), &tok, &["warm up"])?;
            members.push(Arc::new(Mutex::new(engine)));
        }
        rss_by_pool.push((p, rss_mib().0));
        let pool = Arc::new(Pool {
            members: members[..p].to_vec(),
            next: AtomicUsize::new(0),
        });
        for (wl, texts) in &workloads {
            for &c in caller_counts.iter() {
                let (requests, rps, mut lat, cores_busy) =
                    run_point(&pool, &tok, texts, c, window)?;
                lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let point = PoolPoint {
                    pool: p,
                    callers: c,
                    workload: wl.to_string(),
                    requests,
                    req_per_sec: rps,
                    p50_ms: pct(&lat, 0.50),
                    p99_ms: pct(&lat, 0.99),
                    cores_busy,
                };
                eprintln!(
                    "[{name}] pool={p} callers={c} {wl}: {:.1} req/s, p50 {:.1} ms, p99 {:.1} ms, {:.1} cores",
                    point.req_per_sec, point.p50_ms, point.p99_ms, point.cores_busy
                );
                points.push(point);
            }
        }
    }
    Ok(PoolReport {
        engine: name.to_string(),
        threads_per_session: std::env::var("ENGINE_THREADS")
            .or_else(|_| std::env::var("RAYON_NUM_THREADS"))
            .unwrap_or_else(|_| "default".into()),
        rss_by_pool,
        points,
    })
}
