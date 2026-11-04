//! Deterministic dashboard benchmark stores, shared by
//! `examples/dashboard_bench_store.rs` (builds them on disk for the browser
//! bench) and `crates/engram-dashboard/benches/dashboard_graph.rs` (builds
//! them in a temp dir for the in-process handler bench) through `#[path]`.
//! Not an example of its own: Cargo only discovers `examples/*.rs` and
//! `examples/*/main.rs`.
//!
//! A store is written the way a clone of a repository arrives: the `.md`
//! files first, then one `reindex` — no embedding model, no per-memory
//! LanceDB write (10,000 creates would leave 10,000 table versions). The
//! shape follows the layout harness's generator
//! (`crates/engram-dashboard/bench/data.mjs`): about ten top scopes with
//! sub-scopes and skewed (Zipf-like) sizes, about 0.7 relations per memory,
//! most inside a scope, cross-scope ones concentrated on a few scope pairs.
//! Every relation kind and status appears: confirmed / proposed / rejected,
//! `supersedes` through the canonical `supersedes`/`superseded_by` fields
//! (its target invalidated), a few relations to deleted memories, a few
//! challenged and invalidated memories, and read events for the reads-sized
//! glyphs. Ids and timestamps are fixed, so the same `n` always yields the
//! same files (usage events carry the build time; their counts do not).

#![allow(dead_code)] // each includer uses a different part

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use engramdb::ops::usage::{MemoryEvent, StoreRef, Surface, UsageKind};
use engramdb::storage::memory_file::{memory_filename, write_memory_file};
use engramdb::storage::{paths, FileRegistry, MemoryStore};
use engramdb::types::{
    Challenge, Link, LinkKind, LinkOrigin, LinkStatus, Memory, MemoryType, Provenance,
};

/// The sizes the benches use, with their directory names.
pub const SIZES: [(&str, usize); 4] = [
    ("s60", 60),
    ("s1k", 1_000),
    ("s2k", 2_000),
    ("s10k", 10_000),
];

/// mulberry32, the generator `bench/data.mjs` uses.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(1 | t);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
    fn below(&mut self, n: usize) -> usize {
        ((self.next() * n as f64) as usize).min(n.saturating_sub(1))
    }
    /// An index by weight.
    fn weighted(&mut self, w: &[f64]) -> usize {
        let mut q = self.next() * w.iter().sum::<f64>();
        for (i, x) in w.iter().enumerate() {
            if q < *x {
                return i;
            }
            q -= x;
        }
        w.len() - 1
    }
}

const TOPS: [(&str, &[&str]); 10] = [
    ("storage", &["index", "paths", "migration", "lock"]),
    ("retrieval", &["engine", "rerank", "filter"]),
    ("daemon", &["socket", "cache", "spawn"]),
    ("harvest", &["ledger", "index", "pin"]),
    ("cli", &["output", "hooks"]),
    ("mcp", &["server", "startup"]),
    ("scoring", &["decay", "signals"]),
    ("dashboard", &["graph", "security"]),
    ("models", &["onnx", "nli"]),
    ("ci", &[]),
];
/// Cross-scope affinity: these pairs get most of the cross-scope relations.
const AFFINITY: [(&str, &str, f64); 9] = [
    ("storage", "retrieval", 4.0),
    ("retrieval", "scoring", 4.0),
    ("daemon", "models", 3.0),
    ("harvest", "storage", 3.0),
    ("cli", "mcp", 2.0),
    ("mcp", "daemon", 2.0),
    ("dashboard", "storage", 1.5),
    ("models", "retrieval", 2.0),
    ("ci", "cli", 1.0),
];
const TYPES: [MemoryType; 6] = [
    MemoryType::Decision,
    MemoryType::Hazard,
    MemoryType::Convention,
    MemoryType::Context,
    MemoryType::Debug,
    MemoryType::Intent,
];
const WORDS: [&str; 18] = [
    "index", "lock", "vector", "schema", "retry", "socket", "token", "prune", "cache", "rank",
    "hook", "pool", "ledger", "digest", "budget", "edge", "decay", "filter",
];
const NOUNS: [&str; 5] = ["rule", "note", "fix", "trap", "choice"];
const KINDS: [(&str, f64); 6] = [
    ("related_to", 0.5),
    ("supports", 0.2),
    ("derived_from", 0.1),
    ("supersedes", 0.07),
    ("duplicates", 0.07),
    ("contradicts", 0.06),
];

