//! Embedding-strategy benchmark matrix: chunking x field-composition x model.
//!
//! Answers, with a labeled offline corpus (`examples/data/embed_eval.json`):
//! 1. How much does chunk size (and chunking at all, vs truncated full text)
//!    affect retrieval quality?
//! 2. How much does the embedding model matter (int8/fp32 MiniLM, BGE-small,
//!    nomic-embed-text), and do retrieval-tuned models need their query
//!    instruction prefix to win?
//! 3. Does composing the embed text from more fields (title, tags, structured
//!    labels) — or embedding fields as separate vectors — improve recall?
//!
//! The harness mirrors the store's real behavior: documents become one or
//! more chunk vectors (`engramdb::embeddings::chunk_text`), queries are
//! embedded whole, and per-memory scores aggregate over chunk vectors
//! (max, like `LanceIndex::vector_search`; mean reported as an ablation).
//!
//! Run: `cargo run --release --example embed_matrix`
//! Env: `EMBED_EVAL_DATA` (dataset path), `EMBED_EVAL_OUT` (results JSON),
//!      `EMBED_EVAL_MODELS` (comma filter, e.g. "minilm-q,bge-small-q").

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use anyhow::{Context, Result};
use engramdb::embeddings::{
    chunk_text, EmbeddingProvider, OnnxModelSpec, OnnxProvider, ONNX_ALL_MINILM,
    ONNX_ALL_MINILM_Q, ONNX_BGE_SMALL_EN_Q, ONNX_NOMIC_EMBED_TEXT_Q,
};
use engramdb::onnx_ep::Backend;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Dataset
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Dataset {
    memories: Vec<Mem>,
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Mem {
    id: String,
    #[serde(rename = "type")]
    type_: String,
    title: String,
    summary: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    logical: Vec<String>,
}

#[derive(Deserialize)]
struct Query {
    id: String,
    text: String,
    archetype: String,
    /// memory id -> relevance grade (2 = primary target, 1 = partial).
    relevant: BTreeMap<String, u8>,
}

// ---------------------------------------------------------------------------
// Variants
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Comp {
    /// What the store embeds today: `"{summary} {content}"`.
    Baseline,
    /// Title prepended.
    PlusTitle,
    /// Title + tags prepended (tags as a keyword clause up front so they
    /// survive chunking).
    PlusTitleTags,
    /// Structured metadata label line, then title/summary/content.
    LabeledPrefix,
    /// Metadata ("{title}. {summary}. tags: ...") as its OWN vector alongside
    /// plain content chunks — multi-vector, no dilution of content text.
    FieldVectors,
    /// Contextual chunking: every content chunk carries the title+summary
    /// header, so later chunks keep the memory's global context.
    ContextChunks,
}

#[derive(Clone, Copy)]
struct DocVariant {
    name: &'static str,
    comp: Comp,
    /// None = no chunking: one embed call, model truncates at max_tokens.
    chunk_tokens: Option<usize>,
}

const DOC_VARIANTS: &[DocVariant] = &[
    // Chunk-size sweep on today's composition. c256 is the production config.
    DocVariant { name: "base_c256", comp: Comp::Baseline, chunk_tokens: Some(256) },
    DocVariant { name: "base_c192", comp: Comp::Baseline, chunk_tokens: Some(192) },
    DocVariant { name: "base_c128", comp: Comp::Baseline, chunk_tokens: Some(128) },
    DocVariant { name: "base_c64", comp: Comp::Baseline, chunk_tokens: Some(64) },
    // Full text at once (single truncated vector) — "no chunking" arm.
    DocVariant { name: "base_trunc", comp: Comp::Baseline, chunk_tokens: None },
    // Field-composition arms at the production chunk size.
    DocVariant { name: "title_c256", comp: Comp::PlusTitle, chunk_tokens: Some(256) },
    DocVariant { name: "title_tags_c256", comp: Comp::PlusTitleTags, chunk_tokens: Some(256) },
    DocVariant { name: "labeled_c256", comp: Comp::LabeledPrefix, chunk_tokens: Some(256) },
    DocVariant { name: "fieldvec_c256", comp: Comp::FieldVectors, chunk_tokens: Some(256) },
    DocVariant { name: "ctx_c256", comp: Comp::ContextChunks, chunk_tokens: Some(256) },
    DocVariant { name: "ctx_c128", comp: Comp::ContextChunks, chunk_tokens: Some(128) },
];

fn meta_header(m: &Mem) -> String {
    let mut s = format!("{}. {}", m.title, m.summary);
    if !m.tags.is_empty() {
        s.push_str(&format!(". tags: {}", m.tags.join(", ")));
    }
    s
}

/// Produce the chunk texts the store would hold for this memory under a
/// variant. `budget` already folds in the provider's real token limit.
fn doc_texts(m: &Mem, v: DocVariant, budget: usize) -> Vec<String> {
    let chunked = |text: &str| -> Vec<String> {
        match v.chunk_tokens {
            Some(_) => chunk_text(text, budget),
            None => vec![text.to_string()],
        }
    };
    match v.comp {
        Comp::Baseline => chunked(&format!("{} {}", m.summary, m.content)),
        Comp::PlusTitle => chunked(&format!("{}. {} {}", m.title, m.summary, m.content)),
        Comp::PlusTitleTags => {
            let tags = if m.tags.is_empty() {
                String::new()
            } else {
                format!(" tags: {}.", m.tags.join(", "))
            };
            chunked(&format!("{}.{} {} {}", m.title, tags, m.summary, m.content))
        }
        Comp::LabeledPrefix => chunked(&format!(
            "type: {} | scope: {} | tags: {}\n{}. {} {}",
            m.type_,
            m.logical.join(", "),
            m.tags.join(", "),
            m.title,
            m.summary,
            m.content
        )),
        Comp::FieldVectors => {
            let mut texts = vec![meta_header(m)];
            texts.extend(chunked(&m.content).into_iter().filter(|c| !c.is_empty()));
            texts
        }
        Comp::ContextChunks => {
            let header = format!("{}. {}. ", m.title, m.summary);
            let chunks = chunked(&m.content);
            if chunks.is_empty() {
                vec![header]
            } else {
                chunks.into_iter().map(|c| format!("{header}{c}")).collect()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

struct ModelUnderTest {
    key: &'static str,
    spec: OnnxModelSpec,
    /// Retrieval instruction prefix for the query side, if the model was
    /// trained with one (fastembed adds none itself).
    query_prefix: Option<&'static str>,
    /// Document-side prefix, if the model expects one (nomic).
    doc_prefix: Option<&'static str>,
}

const MODELS: &[ModelUnderTest] = &[
    ModelUnderTest {
        key: "minilm-q",
        spec: ONNX_ALL_MINILM_Q,
        query_prefix: None,
        doc_prefix: None,
    },
    ModelUnderTest {
        key: "minilm-fp32",
        spec: ONNX_ALL_MINILM,
        query_prefix: None,
        doc_prefix: None,
    },
    ModelUnderTest {
        key: "bge-small-q",
        spec: ONNX_BGE_SMALL_EN_Q,
        query_prefix: Some("Represent this sentence for searching relevant passages: "),
        doc_prefix: None,
    },
    ModelUnderTest {
        key: "nomic-q",
        spec: ONNX_NOMIC_EMBED_TEXT_Q,
        query_prefix: Some("search_query: "),
        doc_prefix: Some("search_document: "),
    },
];

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Default)]
struct Metrics {
    n: usize,
    p_at_1: f64,
    recall_at_5: f64,
    mrr_at_10: f64,
    ndcg_at_10: f64,
}

/// Per-query numbers, averaged later per archetype and overall.
struct QueryScore {
    archetype: String,
    p1: f64,
    r5: f64,
    mrr: f64,
    ndcg: f64,
    /// Rank (1-based) of the best grade-2 doc; usize::MAX if not in ranking.
    primary_rank: usize,
}

fn ndcg_at_k(ranked: &[&str], rels: &BTreeMap<String, u8>, k: usize) -> f64 {
    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, id)| {
            let g = *rels.get(*id).unwrap_or(&0) as f64;
            g / ((i + 2) as f64).log2()
        })
        .sum();
    let mut ideal: Vec<f64> = rels.values().map(|g| *g as f64).collect();
    ideal.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let idcg: f64 = ideal
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, g)| g / ((i + 2) as f64).log2())
        .sum();
    if idcg > 0.0 {
        dcg / idcg
    } else {
        0.0
    }
}

