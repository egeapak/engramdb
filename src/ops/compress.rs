//! Memory compression operations.
//!
//! Provides two functions:
//! - `compress_candidates` — lists memories eligible for compression
//! - `compress_apply` — creates a summary memory that supersedes the given sources

use crate::ops::{create_memory, CreateParams};
use crate::storage::MemoryStore;
use crate::title::TitleStrategy;
use crate::types::{MemoryType, Provenance, Visibility};
use anyhow::{bail, Result};
use fearless_simd::{dispatch, prelude::*, Level};
use serde::Serialize;

/// A memory eligible for compression.
#[derive(Debug, Clone, Serialize)]
pub struct CompressCandidate {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub summary: String,
    pub criticality: f64,
}

/// Result of listing compression candidates.
#[derive(Debug, Serialize)]
pub struct CompressCandidatesResult {
    pub candidates: Vec<CompressCandidate>,
    pub total: usize,
    pub threshold: f64,
}

/// Result of applying compression.
#[derive(Debug, Serialize)]
pub struct CompressApplyResult {
    pub new_id: String,
    /// Number of source memories the summary supersedes (always
    /// `source_ids.len()` — the `supersedes` list on the new memory).
    pub superseded_count: usize,
    /// Source IDs that were already gone when deletion ran (deleted
    /// concurrently after validation). The summary memory is still valid;
    /// its `supersedes` may reference these missing IDs, which is harmless —
    /// `supersedes` is informational metadata and is never dereferenced.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_sources: Vec<String>,
}

/// List memories eligible for compression based on criticality threshold and scope.
pub async fn compress_candidates(
    store: &MemoryStore,
    scope: Option<&str>,
    threshold: Option<f64>,
) -> Result<CompressCandidatesResult> {
    let entries = store.list_filterable().await?;
    let threshold = threshold.unwrap_or(0.4);

    let candidates: Vec<CompressCandidate> = entries
        .iter()
        .filter(|e| {
            if let Some(scope) = scope {
                e.logical.iter().any(|s| s == scope) || e.physical.iter().any(|p| p == scope)
            } else {
                true
            }
        })
        .filter(|e| e.criticality <= threshold)
        .map(|e| CompressCandidate {
            id: e.id.clone(),
            type_: format!("{:?}", e.type_).to_lowercase(),
            summary: e.summary.clone(),
            criticality: e.criticality,
        })
        .collect();

    let total = candidates.len();
    Ok(CompressCandidatesResult {
        candidates,
        total,
        threshold,
    })
}

/// Parameters for applying compression (mirrors `CreateParams`/`UpdateParams`).
pub struct CompressApplyParams {
    pub source_ids: Vec<String>,
    pub summary: String,
    pub content: String,
    pub scope: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    /// Forwarded to `create_memory` for the replacement summary — see there.
    pub embed_async: bool,
}

/// Create a summary memory that supersedes the given source memories.
///
/// The new memory is created as type Context with provenance agent("compress").
/// The caller (typically an LLM agent) provides the summary and content.
///
/// `engine` embeds the replacement memory: compression invalidates sources
/// that had vectors (default retrieval excludes them), so leaving the
/// consolidated summary UN-embedded would make exactly the compressed
/// knowledge invisible to semantic search until a manual reindex. Pass the
/// same engine the front-end uses for `create`.
/// The audience a consolidated memory should carry when it supersedes
/// `sources` in a shared (group/global) store.
///
/// Safe-by-construction — it never widens visibility beyond the union of the
/// sources' explicitly-listed audiences. If every source is unrestricted
/// (`audience == None`), the result is `None` (whole-group). If any source is
/// restricted, the result is the union of the restricted audiences; an
/// unrestricted source contributes nothing, so consolidating a public source
/// with a restricted one *over-restricts* the summary rather than re-publishing
/// the restricted content store-wide. Returns `None` on a project-local store
/// (audience is inert there), so consolidation outside a shared store is
/// unchanged.
fn consolidated_audience(
    store: &MemoryStore,
    sources: &[crate::types::Memory],
) -> Option<Vec<String>> {
    if !(store.is_group() || store.is_global()) {
        return None;
    }
    let mut union: Vec<String> = Vec::new();
    for s in sources {
        if let Some(list) = &s.audience {
            for id in list {
                if !union.contains(id) {
                    union.push(id.clone());
                }
            }
        }
    }
    (!union.is_empty()).then_some(union)
}

