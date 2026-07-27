//! Retrieval-quality comparison: the current in-Rust keyword scorer vs
//! LanceDB's BM25 full-text search.
//!
//! ## Why this exists
//!
//! The composite score is `0.45*keyword + 0.30*semantic + 0.25*relevance`, so
//! the keyword term carries the largest single weight — more than semantic.
//! Its current implementation is a weighted count of distinct query tokens
//! found per field (summary 3x, tags 2x, content 1x) with no notion of which
//! terms are informative. That is cheap and predictable, but it has known
//! blind spots: no IDF, no stemming, no fuzzy, no phrases.
//!
//! An earlier benchmark (`benches/index_leverage.rs`) measured the *speed* of
//! the keyword scorer and found it costs ~0.6ms on a realistic candidate set.
//! Speed was the wrong question: FTS is a quality proposition, not a
//! throughput one. This harness asks the right one.
//!
//! ## What is compared
//!
//! An ablation ladder — each arm adds exactly ONE thing to the one before, so
//! a movement is attributable to the feature that caused it:
//!
//! * **A — current**: `engramdb::search::keyword_search` over loaded memories.
//! * **B — + BM25 scoring**: a LanceDB inverted index over `summary` / `tags` /
//!   `content` with stemming OFF, queried as a boolean should-query whose three
//!   clauses carry the same 3/2/1 field boosts. Only the term-weighting model
//!   changes (IDF, term-frequency saturation, length normalisation), so this
//!   arm isolates BM25's *scoring* from its tokenizer.
//! * **C — + stemming**: as B with `stem(true)`.
//! * **D — + fuzzy**: as C with AUTO fuzziness.
//!
//! Metrics are Recall@1, Recall@5 and MRR, reported per probe category so a
//! win can be attributed to a mechanism instead of being a single average that
//! hides which cases moved.
//!
//! Every BM25 arm builds a **standalone** Lance table in a temp dir. The
//! production memories table has no `content` column and this deliberately
//! does not add one — schema work should follow evidence, not precede it.
//!
//! Run: `cargo run --release --example fts_quality`

#[path = "fts_corpus.rs"]
mod fts_corpus;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures_util::StreamExt;
use lancedb::index::scalar::{
    BooleanQuery, FtsIndexBuilder, FullTextSearchQuery, MatchQuery, Occur,
};
use lancedb::index::Index;
use lancedb::query::{ExecutableQuery, QueryBase};

use engramdb::search::keyword_search;
use engramdb::types::{Memory, MemoryType, Provenance};

use fts_corpus::{Probe, CORPUS, SCENARIOS};

/// Field boosts, mirroring the current scorer's 3x / 2x / 1x weighting so the
/// comparison isolates the term-weighting model rather than the field priors.
const BOOST_SUMMARY: f32 = 3.0;
const BOOST_TAGS: f32 = 2.0;
const BOOST_CONTENT: f32 = 1.0;

const TOP_K: usize = 5;

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct Metrics {
    n: usize,
    hit_at_1: usize,
    hit_at_5: usize,
    reciprocal_rank_sum: f64,
}

impl Metrics {
    /// Record one scenario given the ranked ids the system returned.
    fn record(&mut self, ranked: &[String], relevant: &[&str]) {
        self.n += 1;
        // Rank (1-based) of the first relevant id, if any made the cut.
        let rank = ranked
            .iter()
            .position(|id| relevant.contains(&id.as_str()))
            .map(|i| i + 1);
        if let Some(r) = rank {
            if r == 1 {
                self.hit_at_1 += 1;
            }
            if r <= TOP_K {
                self.hit_at_5 += 1;
            }
            self.reciprocal_rank_sum += 1.0 / r as f64;
        }
    }

    fn recall_at_1(&self) -> f64 {
        self.hit_at_1 as f64 / self.n.max(1) as f64
    }
    fn recall_at_5(&self) -> f64 {
        self.hit_at_5 as f64 / self.n.max(1) as f64
    }
    fn mrr(&self) -> f64 {
        self.reciprocal_rank_sum / self.n.max(1) as f64
    }
}

// ---------------------------------------------------------------------------
// System A — the current in-Rust keyword scorer
// ---------------------------------------------------------------------------

fn build_memories() -> Vec<Memory> {
    CORPUS
        .iter()
        .map(|(id, summary, tags, content)| {
            let mut m = Memory::new(MemoryType::Context, *summary, *content, Provenance::human());
            m.id = (*id).to_string();
            m.tags = tags.iter().map(|t| (*t).to_string()).collect();
            m
        })
        .collect()
}