fn score_query(ranked: &[&str], q: &Query) -> QueryScore {
    let rels = &q.relevant;
    let p1 = ranked
        .first()
        .map(|id| f64::from(*rels.get(*id).unwrap_or(&0) >= 1))
        .unwrap_or(0.0);
    let total_rel = rels.values().filter(|g| **g >= 1).count();
    let hits5 = ranked
        .iter()
        .take(5)
        .filter(|id| *rels.get(**id).unwrap_or(&0) >= 1)
        .count();
    let r5 = if total_rel > 0 {
        hits5 as f64 / total_rel as f64
    } else {
        0.0
    };
    let mrr = ranked
        .iter()
        .take(10)
        .position(|id| *rels.get(*id).unwrap_or(&0) == 2)
        .map(|i| 1.0 / (i + 1) as f64)
        .unwrap_or(0.0);
    let primary_rank = ranked
        .iter()
        .position(|id| *rels.get(*id).unwrap_or(&0) == 2)
        .map(|i| i + 1)
        .unwrap_or(usize::MAX);
    QueryScore {
        archetype: q.archetype.clone(),
        p1,
        r5,
        mrr,
        ndcg: ndcg_at_k(ranked, rels, 10),
        primary_rank,
    }
}

fn aggregate(scores: &[&QueryScore]) -> Metrics {
    let n = scores.len();
    if n == 0 {
        return Metrics::default();
    }
    let nf = n as f64;
    Metrics {
        n,
        p_at_1: scores.iter().map(|s| s.p1).sum::<f64>() / nf,
        recall_at_5: scores.iter().map(|s| s.r5).sum::<f64>() / nf,
        mrr_at_10: scores.iter().map(|s| s.mrr).sum::<f64>() / nf,
        ndcg_at_10: scores.iter().map(|s| s.ndcg).sum::<f64>() / nf,
    }
}