pub async fn compress_apply(
    store: &MemoryStore,
    params: CompressApplyParams,
    engine: Option<&crate::retrieval::engine::RetrievalEngine>,
) -> Result<CompressApplyResult> {
    let CompressApplyParams {
        source_ids,
        summary,
        content,
        scope,
        tags,
        embed_async,
    } = params;
    if source_ids.is_empty() {
        bail!("source_ids must not be empty");
    }

    // Validate all source IDs exist (single dir scan, no file reads),
    // immediately before creating the summary. This cannot be transactional
    // (no cross-file transactions exist), but keeping the check adjacent to
    // the create shrinks the window in which a source can vanish unnoticed.
    let existing = store
        .batch_exists(&source_ids)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to check source IDs: {}", e))?;
    for id in &source_ids {
        if !existing.contains(id.as_str()) {
            bail!("Source memory not found: {}", id);
        }
    }

    let superseded_count = source_ids.len();

    // On a shared store, carry a safe (never-widening) audience from the sources
    // so consolidating an audience-restricted memory doesn't re-publish it to
    // the whole group. No-op (None) on a project-local store, so the common path
    // pays no extra reads.
    let audience = if store.is_group() || store.is_global() {
        // One batched read: the file already uses `batch_exists` on this exact
        // list a few lines up, so a per-id `get` loop here was gratuitous.
        //
        // The completeness check is NOT optional. `consolidated_audience`
        // unions the sources' restricted audiences, so a source silently
        // dropped from the batch (unreadable file — `get_batch` warns and
        // skips) would contribute nothing and the summary would end up LESS
        // restricted than its evidence. That is precisely the widening
        // `consolidated_audience` is documented never to do, so a short batch
        // has to fail the way the per-id `get(...)?` it replaced did.
        let refs: Vec<&str> = source_ids.iter().map(String::as_str).collect();
        let loaded = store.get_batch(&refs).await?;
        if loaded.len() != source_ids.len() {
            let missing: Vec<&str> = {
                let got: std::collections::HashSet<&str> =
                    loaded.iter().map(|(id, _)| id.as_str()).collect();
                source_ids
                    .iter()
                    .map(String::as_str)
                    .filter(|id| !got.contains(id))
                    .collect()
            };
            bail!(
                "Cannot compute the consolidated audience: source memor{} {} could not be read. \
                 Refusing to proceed — a partial read would produce a summary visible more \
                 widely than its sources.",
                if missing.len() == 1 { "y" } else { "ies" },
                missing.join(", ")
            );
        }
        let mems: Vec<crate::types::Memory> = loaded.into_iter().map(|(_, m)| m).collect();
        consolidated_audience(store, &mems)
    } else {
        None
    };

    let result = create_memory(
        store,
        CreateParams {
            type_: MemoryType::Context,
            content,
            summary,
            title: None,
            physical: vec!["/".to_string()],
            logical: scope.unwrap_or_default(),
            tags: tags.unwrap_or_default(),
            criticality: 0.5,
            confidence: 0.8,
            details: None,
            visibility: Visibility::Shared,
            provenance: Provenance::agent("compress"),
            supersedes: source_ids.clone(),
            audience,
            epistemic: None,
            premise: None,
            invalidated_by: vec![],
            origin_task: None,
            generality: None,
            valid_from: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            title_strategy: TitleStrategy::None,
            embed_async,
        },
        engine,
    )
    .await?;

    // Sources are INVALIDATED, not deleted (§2.4 writer 3): `create_memory`
    // already closed each live source's validity window (`invalidated_at =
    // now`, `superseded_by = <summary id>`) via its supersession pass. The
    // files stay on disk — queryable under `include_invalidated`, purged
    // eventually by gc's retention rule. Here we verify the outcome so
    // partial failures surface exactly like the old delete loop did:
    // - a source that vanished concurrently is skipped and reported in
    //   `skipped_sources`;
    // - a source still live (its window-close failed, e.g. I/O) is recorded,
    //   the REMAINING sources are still checked, and a partial-failure error
    //   listing the un-invalidated IDs (and the new memory's ID) is returned
    //   so the user can re-run. The summary memory remains valid either way.
    let mut skipped_sources = Vec::new();
    let mut failed_sources: Vec<String> = Vec::new();
    // One batched read rather than a `get` (full directory scan) per source.
    //
    // `get_batch` drops unreadable files with a warning, so "absent from the
    // map" alone cannot distinguish a source that was concurrently deleted
    // (benign — skip) from one whose file is corrupt or unreadable (a real
    // failure the caller must be told about so it can re-run). The per-id loop
    // this replaced drew that line via `Err(NotFound)` vs `Err(other)`.
    // `batch_exists` restores it in one extra directory scan: a source with no
    // file on disk is gone, one whose file is present but did not load is a
    // failure.
    let verify_refs: Vec<&str> = source_ids.iter().map(String::as_str).collect();
    let on_disk = store
        .batch_exists(&verify_refs)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to re-check source IDs: {}", e))?;
    let verified: std::collections::HashMap<String, crate::types::Memory> =
        store.get_batch(&verify_refs).await?.into_iter().collect();
    for id in &source_ids {
        match verified.get(id) {
            None if !on_disk.contains(id.as_str()) => skipped_sources.push(id.clone()),
            // File present but did not load — corrupt, unreadable, or replaced
            // by something that is not a memory file.
            None => failed_sources.push(format!("{} (unreadable)", id)),
            // Invalidated — by this compress or an earlier writer; either
            // way the window is closed.
            Some(m) if m.is_invalidated() => {}
            Some(_) => failed_sources.push(format!("{} (still active)", id)),
        }
    }

    if !failed_sources.is_empty() {
        bail!(
            "Compressed memory {} was created, but {} source memor{} could not be invalidated: {}. \
             Re-run compress or `resolve --action invalidate` the listed memories manually \
             (the compressed memory is valid and supersedes them).",
            result.id,
            failed_sources.len(),
            if failed_sources.len() == 1 { "y" } else { "ies" },
            failed_sources.join(", ")
        );
    }

    Ok(CompressApplyResult {
        new_id: result.id,
        superseded_count,
        skipped_sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance, ProvenanceSource, Visibility};
    use tempfile::TempDir;

    async fn setup_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        (temp_dir, store)
    }

    /// Deterministic pseudo-random vector at the embedding dimension.
    fn synth_vector(seed: u64, dims: usize) -> Vec<f32> {
        let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (0..dims)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 33) as f32 / (1u32 << 31) as f32) - 0.5
            })
            .collect()
    }

    /// The whole justification for the fast path: it has to compute the same
    /// number the straightforward version does.
    ///
    /// Tolerance is 1e-5 because the fast path accumulates in `f32` across
    /// eight lanes while the reference accumulates in `f64` — the results are
    /// equal in exact arithmetic and differ only in rounding. Includes a
    /// length that is not a multiple of `DOT_LANES` (385) so the scalar
    /// remainder tail is covered, and the odd sizes around the lane boundary.
    #[test]
    fn dot_unit_agrees_with_cosine() {
        for dims in [1, 7, 8, 9, 15, 16, 384, 385, 768] {
            for seed in 0..8u64 {
                let a = synth_vector(seed * 2, dims);
                let b = synth_vector(seed * 2 + 1, dims);
                let reference = cosine(&a, &b);
                let fast = dot_unit(&l2_normalized(&a), &l2_normalized(&b));
                assert!(
                    (reference - fast).abs() < 1e-5,
                    "dims={dims} seed={seed}: reference {reference} vs fast {fast}"
                );
            }
        }
    }

    /// The dispatched kernel must agree with the scalar reference. Lengths
    /// deliberately straddle the 4/8/16-element strides so the tail loop after
    /// `chunks_exact` is exercised, including inputs shorter than one vector.
    ///
    /// **This covers one SIMD level: whichever this host has.** When the
    /// backends were hand-written they could be called directly, so a single
    /// run checked all of them. `fearless_simd`'s `dispatch!` normalises *up*
    /// to the best level the CPU supports and offers no runtime way down
    /// (`Level::__dispatch_target`), so the lower levels are now only
    /// reachable by rebuilding. A developer box and CI both run AVX2 or
    /// better, which would leave SSE2 — what every pre-Haswell CPU and every
    /// AVX2-masking VM executes — compiled, shipped and never executed.
    ///
    /// The `simd-levels` CI job is what restores that guarantee: it re-runs
    /// this module under each `--cfg disable_dispatch_*` combination. If you
    /// are debugging a level-specific result locally, that is the knob:
    ///
    /// ```text
    /// RUSTFLAGS='--cfg disable_dispatch_avx512 --cfg disable_dispatch_avx2 \
    ///            --cfg disable_dispatch_sse4_2' \
    ///   cargo nextest run -p engramdb --lib -E 'test(compress::tests)'
    /// ```
    #[test]
    fn dot_unit_backends_agree() {
        for dims in [1, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 384, 385, 768] {
            for seed in 0..4u64 {
                let a = l2_normalized(&synth_vector(seed * 2, dims));
                let b = l2_normalized(&synth_vector(seed * 2 + 1, dims));
                let reference = dot_unit_scalar(&a, &b);
                let dispatched = dot_unit(&a, &b);
                assert!(
                    (reference - dispatched).abs() < 1e-5,
                    "dims={dims} seed={seed}: scalar {reference} vs dispatched \
                     {dispatched} (level {:?})",
                    Level::new()
                );
            }
        }
    }

    /// A source that exists but cannot be READ must fail the compress, not be
    /// silently skipped.
    ///
    /// Regression guard for the `get` -> `get_batch` conversion. The per-id
    /// loops propagated a read error; `get_batch` warns and omits the row, so
    /// a naive conversion turns "this source is corrupt" into "this source is
    /// fine". Two places cared: the post-invalidation verification (which
    /// would report success while leaving a source active) and the audience
    /// union (which would publish the summary wider than its evidence).
    #[tokio::test]
    async fn unreadable_source_fails_compress_rather_than_being_skipped() {
        let (temp, store) = setup_store().await;
        let broken = add_memory(&store, MemoryType::Debug, "broken", 0.1, vec![]).await;
        let ok = add_memory(&store, MemoryType::Debug, "fine", 0.1, vec![]).await;

        // A directory where the memory file should be: the id still "exists"
        // on disk (batch_exists sees the stem) but no read can succeed.
        let memories_dir = temp.path().join(".engramdb").join("memories");
        for entry in std::fs::read_dir(&memories_dir).unwrap() {
            let path = entry.unwrap().path();
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.contains(&broken))
            {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
        }

        let err = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec![broken.clone(), ok.clone()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains(&broken),
            "the unreadable source must be named in the error, got: {msg}"
        );
        assert!(
            !msg.contains(&ok),
            "the readable source must not be reported as failed, got: {msg}"
        );
    }

    /// Max-over-chunk-pairs: two memories are similar if ANY chunk of one is
    /// similar to ANY chunk of the other.
    ///
    /// This is the aggregation choice, so it gets a test that would fail under
    /// mean (which would average the strong match away) and under
    /// first-chunk-only (which would miss it entirely).
    #[test]
    fn similar_pairs_aggregates_chunks_by_max() {
        let dims = 384;
        let shared = synth_vector(7, dims);
        let noise_a = synth_vector(100, dims);
        let noise_b = synth_vector(200, dims);
        let noise_c = synth_vector(300, dims);

        // Memory 0 and memory 1 share one near-identical chunk each, buried
        // behind unrelated ones. Memory 2 shares nothing.
        let vectors = vec![
            vec![noise_a.clone(), shared.clone()],
            vec![shared.clone(), noise_b.clone()],
            vec![noise_c.clone()],
        ];

        // 0.99: only the shared pair clears it, so the assertion is about the
        // aggregation and not about the corpus happening to be similar.
        let pairs = similar_pairs(&vectors, 0.99);
        assert_eq!(
            pairs,
            vec![(0, 1)],
            "max over chunk pairs must find the shared chunk; mean or \
             first-chunk-only would miss it"
        );

        // The same corpus with the shared chunks removed must find nothing —
        // otherwise the test above proves nothing.
        let unrelated = vec![
            vec![noise_a.clone()],
            vec![noise_b.clone()],
            vec![noise_c.clone()],
        ];
        assert!(similar_pairs(&unrelated, 0.99).is_empty());
    }

    /// A memory with no stored vectors participates in no pair rather than
    /// panicking or matching everything.
    #[test]
    fn similar_pairs_skips_memories_without_vectors() {
        let dims = 384;
        let v = synth_vector(1, dims);
        let vectors = vec![vec![v.clone()], Vec::new(), vec![v.clone()]];
        // 0 and 2 are identical; 1 has nothing.
        assert_eq!(similar_pairs(&vectors, 0.99), vec![(0, 2)]);
    }

    /// Degenerate inputs must behave like the reference, not panic or produce
    /// a NaN that would then poison the `>= similarity` comparison.
    #[test]
    fn dot_unit_handles_degenerate_vectors() {
        let zero = vec![0.0f32; 384];
        let v = synth_vector(1, 384);
        assert_eq!(dot_unit(&l2_normalized(&zero), &l2_normalized(&v)), 0.0);
        assert_eq!(dot_unit(&l2_normalized(&zero), &l2_normalized(&zero)), 0.0);
        // Length mismatch and empty input are guards, matching `cosine`.
        assert_eq!(dot_unit(&v, &synth_vector(2, 128)), 0.0);
        assert_eq!(dot_unit(&[], &[]), 0.0);
        // A unit vector against itself is 1.0.
        let unit = l2_normalized(&v);
        assert!((dot_unit(&unit, &unit) - 1.0).abs() < 1e-5);
    }

    /// `similar_pairs` must return exactly what the sequential nested loop
    /// returned, in the same order — including the `None` (failed-to-embed)
    /// holes, which take part in no pair.
    #[test]
    fn similar_pairs_matches_sequential_reference() {
        let dims = 384;
        // One vector per memory here, so max-over-chunk-pairs degenerates to
        // the plain pairwise cosine the sequential reference computes. The
        // multi-chunk aggregation is covered by
        // `similar_pairs_aggregates_chunks_by_max`.
        let mut vectors: Vec<Vec<Vec<f32>>> = (0..40u64)
            .map(|i| vec![synth_vector(i % 12, dims)])
            .collect();
        // Memories with no stored vectors take part in no pair.
        vectors[3] = Vec::new();
        vectors[17] = Vec::new();

        // Threshold low enough that plenty of pairs qualify, so this is not
        // vacuously "both returned nothing".
        let similarity = 0.5;
        let mut expected: Vec<(usize, usize)> = Vec::new();
        for i in 0..vectors.len() {
            for j in (i + 1)..vectors.len() {
                if let (Some(a), Some(b)) = (vectors[i].first(), vectors[j].first()) {
                    if cosine(a, b) >= similarity {
                        expected.push((i, j));
                    }
                }
            }
        }
        assert!(
            !expected.is_empty(),
            "test corpus produced no pairs; the comparison would be vacuous"
        );
        assert_eq!(similar_pairs(&vectors, similarity), expected);
    }

    async fn add_memory(
        store: &MemoryStore,
        type_: MemoryType,
        summary: &str,
        criticality: f64,
        logical: Vec<String>,
    ) -> String {
        let result = create_memory(
            store,
            CreateParams {
                type_,
                content: format!("Content for {}", summary),
                summary: summary.to_string(),
                title: None,
                physical: vec!["/".to_string()],
                logical,
                tags: vec![],
                criticality,
                confidence: 0.8,
                details: None,
                visibility: Visibility::Shared,
                provenance: Provenance::human(),
                supersedes: vec![],
                audience: None,
                epistemic: None,
                premise: None,
                invalidated_by: vec![],
                origin_task: None,
                generality: None,
                valid_from: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                title_strategy: TitleStrategy::None,
                embed_async: false,
            },
            None,
        )
        .await
        .unwrap();
        result.id
    }

    #[tokio::test]
    async fn test_compress_candidates_basic() {
        let (_temp, store) = setup_store().await;

        add_memory(&store, MemoryType::Debug, "low crit debug", 0.1, vec![]).await;
        add_memory(
            &store,
            MemoryType::Decision,
            "high crit decision",
            0.9,
            vec![],
        )
        .await;
        add_memory(
            &store,
            MemoryType::Context,
            "medium crit context",
            0.3,
            vec![],
        )
        .await;

        let result = compress_candidates(&store, None, Some(0.4)).await.unwrap();

        assert_eq!(result.total, 2);
        assert_eq!(result.threshold, 0.4);
        let summaries: Vec<&str> = result
            .candidates
            .iter()
            .map(|c| c.summary.as_str())
            .collect();
        assert!(summaries.contains(&"low crit debug"));
        assert!(summaries.contains(&"medium crit context"));
        assert!(!summaries.contains(&"high crit decision"));
    }

    #[tokio::test]
    async fn test_compress_candidates_scope_filter() {
        let (_temp, store) = setup_store().await;

        add_memory(
            &store,
            MemoryType::Debug,
            "auth debug",
            0.1,
            vec!["auth".to_string()],
        )
        .await;
        add_memory(
            &store,
            MemoryType::Debug,
            "db debug",
            0.1,
            vec!["db".to_string()],
        )
        .await;

        let result = compress_candidates(&store, Some("auth"), Some(0.4))
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.candidates[0].summary, "auth debug");
    }

    #[tokio::test]
    async fn test_compress_candidates_empty() {
        let (_temp, store) = setup_store().await;

        add_memory(&store, MemoryType::Decision, "important", 0.9, vec![]).await;

        let result = compress_candidates(&store, None, Some(0.4)).await.unwrap();
        assert_eq!(result.total, 0);
        assert!(result.candidates.is_empty());
    }

    #[tokio::test]
    async fn test_compress_apply_basic() {
        let (_temp, store) = setup_store().await;

        let id1 = add_memory(&store, MemoryType::Debug, "debug 1", 0.1, vec![]).await;
        let id2 = add_memory(&store, MemoryType::Debug, "debug 2", 0.2, vec![]).await;

        let result = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec![id1.clone(), id2.clone()],
                summary: "Combined debug summary".to_string(),
                content: "Merged content from debug 1 and 2".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.superseded_count, 2);
        assert!(
            result.skipped_sources.is_empty(),
            "all sources existed, nothing should be skipped"
        );

        // Verify the new memory exists and has correct supersedes
        let new_memory = store.get(&result.new_id).await.unwrap();
        assert_eq!(new_memory.type_, MemoryType::Context);
        assert_eq!(new_memory.summary, "Combined debug summary");
        assert!(new_memory.supersedes.contains(&id1));
        assert!(new_memory.supersedes.contains(&id2));

        // Both sources survive on disk with CLOSED validity windows (§2.4
        // writer 3) — invalidated, superseded by the summary, not deleted.
        for id in [&id1, &id2] {
            let source = store.get(id).await.unwrap();
            assert!(source.invalidated_at.is_some(), "window must be closed");
            assert_eq!(
                source.superseded_by.as_deref(),
                Some(result.new_id.as_str())
            );
        }
    }

    /// A source whose index row is missing (half-deleted by a crash) is
    /// still invalidated through its on-disk file — the window-closing pass
    /// operates on files, so nothing is skipped and the summary stays valid.
    #[tokio::test]
    async fn test_compress_apply_source_without_index_row_still_invalidated() {
        let (temp, store) = setup_store().await;

        let real_id = add_memory(&store, MemoryType::Debug, "real source", 0.1, vec![]).await;

        let ghost = Memory::new(
            MemoryType::Debug,
            "ghost source",
            "gone before deletion",
            Provenance::human(),
        );
        let ghost_id = ghost.id.clone();
        let content = crate::storage::memory_file::write_memory_file(&ghost).unwrap();
        let memories_dir = temp.path().join(".engramdb").join("memories");
        std::fs::write(memories_dir.join(format!("{}.md", ghost_id)), content).unwrap();

        let result = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec![real_id.clone(), ghost_id.clone()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.superseded_count, 2);
        assert!(
            result.skipped_sources.is_empty(),
            "a file-backed source is invalidatable even without an index row"
        );

        let new_memory = store.get(&result.new_id).await.unwrap();
        assert!(new_memory.supersedes.contains(&real_id));
        assert!(new_memory.supersedes.contains(&ghost_id));

        // Both sources were invalidated in place, not deleted.
        for id in [&real_id, &ghost_id] {
            let source = store.get(id).await.unwrap();
            assert!(source.invalidated_at.is_some());
        }
    }

    /// A REAL invalidation error (I/O) must not abort the sweep mid-way:
    /// the remaining sources are still processed, and the returned error
    /// lists the still-active IDs plus the (valid) new memory's ID.
    ///
    /// Failure injection: the first source's `.md` file is replaced by a
    /// directory of the same name — reading/rewriting it fails with a
    /// genuine I/O error rather than NotFound, and it works regardless of
    /// the user the tests run as (unlike chmod tricks, which root ignores).
    #[tokio::test]
    async fn test_compress_apply_continues_past_real_invalidate_failure() {
        let (temp, store) = setup_store().await;

        let broken_id = add_memory(&store, MemoryType::Debug, "undeletable", 0.1, vec![]).await;
        let ok_id = add_memory(&store, MemoryType::Debug, "deletable", 0.1, vec![]).await;

        // Replace broken's file with a same-named directory.
        let memories_dir = temp.path().join(".engramdb").join("memories");
        for entry in std::fs::read_dir(&memories_dir).unwrap() {
            let path = entry.unwrap().path();
            let is_broken = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.contains(&broken_id))
                .unwrap_or(false);
            if is_broken {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
        }

        // broken first, ok second: proves the loop continues past the failure.
        let err = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec![broken_id.clone(), ok_id.clone()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("could not be invalidated"),
            "partial failure must be reported: {}",
            msg
        );
        assert!(
            msg.contains(&broken_id),
            "error must list the still-active id: {}",
            msg
        );
        assert!(
            !msg.contains(&ok_id),
            "successfully invalidated source must not be listed as failed: {}",
            msg
        );

        // The later source was still processed and invalidated.
        assert!(store.get(&ok_id).await.unwrap().invalidated_at.is_some());

        // The summary memory was created and remains valid.
        let entries = store.list_filterable().await.unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.summary == "Summary" && e.type_ == MemoryType::Context),
            "summary memory must exist despite the partial deletion failure"
        );
    }

    #[tokio::test]
    async fn test_compress_apply_invalid_source() {
        let (_temp, store) = setup_store().await;

        let result = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec!["nonexistent-id".to_string()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Source memory not found"));
    }

    #[tokio::test]
    async fn test_compress_candidates_default_threshold_is_0_4() {
        let (_temp, store) = setup_store().await;

        add_memory(&store, MemoryType::Debug, "low crit", 0.3, vec![]).await;
        add_memory(&store, MemoryType::Decision, "high crit", 0.9, vec![]).await;

        let result = compress_candidates(&store, None, None).await.unwrap();

        assert_eq!(result.threshold, 0.4);
        assert_eq!(result.total, 1);
    }

    #[tokio::test]
    async fn test_compress_candidates_includes_equal_to_threshold() {
        let (_temp, store) = setup_store().await;

        add_memory(&store, MemoryType::Debug, "below threshold", 0.39, vec![]).await;
        add_memory(&store, MemoryType::Debug, "at threshold", 0.4, vec![]).await;
        add_memory(&store, MemoryType::Debug, "above threshold", 0.41, vec![]).await;

        let result = compress_candidates(&store, None, Some(0.4)).await.unwrap();

        assert_eq!(result.total, 2);
        let summaries: Vec<&str> = result
            .candidates
            .iter()
            .map(|c| c.summary.as_str())
            .collect();
        assert!(summaries.contains(&"below threshold"));
        assert!(summaries.contains(&"at threshold"));
        assert!(!summaries.contains(&"above threshold"));
    }

    #[tokio::test]
    async fn test_compress_candidates_physical_scope_match() {
        let (_temp, store) = setup_store().await;

        // Create memory with specific physical scope using Memory::new directly
        let mut mem_auth = Memory::new(
            MemoryType::Debug,
            "auth debug",
            "Auth content",
            Provenance::human(),
        );
        mem_auth.physical = vec!["/src/auth/".to_string()];
        mem_auth.criticality = 0.1;
        store.create(&mem_auth).await.unwrap();

        let mut mem_db = Memory::new(
            MemoryType::Debug,
            "db debug",
            "DB content",
            Provenance::human(),
        );
        mem_db.physical = vec!["/src/db/".to_string()];
        mem_db.criticality = 0.1;
        store.create(&mem_db).await.unwrap();

        let result = compress_candidates(&store, Some("/src/auth/"), Some(0.4))
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.candidates[0].summary, "auth debug");
    }

    #[tokio::test]
    async fn test_compress_apply_empty_source_ids_returns_error() {
        let (_temp, store) = setup_store().await;

        let result = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec![],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must not be empty"));
    }

    #[tokio::test]
    async fn test_compress_apply_creates_context_type_with_agent_provenance() {
        let (_temp, store) = setup_store().await;

        let id = add_memory(&store, MemoryType::Debug, "source debug", 0.1, vec![]).await;

        let result = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec![id],
                summary: "Compressed summary".to_string(),
                content: "Compressed content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await
        .unwrap();

        let new_memory = store.get(&result.new_id).await.unwrap();
        assert_eq!(new_memory.type_, MemoryType::Context);
        assert_eq!(new_memory.provenance.source, ProvenanceSource::Agent);
        assert_eq!(new_memory.provenance.agent_id, Some("compress".to_string()));
    }

    #[tokio::test]
    async fn test_compress_apply_with_scope_and_tags() {
        let (_temp, store) = setup_store().await;

        let id = add_memory(&store, MemoryType::Debug, "source", 0.1, vec![]).await;

        let result = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec![id],
                summary: "Scoped summary".to_string(),
                content: "Scoped content".to_string(),
                scope: Some(vec!["app.auth".to_string(), "app.core".to_string()]),
                tags: Some(vec!["compressed".to_string(), "auth".to_string()]),
                embed_async: false,
            },
            None,
        )
        .await
        .unwrap();

        let new_memory = store.get(&result.new_id).await.unwrap();
        assert_eq!(
            new_memory.logical,
            vec!["app.auth".to_string(), "app.core".to_string()]
        );
        assert_eq!(
            new_memory.tags,
            vec!["compressed".to_string(), "auth".to_string()]
        );
    }

    #[tokio::test]
    async fn test_compress_apply_partial_invalid_source_ids_returns_error() {
        let (_temp, store) = setup_store().await;

        let valid_id = add_memory(&store, MemoryType::Debug, "valid source", 0.1, vec![]).await;
        let count_before = store.count().await.unwrap();

        let result = compress_apply(
            &store,
            CompressApplyParams {
                source_ids: vec![valid_id, "nonexistent-id".to_string()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await;

        assert!(result.is_err());
        // Verify no new memory was created
        let count_after = store.count().await.unwrap();
        assert_eq!(count_before, count_after);
    }
}

// ---------------------------------------------------------------------------
// Consolidation (§11.4): observation clusters → derived fact
// ---------------------------------------------------------------------------

/// One consolidation candidate cluster.
#[derive(Debug, Clone)]
pub struct ConsolidationCluster {
    pub source_ids: Vec<String>,
    pub summaries: Vec<String>,
}

/// Report from one consolidation pass.
#[derive(Debug, Default)]
pub struct ConsolidationReport {
    /// Candidate clusters (suggestion mode reports these; apply mode also
    /// records what it created).
    pub clusters: Vec<ConsolidationCluster>,
    /// Ids of the Fact memories created (apply mode only).
    pub created: Vec<String>,
    /// True when embedding/NLI providers were unavailable and the pass
    /// skipped (§14.11 graceful-skip contract).
    pub skipped_no_providers: bool,
    /// True when the store had more active observations than one throttled
    /// pass will pairwise-compare (O(n²) bound); nothing was clustered.
    pub skipped_too_many: bool,
}

/// Union-find clustering over similarity pairs. Returns clusters of size ≥
/// `min_size`, each sorted ascending. Pure so the geometry is testable
/// without providers.
pub fn cluster_pairs(n: usize, pairs: &[(usize, usize)], min_size: usize) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut Vec<usize>, x: usize) -> usize {
        if parent[x] != x {
            let root = find(parent, parent[x]);
            parent[x] = root;
        }
        parent[x]
    }
    for &(a, b) in pairs {
        if a >= n || b >= n {
            continue;
        }
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    let mut clusters: Vec<Vec<usize>> = groups
        .into_values()
        .filter(|g| g.len() >= min_size.max(2))
        .collect();
    for c in &mut clusters {
        c.sort_unstable();
    }
    clusters.sort_by_key(|c| c[0]);
    clusters
}

/// The straightforward cosine: one `f64` accumulator chain, norms recomputed
/// per call.
///
/// Superseded on the hot path by [`l2_normalized`] + [`dot_unit`], and kept as
/// the reference the equivalence test scores against — the fast path is only
/// worth having if it agrees with this to floating-point tolerance.
#[cfg(test)]
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// A memory's embedding scaled to unit length, so that the cosine of two of
/// them is just their dot product.
///
/// `consolidation_pass` compares every observation against every other one,
/// and [`cosine`] recomputes `‖a‖` and `‖b‖` inside each of those n(n-1)/2
/// comparisons even though a vector has exactly one norm. Hoisting the norms
/// into an O(n) prepass removes two thirds of the arithmetic, and leaves the
/// O(n²) body a bare dot product.
///
/// `‖v‖²` is itself a dot product, so it reuses [`dot_unit`] and gets the same
/// vector instructions rather than a second scalar reduction. The scaling pass
/// is left scalar deliberately: this whole function is O(n) against the O(n²)
/// body it feeds — at the 500-observation cap it is ~0.4% of the pass — so a
/// third set of per-architecture intrinsics would be maintenance for a
/// rounding error.
///
/// Returns the vector unchanged when it has no length — matching [`cosine`],
/// which reports `0.0` similarity for a zero vector, since `dot_unit` against
/// an unscaled zero vector is likewise `0.0`.
pub fn l2_normalized(v: &[f32]) -> Vec<f32> {
    let norm = (dot_unit(v, v) as f32).sqrt();
    if norm == 0.0 || !norm.is_finite() {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// Cosine similarity of two [`l2_normalized`] vectors: their dot product.
///
/// `pub` so `benches/parallel_simd.rs` measures *this* function rather than a
/// copy. A benchmark that reimplements the code it claims to measure silently
/// stops being true the moment production changes — which had already happened
/// here once.
///
/// Written with explicit SIMD — via [`fearless_simd`] — rather than a shape
/// the auto-vectorizer might pick up, and that is *not* a consequence of the
/// optimization level. A plain `f32` reduction does not vectorize at `-Oz`,
/// at `2`, or at `3`: IEEE addition is not associative, so LLVM will not
/// reassociate the accumulator without fast-math. Measured, 384-dim pairs, the
/// scalar loop is 441 / 442 / 446 ns at those three levels — flat.
///
/// What the optimization level *does* decide is whether a safe SIMD
/// abstraction can be used at all. Under the old `opt-level = "z"` profile
/// nothing inlined, so this function was four hand-written `std::arch`
/// backends and portable crates measured 1.3-7x slower than them. At the
/// current `opt-level = 2` that reverses: one generic kernel over
/// `fearless_simd` is *faster* than the intrinsics it replaced, and this was
/// the last `unsafe` in EngramDB's own logic.
///
/// Measured with the real release profile (`lto = true`, `codegen-units = 1`),
/// 384-dim pairs, interleaved (`tools/simd-probe`):
///
/// | form | `-Oz` | `opt-level = 2` |
/// |---|---|---|
/// | plain scalar loop | 441 | 442 |
/// | eight unrolled accumulators (no SIMD) | 720 | — |
/// | old: SSE2 intrinsics | 61 | 60 |
/// | old: AVX2 + FMA intrinsics, dispatched | 50 | 34 |
/// | **this, `fearless_simd` x4 accumulators** | 185 | **34** |
///
/// Read the last two rows together: the profile change is what bought the
/// speed (50 -> 34), and it would have bought it for the intrinsics too. What
/// `fearless_simd` adds is that it matches them *and* deletes the `unsafe`.
///
/// Why not `fastembed::similarity::cosine_similarity`, which is already in the
/// tree? It cannot be reached from here — `fastembed` is an optional
/// dependency of `engram-models` gated behind `onnxruntime`, and this crate
/// must keep working under `--no-default-features --features ollama` — and it
/// is also the shape this replaced: three `dot` calls per comparison
/// (recomputing both norms every time) over a single non-vectorizing
/// accumulator chain. Measured at `-Oz`, 384-dim, identical results: 1358 ns
/// against 51 ns here.
///
/// Mismatched lengths score `0.0`, as in [`cosine`] — the callers pair vectors
/// from one provider, so this is a guard, not a code path.
pub fn dot_unit(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    dispatch!(Level::new(), simd => dot_unit_kernel(simd, a, b))
}

/// The kernel, generic over `fearless_simd`'s SIMD level.
///
/// `#[inline(always)]` is not optional: the crate creates target-feature
/// contexts by inlining this body into a per-level wrapper, so without it the
/// vector work compiles for the baseline and the whole thing is pointless.
///
/// **Four accumulators, not two.** Two was the obvious choice — it matches the
/// old hand-written backends — and it left ~10% on the table against them. A
/// 512-bit `mul_add` has enough latency that two dependency chains do not fill
/// the pipeline; four do, and that alone is what took this from 0.90x of the
/// intrinsics to 1.02x. Measured, not reasoned: `tools/simd-probe` carries the
/// 1/2/4-accumulator variants side by side.
///
/// The loop shape is the crate's documented idiom — zip two `chunks_exact`
/// iterators — from its own `sigmoid.rs` example. Hand-rolled `&a[i..i + w]`
/// indexing was measured too and is slower here.
///
/// There is no horizontal-sum in `fearless_simd` v0.7.0 (no `reduce_sum`, and
/// `SimdSplit` is not among `S::f32s`' bounds, so a log-depth fold cannot even
/// be written generically), hence the scalar walk over the lanes at the end.
/// It runs once per call, not once per element.
#[inline(always)]
fn dot_unit_kernel<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    let n = S::f32s::N;
    let step = n * 4;
    let mut acc = [S::f32s::splat(simd, 0.0); 4];
    let (mut ca, mut cb) = (a.chunks_exact(step), b.chunks_exact(step));
    for (x, y) in (&mut ca).zip(&mut cb) {
        for k in 0..4 {
            acc[k] = S::f32s::from_slice(simd, &x[k * n..(k + 1) * n])
                .mul_add(S::f32s::from_slice(simd, &y[k * n..(k + 1) * n]), acc[k]);
        }
    }
    let mut dot: f32 = ((acc[0] + acc[1]) + (acc[2] + acc[3]))
        .as_slice()
        .iter()
        .sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
        dot += x * y;
    }
    dot as f64
}

/// The scalar reference the kernel is scored against.
///
/// Production no longer has a scalar path — `fearless_simd` supplies a
/// `Fallback` level on targets with no SIMD baseline — but the equivalence
/// tests need something to compare to that is obviously correct.
#[cfg(test)]
fn dot_unit_scalar(a: &[f32], b: &[f32]) -> f64 {
    let mut dot = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
    }
    dot as f64
}

