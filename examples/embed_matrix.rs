//! Embedding-strategy benchmark matrix: chunking x field-composition x model.
//!
//! Executes the experiment plan from the embedding-quality research review:
//! - E1 field composition: does embedding title/tags/labels (today only
//!   `"{summary} {content}"` is embedded) fix the title-echo/tag-only blind
//!   spot — and is a separate metadata vector better than concatenation?
//! - E2 model swap + prefixes: retrieval-tuned bge-small-en-v1.5 / nomic
//!   vs the MiniLM default, with and without their instruction prefixes
//!   (fastembed adds none; EngramDB adds none today).
//! - E3 token-budget overflow: true-token counts of the word-count chunker's
//!   output (code-dense text overflows the 256-token model limit silently).
//! - E4 chunk-budget sweep: 64..256-token budgets vs no chunking at all.
//! - E5 boundary defects: word overlap, sentence-aware packing, runt-merge,
//!   scored on planted facts at start/straddle/end of long memories.
//! - E6 aggregation: max (production) vs mean vs top-2 mean over chunks.
//!
//! The harness mirrors the store's real behavior: documents become one or
//! more chunk vectors (`engramdb::embeddings::chunk_text` or an experimental
//! variant), queries are embedded whole, and per-memory scores aggregate
//! over chunk vectors (max, like `LanceIndex::vector_search`; vectors are
//! L2-normalized so cosine ordering matches the store's `1/(1+L2)`).
//!
//! Run: `cargo run --release --example embed_matrix`
//! Env: `EMBED_EVAL_DATA` (dataset path), `EMBED_EVAL_OUT` (results JSON),
//!      `EMBED_EVAL_MODELS` (comma filter, e.g. "minilm-q,bge-small-q").

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use anyhow::{Context, Result};
use engramdb::embeddings::{
    chunk_text, EmbeddingProvider, OnnxModelSpec, OnnxProvider, ONNX_ALL_MINILM, ONNX_ALL_MINILM_Q,
    ONNX_BGE_SMALL_EN_Q, ONNX_NOMIC_EMBED_TEXT_Q,
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
    /// For long memories with a planted fact: where it sits relative to the
    /// 192-word chunk boundary ("start" | "straddle" | "end").
    #[serde(default)]
    fact_pos: Option<String>,
    /// Heavy in code identifiers/paths/env vars (token-overflow stratum).
    #[serde(default)]
    code_dense: bool,
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
// Chunker variants (E4/E5)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Chunker {
    /// Production `chunk_text`: fixed word blocks, no overlap.
    Fixed,
    /// Fixed blocks with N words of overlap between consecutive chunks.
    Overlap(usize),
    /// Greedy sentence packing up to the word budget.
    Sentence,
    /// Fixed blocks, then merge a trailing runt (<32 words) into its
    /// predecessor.
    RuntMerge,
    /// No chunking: one embed call, the model truncates at max_tokens.
    None,
}

fn budget_words(budget_tokens: usize) -> usize {
    (budget_tokens * 3 / 4).max(1)
}

fn chunk_overlap(text: &str, budget_tokens: usize, overlap: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let max_words = budget_words(budget_tokens);
    if words.len() <= max_words {
        return vec![words.join(" ")];
    }
    let stride = max_words.saturating_sub(overlap).max(1);
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let end = (i + max_words).min(words.len());
        out.push(words[i..end].join(" "));
        if end == words.len() {
            break;
        }
        i += stride;
    }
    out
}