// ---------------------------------------------------------------------------
// Embedding cache
// ---------------------------------------------------------------------------

struct EmbedCache {
    map: HashMap<String, Vec<f32>>,
    total_texts: usize,
    total_secs: f64,
}

impl EmbedCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            total_texts: 0,
            total_secs: 0.0,
        }
    }

    async fn ensure(&mut self, provider: &OnnxProvider, texts: &[String]) -> Result<()> {
        let missing: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            texts
                .iter()
                .filter(|t| !self.map.contains_key(*t) && seen.insert(t.as_str()))
                .cloned()
                .collect()
        };
        for batch in missing.chunks(32) {
            let refs: Vec<&str> = batch.iter().map(|s| s.as_str()).collect();
            let t = Instant::now();
            let vecs = provider.embed_batch(&refs).await?;
            self.total_secs += t.elapsed().as_secs_f64();
            self.total_texts += batch.len();
            for (text, mut v) in batch.iter().zip(vecs) {
                // Normalize so dot product == cosine; ranking then matches the
                // store's L2-based 1/(1+d) ordering on unit vectors.
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    v.iter_mut().for_each(|x| *x /= norm);
                }
                self.map.insert(text.clone(), v);
            }
        }
        Ok(())
    }

    fn get(&self, text: &str) -> &[f32] {
        &self.map[text]
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x * y) as f64).sum()
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct VariantResult {
    overall: Metrics,
    by_archetype: BTreeMap<String, Metrics>,
    /// Queries whose primary-target rank improved / worsened vs base_c256
    /// (same model, agg=max, no query prefix).
    wins_vs_baseline: usize,
    losses_vs_baseline: usize,
}

#[derive(Serialize)]
struct ModelReport {
    model_id: String,
    dimensions: usize,
    texts_embedded: usize,
    embed_secs_total: f64,
    ms_per_text: f64,
    /// variant -> agg -> query_mode -> result
    results: BTreeMap<String, BTreeMap<String, BTreeMap<String, VariantResult>>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let data_path = std::env::var("EMBED_EVAL_DATA")
        .unwrap_or_else(|_| "examples/data/embed_eval.json".into());
    let out_path = std::env::var("EMBED_EVAL_OUT")
        .unwrap_or_else(|_| "target/embed_matrix_results.json".into());
    let model_filter: Option<Vec<String>> = std::env::var("EMBED_EVAL_MODELS")
        .ok()
        .map(|s| s.split(',').map(|m| m.trim().to_string()).collect());