/// Every pair of observations whose embeddings are at least `similarity`
/// alike, as `(i, j)` index pairs with `i < j`.
///
/// This is the O(n²) heart of [`consolidation_pass`]. Three things make it
/// cheap enough to keep running inline in the maintenance pass:
///
/// 1. norms hoisted out of the inner loop ([`l2_normalized`]),
/// 2. an inner product split into independent accumulator chains
///    ([`dot_unit`]),
/// 3. the outer index spread across the rayon pool.
///
/// Row `i` does `n - i` comparisons, so the per-index work is triangular;
/// rayon's work stealing absorbs that without manual chunking. Results are
/// collected per outer index and concatenated in order, so the pair list is
/// identical to the sequential one — `cluster_pairs` is order-insensitive
/// anyway, but a stable order keeps the reports reproducible.
///
/// A memory with no vectors takes part in no pair, exactly as before.
///
/// **Aggregation is max over chunk pairs.** A memory is represented by several
/// vectors (a metadata row plus one per content chunk), so "how similar are
/// these two memories" needs a reduction. Max matches what the query path
/// already does when it aggregates chunk hits by `memory_id`
/// (`lance_index::vector_search`), so consolidation and search agree on what
/// "similar" means. Mean would dilute a strong match on one chunk against
/// unrelated chunks in a long memory — precisely the case consolidation is
/// looking for.
pub fn similar_pairs(vectors: &[Vec<Vec<f32>>], similarity: f64) -> Vec<(usize, usize)> {
    use rayon::prelude::*;

    // Normalize every chunk once. With `k` chunks per memory the inner loop is
    // k_i * k_j dot products instead of one, so hoisting the norms matters
    // more here than it did in the single-vector version, not less.
    let unit: Vec<Vec<Vec<f32>>> = vectors
        .iter()
        .map(|chunks| chunks.iter().map(|v| l2_normalized(v)).collect())
        .collect();

    (0..unit.len())
        .into_par_iter()
        .map(|i| {
            if unit[i].is_empty() {
                return Vec::new();
            }
            ((i + 1)..unit.len())
                .filter(|&j| {
                    unit[j]
                        .iter()
                        .any(|b| unit[i].iter().any(|a| dot_unit(a, b) >= similarity))
                })
                .map(|j| (i, j))
                .collect::<Vec<_>>()
        })
        .reduce(Vec::new, |mut acc, mut v| {
            acc.append(&mut v);
            acc
        })
}