fn chunk_sentences(text: &str, budget_tokens: usize) -> Vec<String> {
    let max_words = budget_words(budget_tokens);
    // Sentence terminators; keeps it dependency-free. A "sentence" longer
    // than the budget falls back to word splitting.
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    for part in text.split_inclusive(['.', '!', '?', '\n']) {
        cur.push_str(part);
        if part.ends_with(['.', '!', '?', '\n']) {
            let s = cur.trim();
            if !s.is_empty() {
                sentences.push(s.to_string());
            }
            cur = String::new();
        }
    }
    let tail = cur.trim();
    if !tail.is_empty() {
        sentences.push(tail.to_string());
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut buf: Vec<String> = Vec::new();
    let mut buf_words = 0usize;
    let flush = |buf: &mut Vec<String>, buf_words: &mut usize, chunks: &mut Vec<String>| {
        if !buf.is_empty() {
            chunks.push(buf.join(" "));
            buf.clear();
            *buf_words = 0;
        }
    };
    for s in sentences {
        let w = s.split_whitespace().count();
        if w > max_words {
            flush(&mut buf, &mut buf_words, &mut chunks);
            chunks.extend(chunk_text(&s, budget_tokens));
            continue;
        }
        if buf_words + w > max_words {
            flush(&mut buf, &mut buf_words, &mut chunks);
        }
        buf_words += w;
        buf.push(s);
    }
    flush(&mut buf, &mut buf_words, &mut chunks);
    chunks
}

fn runt_merge(mut chunks: Vec<String>) -> Vec<String> {
    if chunks.len() >= 2 {
        let last_words = chunks.last().map(|c| c.split_whitespace().count());
        if let Some(w) = last_words {
            if w < 32 {
                let runt = chunks.pop().unwrap_or_default();
                if let Some(prev) = chunks.last_mut() {
                    prev.push(' ');
                    prev.push_str(&runt);
                }
            }
        }
    }
    chunks
}

fn run_chunker(text: &str, chunker: Chunker, budget_tokens: usize) -> Vec<String> {
    match chunker {
        Chunker::Fixed => chunk_text(text, budget_tokens),
        Chunker::Overlap(n) => chunk_overlap(text, budget_tokens, n),
        Chunker::Sentence => chunk_sentences(text, budget_tokens),
        Chunker::RuntMerge => runt_merge(chunk_text(text, budget_tokens)),
        Chunker::None => {
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![text.to_string()]
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Composition variants (E1)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Comp {
    /// What the store embeds today: `"{summary} {content}"`.
    Baseline,
    /// Title prepended.
    PlusTitle,
    /// Title + tags prepended (up front so they survive chunking).
    PlusTitleTags,
    /// Structured metadata label line (negative control per the plan).
    LabeledPrefix,
    /// Metadata as its OWN vector alongside plain content chunks.
    FieldVectors,
    /// Every content chunk carries the title+summary header.
    ContextChunks,
}

#[derive(Clone, Copy)]
struct DocVariant {
    name: &'static str,
    comp: Comp,
    chunker: Chunker,
    chunk_tokens: usize,
}

const DOC_VARIANTS: &[DocVariant] = &[
    // E4: chunk-budget sweep on today's composition. c256 = production.
    DocVariant {
        name: "base_c256",
        comp: Comp::Baseline,
        chunker: Chunker::Fixed,
        chunk_tokens: 256,
    },
    DocVariant {
        name: "base_c192",
        comp: Comp::Baseline,
        chunker: Chunker::Fixed,
        chunk_tokens: 192,
    },
    DocVariant {
        name: "base_c128",
        comp: Comp::Baseline,
        chunker: Chunker::Fixed,
        chunk_tokens: 128,
    },
    DocVariant {
        name: "base_c96",
        comp: Comp::Baseline,
        chunker: Chunker::Fixed,
        chunk_tokens: 96,
    },
    DocVariant {
        name: "base_c64",
        comp: Comp::Baseline,
        chunker: Chunker::Fixed,
        chunk_tokens: 64,
    },
    DocVariant {
        name: "base_trunc",
        comp: Comp::Baseline,
        chunker: Chunker::None,
        chunk_tokens: 256,
    },
    // E5: chunker-structure variants at the production budget.
    DocVariant {
        name: "base_ov24_c256",
        comp: Comp::Baseline,
        chunker: Chunker::Overlap(24),
        chunk_tokens: 256,
    },
    DocVariant {
        name: "base_ov48_c256",
        comp: Comp::Baseline,
        chunker: Chunker::Overlap(48),
        chunk_tokens: 256,
    },
    DocVariant {
        name: "base_sent_c256",
        comp: Comp::Baseline,
        chunker: Chunker::Sentence,
        chunk_tokens: 256,
    },
    DocVariant {
        name: "base_runt_c256",
        comp: Comp::Baseline,
        chunker: Chunker::RuntMerge,
        chunk_tokens: 256,
    },
    // E1: field-composition arms at the production chunk size.
    DocVariant {
        name: "title_c256",
        comp: Comp::PlusTitle,
        chunker: Chunker::Fixed,
        chunk_tokens: 256,
    },
    DocVariant {
        name: "title_tags_c256",
        comp: Comp::PlusTitleTags,
        chunker: Chunker::Fixed,
        chunk_tokens: 256,
    },
    DocVariant {
        name: "labeled_c256",
        comp: Comp::LabeledPrefix,
        chunker: Chunker::Fixed,
        chunk_tokens: 256,
    },
    DocVariant {
        name: "fieldvec_c256",
        comp: Comp::FieldVectors,
        chunker: Chunker::Fixed,
        chunk_tokens: 256,
    },
    DocVariant {
        name: "ctx_c256",
        comp: Comp::ContextChunks,
        chunker: Chunker::Fixed,
        chunk_tokens: 256,
    },
    DocVariant {
        name: "ctx_c128",
        comp: Comp::ContextChunks,
        chunker: Chunker::Fixed,
        chunk_tokens: 128,
    },
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
    let chunked = |text: &str| run_chunker(text, v.chunker, budget);
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
// Models (E2)
// ---------------------------------------------------------------------------

const BGE_QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

struct ModelUnderTest {
    key: &'static str,
    spec: OnnxModelSpec,
    /// Retrieval instruction prefix for the query side, if tested for this
    /// model (fastembed adds none itself). For minilm-q this is the BGE
    /// instruction as a NEGATIVE CONTROL — MiniLM was not trained with it.
    query_prefix: Option<&'static str>,
    /// Document-side prefix, if the model expects one (nomic).
    doc_prefix: Option<&'static str>,
}

const MODELS: &[ModelUnderTest] = &[
    ModelUnderTest {
        key: "minilm-q",
        spec: ONNX_ALL_MINILM_Q,
        query_prefix: Some(BGE_QUERY_PREFIX), // negative control
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
        query_prefix: Some(BGE_QUERY_PREFIX),
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
    groups: Vec<String>,
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

fn score_query(ranked: &[&str], q: &Query, groups: Vec<String>) -> QueryScore {
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
        hits5 as f64 / total_rel.min(5) as f64
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
        groups,
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
// E3 stage 1: true-token overflow measurement
// ---------------------------------------------------------------------------

#[derive(Serialize, Default)]
struct OverflowBucket {
    chunks: usize,
    over_limit: usize,
    mean_tokens: f64,
    max_tokens: usize,
}

#[derive(Serialize)]
struct OverflowReport {
    token_limit: usize,
    all: OverflowBucket,
    code_dense: OverflowBucket,
    prose: OverflowBucket,
}

fn measure_overflow(ds: &Dataset, token_limit: usize) -> Option<OverflowReport> {
    let cache = engramdb::storage::paths::model_cache_dir().ok()?;
    let tok_path = cache
        .join("models--Xenova--all-MiniLM-L6-v2")
        .join("snapshots")
        .join("main")
        .join("tokenizer.json");
    let mut tokenizer = tokenizers::Tokenizer::from_file(&tok_path).ok()?;
    // The Xenova export ships a baked-in truncation config; disable it so we
    // count TRUE token lengths (fastembed's own truncation is what we're
    // measuring the chunker against).
    tokenizer.with_truncation(None).ok()?;

    let mut all = OverflowBucket::default();
    let mut code = OverflowBucket::default();
    let mut prose = OverflowBucket::default();
    let mut all_sum = 0usize;
    let mut code_sum = 0usize;
    let mut prose_sum = 0usize;

    for m in &ds.memories {
        let text = format!("{} {}", m.summary, m.content);
        for chunk in chunk_text(&text, token_limit) {
            // +2 for the [CLS]/[SEP] specials fastembed adds.
            let n = tokenizer
                .encode(chunk.as_str(), false)
                .map(|e| e.len() + 2)
                .unwrap_or(0);
            for (bucket, sum) in [(&mut all, &mut all_sum)]
                .into_iter()
                .chain(std::iter::once(if m.code_dense {
                    (&mut code, &mut code_sum)
                } else {
                    (&mut prose, &mut prose_sum)
                }))
            {
                bucket.chunks += 1;
                bucket.over_limit += usize::from(n > token_limit);
                bucket.max_tokens = bucket.max_tokens.max(n);
                *sum += n;
            }
        }
    }
    for (bucket, sum) in [
        (&mut all, all_sum),
        (&mut code, code_sum),
        (&mut prose, prose_sum),
    ] {
        bucket.mean_tokens = sum as f64 / bucket.chunks.max(1) as f64;
    }
    Some(OverflowReport {
        token_limit,
        all,
        code_dense: code,
        prose,
    })
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
            let mut list: Vec<String> = texts
                .iter()
                .filter(|t| !self.map.contains_key(*t) && seen.insert(t.as_str()))
                .cloned()
                .collect();
            // Deterministic batch composition: callers collect texts from
            // HashMaps, and batch membership perturbs dynamically-quantized
            // int8 outputs slightly — sorted order makes runs reproducible.
            list.sort();
            list
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
    by_group: BTreeMap<String, Metrics>,
    /// Queries whose primary-target rank improved / worsened vs base_c256
    /// (same model, agg=max, no query prefix).
    wins_vs_baseline: usize,
    losses_vs_baseline: usize,
    /// Index-cost proxy: total chunk vectors across the corpus.
    total_chunks: usize,
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

#[derive(Serialize)]
struct FullReport {
    overflow: Option<OverflowReport>,
    models: BTreeMap<String, ModelReport>,
}

const AGGS: &[&str] = &["max", "mean", "top2"];

fn agg_score(sims: &[f64], agg: &str) -> f64 {
    if sims.is_empty() {
        return f64::MIN;
    }
    match agg {
        "max" => sims.iter().cloned().fold(f64::MIN, f64::max),
        "mean" => sims.iter().sum::<f64>() / sims.len() as f64,
        _ => {
            // top2: mean of the two best (single chunk falls back to itself).
            let mut s = sims.to_vec();
            s.sort_by(|a, b| b.partial_cmp(a).unwrap());
            let k = s.len().min(2);
            s[..k].iter().sum::<f64>() / k as f64
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let data_path =
        std::env::var("EMBED_EVAL_DATA").unwrap_or_else(|_| "examples/data/embed_eval.json".into());
    let out_path = std::env::var("EMBED_EVAL_OUT")
        .unwrap_or_else(|_| "target/embed_matrix_results.json".into());
    let model_filter: Option<Vec<String>> = std::env::var("EMBED_EVAL_MODELS")
        .ok()
        .map(|s| s.split(',').map(|m| m.trim().to_string()).collect());

    let raw = std::fs::read_to_string(&data_path)
        .with_context(|| format!("reading dataset {data_path}"))?;
    let ds: Dataset = serde_json::from_str(&raw).context("parsing dataset")?;
    let mem_by_id: HashMap<&str, &Mem> = ds.memories.iter().map(|m| (m.id.as_str(), m)).collect();
    println!(
        "dataset: {} memories, {} queries",
        ds.memories.len(),
        ds.queries.len()
    );

    // Grouping keys per query: archetype, plus fact-position and code-dense
    // sub-buckets so E3/E5 can read their strata directly.
    let query_groups: Vec<Vec<String>> = ds
        .queries
        .iter()
        .map(|q| {
            let mut groups = vec![q.archetype.clone()];
            if let Some(target) = q
                .relevant
                .iter()
                .find(|(_, g)| **g == 2)
                .and_then(|(id, _)| mem_by_id.get(id.as_str()))
            {
                if let Some(pos) = &target.fact_pos {
                    groups.push(format!("factpos:{pos}"));
                }
                if target.code_dense {
                    groups.push("code_dense".to_string());
                }
            }
            groups
        })
        .collect();

    let overflow = measure_overflow(&ds, 256);
    if let Some(o) = &overflow {
        println!(
            "E3 overflow (MiniLM tokenizer, limit {}): all {}/{} chunks over (mean {:.0} tok, max {}), code-dense {}/{}, prose {}/{}",
            o.token_limit,
            o.all.over_limit,
            o.all.chunks,
            o.all.mean_tokens,
            o.all.max_tokens,
            o.code_dense.over_limit,
            o.code_dense.chunks,
            o.prose.over_limit,
            o.prose.chunks
        );
    } else {
        println!("E3 overflow: MiniLM tokenizer not cached; skipping");
    }

    let mut reports: BTreeMap<String, ModelReport> = BTreeMap::new();

    for mut_ in MODELS {
        if let Some(filter) = &model_filter {
            if !filter.iter().any(|f| f == mut_.key) {
                continue;
            }
        }
        println!("\n=== model {} ===", mut_.key);
        let provider = match OnnxProvider::with_model_on(mut_.spec.clone(), Backend::Cpu) {
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
        let mut texts: HashMap<(&str, &str), Vec<Vec<String>>> = HashMap::new();
        for v in DOC_VARIANTS {
            let budget = v.chunk_tokens.min(mut_.spec.max_tokens);
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
                let total_chunks: usize = per_mem.iter().map(|c| c.len()).sum();
                // Chunk similarities are shared across aggregations; compute
                // once per (variant, query).
                for agg in AGGS {
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
                                (m.id.as_str(), agg_score(&sims, agg))
                            })
                            .collect();
                        scored.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap().then_with(|| a.0.cmp(b.0))
                        });
                        let ranked: Vec<&str> = scored.iter().map(|(id, _)| *id).collect();
                        qscores.push(score_query(&ranked, q, query_groups[qi].clone()));
                    }

                    if v.name == "base_c256" && *qm == "raw" && *agg == "max" {
                        for (q, s) in ds.queries.iter().zip(&qscores) {
                            baseline_ranks.insert(q.id.as_str(), s.primary_rank);
                        }
                    }

                    let mut group_map: BTreeMap<&str, Vec<&QueryScore>> = BTreeMap::new();
                    for s in &qscores {
                        for g in &s.groups {
                            group_map.entry(g.as_str()).or_default().push(s);
                        }
                    }
                    let by_group = group_map
                        .iter()
                        .map(|(g, list)| ((*g).to_string(), aggregate(list)))
                        .collect();
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
                        .entry((*agg).to_string())
                        .or_default()
                        .insert(
                            (*qm).to_string(),
                            VariantResult {
                                overall: aggregate(&all_refs),
                                by_group,
                                wins_vs_baseline: wins,
                                losses_vs_baseline: losses,
                                total_chunks,
                            },
                        );
                }
            }
        }

        // Compact stdout table: variant (max agg, raw queries).
        println!(
            "{:<18} {:>6} {:>6} {:>6} {:>6}  {:>9} {:>7}",
            "variant(max,raw)", "P@1", "R@5", "MRR", "nDCG", "win/loss", "chunks"
        );
        for v in DOC_VARIANTS {
            if let Some(r) = results
                .get(v.name)
                .and_then(|a| a.get("max"))
                .and_then(|m| m.get("raw"))
            {
                println!(
                    "{:<18} {:>6.3} {:>6.3} {:>6.3} {:>6.3}  {:>4}/{:<4} {:>7}",
                    v.name,
                    r.overall.p_at_1,
                    r.overall.recall_at_5,
                    r.overall.mrr_at_10,
                    r.overall.ndcg_at_10,
                    r.wins_vs_baseline,
                    r.losses_vs_baseline,
                    r.total_chunks
                );
            }
        }
        if query_modes.contains(&"prefixed") {
            println!("-- with query prefix ({}) --", mut_.key);
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

    let report = FullReport {
        overflow,
        models: reports,
    };
    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&out_path, &json).with_context(|| format!("writing {out_path}"))?;
    println!("\nresults written to {out_path}");
    Ok(())
}
