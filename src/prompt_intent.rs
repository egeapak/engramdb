//! Prompt intent: two decisions the UserPromptSubmit hook makes about the
//! prompt text before it queries memories.
//!
//! 1. **Situation** — is the user debugging a failure, weighing a design
//!    choice, or neither? The answer reweights epistemic classes in scoring
//!    (`[retrieval.scoring.situation]`).
//! 2. **Context gate** — does the prompt carry task content at all? Turn
//!    control ("ok", "continue", "thanks") has nothing to retrieve against,
//!    so running a search for it can only inject noise.
//!
//! Two implementations live here:
//!
//! - [`keyword_situation`] is the shipped heuristic: lowercase substring
//!   tables, first match wins. It needs no model, which is why the hook uses
//!   it, and it matches inside words ("terror" is Debugging, "redesign" is
//!   DesignChoice).
//! - [`PrototypeClassifier`] embeds a fixed set of labelled seed prompts once
//!   (cached on disk per model, see [`PrototypeClassifier::load_or_build`]),
//!   then labels a prompt by its nearest seeds (mean of the top-`k` cosine
//!   similarities per class). It needs an [`EmbeddingProvider`]; the hook uses
//!   it for the situation only when a daemon is already running, and falls
//!   back to [`keyword_situation`] otherwise. Its context gate
//!   ([`PromptDecision::needs_context`]) is **not** used by the hook: the
//!   evaluations in `docs/contributors/prompt-intent-eval.md` found no gate
//!   safe enough, and a search costs only milliseconds.
//!
//! `examples/prompt_intent_eval.rs` measures both against
//! `examples/data/prompt_intent_eval.json`. The seeds below are deliberately
//! worded differently from that set: a seed that duplicates an eval prompt
//! would score as a perfect match and inflate the result.

use std::path::Path;

use anyhow::{ensure, Result};

use crate::embeddings::EmbeddingProvider;
use crate::types::Situation;

/// Situation-inference keyword tables for UserPromptSubmit (§8.5.1). Cheap
/// substring heuristics — the only place Debugging/DesignChoice can be
/// inferred automatically without a model.
const DEBUGGING_KEYWORDS: &[&str] = &["error", "failing", "why does", "debug", "panic", "crash"];
const DESIGN_KEYWORDS: &[&str] = &[
    "should we",
    "choose",
    " vs ",
    "design",
    "approach",
    "architecture",
];

/// Infer a situation from the submitted prompt text with the keyword tables
/// (`None` when no table matches — neutral scoring). Debugging is checked
/// first, so it wins when both tables match.
pub fn keyword_situation(prompt: &str) -> Option<Situation> {
    let lower = prompt.to_lowercase();
    if DEBUGGING_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return Some(Situation::Debugging);
    }
    if DESIGN_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return Some(Situation::DesignChoice);
    }
    None
}

/// The labels the prototype classifier scores. `Other` is a real class with
/// its own seeds, not "below a threshold": an implementation request is a
/// positive example of neither situation, and a nearest-seed vote needs
/// something to be nearest to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Label {
    Debugging,
    Design,
    Other,
    /// Turn control with no task content (the context gate's "skip" class).
    NoContext,
}