/// §11.4 consolidation pass: find clusters of ≥
/// `[epistemic] consolidation_min_sources` Active observation-class memories
/// with pairwise embedding similarity ≥ `consolidation_similarity` and no
/// pairwise NLI contradiction. Suggestion-first: clusters are returned;
/// `apply` (the `[epistemic] auto_consolidate` path) additionally creates
/// the derived Fact and demotes the sources.
///
/// Model-dependent steps run only where providers already run — with no
/// embedding or NLI provider the pass skips gracefully with a logged notice.
pub async fn consolidation_pass(
    store: &MemoryStore,
    engine: &crate::retrieval::engine::RetrievalEngine,
    config: &crate::types::EngramConfig,
    apply: bool,
) -> Result<ConsolidationReport> {
    use crate::types::{Epistemic, Status};

    let mut report = ConsolidationReport::default();
    if !engine.embeddings_available() || !engine.nli_available() {
        tracing::info!(
            "consolidation: skipped — embedding/NLI providers unavailable (graceful skip)"
        );
        report.skipped_no_providers = true;
        return Ok(report);
    }

    let min_sources = config.epistemic.consolidation_min_sources;
    let similarity = config.epistemic.consolidation_similarity;
    let ids = store.list_ids().await?;
    let loaded = store.get_batch(&ids).await?;
    let now = chrono::Utc::now();

    // Idempotence: observations already consumed by a live derived fact must
    // not re-cluster — without this, every throttled maintenance pass would
    // mint a duplicate fact from the same (demoted-but-Active) sources. If
    // the derived fact is later invalidated, its sources become eligible
    // again, which is the desired "re-derive after retraction" behavior.
    let already_derived: std::collections::HashSet<&str> = loaded
        .iter()
        .filter(|(_, m)| !m.is_invalidated_at(now))
        .filter_map(|(_, m)| m.valid_while.as_ref())
        .flat_map(|v| v.derived_from.iter().map(String::as_str))
        .collect();

    let observations: Vec<&(String, crate::types::Memory)> = loaded
        .iter()
        .filter(|(id, m)| {
            m.epistemic == Epistemic::Observation
                && m.status == Status::Active
                && !m.is_invalidated_at(now)
                && !already_derived.contains(id.as_str())
        })
        .collect();
    if observations.len() < min_sources.max(2) {
        return Ok(report);
    }
    // Pairwise-similarity bound: n observations cost n(n-1)/2 cosines. Past
    // this size the throttled maintenance pass is the wrong tool — defer with
    // a notice instead of stalling (same gated-O(n²) discipline as #58).
    const MAX_OBSERVATIONS_PER_PASS: usize = 500;
    if observations.len() > MAX_OBSERVATIONS_PER_PASS {
        tracing::info!(
            count = observations.len(),
            "consolidation: more than {MAX_OBSERVATIONS_PER_PASS} active observations; \
             skipping this pass (use compress for bulk cleanup)"
        );
        report.skipped_too_many = true;
        return Ok(report);
    }

    // Vectors come from the chunk table — the ones the write path already
    // produced — rather than being re-derived here.
    //
    // This is a correctness fix as much as a speed one. The pass used to embed
    // `format!("{summary} {content}")` as ONE string, which the tokenizer
    // truncates at the model's `max_length`: long observations were compared
    // on their first ~256 tokens and nothing else, and `title`/`tags` never
    // entered the comparison at all. The write path meanwhile chunks
    // (`embedding_texts` -> `chunk_text`) and emits a metadata row carrying
    // exactly those fields. Two compositions for the same store, and only one
    // of them was the documented one. Reading the stored vectors makes this
    // pass see what search sees.
    //
    // Reading is also now ~8x faster than re-embedding (`export_chunks_batch`,
    // one scan instead of one per memory).
    let obs_ids: Vec<&str> = observations.iter().map(|(id, _)| id.as_str()).collect();
    let mut stored = store
        .export_chunks_batch(&obs_ids)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("consolidation: batched chunk read failed ({e}); embedding on demand");
            std::collections::HashMap::new()
        });

    // Fallback for memories with no stored vectors — created before embedding
    // was available, or written while the provider was down. Embedding them
    // here keeps them eligible instead of silently dropping them from
    // consolidation, and uses `embedding_texts` so the composition matches
    // what the write path would have stored.
    let missing: Vec<&crate::types::Memory> = observations
        .iter()
        .map(|(_, m)| m)
        .filter(|m| !stored.contains_key(&m.id))
        .collect();
    if !missing.is_empty() {
        tracing::debug!(
            count = missing.len(),
            "consolidation: embedding memories with no stored vectors"
        );
        let owned: Vec<crate::types::Memory> = missing.into_iter().cloned().collect();
        for (id, chunks) in engine.embed_memory_texts(&owned).await {
            stored.insert(id, chunks);
        }
    }

    let vectors: Vec<Vec<Vec<f32>>> = observations
        .iter()
        .map(|(id, _)| stored.remove(id).unwrap_or_default())
        .collect();

    // Pairwise similarity → union-find clusters.
    let pairs = similar_pairs(&vectors, similarity);
    let clusters = cluster_pairs(observations.len(), &pairs, min_sources);

    // Pairwise-NLI bound per cluster: k sources cost k(k-1)/2 cross-encoder
    // inferences, so an unbounded near-duplicate cluster would stall the
    // (synchronous) maintenance pass for minutes. Oversized clusters are
    // deferred with a notice rather than half-checked — mirroring the
    // gated-O(n²) discipline from the workspace robustness pass (#58).
    const MAX_CLUSTER_SOURCES: usize = 12;

    for cluster in clusters {
        if cluster.len() > MAX_CLUSTER_SOURCES {
            tracing::info!(
                size = cluster.len(),
                "consolidation: cluster exceeds {MAX_CLUSTER_SOURCES} sources; skipping this pass \
                 (compress it manually or raise consolidation_similarity)"
            );
            continue;
        }
        // NLI gate: any pairwise contradiction disqualifies the cluster
        // (contradictory observations are a dispute, not a consolidation).
        let mut nli_pairs: Vec<(&str, &str)> = Vec::new();
        for (pos, &i) in cluster.iter().enumerate() {
            for &j in &cluster[pos + 1..] {
                nli_pairs.push((
                    observations[i].1.summary.as_str(),
                    observations[j].1.summary.as_str(),
                ));
            }
        }
        let contradicted = match engine.nli_contradictions(&nli_pairs).await {
            Some(scores) => scores
                .iter()
                .any(|s| *s as f64 >= config.nli.contradiction_threshold),
            // NLI failed mid-pass: be conservative, skip the cluster.
            None => true,
        };
        if contradicted {
            continue;
        }

        let source_ids: Vec<String> = cluster.iter().map(|&i| observations[i].0.clone()).collect();
        let summaries: Vec<String> = cluster
            .iter()
            .map(|&i| observations[i].1.summary.clone())
            .collect();

        if apply {
            match consolidate_cluster_apply(store, &source_ids, Some(engine)).await {
                Ok(new_id) => report.created.push(new_id),
                Err(e) => {
                    tracing::warn!("consolidation apply failed for {source_ids:?}: {e}");
                    continue;
                }
            }
        }
        report.clusters.push(ConsolidationCluster {
            source_ids,
            summaries,
        });
    }
    Ok(report)
}