fn rank_current(query: &str, memories: &[Memory]) -> Vec<String> {
    keyword_search(query, memories)
        .into_iter()
        .take(TOP_K)
        .map(|(idx, _score)| memories[idx].id.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// System B/C — LanceDB BM25
// ---------------------------------------------------------------------------

async fn build_fts_table(dir: &std::path::Path, stem: bool) -> Result<lancedb::Table> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("summary", DataType::Utf8, false),
        Field::new("tags", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
    ]));

    let ids: Vec<&str> = CORPUS.iter().map(|(id, ..)| *id).collect();
    let summaries: Vec<&str> = CORPUS.iter().map(|(_, s, ..)| *s).collect();
    // Tags are joined with spaces rather than kept as JSON: this table is a
    // search index, so the tokenizer should see bare words.
    let tags: Vec<String> = CORPUS.iter().map(|(_, _, t, _)| t.join(" ")).collect();
    let contents: Vec<&str> = CORPUS.iter().map(|(.., c)| *c).collect();

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(StringArray::from(summaries)),
            Arc::new(StringArray::from(tags)),
            Arc::new(StringArray::from(contents)),
        ],
    )
    .context("failed to build corpus batch")?;

    let conn = lancedb::connect(dir.to_str().context("non-utf8 temp dir")?)
        .execute()
        .await?;
    let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
    let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(batches);
    let table = conn
        .create_table("corpus", reader)
        .execute()
        .await
        .context("failed to create corpus table")?;

    // Stemming and stop-word removal are the two mechanisms the current scorer
    // lacks; positions are needed for phrase support.
    for column in ["summary", "tags", "content"] {
        let params = FtsIndexBuilder::default()
            .stem(stem)
            .remove_stop_words(true)
            .with_position(true);
        table
            .create_index(&[column], Index::FTS(params))
            .execute()
            .await
            .with_context(|| format!("failed to create FTS index on `{column}`"))?;
    }

    Ok(table)
}

/// A boolean should-query over the three fields, each boosted to mirror the
/// current scorer's field weighting.
///
/// NOTE the `fuzziness` encoding, which is easy to get backwards:
/// `Some(0)` is exact matching, `Some(n)` is a fixed maximum edit distance,
/// and **`None` means AUTO** — `auto_fuzziness` allows distance 1 for 3-5
/// character tokens and 2 for anything longer. `MatchQuery::new` defaults to
/// `Some(0)`, so passing `None` *enables* fuzzy rather than disabling it.
fn boosted_query(query: &str, fuzziness: Option<u32>) -> FullTextSearchQuery {
    let clause = |column: &str, boost: f32| {
        let mut m = MatchQuery::new(query.to_string())
            .with_column(Some(column.to_string()))
            .with_boost(boost);
        m = m.with_fuzziness(fuzziness);
        (Occur::Should, m.into())
    };

    let boolean = BooleanQuery::new(vec![
        clause("summary", BOOST_SUMMARY),
        clause("tags", BOOST_TAGS),
        clause("content", BOOST_CONTENT),
    ]);

    FullTextSearchQuery::new_query(boolean.into()).limit(Some(TOP_K as i64))
}

async fn rank_bm25(
    table: &lancedb::Table,
    query: &str,
    fuzziness: Option<u32>,
) -> Result<Vec<String>> {
    let mut stream = table
        .query()
        .full_text_search(boosted_query(query, fuzziness))
        .select(lancedb::query::Select::Columns(vec!["id".into()]))
        .limit(TOP_K)
        .execute()
        .await
        .with_context(|| format!("FTS query failed for `{query}`"))?;

    let mut ids = Vec::new();
    while let Some(batch) = stream.next().await {
        let batch = batch?;
        let col = batch
            .column_by_name("id")
            .context("missing id column")?
            .as_any()
            .downcast_ref::<StringArray>()
            .context("id column is not Utf8")?;
        for i in 0..col.len() {
            ids.push(col.value(i).to_string());
        }
    }
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn probe_name(p: Probe) -> &'static str {
    match p {
        Probe::Exact => "Exact",
        Probe::Stemming => "Stemming",
        Probe::Idf => "IDF/stopwords",
        Probe::Typo => "Typo",
        Probe::Discrimination => "Discrimination",
        Probe::LengthNorm => "LengthNorm",
        Probe::TermFreq => "TermFreq",
        Probe::TieBreak => "TieBreak",
    }
}