/// Seed prompts. Everything not `NoContext` is also a positive example for
/// the gate's "needs context" class, so the gate compares turn control
/// against every kind of task prompt, not against one hand-picked list.
const SEEDS: &[(Label, &str)] = &[
    // --- Debugging: investigating a failure or unexpected behavior.
    (
        Label::Debugging,
        "the build broke after I pulled main, what happened",
    ),
    (Label::Debugging, "this test passes locally but fails in CI"),
    (Label::Debugging, "I get a segfault when the daemon starts"),
    (
        Label::Debugging,
        "the query returns no results even though the memory exists",
    ),
    (
        Label::Debugging,
        "stack trace says index out of bounds in the parser",
    ),
    (
        Label::Debugging,
        "it hangs forever when two sessions write at once",
    ),
    (
        Label::Debugging,
        "figure out why the output is wrong after the migration",
    ),
    (
        Label::Debugging,
        "the command exits with code 101 and I don't know why",
    ),
    (
        Label::Debugging,
        "something is broken in the login flow, investigate",
    ),
    (
        Label::Debugging,
        "this got much slower since yesterday, find the regression",
    ),
    (Label::Debugging, "the app throws an exception on startup"),
    (
        Label::Debugging,
        "fix the bug where saving twice loses the second edit",
    ),
    // --- Design: weighing options, tradeoffs, how to structure something.
    (
        Label::Design,
        "what is the best way to structure the plugin system",
    ),
    (
        Label::Design,
        "is it better to use a trait object or an enum here",
    ),
    (
        Label::Design,
        "compare storing this in a file versus a database table",
    ),
    (Label::Design, "what are the tradeoffs of making this async"),
    (
        Label::Design,
        "how would you lay out the modules for this feature",
    ),
    (
        Label::Design,
        "do you think we need a cache layer or is that overkill",
    ),
    (
        Label::Design,
        "pick a library for parsing the config, what do you recommend",
    ),
    (
        Label::Design,
        "which option is cleaner: one big table or several small ones",
    ),
    (
        Label::Design,
        "propose a plan for splitting the server into crates",
    ),
    (
        Label::Design,
        "is a background worker the right pattern for this",
    ),
    (
        Label::Design,
        "help me decide between REST and gRPC for the new service",
    ),
    (Label::Design, "what would a good API for this look like"),
    // --- Other: concrete tasks, explanations, chores.
    (Label::Other, "add a --verbose flag to the list command"),
    (Label::Other, "rename this function to something clearer"),
    (Label::Other, "write unit tests for the slug helper"),
    (Label::Other, "explain what this module does"),
    (Label::Other, "update the README with the new install steps"),
    (Label::Other, "run the formatter and commit"),
    (Label::Other, "implement the export command we planned"),
    (Label::Other, "move these helpers into their own file"),
    (
        Label::Other,
        "bump the version and write the changelog entry",
    ),
    (Label::Other, "show me where the config is loaded"),
    (Label::Other, "add a column for the timestamp to the table"),
    (Label::Other, "summarize the changes in this branch"),
    // --- NoContext: turn control, acknowledgements, approvals.
    (Label::NoContext, "ok"),
    (Label::NoContext, "thanks!"),
    (Label::NoContext, "yes please"),
    (Label::NoContext, "go ahead"),
    (Label::NoContext, "keep going"),
    (Label::NoContext, "sounds good to me"),
    (Label::NoContext, "perfect, thank you"),
    (Label::NoContext, "sure"),
    (Label::NoContext, "nice work"),
    (Label::NoContext, "do it"),
    (Label::NoContext, "alright"),
    (Label::NoContext, "cool"),
];

/// Tuning knobs for [`PrototypeClassifier`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrototypeParams {
    /// How many nearest seeds per class are averaged into the class score.
    /// `1` is plain nearest neighbour; larger values are less sensitive to
    /// one seed that happens to share wording with the prompt.
    pub top_k: usize,
    /// The context gate skips only when the "no context" score beats the
    /// best task-prompt score by more than this. A wrong skip hides memories
    /// the user needed, so a positive margin trades missed skips for fewer
    /// wrong ones.
    pub gate_margin: f32,
    /// Length guard: a prompt with more words than this is never skipped,
    /// whatever its scores. Turn control is short, and a longer prompt that
    /// opens with an acknowledgement ("looks good, push it and open a PR")
    /// embeds close to the acknowledgement seeds while still carrying a task.
    /// `None` turns the guard off.
    pub max_skip_words: Option<usize>,
}

impl Default for PrototypeParams {
    fn default() -> Self {
        Self {
            top_k: 3,
            gate_margin: 0.05,
            max_skip_words: Some(7),
        }
    }
}

/// What the prototype classifier decided about one prompt, with the class
/// scores it decided from (for inspection and threshold tuning).
#[derive(Debug, Clone, PartialEq)]
pub struct PromptDecision {
    /// `Some(Debugging)`, `Some(DesignChoice)`, or `None` (neither).
    pub situation: Option<Situation>,
    /// `false` when the prompt is turn control and the hook should not
    /// search at all.
    pub needs_context: bool,
    /// Class scores: debugging, design, other, no-context.
    pub scores: ClassScores,
}

