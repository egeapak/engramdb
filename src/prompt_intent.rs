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
//! - [`keyword_situation`] is the fallback: lowercase substring tables, first
//!   match wins. It needs no model, so the hook uses it whenever no embedding
//!   model is available, and it matches inside words ("terror" is Debugging,
//!   "redesign" is DesignChoice).
//! - [`PrototypeClassifier`] embeds a fixed set of labelled seed prompts, then
//!   labels a prompt by its nearest seeds (mean of the top-`k` cosine
//!   similarities per class). The hook calls
//!   [`PrototypeClassifier::classify_once`], which embeds the seeds and the
//!   prompt in one batch and keeps nothing, so there is no cache to go stale.
//!   It needs an [`EmbeddingProvider`]; the hook falls back to
//!   [`keyword_situation`] when none loads. Its context gate
//!   ([`PromptDecision::needs_context`]) is **not** used by the hook: the
//!   evaluations in `docs/contributors/prompt-intent-eval.md` found no gate
//!   safe enough, and a search costs only milliseconds.
//!
//! `examples/prompt_intent_eval.rs` measures both against
//! `examples/data/prompt_intent_eval.json`. The seeds below are deliberately
//! worded differently from that set: a seed that duplicates an eval prompt
//! would score as a perfect match and inflate the result.

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
    /// How many seeds per class are embedded (the first ones in [`SEEDS`]
    /// order). Every hook call embeds them anew, so this is the cost knob:
    /// fewer seeds embed faster and decide less reliably.
    pub seeds_per_class: usize,
}

impl Default for PrototypeParams {
    fn default() -> Self {
        Self {
            top_k: 3,
            gate_margin: 0.05,
            max_skip_words: Some(7),
            // 6 measured best on examples/data/prompt_intent_eval.json:
            // 89.3% at ~40 ms per call in-process, against 87.3% at ~75 ms
            // for all 12 and 78.0% at ~23 ms for 3.
            seeds_per_class: 6,
        }
    }
}

/// The first `per_class` seeds of every class, in [`SEEDS`] order.
fn selected_seeds(per_class: usize) -> Vec<(Label, &'static str)> {
    let mut taken: Vec<(Label, usize)> = Vec::new();
    SEEDS
        .iter()
        .filter(|(label, _)| {
            let n = match taken.iter_mut().find(|(l, _)| l == label) {
                Some((_, n)) => n,
                None => {
                    taken.push((*label, 0));
                    &mut taken.last_mut().unwrap().1
                }
            };
            *n += 1;
            *n <= per_class
        })
        .copied()
        .collect()
}