    let raw = std::fs::read_to_string(&data_path)
        .with_context(|| format!("reading dataset {data_path}"))?;
    let ds: Dataset = serde_json::from_str(&raw).context("parsing dataset")?;
    println!(
        "dataset: {} memories, {} queries",
        ds.memories.len(),
        ds.queries.len()
    );

    let mut reports: BTreeMap<String, ModelReport> = BTreeMap::new();

    for mut_ in MODELS {
        if let Some(filter) = &model_filter {
            if !filter.iter().any(|f| f == mut_.key) {
                continue;
            }
        }
        println!("\n=== model {} ===", mut_.key);
        let provider = match OnnxProvider::with_model_on(mut_.spec, Backend::Cpu) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skipping {}: {e:#}", mut_.key);
                continue;
            }
        };
        let mut cache = EmbedCache::new();

        // Document texts per (variant, doc_mode). doc_mode "raw" always runs;
        // "prefixed" only for models with a documented doc-side prefix.
        let mut doc_modes: Vec<&'static str> = vec!["raw"];
        if mut_.doc_prefix.is_some() {
            doc_modes.push("prefixed");
        }
        // texts[variant][doc_mode][mem_idx] = chunk texts
        let mut texts: HashMap<(&str, &str), Vec<Vec<String>>> = HashMap::new();
        for v in DOC_VARIANTS {
            let budget = v
                .chunk_tokens
                .unwrap_or(mut_.spec.max_tokens)
                .min(mut_.spec.max_tokens);
            for dm in &doc_modes {
                let per_mem: Vec<Vec<String>> = ds
                    .memories
                    .iter()
                    .map(|m| {
                        doc_texts(m, *v, budget)
                            .into_iter()
                            .map(|t| {
                                if *dm == "prefixed" {
                                    format!("{}{}", mut_.doc_prefix.unwrap(), t)
                                } else {
                                    t
                                }
                            })
                            .collect()
                    })
                    .collect();
                texts.insert((v.name, dm), per_mem);
            }
        }
        // Query texts per query_mode.
        let mut query_modes: Vec<&'static str> = vec!["raw"];
        if mut_.query_prefix.is_some() {
            query_modes.push("prefixed");
        }
        let query_texts: HashMap<&str, Vec<String>> = query_modes
            .iter()
            .map(|qm| {
                let list = ds
                    .queries
                    .iter()
                    .map(|q| {
                        if *qm == "prefixed" {
                            format!("{}{}", mut_.query_prefix.unwrap(), q.text)
                        } else {
                            q.text.clone()
                        }
                    })
                    .collect();
                (*qm, list)
            })
            .collect();

        // Embed everything once (cache dedupes across variants).
        let mut all: Vec<String> = Vec::new();
        for per_mem in texts.values() {
            for chunks in per_mem {
                all.extend(chunks.iter().cloned());
            }
        }
        for list in query_texts.values() {
            all.extend(list.iter().cloned());
        }
        cache.ensure(&provider, &all).await?;
        println!(
            "embedded {} unique texts in {:.1}s ({:.1} ms/text)",
            cache.total_texts,
            cache.total_secs,
            1000.0 * cache.total_secs / cache.total_texts.max(1) as f64
        );

        // Score. Doc mode pairs with query mode for nomic ("prefixed" doc
        // side only ever scores against "prefixed" queries).
        let mut results: BTreeMap<String, BTreeMap<String, BTreeMap<String, VariantResult>>> =
            BTreeMap::new();
        // Baseline primary ranks for win/loss: base_c256 / max / raw.
        let mut baseline_ranks: HashMap<&str, usize> = HashMap::new();

