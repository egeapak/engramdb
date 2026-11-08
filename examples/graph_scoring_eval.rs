//! What the phase-5 retrieval knobs do, and what they cost.
//!
//! The knobs (`[retrieval.scoring].contradicts_penalty`, `supports_boost`,
//! `usage_weight`, and `[retrieval].expand_links`) are all off by default.
//! No existing eval in this repository carries edges or usage — `fts_quality`
//! and `embed_quality` score text matching only — so this harness builds its
//! own fixtures:
//!
//! 1. **Quality**, on a constructed store where each scenario has a known
//!    right answer the knob is *designed* to surface. It shows the mechanism
//!    works and by how much it moves a rank; it is not evidence that the
//!    knobs help on a real store, and defaults should not be tuned on it.
//!    Control probes (no edges, no usage) must not move at all.
//! 2. **Latency**, on a ~1,000-memory store with a few hundred edges and
//!    usage events: query time with each knob off and on.
//!
//! No embedding model is used (keyword and degraded scoring only), so the
//! numbers are deterministic and the harness runs without an ONNX runtime.
//!
//! Run: `cargo run --profile eval --example graph_scoring_eval`
//! (debug timings mostly measure unoptimized LanceDB).

use std::collections::HashMap;
use std::time::Instant;

use engramdb::retrieval::engine::{RetrievalEngine, RetrievalMode, RetrievalQuery};
use engramdb::storage::{InMemoryRegistry, MemoryStore};
use engramdb::telemetry::usage::{record_memory_events, MemoryEvent, Surface, UsageKind};
use engramdb::types::{
    EngramConfig, Link, LinkKind, LinkOrigin, LinkStatus, Memory, MemoryType, Provenance,
    Visibility,
};

fn mem(summary: &str, content: &str, crit: f64, logical: &[&str]) -> Memory {
    let mut m = Memory::new(MemoryType::Decision, summary, content, Provenance::human());
    m.visibility = Visibility::Shared;
    m.criticality = crit;
    m.decay = None;
    m.logical = logical.iter().map(|s| s.to_string()).collect();
    m
}

fn link(to: &Memory, kind: LinkKind, status: LinkStatus) -> Link {
    let mut l = Link::new(&to.id, kind, LinkOrigin::Human);
    l.status = status;
    l
}

fn usage(store: &MemoryStore, id: &str, fetched: usize, injected_sessions: usize) {
    let mut ev = Vec::new();
    for i in 0..fetched {
        let mut e = MemoryEvent::new(id, &store.project_id, UsageKind::Fetched, Surface::Mcp);
        e.session_id = Some(format!("f{i}"));
        ev.push(e);
    }
    for i in 0..injected_sessions {
        let mut e = MemoryEvent::new(
            id,
            &store.project_id,
            UsageKind::Injected,
            Surface::HookPrompt,
        );
        e.session_id = Some(format!("i{i}"));
        ev.push(e);
    }
    record_memory_events(&store.project_dir, &ev).unwrap();
}

/// One probe: a query and the memory that should rank first.
struct Probe {
    scenario: &'static str,
    query: RetrievalQuery,
    target: String,
}

fn filter(q: &str) -> RetrievalQuery {
    RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some(q.to_string()),
        max_results: Some(5),
        ..Default::default()
    }
}

const TOPICS: [&str; 8] = [
    "billing", "search", "ingest", "auth", "export", "metrics", "upload", "notify",
];