/// Apply one consolidation cluster (§11.4): create a Fact-class memory (type
/// `context` unless all sources share a type) with
/// `valid_while.derived_from = sources`, `provenance: inferred`,
/// criticality = max(sources), decay = none — then DEMOTE the sources
/// (decay → exponential 30d, floor 0.1). Sources are never deleted: they are
/// the evidence the §10.3 derived-from check depends on.
pub async fn consolidate_cluster_apply(
    store: &MemoryStore,
    source_ids: &[String],
    engine: Option<&crate::retrieval::engine::RetrievalEngine>,
) -> Result<String> {
    use crate::types::{Epistemic, Memory, MemoryType, Provenance, Validity};

    if source_ids.len() < 2 {
        bail!("a consolidation cluster needs at least 2 sources");
    }
    // One batched read for the cluster's sources.
    let src_refs: Vec<&str> = source_ids.iter().map(String::as_str).collect();
    let by_id: std::collections::HashMap<String, crate::types::Memory> =
        store.get_batch(&src_refs).await?.into_iter().collect();
    let mut sources = Vec::with_capacity(source_ids.len());
    for id in source_ids {
        sources.push(
            by_id
                .get(id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("source memory not found: {id}"))?,
        );
    }

    let all_same_type = sources.windows(2).all(|w| w[0].type_ == w[1].type_);
    let common_type = if all_same_type {
        sources[0].type_
    } else {
        MemoryType::Context
    };
    let criticality = sources.iter().map(|m| m.criticality).fold(0.0f64, |a, b| {
        if b.is_finite() {
            a.max(b)
        } else {
            a
        }
    });

    let summary_max_chars = engine
        .map(crate::retrieval::engine::RetrievalEngine::summary_max_chars)
        .unwrap_or(crate::types::DEFAULT_SUMMARY_MAX_CHARS);
    let mut summary = format!("Consolidated: {}", sources[0].summary);
    if summary.chars().count() > summary_max_chars {
        // Reserve 3 chars for the ellipsis so the result still fits the bound.
        let keep = summary_max_chars.saturating_sub(3);
        summary = summary.chars().take(keep).collect::<String>() + "...";
    }
    let content = sources
        .iter()
        .map(|m| format!("- {}", m.summary))
        .collect::<Vec<_>>()
        .join("\n");

    let mut fact = Memory::new(common_type, &summary, &content, Provenance::inferred());
    fact.epistemic = Epistemic::Fact;
    fact.criticality = criticality;
    fact.decay = Some(crate::types::Decay::none());
    fact.valid_while = Some(Validity {
        derived_from: source_ids.to_vec(),
        ..Default::default()
    });
    // Union the sources' scopes so the fact applies where its evidence did.
    let mut physical: Vec<String> = sources.iter().flat_map(|m| m.physical.clone()).collect();
    physical.sort();
    physical.dedup();
    fact.physical = physical;
    let mut logical: Vec<String> = sources.iter().flat_map(|m| m.logical.clone()).collect();
    logical.sort();
    logical.dedup();
    fact.logical = logical;

    // Shared-store hygiene: this path uses the raw `store.create` (not
    // `create_memory`), so it must itself strip repo-relative physical scope and
    // carry a safe (never-widening) audience — otherwise consolidation would
    // leak local paths and re-publish audience-restricted content store-wide.
    if store.is_group() || store.is_global() {
        fact.physical.clear();
        fact.audience = consolidated_audience(store, &sources);
    }

    let new_id = store.create(&fact).await?;

    // Embed the derived fact so it participates in vector search immediately
    // (plain `store.create` writes no vector). Best-effort: a failed embed
    // leaves the fact index-searchable until the next reindex.
    if let Some(engine) = engine {
        if engine.embeddings_available() {
            if let Ok(saved) = store.get(&new_id).await {
                if let Err(e) = engine.embed_memory(&saved).await {
                    tracing::warn!(memory_id = %new_id, "consolidated fact embed failed: {e}");
                }
            }
        }
    }

    // Demote sources: 30d exponential, floor 0.1 — evidence fades, never
    // vanishes.
    let (_, demote_failures) = store
        .update_batch_with(source_ids, |m| {
            m.decay =
                Some(crate::types::Decay::exponential(chrono::Duration::days(30)).with_floor(0.1));
            Ok(())
        })
        .await?;
    for (id, e) in demote_failures {
        tracing::warn!(memory_id = %id, "consolidation source demotion failed: {e}");
    }
    Ok(new_id)
}