fn print_table(title: &str, rows: &[(String, [Metrics; 4])]) {
    println!("\n{title}");
    println!(
        "{:<16} {:>4}   {:>17}   {:>17}   {:>17}   {:>17}",
        "", "n", "A current", "B bm25 only", "C +stemming", "D +fuzzy"
    );
    print!("{:<16} {:>4}", "probe", "");
    for _ in 0..4 {
        print!("   {:>5} {:>5} {:>5}", "R@1", "R@5", "MRR");
    }
    println!();
    println!("{}", "-".repeat(96));
    for (name, m) in rows {
        print!("{:<16} {:>4}", name, m[0].n);
        for x in m {
            print!(
                "   {:>5.2} {:>5.2} {:>5.2}",
                x.recall_at_1(),
                x.recall_at_5(),
                x.mrr()
            );
        }
        println!();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let memories = build_memories();
    let temp = tempfile::TempDir::new()?;
    let table = build_fts_table(temp.path(), true).await?;
    // Separate index with stemming OFF. Fuzzy matching compares the query term
    // against the terms actually stored in the index — which are stems when
    // stemming is on — so the two features can interfere. This arm isolates
    // that: if typos only resolve here, stemming and fuzzy are not composable.
    let temp_nostem = tempfile::TempDir::new()?;
    let table_nostem = build_fts_table(temp_nostem.path(), false).await?;

    println!(
        "corpus: {} memories, {} scenarios, top_k = {TOP_K}",
        CORPUS.len(),
        SCENARIOS.len()
    );

    let mut per_probe: HashMap<&str, [Metrics; 4]> = HashMap::new();
    let mut overall: [Metrics; 4] = Default::default();
    let mut regressions: Vec<String> = Vec::new();

    for (query, relevant, probe, note) in SCENARIOS {
        // Ablation ladder — each arm adds exactly ONE thing to the one before,
        // so a movement can be attributed to the feature that caused it:
        //   A  current scorer
        //   B  + BM25 scoring (IDF, TF saturation, length norm), no stemming
        //   C  + stemming
        //   D  + fuzzy matching
        // An earlier version had no "BM25 alone" cell, which made it
        // impossible to tell whether BM25's *scoring* contributed anything
        // independent of its tokenizer.
        let a = rank_current(query, &memories);
        let b = rank_bm25(&table_nostem, query, Some(0)).await?;
        let c = rank_bm25(&table, query, Some(0)).await?;
        let d = rank_bm25(&table, query, None).await?;

        let entry = per_probe.entry(probe_name(*probe)).or_default();
        for (slot, ranked) in entry.iter_mut().zip([&a, &b, &c, &d]) {
            slot.record(ranked, relevant);
        }
        for (slot, ranked) in overall.iter_mut().zip([&a, &b, &c, &d]) {
            slot.record(ranked, relevant);
        }

        let top = |v: &[String]| v.first().cloned().unwrap_or_else(|| "-".into());
        let mark = |v: &[String]| {
            if v.first().is_some_and(|id| relevant.contains(&id.as_str())) {
                "OK "
            } else if v.iter().any(|id| relevant.contains(&id.as_str())) {
                "~  "
            } else {
                "MISS"
            }
        };
        println!(
            "\n[{}] {:?}\n  want {:?}  ({note})\n  A {} top={}\n  B {} top={}\n  C {} top={}\n  D {} top={}",
            probe_name(*probe),
            query,
            relevant,
            mark(&a),
            top(&a),
            mark(&b),
            top(&b),
            mark(&c),
            top(&c),
            mark(&d),
            top(&d),
        );

        // Flag cases the current system gets right and BM25 does not — the
        // honest counterweight to an aggregate win.
        let a_ok = a.first().is_some_and(|id| relevant.contains(&id.as_str()));
        let b_ok = b.first().is_some_and(|id| relevant.contains(&id.as_str()));
        if a_ok && !b_ok {
            regressions.push(format!(
                "  {:?} (want {:?}, bm25 gave {})",
                query,
                relevant,
                top(&b)
            ));
        }
    }

    let mut rows: Vec<(String, [Metrics; 4])> = per_probe
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.push(("ALL".to_string(), overall));

    print_table("=== Retrieval quality ===", &rows);

    if regressions.is_empty() {
        println!("\nNo case where the current scorer beats BM25 at rank 1.");
    } else {
        println!(
            "\nCases the CURRENT scorer wins at rank 1 ({}):",
            regressions.len()
        );
        for r in &regressions {
            println!("{r}");
        }
    }

    Ok(())
}