async fn build_quality_store() -> (tempfile::TempDir, MemoryStore, Vec<Probe>) {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut all: Vec<Memory> = Vec::new();
    let mut probes = Vec::new();
    let mut usage_plan: Vec<(String, usize, usize)> = Vec::new();

    for t in TOPICS {
        // A: contradiction. Two contested claims outrank the uncontested one
        // on criticality; a confirmed (undirected) contradiction penalizes
        // both, so the uncontested memory should come first.
        let s = mem(
            &format!("{t} cache policy is write-through"),
            "cache",
            0.80,
            &[t],
        );
        let mut c = mem(
            &format!("{t} cache policy is write-back"),
            "cache",
            0.78,
            &[t],
        );
        let u = mem(
            &format!("{t} cache policy evicts lru entries"),
            "cache",
            0.72,
            &[t],
        );
        c.links = vec![link(&s, LinkKind::Contradicts, LinkStatus::Confirmed)];
        probes.push(Probe {
            scenario: "contradicts",
            query: filter(&format!("{t} cache policy")),
            target: u.id.clone(),
        });
        // B: supports. The evidence-backed rule sits just under a distractor.
        let w = mem(
            &format!("{t} retries are capped at three"),
            "retry",
            0.60,
            &[t],
        );
        let x = mem(
            &format!("{t} retries use jitter backoff"),
            "retry",
            0.63,
            &[t],
        );
        let mut e1 = mem(&format!("incident {t} 41 storm"), "evidence one", 0.4, &[t]);
        let mut e2 = mem(&format!("incident {t} 57 storm"), "evidence two", 0.4, &[t]);
        e1.links = vec![link(&w, LinkKind::Supports, LinkStatus::Confirmed)];
        e2.links = vec![link(&w, LinkKind::Supports, LinkStatus::Confirmed)];
        probes.push(Probe {
            scenario: "supports",
            query: filter(&format!("{t} retries")),
            target: w.id.clone(),
        });
        // C: expansion. The answer shares no word with the query; only an
        // edge from the hit reaches it.
        let h = mem(
            &format!("{t} deploys freeze on fridays"),
            "freeze",
            0.7,
            &[t],
        );
        let mut r = mem(&format!("{t} release window rules"), "window", 0.6, &[t]);
        r.links = vec![link(&h, LinkKind::DerivedFrom, LinkStatus::Confirmed)];
        let mut q = filter(&format!("{t} deploys freeze"));
        q.max_results = Some(1);
        probes.push(Probe {
            scenario: "expand",
            query: q,
            target: r.id.clone(),
        });
        // D: usage. Equal matches: one agents open, one hooks inject that
        // nobody opens (and that ranks a hair higher).
        let f = mem(
            &format!("{t} schema migrations run offline"),
            "mig",
            0.60,
            &[t],
        );
        let i = mem(
            &format!("{t} schema migrations need review"),
            "mig",
            0.62,
            &[t],
        );
        usage_plan.push((f.id.clone(), 8, 4));
        usage_plan.push((i.id.clone(), 0, 60));
        probes.push(Probe {
            scenario: "usage",
            query: filter(&format!("{t} schema migrations")),
            target: f.id.clone(),
        });
        // Control: no edges, no usage. Must not move.
        let k1 = mem(&format!("{t} logging format is json"), "log", 0.7, &[t]);
        let k2 = mem(&format!("{t} logging level is info"), "log", 0.6, &[t]);
        probes.push(Probe {
            scenario: "control",
            query: filter(&format!("{t} logging")),
            target: k1.id.clone(),
        });
        // A proposed contradiction on the control pair: must not count.
        let mut k2 = k2;
        k2.links = vec![link(&k1, LinkKind::Contradicts, LinkStatus::Proposed)];
        all.extend([s, c, u, w, x, e1, e2, h, r, f, i, k1, k2]);
    }
    store.create_batch(&all).await.unwrap();
    store.reindex().await.unwrap();
    for (id, f, s) in usage_plan {
        usage(&store, &id, f, s);
    }
    (tmp, store, probes)
}

/// (rank with knobs off, rank with knobs on, whether the list changed).
type Outcome = (Option<usize>, Option<usize>, bool);

fn rank_of(r: &engramdb::retrieval::engine::RetrievalResult, id: &str) -> Option<usize> {
    r.memories
        .iter()
        .position(|sm| sm.memory.id == id)
        .map(|p| p + 1)
}