/// What the prototype classifier decided about one prompt, with the class
/// scores it decided from (for inspection and threshold tuning).
#[derive(Debug, Clone, PartialEq)]
pub struct PromptDecision {
    /// `Some(Debugging)`, `Some(DesignChoice)`, or `None` (neither, or a
    /// prompt the gate skips).
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
        Self::check(params)?;
        let seeds = selected_seeds(params.seeds_per_class);
        let texts: Vec<&str> = seeds.iter().map(|(_, t)| *t).collect();
        let vectors = provider.embed_batch(&texts).await?;
        Self::from_vectors(&seeds, vectors, params, provider.model_id())
    }

    /// Embed the seeds and `prompt` in **one** batch call and classify it.
    ///
    /// For a caller that classifies a single prompt and exits (the
    /// UserPromptSubmit hook): nothing is kept between calls, so there is no
    /// cache to go stale when the model or [`SEEDS`] change, at the cost of
    /// embedding the seeds every time ([`PrototypeParams::seeds_per_class`]).
    pub async fn classify_once(
        provider: &dyn EmbeddingProvider,
        params: PrototypeParams,
        prompt: &str,
    ) -> Result<PromptDecision> {
        Self::check(params)?;
        let seeds = selected_seeds(params.seeds_per_class);
        let mut texts: Vec<&str> = seeds.iter().map(|(_, t)| *t).collect();
        texts.push(prompt);
        let mut vectors = provider.embed_batch(&texts).await?;
        ensure!(
            vectors.len() == texts.len(),
            "provider returned {} vectors for {} texts",
            vectors.len(),
            texts.len()
        );
        let prompt_vec = vectors.pop().expect("length checked above");
        let classifier = Self::from_vectors(&seeds, vectors, params, provider.model_id())?;
        Ok(classifier.classify_embedded(prompt, &prompt_vec))
    }

    fn check(params: PrototypeParams) -> Result<()> {
        ensure!(params.top_k > 0, "top_k must be at least 1");
        ensure!(
            params.seeds_per_class > 0,
            "seeds_per_class must be at least 1"
        );
        Ok(())
    }

    fn from_vectors(
        seeds: &[(Label, &str)],
        vectors: Vec<Vec<f32>>,
        params: PrototypeParams,
        model_id: String,
    ) -> Result<Self> {
        ensure!(
            vectors.len() == seeds.len(),
            "provider returned {} vectors for {} seeds",
            vectors.len(),
            seeds.len()
        );
        let seeds = seeds
            .iter()
            .zip(vectors)
            .map(|((label, _), v)| (*label, normalized(v)))
            .collect();
        Ok(Self {
            seeds,
            params,
            model_id,
        })
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

        // A skipped prompt has no situation: its debugging / design / other
        // scores are all low, and the larger of them is noise.
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
        // Four classes, `seeds_per_class` each.
        assert_eq!(
            c.seed_count(),
            4 * PrototypeParams::default().seeds_per_class
        );
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

    /// Counts batch calls.
    struct CountingEmbedder(std::sync::atomic::AtomicUsize);

    #[async_trait]
    impl EmbeddingProvider for CountingEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            FamilyEmbedder.embed(text).await
        }
        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            FamilyEmbedder.embed_batch(texts).await
        }
        fn dimensions(&self) -> usize {
            FamilyEmbedder.dimensions()
        }
        fn max_tokens(&self) -> usize {
            512
        }
        fn model_id(&self) -> String {
            "counting-stub".into()
        }
    }

    #[tokio::test]
    async fn classify_once_uses_one_batch_and_matches_build() {
        let p = CountingEmbedder(std::sync::atomic::AtomicUsize::new(0));
        let params = PrototypeParams::default();
        let built = PrototypeClassifier::build(&FamilyEmbedder, params)
            .await
            .unwrap();
        for prompt in [
            "the server segfault is a bug",
            "compare the tradeoff, what do you recommend",
            "rename and move the file",
            "ok thanks",
        ] {
            let before = p.0.load(std::sync::atomic::Ordering::SeqCst);
            let once = PrototypeClassifier::classify_once(&p, params, prompt)
                .await
                .unwrap();
            assert_eq!(p.0.load(std::sync::atomic::Ordering::SeqCst), before + 1);
            let v = FamilyEmbedder.embed(prompt).await.unwrap();
            assert_eq!(once, built.classify_embedded(prompt, &v), "{prompt}");
        }
    }

    #[test]
    fn seeds_per_class_takes_the_first_seeds_of_every_class() {
        assert_eq!(selected_seeds(usize::MAX).len(), SEEDS.len());
        let three = selected_seeds(3);
        assert_eq!(three.len(), 12);
        for label in [
            Label::Debugging,
            Label::Design,
            Label::Other,
            Label::NoContext,
        ] {
            let mine: Vec<&str> = three
                .iter()
                .filter(|(l, _)| *l == label)
                .map(|(_, t)| *t)
                .collect();
            let first: Vec<&str> = SEEDS
                .iter()
                .filter(|(l, _)| *l == label)
                .map(|(_, t)| *t)
                .take(3)
                .collect();
            assert_eq!(mine, first);
        }
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
        let err = PrototypeClassifier::classify_once(
            &FamilyEmbedder,
            PrototypeParams {
                seeds_per_class: 0,
                ..PrototypeParams::default()
            },
            "x",
        )
        .await;
        assert!(err.is_err());
    }
}