#[cfg(test)]
mod consolidation_tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{DecayStrategy, Epistemic, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    #[test]
    fn cluster_pairs_union_find() {
        // 0-1-2 chained, 3-4 pair, 5 isolated.
        let pairs = [(0, 1), (1, 2), (3, 4)];
        let clusters = cluster_pairs(6, &pairs, 3);
        assert_eq!(clusters, vec![vec![0, 1, 2]]);
        let clusters = cluster_pairs(6, &pairs, 2);
        assert_eq!(clusters, vec![vec![0, 1, 2], vec![3, 4]]);
        // Out-of-range pairs are ignored; empty input yields nothing.
        assert!(cluster_pairs(2, &[(0, 5)], 2).is_empty());
        assert!(cluster_pairs(0, &[], 2).is_empty());
    }

    // Consolidating in a group store must NOT re-publish audience-restricted
    // content store-wide: the merged memory inherits the union of the sources'
    // restrictive audiences (a public source contributes nothing → over-restrict,
    // never leak), and its physical scope is stripped (cross-repo hygiene).
    #[tokio::test]
    async fn consolidation_preserves_restrictive_audience_in_group_store() {
        let gid = crate::storage::paths::compute_group_id("consolidate-audience-test");
        let store = MemoryStore::init_group(&gid).await.unwrap();

        let mut restricted = Memory::new(
            MemoryType::Convention,
            "rule a",
            "content a",
            Provenance::human(),
        );
        restricted.audience = Some(vec!["projX".to_string()]);
        restricted.physical = vec!["src/a.rs".to_string()];
        let id_a = store.create(&restricted).await.unwrap();

        // audience None (public within the group).
        let mut public = Memory::new(
            MemoryType::Convention,
            "rule b",
            "content b",
            Provenance::human(),
        );
        public.physical = vec!["src/b.rs".to_string()];
        let id_b = store.create(&public).await.unwrap();

        let new_id = consolidate_cluster_apply(&store, &[id_a, id_b], None)
            .await
            .unwrap();
        let merged = store.get(&new_id).await.unwrap();

        assert_eq!(
            merged.audience,
            Some(vec!["projX".to_string()]),
            "merged memory must stay restricted to the restrictive source's audience"
        );
        assert!(
            merged.physical.is_empty(),
            "group-store consolidation must strip repo-relative physical scope"
        );
    }