async fn quality() {
    let (_tmp, store, probes) = build_quality_store().await;
    let off = EngramConfig::default();
    let mut on = EngramConfig::default();
    on.retrieval.scoring.contradicts_penalty = 0.10;
    on.retrieval.scoring.supports_boost = 0.05;
    on.retrieval.scoring.usage_weight = 0.05;
    let e_off = RetrievalEngine::new(store.clone(), off);
    let e_on = RetrievalEngine::new(store, on);

    println!(
        "## Quality (constructed fixture, {} topics)\n",
        TOPICS.len()
    );
    println!(
        "{:<12} {:>7} {:>9} {:>9} {:>9} {:>9}  {:>7}",
        "scenario", "probes", "R@1 off", "R@1 on", "MRR off", "MRR on", "moved"
    );
    let mut by: HashMap<&str, Vec<Outcome>> = HashMap::new();
    for p in &probes {
        let a = e_off.query(&p.query).await.unwrap();
        let mut q_on = p.query.clone();
        if p.scenario == "expand" {
            q_on.expand_links = Some(3);
        }
        let b = e_on.query(&q_on).await.unwrap();
        let ids = |r: &engramdb::retrieval::engine::RetrievalResult| {
            r.memories
                .iter()
                .map(|sm| (sm.memory.id.clone(), sm.score.to_bits()))
                .collect::<Vec<_>>()
        };
        by.entry(p.scenario).or_default().push((
            rank_of(&a, &p.target),
            rank_of(&b, &p.target),
            ids(&a) != ids(&b),
        ));
    }
    for s in ["contradicts", "supports", "expand", "usage", "control"] {
        let v = &by[s];
        let n = v.len() as f64;
        let r1 = |f: fn(&Outcome) -> Option<usize>| {
            v.iter().filter(|x| f(x) == Some(1)).count() as f64 / n
        };
        let mrr = |f: fn(&Outcome) -> Option<usize>| {
            v.iter()
                .map(|x| f(x).map_or(0.0, |r| 1.0 / r as f64))
                .sum::<f64>()
                / n
        };
        println!(
            "{:<12} {:>7} {:>9.2} {:>9.2} {:>9.3} {:>9.3}  {:>7}",
            s,
            v.len(),
            r1(|x| x.0),
            r1(|x| x.1),
            mrr(|x| x.0),
            mrr(|x| x.1),
            v.iter().filter(|x| x.2).count()
        );
    }
    println!(
        "\n(on: contradicts_penalty 0.10, supports_boost 0.05, usage_weight 0.05; \
         expand probes add expand_links 3. `moved` = probes whose ranked list changed.)\n"
    );
}

const WORDS: [&str; 24] = [
    "cache", "token", "retry", "schema", "queue", "index", "deploy", "config", "socket", "worker",
    "upload", "parser", "router", "billing", "export", "metric", "logger", "session", "daemon",
    "vector", "ledger", "backup", "quota", "replica",
];

async fn build_latency_store(n: usize) -> (tempfile::TempDir, MemoryStore) {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut mems: Vec<Memory> = (0..n)
        .map(|i| {
            let a = WORDS[i % WORDS.len()];
            let b = WORDS[(i * 7 + 3) % WORDS.len()];
            let c = WORDS[(i * 13 + 5) % WORDS.len()];
            mem(
                &format!("{a} {b} rule number {i}"),
                &format!("The {a} layer talks to {b} through {c}; memory {i} records why."),
                0.3 + (i % 7) as f64 * 0.1,
                &[&format!("area.{}", WORDS[i % 6])],
            )
        })
        .collect();
    // ~300 confirmed edges of the three scoring/expansion kinds, plus 100
    // proposed contradictions that must cost a lookup but change nothing.
    for i in 0..400usize {
        let src = (i * 31 + 7) % n;
        let dst = (i * 17 + 11) % n;
        if src == dst {
            continue;
        }
        let (kind, status) = match i % 4 {
            0 => (LinkKind::Supports, LinkStatus::Confirmed),
            1 => (LinkKind::Contradicts, LinkStatus::Confirmed),
            2 => (LinkKind::RelatedTo, LinkStatus::Confirmed),
            _ => (LinkKind::Contradicts, LinkStatus::Proposed),
        };
        let to = mems[dst].clone();
        mems[src].links.push(link(&to, kind, status));
    }
    store.create_batch(&mems).await.unwrap();
    store.reindex().await.unwrap();
    for (i, m) in mems.iter().enumerate().filter(|(i, _)| i % 3 == 0) {
        usage(&store, &m.id, i % 5, i % 11);
    }
    (tmp, store)
}

fn pct(mut v: Vec<f64>, q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * q).round() as usize]
}