/// A memory id, fixed per (`n`, index): uuid-shaped, so the filename helpers
/// and prefix resolution treat it like a real one.
pub fn memory_id(n: usize, i: usize) -> String {
    format!(
        "0b{:06x}-{:04x}-7000-8000-{:012x}",
        n,
        (i >> 16) & 0xffff,
        i
    )
}

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap()
}

/// The memories of a store of `n`, with their relations, and how often
/// each is read (the usage events [`build`] records).
pub fn generate(n: usize) -> (Vec<Memory>, Vec<u32>) {
    let mut rng = Rng((7u32.wrapping_mul(1_000_003)).wrapping_add(n as u32));
    let zipf: Vec<f64> = (0..TOPS.len())
        .map(|i| 1.0 / ((i + 1) as f64).powf(0.9))
        .collect();
    let t0 = base_time();
    let mut mems = Vec::with_capacity(n);
    let mut top_of = Vec::with_capacity(n);
    let mut scope_of = Vec::with_capacity(n);
    let mut reads = Vec::with_capacity(n);
    for i in 0..n {
        let (scope, top) = if rng.next() > 0.05 {
            let t = rng.weighted(&zipf);
            let (top, subs) = TOPS[t];
            if !subs.is_empty() && rng.next() > 0.3 {
                let sw: Vec<f64> = (0..subs.len()).map(|k| 1.0 / (k + 1) as f64).collect();
                (
                    format!("{top}.{}", subs[rng.weighted(&sw)]),
                    top.to_string(),
                )
            } else {
                (top.to_string(), top.to_string())
            }
        } else {
            (String::new(), String::new())
        };
        let label = format!(
            "{} {} {}",
            WORDS[rng.below(18)],
            WORDS[rng.below(18)],
            NOUNS[rng.below(5)]
        );
        let type_ = TYPES[rng.below(TYPES.len())];
        let summary = format!(
            "{}{}: {} memory {i}",
            label[..1].to_uppercase(),
            &label[1..],
            if scope.is_empty() { "unscoped" } else { &scope }
        );
        let mut m = Memory::new(
            type_,
            summary,
            format!("Content of memory {i}.\n\nWhy it matters, in a sentence or two."),
            Provenance::human(),
        );
        m.id = memory_id(n, i);
        m.title = Some(format!("M {i} {label}"));
        if !scope.is_empty() {
            m.logical = vec![scope.clone()];
        }
        m.criticality = (rng.next() * 10.0).round() / 10.0;
        let at = t0 + Duration::minutes(i as i64);
        m.created_at = at;
        m.updated_at = at;
        m.accessed_at = at;
        if rng.next() < 0.03 {
            m.invalidated_at = Some(at + Duration::days(3));
        }
        if rng.next() < 0.02 {
            let mut c = Challenge::new(format!("Observed otherwise in run {i}."));
            c.timestamp = at + Duration::days(1);
            m.challenges.push(c);
        }
        reads.push((rng.next().powi(3) * 24.0) as u32);
        top_of.push(top);
        scope_of.push(scope);
        mems.push(m);
    }

    // Members of each scope and top scope, by index.
    let mut by_scope: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    let mut by_top: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for i in 0..n {
        by_scope.entry(scope_of[i].as_str()).or_default().push(i);
        by_top.entry(top_of[i].as_str()).or_default().push(i);
    }
    let tops: Vec<&str> = TOPS
        .iter()
        .map(|t| t.0)
        .filter(|t| by_top.contains_key(t))
        .collect();
    let affinity = |a: &str, b: &str| {
        AFFINITY
            .iter()
            .find(|(x, y, _)| (*x == a && *y == b) || (*x == b && *y == a))
            .map_or(0.15, |p| p.2)
    };

    let want = (n as f64 * 0.7).round() as usize;
    let mut seen = std::collections::HashSet::new();
    let mut made = 0;
    let mut guard = 0;
    let at = t0 + Duration::days(2);
    while made < want && guard < want * 20 {
        guard += 1;
        // Skewed sources (hubs), so many memories stay isolated.
        let s = ((rng.next().powf(1.6)) * n as f64) as usize % n;
        let r = rng.next();
        let pick = |rng: &mut Rng, v: &[usize]| v[rng.below(v.len())];
        let t = if r < 0.45 && by_scope[scope_of[s].as_str()].len() > 1 {
            pick(&mut rng, &by_scope[scope_of[s].as_str()])
        } else if r < 0.75 && by_top[top_of[s].as_str()].len() > 1 {
            pick(&mut rng, &by_top[top_of[s].as_str()])
        } else {
            let cands: Vec<&str> = tops.iter().copied().filter(|x| *x != top_of[s]).collect();
            if cands.is_empty() {
                continue;
            }
            let w: Vec<f64> = cands.iter().map(|x| affinity(&top_of[s], x)).collect();
            let c = cands[rng.weighted(&w)];
            pick(&mut rng, &by_top[c])
        };
        if t == s || !seen.insert((s.min(t), s.max(t))) {
            continue;
        }
        let mut kind = KINDS[rng.weighted(&KINDS.map(|k| k.1))].0;
        let q = rng.next();
        let status = if q < 0.7 {
            LinkStatus::Confirmed
        } else if q < 0.9 {
            LinkStatus::Proposed
        } else {
            LinkStatus::Rejected
        };
        made += 1;
        if kind == "supersedes" {
            // Canonical fields, confirmed only; a target superseded once.
            if status == LinkStatus::Confirmed
                && mems[t].superseded_by.is_none()
                && mems[s].superseded_by.is_none()
            {
                let target = mems[t].id.clone();
                mems[s].supersedes.push(target);
                mems[t].superseded_by = Some(mems[s].id.clone());
                mems[t].invalidated_at.get_or_insert(at);
                continue;
            }
            kind = "derived_from";
        }
        let mut link = Link::new(
            mems[t].id.clone(),
            LinkKind::parse_lenient(kind),
            LinkOrigin::parse_lenient(if status == LinkStatus::Confirmed {
                "human"
            } else {
                "nli"
            }),
        );
        link.status = status;
        link.confidence = 0.55 + (rng.next() * 45.0).round() / 100.0;
        link.at = Some(at + Duration::minutes(made as i64));
        mems[s].links.push(link);
    }
    // About 1% of relations end at a memory that was deleted.
    for k in 0..(n / 100).max(1) {
        let s = (k * 7919) % n;
        let mut link = Link::new(
            memory_id(n, n + k),
            LinkKind::RelatedTo,
            LinkOrigin::parse_lenient("human"),
        );
        link.at = Some(at);
        mems[s].links.push(link);
    }
    (mems, reads)
}