        for v in DOC_VARIANTS {
            for qm in &query_modes {
                let dm = if *qm == "prefixed" && mut_.doc_prefix.is_some() {
                    "prefixed"
                } else {
                    "raw"
                };
                let per_mem = &texts[&(v.name, dm)];
                for agg in ["max", "mean"] {
                    let mut qscores: Vec<QueryScore> = Vec::new();
                    for (qi, q) in ds.queries.iter().enumerate() {
                        let qv = cache.get(&query_texts[qm][qi]);
                        let mut scored: Vec<(&str, f64)> = ds
                            .memories
                            .iter()
                            .zip(per_mem)
                            .map(|(m, chunks)| {
                                let sims: Vec<f64> =
                                    chunks.iter().map(|c| cosine(qv, cache.get(c))).collect();
                                let s = if sims.is_empty() {
                                    f64::MIN
                                } else if agg == "max" {
                                    sims.iter().cloned().fold(f64::MIN, f64::max)
                                } else {
                                    sims.iter().sum::<f64>() / sims.len() as f64
                                };
                                (m.id.as_str(), s)
                            })
                            .collect();
                        scored.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap().then_with(|| a.0.cmp(b.0))
                        });
                        let ranked: Vec<&str> = scored.iter().map(|(id, _)| *id).collect();
                        qscores.push(score_query(&ranked, q));
                    }

                    if v.name == "base_c256" && *qm == "raw" && agg == "max" {
                        for (q, s) in ds.queries.iter().zip(&qscores) {
                            baseline_ranks.insert(q.id.as_str(), s.primary_rank);
                        }
                    }

                    let mut by_arch: BTreeMap<String, Metrics> = BTreeMap::new();
                    let mut arch_groups: BTreeMap<&str, Vec<&QueryScore>> = BTreeMap::new();
                    for s in &qscores {
                        arch_groups.entry(s.archetype.as_str()).or_default().push(s);
                    }
                    for (arch, group) in &arch_groups {
                        by_arch.insert((*arch).to_string(), aggregate(group));
                    }
                    let all_refs: Vec<&QueryScore> = qscores.iter().collect();
                    let (mut wins, mut losses) = (0usize, 0usize);
                    for (q, s) in ds.queries.iter().zip(&qscores) {
                        if let Some(base) = baseline_ranks.get(q.id.as_str()) {
                            if s.primary_rank < *base {
                                wins += 1;
                            } else if s.primary_rank > *base {
                                losses += 1;
                            }
                        }
                    }
                    results
                        .entry(v.name.to_string())
                        .or_default()
                        .entry(agg.to_string())
                        .or_default()
                        .insert(
                            (*qm).to_string(),
                            VariantResult {
                                overall: aggregate(&all_refs),
                                by_archetype: by_arch,
                                wins_vs_baseline: wins,
                                losses_vs_baseline: losses,
                            },
                        );
                }
            }
        }

        // Compact stdout table: variant x (max/raw) overall metrics.
        println!(
            "{:<18} {:>6} {:>6} {:>6} {:>6}  {:>9}",
            "variant(max,raw)", "P@1", "R@5", "MRR", "nDCG", "win/loss"
        );
        for v in DOC_VARIANTS {
            if let Some(r) = results
                .get(v.name)
                .and_then(|a| a.get("max"))
                .and_then(|m| m.get("raw"))
            {
                println!(
                    "{:<18} {:>6.3} {:>6.3} {:>6.3} {:>6.3}  {:>4}/{:<4}",
                    v.name,
                    r.overall.p_at_1,
                    r.overall.recall_at_5,
                    r.overall.mrr_at_10,
                    r.overall.ndcg_at_10,
                    r.wins_vs_baseline,
                    r.losses_vs_baseline
                );
            }
        }
        if query_modes.contains(&"prefixed") {
            println!("-- with query prefix --");
            for v in ["base_c256", "title_tags_c256", "fieldvec_c256", "ctx_c256"] {
                if let Some(r) = results
                    .get(v)
                    .and_then(|a| a.get("max"))
                    .and_then(|m| m.get("prefixed"))
                {
                    println!(
                        "{:<18} {:>6.3} {:>6.3} {:>6.3} {:>6.3}  {:>4}/{:<4}",
                        v,
                        r.overall.p_at_1,
                        r.overall.recall_at_5,
                        r.overall.mrr_at_10,
                        r.overall.ndcg_at_10,
                        r.wins_vs_baseline,
                        r.losses_vs_baseline
                    );
                }
            }
        }

        reports.insert(
            mut_.key.to_string(),
            ModelReport {
                model_id: provider.model_id(),
                dimensions: provider.dimensions(),
                texts_embedded: cache.total_texts,
                embed_secs_total: cache.total_secs,
                ms_per_text: 1000.0 * cache.total_secs / cache.total_texts.max(1) as f64,
                results,
            },
        );
    }

    let json = serde_json::to_string_pretty(&reports)?;
    std::fs::write(&out_path, &json).with_context(|| format!("writing {out_path}"))?;
    println!("\nresults written to {out_path}");
    Ok(())
}