async fn latency() {
    let n: usize = std::env::var("N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let reps: usize = std::env::var("REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let (_tmp, store) = build_latency_store(n).await;

    let mut configs: Vec<(&str, EngramConfig, Option<usize>)> = Vec::new();
    configs.push(("off (default)", EngramConfig::default(), None));
    let mut edges = EngramConfig::default();
    edges.retrieval.scoring.contradicts_penalty = 0.1;
    edges.retrieval.scoring.supports_boost = 0.05;
    configs.push(("edges (contra+supports)", edges, None));
    let mut usage_c = EngramConfig::default();
    usage_c.retrieval.scoring.usage_weight = 0.05;
    configs.push(("usage", usage_c, None));
    configs.push(("expand_links 5 only", EngramConfig::default(), Some(5)));
    let mut all = EngramConfig::default();
    all.retrieval.scoring.contradicts_penalty = 0.1;
    all.retrieval.scoring.supports_boost = 0.05;
    all.retrieval.scoring.usage_weight = 0.05;
    configs.push(("all + expand_links 5", all, Some(5)));

    let shapes: Vec<(&str, RetrievalQuery)> = vec![
        (
            "filter \"cache retry\"",
            RetrievalQuery {
                mode: RetrievalMode::Filter,
                query: Some("cache retry".into()),
                ..Default::default()
            },
        ),
        (
            "rank, no query (hook shape)",
            RetrievalQuery {
                mode: RetrievalMode::Rank,
                logical: vec!["area.cache".into()],
                ..Default::default()
            },
        ),
        (
            "rank \"token schema\"",
            RetrievalQuery {
                mode: RetrievalMode::Rank,
                query: Some("token schema".into()),
                ..Default::default()
            },
        ),
    ];

    println!("## Latency ({n} memories, ~300 confirmed + ~100 proposed edges, {reps} reps)\n");
    println!(
        "Warm: one engine, usage view cached (a long-lived MCP server or the daemon).\n\
         Cold: a fresh engine and an emptied usage cache per query (what a one-shot\n\
         CLI command or the first query after the 30 s TTL pays).\n"
    );
    println!(
        "{:<30} {:<26} {:>9} {:>9} {:>9} {:>11} {:>11}",
        "query", "config", "p50 ms", "p90 ms", "Δp50 ms", "cold p50", "Δcold p50"
    );
    let cold_reps = (reps / 4).max(5);
    for (shape, q) in &shapes {
        let mut base_p50 = None;
        let mut base_cold = None;
        for (name, cfg, expand) in &configs {
            let engine = RetrievalEngine::new(store.clone(), cfg.clone());
            let mut q = q.clone();
            q.expand_links = *expand;
            for _ in 0..3 {
                engine.query(&q).await.unwrap();
            }
            let mut ms = Vec::with_capacity(reps);
            for _ in 0..reps {
                let t = Instant::now();
                engine.query(&q).await.unwrap();
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let mut cold = Vec::with_capacity(cold_reps);
            for _ in 0..cold_reps {
                engramdb::retrieval::signals::invalidate_usage_cache(None);
                let t = Instant::now();
                let engine = RetrievalEngine::new(store.clone(), cfg.clone());
                engine.query(&q).await.unwrap();
                cold.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let p50 = pct(ms.clone(), 0.5);
            let cold_p50 = pct(cold, 0.5);
            let base = *base_p50.get_or_insert(p50);
            let base_c = *base_cold.get_or_insert(cold_p50);
            println!(
                "{:<30} {:<26} {:>9.2} {:>9.2} {:>+9.2} {:>11.2} {:>+11.2}",
                shape,
                name,
                p50,
                pct(ms, 0.9),
                p50 - base,
                cold_p50,
                cold_p50 - base_c
            );
        }
    }
}

fn main() {
    // Keep the global data dir (LanceDB, usage tables) out of the real one.
    // Set before the runtime exists: `set_var` is only sound while no other
    // thread can read the environment.
    let data = tempfile::TempDir::new().unwrap();
    let config = tempfile::TempDir::new().unwrap();
    std::env::set_var("ENGRAMDB_DATA_DIR", data.path());
    std::env::set_var("ENGRAMDB_CONFIG_DIR", config.path());
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            quality().await;
            latency().await;
        });
}