/// Point the data and config dirs at `machine` (one registry for every
/// store), before anything resolves a path. Call once, first.
pub fn use_machine_dir(machine: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(machine.join("data"))?;
    std::fs::create_dir_all(machine.join("conf"))?;
    std::fs::create_dir_all(machine.join("models"))?;
    // Edition 2021: `set_var` is safe; this runs before any thread reads env.
    std::env::set_var("ENGRAMDB_DATA_DIR", machine.join("data"));
    std::env::set_var("ENGRAMDB_CONFIG_DIR", machine.join("conf"));
    std::env::set_var("ENGRAMDB_MODEL_CACHE_DIR", machine.join("models"));
    std::env::set_var("ENGRAMDB_OFFLINE", "1");
    Ok(())
}

/// Build (or rebuild) the store of `n` memories at `dir` and register it.
/// Returns its project id. The machine dir must already be in effect
/// ([`use_machine_dir`]).
pub async fn build(dir: &Path, n: usize) -> anyhow::Result<String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    let dir: PathBuf = std::fs::canonicalize(dir)?;
    let registry = FileRegistry::global()?;
    let store = MemoryStore::init(&dir, &registry).await?;
    let (mems, reads) = generate(n);
    let mdir = paths::memories_dir(&dir);
    std::fs::create_dir_all(&mdir)?;
    for m in &mems {
        std::fs::write(mdir.join(memory_filename(m)), write_memory_file(m)?)?;
    }
    let indexed = store.reindex().await?;
    anyhow::ensure!(indexed == n, "indexed {indexed} of {n} memories");
    let owner = StoreRef::of(&store);
    let mut events = Vec::new();
    for (m, r) in mems.iter().zip(&reads) {
        for k in 0..*r {
            let kind = match k % 3 {
                0 => UsageKind::Fetched,
                1 => UsageKind::Returned,
                _ => UsageKind::Injected,
            };
            events.push(MemoryEvent::new(
                &m.id,
                &store.project_id,
                kind,
                Surface::Cli,
            ));
        }
    }
    for chunk in events.chunks(4096) {
        engramdb::ops::usage::write_events(&owner, chunk);
    }
    // Drain the log into the table, as a maintenance pass would.
    engramdb::ops::usage::maintain_memory_events(&store).await;
    Ok(store.project_id.clone())
}