    #[tokio::test]
    async fn consolidation_skips_without_providers() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let engine = crate::retrieval::engine::RetrievalEngine::new(
            store.clone(),
            crate::types::EngramConfig::default(),
        );
        let config = crate::types::EngramConfig::default();
        let report = consolidation_pass(&store, &engine, &config, false)
            .await
            .unwrap();
        assert!(report.skipped_no_providers);
        assert!(report.clusters.is_empty());
    }

    #[tokio::test]
    async fn consolidate_cluster_apply_creates_fact_and_demotes_sources() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        for (id, crit) in [("con-a", 0.4), ("con-b", 0.7), ("con-c", 0.5)] {
            let mut m = Memory::new(
                MemoryType::Debug,
                format!("Observation {id}"),
                "body",
                Provenance::human(),
            );
            m.id = id.to_string();
            m.criticality = crit;
            m.physical = vec![format!("src/{id}.rs")];
            store.create(&m).await.unwrap();
        }

        let ids: Vec<String> = ["con-a", "con-b", "con-c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let new_id = consolidate_cluster_apply(&store, &ids, None).await.unwrap();

        let fact = store.get(&new_id).await.unwrap();
        assert_eq!(fact.epistemic, Epistemic::Fact);
        assert_eq!(fact.type_, MemoryType::Debug, "all sources share a type");
        assert_eq!(fact.criticality, 0.7, "max of sources");
        assert_eq!(
            fact.valid_while.as_ref().unwrap().derived_from,
            ids,
            "derivation links recorded for the §10.3 cascade"
        );
        assert_eq!(
            fact.provenance.source,
            crate::types::ProvenanceSource::Inferred
        );
        assert_eq!(fact.decay.as_ref().unwrap().strategy, DecayStrategy::None);
        assert_eq!(fact.physical.len(), 3, "scope union");

        // Sources demoted, never deleted.
        for id in &ids {
            let m = store.get(id).await.unwrap();
            let decay = m.decay.unwrap();
            assert_eq!(decay.strategy, DecayStrategy::Exponential);
            assert_eq!(decay.half_life, Some(chrono::Duration::days(30)));
            assert_eq!(decay.floor, 0.1);
        }

        // Mixed types fall back to Context.
        let mut other = Memory::new(MemoryType::Convention, "Other", "b", Provenance::human());
        other.id = "con-d".to_string();
        store.create(&other).await.unwrap();
        let mixed: Vec<String> = vec!["con-a".into(), "con-d".into()];
        let mixed_id = consolidate_cluster_apply(&store, &mixed, None)
            .await
            .unwrap();
        assert_eq!(
            store.get(&mixed_id).await.unwrap().type_,
            MemoryType::Context
        );
    }

    // --- Gate tests: stub providers so similarity + NLI gating is
    // --- deterministic without loading any real model.

    /// Deterministic embeddings: texts containing the same `group<X>` marker
    /// share an identical (cosine 1.0) vector; different markers are
    /// orthogonal (cosine 0.0).
    struct MarkerEmbedding;

    #[async_trait::async_trait]
    impl crate::embeddings::EmbeddingProvider for MarkerEmbedding {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; 384];
            if text.contains("groupA") {
                v[0] = 1.0;
            } else if text.contains("groupB") {
                v[1] = 1.0;
            } else {
                v[2] = 1.0;
            }
            Ok(v)
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(texts.len());
            for t in texts {
                out.push(self.embed(t).await?);
            }
            Ok(out)
        }
        fn dimensions(&self) -> usize {
            384
        }
        fn max_tokens(&self) -> usize {
            256
        }
        fn model_id(&self) -> String {
            "onnx/marker-stub".to_string()
        }
    }

    /// Stub NLI: any pair where either side contains "flaky" is a full
    /// contradiction; everything else is neutral.
    struct MarkerNli;

    #[async_trait::async_trait]
    impl crate::nli::NliProvider for MarkerNli {
        async fn classify(
            &self,
            premise: &str,
            hypothesis: &str,
        ) -> anyhow::Result<crate::nli::NliResult> {
            let contradicted = premise.contains("flaky") || hypothesis.contains("flaky");
            Ok(crate::nli::NliResult {
                label: if contradicted {
                    crate::nli::NliLabel::Contradiction
                } else {
                    crate::nli::NliLabel::Neutral
                },
                entailment: 0.0,
                neutral: if contradicted { 0.0 } else { 1.0 },
                contradiction: if contradicted { 1.0 } else { 0.0 },
            })
        }
        async fn classify_batch(
            &self,
            pairs: &[(&str, &str)],
        ) -> anyhow::Result<Vec<crate::nli::NliResult>> {
            let mut out = Vec::with_capacity(pairs.len());
            for (p, h) in pairs {
                out.push(self.classify(p, h).await?);
            }
            Ok(out)
        }
    }

    async fn gate_fixture() -> (
        TempDir,
        MemoryStore,
        crate::retrieval::engine::RetrievalEngine,
        crate::types::EngramConfig,
    ) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mut config = crate::types::EngramConfig::default();
        config.nli.enabled = true;
        let engine = crate::retrieval::engine::RetrievalEngine::new(store.clone(), config.clone())
            .with_embedding_provider(std::sync::Arc::new(MarkerEmbedding))
            .with_nli_provider(std::sync::Arc::new(MarkerNli));
        (tmp, store, engine, config)
    }

    async fn observation(store: &MemoryStore, id: &str, summary: &str) {
        // Debug is diagonally Observation-class.
        let mut m = Memory::new(MemoryType::Debug, summary, summary, Provenance::human());
        m.id = id.to_string();
        store.create(&m).await.unwrap();
    }

    /// The §11.4 similarity gate: only observations whose embeddings clear
    /// `consolidation_similarity` cluster; sub-threshold (orthogonal)
    /// observations never do, and clusters below `consolidation_min_sources`
    /// are dropped.
    #[tokio::test]
    async fn consolidation_gate_clusters_by_similarity_only() {
        let (_t, store, engine, config) = gate_fixture().await;

        for id in ["ga-1", "ga-2", "ga-3"] {
            observation(&store, id, &format!("groupA behavior seen in {id}")).await;
        }
        // Only two of these — below min_sources (3) — plus orthogonal class.
        for id in ["gb-1", "gb-2"] {
            observation(&store, id, &format!("groupB behavior seen in {id}")).await;
        }

        let report = consolidation_pass(&store, &engine, &config, false)
            .await
            .unwrap();
        assert!(!report.skipped_no_providers);
        assert_eq!(report.clusters.len(), 1, "only the 3-strong groupA cluster");
        let mut ids = report.clusters[0].source_ids.clone();
        ids.sort();
        assert_eq!(ids, vec!["ga-1", "ga-2", "ga-3"]);
        assert!(report.created.is_empty(), "suggestion mode creates nothing");
    }

    /// The §11.4 NLI gate: a similarity cluster containing a pairwise
    /// contradiction is a dispute, not a consolidation — it must be dropped.
    #[tokio::test]
    async fn consolidation_gate_rejects_contradicting_cluster() {
        let (_t, store, engine, config) = gate_fixture().await;

        observation(&store, "gc-1", "groupA the cache is fast").await;
        observation(&store, "gc-2", "groupA the cache is quick").await;
        // Same embedding group, but the stub NLI contradicts this one.
        observation(&store, "gc-3", "groupA the cache is flaky").await;

        let report = consolidation_pass(&store, &engine, &config, false)
            .await
            .unwrap();
        assert!(
            report.clusters.is_empty(),
            "contradicting cluster must not consolidate: {:?}",
            report.clusters
        );
    }

    /// Idempotence: after an applied consolidation, the (still-Active,
    /// demoted) sources must not re-cluster on the next pass — one derived
    /// fact, not one per maintenance interval.
    #[tokio::test]
    async fn consolidation_apply_is_idempotent_across_passes() {
        let (_t, store, engine, config) = gate_fixture().await;

        for id in ["gi-1", "gi-2", "gi-3"] {
            observation(&store, id, &format!("groupA metric drift in {id}")).await;
        }

        let first = consolidation_pass(&store, &engine, &config, true)
            .await
            .unwrap();
        assert_eq!(first.created.len(), 1, "first pass consolidates");
        let fact = store.get(&first.created[0]).await.unwrap();
        assert_eq!(fact.epistemic, Epistemic::Fact);

        let second = consolidation_pass(&store, &engine, &config, true)
            .await
            .unwrap();
        assert!(
            second.created.is_empty() && second.clusters.is_empty(),
            "consumed sources must not re-cluster: {:?}",
            second.clusters
        );

        // Invalidating the derived fact frees its sources to re-derive.
        store
            .invalidate_with(&first.created[0], None, chrono::Utc::now())
            .await
            .unwrap();
        let third = consolidation_pass(&store, &engine, &config, false)
            .await
            .unwrap();
        assert_eq!(
            third.clusters.len(),
            1,
            "retracted derivation reopens the cluster"
        );
    }

    /// O(n²) bound (#58 discipline): a cluster larger than the per-pass NLI
    /// budget is deferred with a notice, not half-checked or consolidated.
    #[tokio::test]
    async fn consolidation_defers_oversized_clusters() {
        let (_t, store, engine, config) = gate_fixture().await;

        // 13 same-group observations: one cluster of 13 > MAX_CLUSTER_SOURCES.
        for i in 0..13 {
            observation(
                &store,
                &format!("gx-{i}"),
                &format!("groupA repeated pattern {i}"),
            )
            .await;
        }

        let report = consolidation_pass(&store, &engine, &config, true)
            .await
            .unwrap();
        assert!(
            report.clusters.is_empty() && report.created.is_empty(),
            "oversized cluster must be deferred: {:?}",
            report.clusters
        );
    }
}