/// Mean top-`k` cosine similarity of the prompt to each class's seeds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClassScores {
    pub debugging: f32,
    pub design: f32,
    pub other: f32,
    pub no_context: f32,
}

/// Nearest-seed classifier over embedding vectors. See the module docs.
#[derive(Clone)]
pub struct PrototypeClassifier {
    seeds: Vec<(Label, Vec<f32>)>,
    params: PrototypeParams,
    model_id: String,
}

impl PrototypeClassifier {
    /// Embed the seed prompts with `provider` (one batch call). The result is
    /// only valid for vectors from the same model; [`Self::model_id`] records
    /// which one.
    pub async fn build(provider: &dyn EmbeddingProvider, params: PrototypeParams) -> Result<Self> {
        ensure!(params.top_k > 0, "top_k must be at least 1");
        let texts: Vec<&str> = SEEDS.iter().map(|(_, t)| *t).collect();
        let vectors = provider.embed_batch(&texts).await?;
        ensure!(
            vectors.len() == SEEDS.len(),
            "provider returned {} vectors for {} seeds",
            vectors.len(),
            SEEDS.len()
        );
        let seeds = SEEDS
            .iter()
            .zip(vectors)
            .map(|((label, _), v)| (*label, normalized(v)))
            .collect();
        Ok(Self {
            seeds,
            params,
            model_id: provider.model_id(),
        })
    }

    /// [`Self::build`], reusing seed vectors cached in `cache_dir` when they
    /// were embedded by the same model.
    ///
    /// Embedding the seeds costs one batch call (~100 ms in-process), which a
    /// hook process cannot amortize: it handles one prompt and exits. The
    /// cache file is named by a digest of the model id, the dimensions and
    /// every seed (label and text), so a model swap or a seed edit misses
    /// instead of reading stale vectors. A missing, unreadable or malformed
    /// file falls back to embedding; a failed write is logged and ignored.
    pub async fn load_or_build(
        provider: &dyn EmbeddingProvider,
        params: PrototypeParams,
        cache_dir: &Path,
    ) -> Result<Self> {
        ensure!(params.top_k > 0, "top_k must be at least 1");
        let model_id = provider.model_id();
        let path = cache_dir.join(seed_cache_name(&model_id, provider.dimensions()));
        if let Some(vectors) = read_seed_cache(&path, provider.dimensions()) {
            let seeds = SEEDS
                .iter()
                .zip(vectors)
                .map(|((label, _), v)| (*label, v))
                .collect();
            return Ok(Self {
                seeds,
                params,
                model_id,
            });
        }
        let built = Self::build(provider, params).await?;
        if let Err(e) = write_seed_cache(&path, &built.seeds) {
            tracing::debug!(
                "prompt-intent seed cache write to {} failed (non-fatal): {e}",
                path.display()
            );
        }
        Ok(built)
    }

    /// The embedding model the seeds were embedded with.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Number of seed prompts (the one-time embedding cost of [`Self::build`]).
    pub fn seed_count(&self) -> usize {
        self.seeds.len()
    }

    /// Change the tuning knobs without re-embedding the seeds.
    pub fn with_params(mut self, params: PrototypeParams) -> Self {
        self.params = params;
        self
    }

    /// Embed `prompt` and classify it.
    pub async fn classify(
        &self,
        provider: &dyn EmbeddingProvider,
        prompt: &str,
    ) -> Result<PromptDecision> {
        let v = provider.embed(prompt).await?;
        Ok(self.classify_embedded(prompt, &v))
    }

    /// Classify a prompt whose embedding is already computed. Pure: no model
    /// call. `prompt` is the text `prompt_vec` was embedded from; the length
    /// guard reads it.
    pub fn classify_embedded(&self, prompt: &str, prompt_vec: &[f32]) -> PromptDecision {
        let q = normalized(prompt_vec.to_vec());
        let score = |label: Label| self.class_score(&q, label);
        let scores = ClassScores {
            debugging: score(Label::Debugging),
            design: score(Label::Design),
            other: score(Label::Other),
            no_context: score(Label::NoContext),
        };

        let best_task = scores.debugging.max(scores.design).max(scores.other);
        let too_long_to_skip = self
            .params
            .max_skip_words
            .is_some_and(|max| prompt.split_whitespace().count() > max);
        let needs_context =
            too_long_to_skip || scores.no_context <= best_task + self.params.gate_margin;

        // A skipped prompt has no situation: nothing will be retrieved for it.
        let situation = if !needs_context {
            None
        } else if scores.debugging >= scores.design && scores.debugging > scores.other {
            Some(Situation::Debugging)
        } else if scores.design > scores.debugging && scores.design > scores.other {
            Some(Situation::DesignChoice)
        } else {
            None
        };

        PromptDecision {
            situation,
            needs_context,
            scores,
        }
    }

    fn class_score(&self, q: &[f32], label: Label) -> f32 {
        let mut sims: Vec<f32> = self
            .seeds
            .iter()
            .filter(|(l, _)| *l == label)
            .map(|(_, v)| dot(q, v))
            .collect();
        if sims.is_empty() {
            return f32::NEG_INFINITY;
        }
        sims.sort_by(|a, b| b.total_cmp(a));
        let k = self.params.top_k.min(sims.len());
        sims[..k].iter().sum::<f32>() / k as f32
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Seed-cache file layout: magic, then `count` and `dim` as little-endian
/// `u32`, then `count * dim` normalized little-endian `f32` values in
/// [`SEEDS`] order.
const SEED_CACHE_MAGIC: &[u8; 8] = b"EDBPIS01";

/// File name of the seed cache for one model. The digest covers everything
/// the vectors depend on, so any change produces a new name.
fn seed_cache_name(model_id: &str, dimensions: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(SEED_CACHE_MAGIC);
    h.update(model_id.as_bytes());
    h.update((dimensions as u64).to_le_bytes());
    for (label, text) in SEEDS {
        h.update(format!("{label:?}\0{text}\0").as_bytes());
    }
    let digest = h.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("prompt_intent_seeds-{hex}.bin")
}

fn read_seed_cache(path: &Path, dimensions: usize) -> Option<Vec<Vec<f32>>> {
    let bytes = std::fs::read(path).ok()?;
    let header = SEED_CACHE_MAGIC.len() + 8;
    if bytes.len() < header || &bytes[..SEED_CACHE_MAGIC.len()] != SEED_CACHE_MAGIC {
        return None;
    }
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let (count, dim) = (word(8), word(12));
    if count != SEEDS.len() || dim != dimensions || bytes.len() != header + count * dim * 4 {
        return None;
    }
    let floats: Vec<f32> = bytes[header..]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    if floats.iter().any(|f| !f.is_finite()) {
        return None;
    }
    Some(floats.chunks_exact(dim).map(<[f32]>::to_vec).collect())
}

fn write_seed_cache(path: &Path, seeds: &[(Label, Vec<f32>)]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let dim = seeds.first().map_or(0, |(_, v)| v.len());
    let mut bytes = Vec::with_capacity(SEED_CACHE_MAGIC.len() + 8 + seeds.len() * dim * 4);
    bytes.extend_from_slice(SEED_CACHE_MAGIC);
    bytes.extend_from_slice(&(seeds.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(dim as u32).to_le_bytes());
    for (_, v) in seeds {
        for f in v {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    // Temp-then-rename, so a concurrent reader sees the old file or the new
    // one, never a partial write.
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&bytes)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Scale to unit length so a dot product is a cosine. A zero vector stays
/// zero (it then scores 0 against everything rather than NaN).
fn normalized(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[test]
    fn keyword_situation_tables() {
        assert_eq!(
            keyword_situation("why does the test panic here?"),
            Some(Situation::Debugging)
        );
        assert_eq!(
            keyword_situation("Should we choose sqlite vs postgres?"),
            Some(Situation::DesignChoice)
        );
        // Debugging wins when both match (checked first).
        assert_eq!(
            keyword_situation("error in the design"),
            Some(Situation::Debugging)
        );
        assert_eq!(keyword_situation("add a new field to the struct"), None);
    }

    /// The substring weakness the prototype exists to fix, pinned so a
    /// change to the tables is a visible decision.
    #[test]
    fn keyword_situation_matches_inside_words() {
        assert_eq!(
            keyword_situation("add a terrorism filter"),
            Some(Situation::Debugging)
        );
        assert_eq!(
            keyword_situation("redesign the landing page"),
            Some(Situation::DesignChoice)
        );
    }

    /// Embeds by keyword families, so a test controls which seed class a
    /// prompt lands nearest to without loading a model.
    struct FamilyEmbedder;

    const FAMILIES: &[&[&str]] = &[
        &[
            "fail",
            "broke",
            "segfault",
            "hang",
            "exception",
            "bug",
            "wrong",
            "slow",
            "trace",
            "exits",
        ],
        &[
            "better",
            "tradeoff",
            "compare",
            "versus",
            "decide",
            "recommend",
            "pattern",
            "plan",
            "api",
            "structure",
            "cache",
            "lay out",
            "cleaner",
        ],
        &[
            "add",
            "rename",
            "write",
            "explain",
            "update",
            "run",
            "implement",
            "move",
            "bump",
            "show",
            "summarize",
        ],
        &[
            "ok",
            "thank",
            "yes",
            "go ahead",
            "keep going",
            "sounds good",
            "sure",
            "nice",
            "do it",
            "alright",
            "cool",
            "perfect",
        ],
    ];

    #[async_trait]
    impl EmbeddingProvider for FamilyEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let lower = text.to_lowercase();
            let mut v: Vec<f32> = FAMILIES
                .iter()
                .map(|words| words.iter().filter(|w| lower.contains(*w)).count() as f32)
                .collect();
            v.push(0.01);
            Ok(v)
        }
        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(texts.len());
            for t in texts {
                out.push(self.embed(t).await?);
            }
            Ok(out)
        }
        fn dimensions(&self) -> usize {
            FAMILIES.len() + 1
        }
        fn max_tokens(&self) -> usize {
            512
        }
        fn model_id(&self) -> String {
            "family-stub".into()
        }
    }

    async fn classifier() -> PrototypeClassifier {
        PrototypeClassifier::build(&FamilyEmbedder, PrototypeParams::default())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn build_embeds_every_seed_and_records_the_model() {
        let c = classifier().await;
        assert_eq!(c.seed_count(), SEEDS.len());
        assert_eq!(c.model_id(), "family-stub");
    }

    #[tokio::test]
    async fn classifies_each_class() {
        let c = classifier().await;
        let p = &FamilyEmbedder;

        let d = c.classify(p, "the server segfault is a bug").await.unwrap();
        assert_eq!(d.situation, Some(Situation::Debugging));
        assert!(d.needs_context);

        let d = c
            .classify(p, "compare the tradeoff, what do you recommend")
            .await
            .unwrap();
        assert_eq!(d.situation, Some(Situation::DesignChoice));
        assert!(d.needs_context);

        let d = c.classify(p, "rename and move the file").await.unwrap();
        assert_eq!(d.situation, None);
        assert!(d.needs_context);

        let d = c.classify(p, "ok thanks").await.unwrap();
        assert_eq!(d.situation, None);
        assert!(!d.needs_context);
    }

    #[tokio::test]
    async fn gate_margin_keeps_borderline_prompts() {
        let c = classifier().await;
        // Equal turn-control and task signal: the default margin keeps it.
        let d = c.classify(&FamilyEmbedder, "ok, rename it").await.unwrap();
        assert!(d.needs_context, "scores: {:?}", d.scores);

        // A margin far below zero makes the gate skip anything that is not
        // clearly a task.
        let eager = c.with_params(PrototypeParams {
            gate_margin: -1.0,
            ..PrototypeParams::default()
        });
        let d = eager
            .classify(&FamilyEmbedder, "ok, rename it")
            .await
            .unwrap();
        assert!(!d.needs_context, "scores: {:?}", d.scores);
    }

    #[tokio::test]
    async fn length_guard_keeps_long_prompts() {
        let c = classifier().await;
        // Eight words of pure turn control: the scores say skip, the guard
        // (default seven words) keeps it.
        let long = "ok ok thanks thanks sure sure cool cool";
        let d = c.classify(&FamilyEmbedder, long).await.unwrap();
        assert!(d.needs_context, "scores: {:?}", d.scores);

        // With the guard off, the same prompt is skipped.
        let unguarded = c.with_params(PrototypeParams {
            max_skip_words: None,
            ..PrototypeParams::default()
        });
        let d = unguarded.classify(&FamilyEmbedder, long).await.unwrap();
        assert!(!d.needs_context, "scores: {:?}", d.scores);

        // At the limit the guard does not apply.
        let d = unguarded
            .with_params(PrototypeParams::default())
            .classify(&FamilyEmbedder, "ok thanks sure cool ok thanks sure")
            .await
            .unwrap();
        assert!(!d.needs_context, "scores: {:?}", d.scores);
    }

    #[tokio::test]
    async fn zero_vector_scores_zero_not_nan() {
        let c = classifier().await;
        let d = c.classify_embedded("", &[0.0; 5]);
        for s in [
            d.scores.debugging,
            d.scores.design,
            d.scores.other,
            d.scores.no_context,
        ] {
            assert_eq!(s, 0.0);
        }
        // All-tied scores fall through to "needs context, no situation".
        assert!(d.needs_context);
        assert_eq!(d.situation, None);
    }

    /// Counts batch calls (the seed embedding) and can pose as another model.
    struct CountingEmbedder {
        batches: std::sync::atomic::AtomicUsize,
        model: &'static str,
    }

    impl CountingEmbedder {
        fn new(model: &'static str) -> Self {
            Self {
                batches: std::sync::atomic::AtomicUsize::new(0),
                model,
            }
        }
        fn batches(&self) -> usize {
            self.batches.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EmbeddingProvider for CountingEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            FamilyEmbedder.embed(text).await
        }
        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            self.batches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            FamilyEmbedder.embed_batch(texts).await
        }
        fn dimensions(&self) -> usize {
            FamilyEmbedder.dimensions()
        }
        fn max_tokens(&self) -> usize {
            512
        }
        fn model_id(&self) -> String {
            self.model.into()
        }
    }

    #[tokio::test]
    async fn seed_cache_is_reused_per_model() {
        let dir = tempfile::TempDir::new().unwrap();
        let params = PrototypeParams::default();
        let a = CountingEmbedder::new("model-a");

        let first = PrototypeClassifier::load_or_build(&a, params, dir.path())
            .await
            .unwrap();
        assert_eq!(a.batches(), 1, "first load embeds the seeds");
        let second = PrototypeClassifier::load_or_build(&a, params, dir.path())
            .await
            .unwrap();
        assert_eq!(a.batches(), 1, "second load reads the cache");

        // The cached classifier decides exactly like the freshly built one.
        for prompt in [
            "the server segfault is a bug",
            "compare the tradeoff",
            "ok thanks",
        ] {
            let v = a.embed(prompt).await.unwrap();
            assert_eq!(
                first.classify_embedded(prompt, &v),
                second.classify_embedded(prompt, &v)
            );
        }

        // Another model misses the cache and embeds its own seeds.
        let b = CountingEmbedder::new("model-b");
        PrototypeClassifier::load_or_build(&b, params, dir.path())
            .await
            .unwrap();
        assert_eq!(b.batches(), 1);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[tokio::test]
    async fn corrupt_seed_cache_falls_back_to_embedding() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = CountingEmbedder::new("model-a");
        let path = dir.path().join(seed_cache_name("model-a", a.dimensions()));
        for bad in [
            b"short".to_vec(),
            [SEED_CACHE_MAGIC.as_slice(), &[0u8; 8]].concat(),
            vec![0u8; 4096],
        ] {
            std::fs::write(&path, &bad).unwrap();
            let before = a.batches();
            PrototypeClassifier::load_or_build(&a, PrototypeParams::default(), dir.path())
                .await
                .unwrap();
            assert_eq!(a.batches(), before + 1, "a bad file must be re-embedded");
        }
        // The last fallback rewrote a valid file.
        assert!(read_seed_cache(&path, a.dimensions()).is_some());
    }

    #[tokio::test]
    async fn rejects_zero_top_k() {
        let err = PrototypeClassifier::build(
            &FamilyEmbedder,
            PrototypeParams {
                top_k: 0,
                ..PrototypeParams::default()
            },
        )
        .await;
        assert!(err.is_err());
    }
}
