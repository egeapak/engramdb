use super::*;
use crate::storage::InMemoryRegistry;

// -----------------------------------------------------------------------
// Graph/usage scoring fixture (phase 5). Every knob is off by default; the
// golden test below pins the default-config ranking of this fixture to the
// exact bits the formula produced before the knobs existed.
// -----------------------------------------------------------------------

pub(crate) const GA: &str = "0000000a-0000-4000-8000-000000000000";
pub(crate) const GB: &str = "0000000b-0000-4000-8000-000000000000";
pub(crate) const GC: &str = "0000000c-0000-4000-8000-000000000000";
pub(crate) const GD: &str = "0000000d-0000-4000-8000-000000000000";
pub(crate) const GE: &str = "0000000e-0000-4000-8000-000000000000";
pub(crate) const GF: &str = "0000000f-0000-4000-8000-000000000000";
pub(crate) const GG: &str = "00000001-0000-4000-8000-000000000000";

/// Seven memories wired with every edge shape phase 5 reads, plus usage
/// events. Decay is off everywhere so scores do not depend on the clock.
pub(crate) async fn graph_fixture() -> (tempfile::TempDir, MemoryStore) {
    use crate::types::{
        Link, LinkKind, LinkOrigin, LinkStatus, Memory, MemoryType, Provenance, Visibility,
    };
    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mk = |id: &str, t: MemoryType, summary: &str, crit: f64, logical: &[&str]| {
        let mut m = Memory::new(t, summary, format!("{summary} body"), Provenance::human());
        m.id = id.to_string();
        m.visibility = Visibility::Shared;
        m.criticality = crit;
        m.decay = None;
        m.logical = logical.iter().map(|s| s.to_string()).collect();
        m
    };
    let link = |to: &str, kind: LinkKind, status: LinkStatus| {
        let mut l = Link::new(to, kind, LinkOrigin::Human);
        l.status = status;
        l
    };
    let mut a = mk(
        GA,
        MemoryType::Decision,
        "token refresh uses rotation",
        0.8,
        &["auth.oauth"],
    );
    a.supersedes = vec![GF.to_string()];
    let mut b = mk(
        GB,
        MemoryType::Context,
        "token refresh is disabled in prod",
        0.7,
        &["auth.oauth"],
    );
    b.links = vec![link(GA, LinkKind::Contradicts, LinkStatus::Confirmed)];
    let mut c = mk(
        GC,
        MemoryType::Convention,
        "token refresh tests cover rotation",
        0.6,
        &["auth"],
    );
    c.links = vec![link(GA, LinkKind::Supports, LinkStatus::Confirmed)];
    let mut d = mk(
        GD,
        MemoryType::Context,
        "session cookie lifetime",
        0.5,
        &["auth.session"],
    );
    d.links = vec![link(GA, LinkKind::RelatedTo, LinkStatus::Confirmed)];
    let mut e = mk(
        GE,
        MemoryType::Context,
        "token refresh might race",
        0.65,
        &["auth"],
    );
    e.links = vec![link(GC, LinkKind::Contradicts, LinkStatus::Proposed)];
    let mut f = mk(
        GF,
        MemoryType::Decision,
        "token refresh disabled entirely",
        0.9,
        &["auth.oauth"],
    );
    f.superseded_by = Some(GA.to_string());
    f.invalidated_at = Some(Utc::now() - chrono::Duration::days(1));
    let mut g = mk(
        GG,
        MemoryType::Context,
        "cookie logging format",
        0.4,
        &["logging"],
    );
    g.links = vec![link(GD, LinkKind::DerivedFrom, LinkStatus::Confirmed)];
    for m in [&a, &b, &c, &d, &e, &f, &g] {
        store.create(m).await.unwrap();
    }
    // Usage: A is fetched often; D is injected often and never fetched.
    use crate::telemetry::usage::{MemoryEvent, Surface, UsageKind};
    let mut events = Vec::new();
    for i in 0..5 {
        let mut ev = MemoryEvent::new(GA, &store.project_id, UsageKind::Fetched, Surface::Cli);
        ev.session_id = Some(format!("s{i}"));
        events.push(ev);
    }
    for i in 0..3 {
        let mut ev = MemoryEvent::new(
            GA,
            &store.project_id,
            UsageKind::Injected,
            Surface::HookPrompt,
        );
        ev.session_id = Some(format!("s{i}"));
        events.push(ev);
    }
    for i in 0..20 {
        let mut ev = MemoryEvent::new(
            GD,
            &store.project_id,
            UsageKind::Injected,
            Surface::HookPrompt,
        );
        ev.session_id = Some(format!("s{i}"));
        events.push(ev);
    }
    crate::telemetry::usage::record_memory_events(&store.project_dir, &events).unwrap();
    (temp_dir, store)
}

/// The three query shapes the golden test pins: the no-query Rank fast
/// path, Filter with keywords, and Rank with query text (degraded, no
/// embedding provider in unit tests).
pub(crate) fn graph_fixture_queries() -> Vec<RetrievalQuery> {
    vec![
        RetrievalQuery {
            mode: RetrievalMode::Rank,
            logical: vec!["auth".to_string()],
            ..Default::default()
        },
        RetrievalQuery {
            mode: RetrievalMode::Filter,
            query: Some("token refresh".to_string()),
            ..Default::default()
        },
        RetrievalQuery {
            mode: RetrievalMode::Rank,
            query: Some("token refresh".to_string()),
            ..Default::default()
        },
    ]
}

pub(crate) fn ranking_bits(result: &RetrievalResult) -> Vec<(String, u64)> {
    result
        .memories
        .iter()
        .map(|sm| (sm.memory.id.clone(), sm.score.to_bits()))
        .collect()
}

#[tokio::test]
async fn default_config_ranking_is_unchanged_on_graph_fixture() {
    let (_tmp, store) = graph_fixture().await;
    let engine = RetrievalEngine::new(store, EngramConfig::default());
    // Captured from the formula before `contradicts_penalty`,
    // `supports_boost`, `usage_weight` and `expand_links` existed (commit
    // aa1b267). A default config must keep producing these exact bits:
    // the knobs are off, so nothing may be looked up or multiplied in.
    let golden: [&[(&str, u64)]; 3] = [
        &[
            (GA, 4603219251127931371),
            (GE, 4602858963157741732),
            (GB, 4602498675187552091),
            (GC, 4602318531202457272),
            (GD, 4599976659396224614),
        ],
        &[
            (GA, 4605313306968567911),
            (GB, 4604991621280898591),
            (GE, 4604830778437063929),
            (GC, 4604569215416587202),
        ],
        &[
            (GA, 4605313306968567911),
            (GB, 4604991621280898591),
            (GE, 4604830778437063929),
            (GC, 4604569215416587202),
            (GD, 4602678819172646912),
        ],
    ];
    for (q, want) in graph_fixture_queries().iter().zip(golden) {
        let r = engine.query(q).await.unwrap();
        let want: Vec<(String, u64)> = want.iter().map(|(i, b)| (i.to_string(), *b)).collect();
        assert_eq!(ranking_bits(&r), want, "query {q:?}");
        assert!(
            r.memories.iter().all(|sm| sm.score_breakdown.via.is_none()
                && sm.score_breakdown.contradicted_by.is_empty()
                && sm.score_breakdown.supports_multiplier.is_none()
                && sm.score_breakdown.usage_multiplier.is_none()),
            "no graph/usage component may appear with the knobs off"
        );
    }
}

fn knobs(f: impl FnOnce(&mut EngramConfig)) -> EngramConfig {
    let mut c = EngramConfig::default();
    f(&mut c);
    c
}

fn find<'a>(r: &'a RetrievalResult, id: &str) -> Option<&'a ScoredMemory> {
    r.memories.iter().find(|sm| sm.memory.id == id)
}

/// Knobs explicitly at zero are the default: same bits, and the engine
/// attaches no signals at all.
#[tokio::test]
async fn explicit_zero_knobs_match_default() {
    let (_tmp, store) = graph_fixture().await;
    let base = RetrievalEngine::new(store.clone(), EngramConfig::default());
    let zero = RetrievalEngine::new(
        store,
        knobs(|c| {
            c.retrieval.scoring.contradicts_penalty = 0.0;
            c.retrieval.scoring.supports_boost = 0.0;
            c.retrieval.scoring.usage_weight = 0.0;
            c.retrieval.expand_links = 0;
        }),
    );
    for q in graph_fixture_queries() {
        let a = base.query(&q).await.unwrap();
        let b = zero.query(&q).await.unwrap();
        assert_eq!(ranking_bits(&a), ranking_bits(&b));
    }
}

#[tokio::test]
async fn confirmed_contradiction_penalizes_both_live_ends() {
    let (_tmp, store) = graph_fixture().await;
    let base = RetrievalEngine::new(store.clone(), EngramConfig::default());
    let on = RetrievalEngine::new(
        store,
        knobs(|c| c.retrieval.scoring.contradicts_penalty = 0.1),
    );
    for q in graph_fixture_queries() {
        let a = base.query(&q).await.unwrap();
        let b = on.query(&q).await.unwrap();
        for id in [GA, GB] {
            let (Some(x), Some(y)) = (find(&a, id), find(&b, id)) else {
                continue;
            };
            assert!(
                (x.score - 0.1 - y.score).abs() < 1e-12,
                "{id}: {} -> {} for {q:?}",
                x.score,
                y.score
            );
            assert_eq!(y.score_breakdown.contradicts_penalty, Some(0.1));
        }
        let a_on = find(&b, GA).unwrap();
        assert_eq!(a_on.score_breakdown.contradicted_by, vec![GB.to_string()]);
        let b_on = find(&b, GB).unwrap();
        assert_eq!(b_on.score_breakdown.contradicted_by, vec![GA.to_string()]);
        // E's contradiction with C is only proposed: no effect on either.
        for id in [GC, GE] {
            if let (Some(x), Some(y)) = (find(&a, id), find(&b, id)) {
                assert_eq!(x.score.to_bits(), y.score.to_bits(), "{id} must not move");
                assert!(y.score_breakdown.contradicts_penalty.is_none());
            }
        }
    }
}

/// A contradiction whose other side has been invalidated (or deleted) no
/// longer penalizes: only a live memory is a standing objection.
#[tokio::test]
async fn contradiction_with_invalidated_or_missing_side_has_no_effect() {
    let (_tmp, store) = graph_fixture().await;
    store
        .update_with(GB, |m| {
            m.invalidated_at = Some(Utc::now() - chrono::Duration::hours(1));
            Ok(())
        })
        .await
        .unwrap();
    let on = RetrievalEngine::new(
        store.clone(),
        knobs(|c| c.retrieval.scoring.contradicts_penalty = 0.1),
    );
    let q = &graph_fixture_queries()[1];
    let r = on.query(q).await.unwrap();
    let a = find(&r, GA).unwrap();
    assert!(a.score_breakdown.contradicts_penalty.is_none());
    assert!(find(&r, GB).is_none(), "invalidated B is hidden by default");

    // With B included, B (the invalidated side) still pays: A is live.
    let mut q2 = q.clone();
    q2.include_invalidated = Some(true);
    let r2 = on.query(&q2).await.unwrap();
    assert_eq!(
        find(&r2, GB).unwrap().score_breakdown.contradicts_penalty,
        Some(0.1)
    );
    assert!(find(&r2, GA)
        .unwrap()
        .score_breakdown
        .contradicts_penalty
        .is_none());

    // Missing: delete B entirely; the edge on B's side goes with it, and
    // an edge *into* a deleted memory is dangling — no signal either way.
    store.delete(GB).await.unwrap();
    let r3 = on.query(q).await.unwrap();
    assert!(find(&r3, GA)
        .unwrap()
        .score_breakdown
        .contradicts_penalty
        .is_none());
}

/// `supersedes` needs nothing extra: the target is invalidated, so it is
/// hidden by default, and the successor is not penalized.
#[tokio::test]
async fn supersedes_demotes_only_the_target() {
    let (_tmp, store) = graph_fixture().await;
    let on = RetrievalEngine::new(
        store,
        knobs(|c| {
            c.retrieval.scoring.contradicts_penalty = 0.1;
            c.retrieval.scoring.supports_boost = 0.05;
        }),
    );
    let mut q = graph_fixture_queries()[1].clone();
    let r = on.query(&q).await.unwrap();
    assert!(find(&r, GF).is_none(), "superseded F is hidden");
    q.include_invalidated = Some(true);
    let r = on.query(&q).await.unwrap();
    let f = find(&r, GF).expect("visible when asked for");
    assert!(f.score_breakdown.contradicts_penalty.is_none());
    let a = find(&r, GA).unwrap();
    assert!(!a.score_breakdown.contradicted_by.contains(&GF.to_string()));
}

#[tokio::test]
async fn supports_boost_counts_live_incoming_supporters() {
    let (_tmp, store) = graph_fixture().await;
    let base = RetrievalEngine::new(store.clone(), EngramConfig::default());
    let on = RetrievalEngine::new(store, knobs(|c| c.retrieval.scoring.supports_boost = 0.05));
    for q in graph_fixture_queries() {
        let a = base.query(&q).await.unwrap();
        let b = on.query(&q).await.unwrap();
        let (x, y) = (find(&a, GA).unwrap(), find(&b, GA).unwrap());
        assert_eq!(y.score_breakdown.supports_multiplier, Some(1.05));
        assert_eq!(y.score_breakdown.supported_by, vec![GC.to_string()]);
        assert!(y.score > x.score || y.score == 1.0, "{q:?}");
        // C supports A; C itself gets nothing from its own outgoing edge.
        if let Some(c) = find(&b, GC) {
            assert!(c.score_breakdown.supports_multiplier.is_none());
        }
    }
}

#[tokio::test]
async fn usage_boost_rewards_fetches_not_injection() {
    let (_tmp, store) = graph_fixture().await;
    let base = RetrievalEngine::new(store.clone(), EngramConfig::default());
    let on = RetrievalEngine::new(store, knobs(|c| c.retrieval.scoring.usage_weight = 0.1));
    let q = &graph_fixture_queries()[0];
    let a = base.query(q).await.unwrap();
    let b = on.query(q).await.unwrap();
    let ga = find(&b, GA).unwrap();
    // fetched 5, injected 3 (3 sessions): ρ = 1, term = ln(1 + 5.9).
    let want = 1.0 + 0.1 * (5.0f64 + 0.9).ln_1p();
    assert!((ga.score_breakdown.usage_multiplier.unwrap() - want).abs() < 1e-12);
    assert!(ga.score > find(&a, GA).unwrap().score);
    // D: injected 20 times in 20 sessions, never fetched: ρ = 1/21.
    let gd = find(&b, GD).unwrap();
    let m = gd.score_breakdown.usage_multiplier.unwrap();
    assert!(m < 1.0 + 0.1 * 0.17, "injection alone stays tiny: {m}");
}

#[tokio::test]
async fn expansion_adds_marked_neighbours_bounded() {
    let (_tmp, store) = graph_fixture().await;
    let engine = RetrievalEngine::new(store, EngramConfig::default());
    // Filter for a term only A matches.
    let q = RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some("rotation".to_string()),
        max_results: Some(1),
        expand_links: Some(5),
        ..Default::default()
    };
    let r = engine.query(&q).await.unwrap();
    assert_eq!(r.memories[0].memory.id, GA);
    let hit = r.memories[0].score;
    let expanded: Vec<(&str, &str)> = r
        .memories
        .iter()
        .filter_map(|sm| {
            sm.score_breakdown
                .via
                .as_ref()
                .map(|v| (sm.memory.id.as_str(), v.kind.as_str()))
        })
        .collect();
    // C supports A, D related_to A; B contradicts A (never followed),
    // F is superseded (hidden), G is two hops away.
    let mut ids: Vec<&str> = expanded.iter().map(|(i, _)| *i).collect();
    ids.sort();
    assert_eq!(ids, vec![GC, GD], "got {expanded:?}");
    for sm in r
        .memories
        .iter()
        .filter(|sm| sm.score_breakdown.via.is_some())
    {
        assert_eq!(sm.score_breakdown.via.as_ref().unwrap().from, GA);
        // Its hit's score times the decay, or its own score when that is
        // higher (it was only cut by max_results) — never above the hit.
        assert!(sm.score >= hit * 0.5 - 1e-12 && sm.score <= hit);
    }
    // Sorted by score, direct hit first.
    assert!(r.memories.windows(2).all(|w| w[0].score >= w[1].score));

    // Bounded.
    let q1 = RetrievalQuery {
        expand_links: Some(1),
        ..q.clone()
    };
    let r1 = engine.query(&q1).await.unwrap();
    assert_eq!(
        r1.memories
            .iter()
            .filter(|sm| sm.score_breakdown.via.is_some())
            .count(),
        1
    );
    // Off.
    let r0 = engine
        .query(&RetrievalQuery {
            expand_links: Some(0),
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(r0.memories.len(), 1);
}

/// Rank mode, no query (the index fast path), expands too, and the
/// config default turns it on.
#[tokio::test]
async fn expansion_works_in_rank_fast_path_from_config_default() {
    let (_tmp, store) = graph_fixture().await;
    // G (criticality 0.4) is below the default 0.45 threshold, and a weak
    // fallback hit expands nothing; lower the threshold so G is a hit.
    let engine = RetrievalEngine::new(
        store.clone(),
        knobs(|c| {
            c.retrieval.expand_links = 3;
            c.retrieval.relevance_threshold = 0.3;
        }),
    );
    let q = RetrievalQuery {
        mode: RetrievalMode::Rank,
        logical: vec!["logging".to_string()],
        max_results: Some(1),
        ..Default::default()
    };
    let r = engine.query(&q).await.unwrap();
    assert_eq!(r.memories[0].memory.id, GG);
    let d = find(&r, GD).expect("G derived_from D");
    let via = d.score_breakdown.via.as_ref().unwrap();
    assert_eq!(via.from, GG);
    assert_eq!(via.kind, crate::types::LinkKind::DerivedFrom);
    assert!(!d.memory.content.is_empty(), "neighbour is materialized");

    // At the default threshold G is only a weak fallback lead: no expansion.
    let weak = RetrievalEngine::new(store, knobs(|c| c.retrieval.expand_links = 3));
    let r = weak.query(&q).await.unwrap();
    assert!(r.memories[0].score_breakdown.below_threshold);
    assert!(r.memories.iter().all(|sm| sm.score_breakdown.via.is_none()));
}

/// The visibility rule: a neighbour the query's own filters exclude is
/// never reached through an edge.
#[tokio::test]
async fn expansion_respects_every_hard_filter() {
    let (_tmp, store) = graph_fixture().await;
    let engine = RetrievalEngine::new(store, EngramConfig::default());
    let base = RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some("rotation".to_string()),
        max_results: Some(1),
        expand_links: Some(5),
        ..Default::default()
    };
    // Type filter: only decisions and conventions; D (context) is out.
    let r = engine
        .query(&RetrievalQuery {
            types: Some(vec![MemoryType::Decision, MemoryType::Convention]),
            ..base.clone()
        })
        .await
        .unwrap();
    assert!(find(&r, GD).is_none());
    assert!(find(&r, GC).is_some());
    // Logical hard filter in filter mode: D is auth.session, A auth.oauth.
    let r = engine
        .query(&RetrievalQuery {
            logical: vec!["auth.oauth".to_string()],
            ..base.clone()
        })
        .await
        .unwrap();
    assert!(find(&r, GD).is_none(), "auth.session is a sibling");
    // Minimum criticality.
    let r = engine
        .query(&RetrievalQuery {
            min_criticality: Some(0.55),
            ..base.clone()
        })
        .await
        .unwrap();
    assert!(find(&r, GD).is_none());
    assert!(find(&r, GC).is_some());
}

#[tokio::test]
async fn invalidated_neighbour_is_hidden_unless_asked_for() {
    let (_tmp, store) = graph_fixture().await;
    store
        .update_with(GC, |m| {
            m.invalidated_at = Some(Utc::now() - chrono::Duration::hours(1));
            Ok(())
        })
        .await
        .unwrap();
    let engine = RetrievalEngine::new(store, EngramConfig::default());
    let q = RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some("rotation".to_string()),
        max_results: Some(1),
        expand_links: Some(5),
        ..Default::default()
    };
    let r = engine.query(&q).await.unwrap();
    assert!(find(&r, GC).is_none());
    let r = engine
        .query(&RetrievalQuery {
            include_invalidated: Some(true),
            ..q
        })
        .await
        .unwrap();
    assert!(find(&r, GC).is_some());
}

/// With every knob at its default, a query performs no phase-5 lookup:
/// no edges read, no usage read. The golden test cannot show this (a
/// zero knob scores bit-identically whether or not evidence was loaded),
/// so the read counters in `retrieval::signals` do.
#[tokio::test]
async fn default_config_reads_no_edges_or_usage() {
    use crate::retrieval::signals::lookup_counts;
    let (_tmp, store) = graph_fixture().await;
    let engine = RetrievalEngine::new(store.clone(), EngramConfig::default());
    let before = lookup_counts();
    for q in graph_fixture_queries() {
        engine.query(&q).await.unwrap();
    }
    assert_eq!(
        lookup_counts(),
        before,
        "default config must not look anything up"
    );

    // The counters do count: each knob reads only what it needs.
    let edges = RetrievalEngine::new(
        store.clone(),
        knobs(|c| c.retrieval.scoring.contradicts_penalty = 0.1),
    );
    edges.query(&graph_fixture_queries()[1]).await.unwrap();
    let after_edges = lookup_counts();
    assert_eq!(after_edges.0, before.0 + 1);
    assert_eq!(after_edges.1, before.1, "an edge knob reads no usage");
    let usage = RetrievalEngine::new(store, knobs(|c| c.retrieval.scoring.usage_weight = 0.1));
    usage.query(&graph_fixture_queries()[1]).await.unwrap();
    let after_usage = lookup_counts();
    assert_eq!(
        after_usage.0, after_edges.0,
        "the usage knob reads no edges"
    );
    assert_eq!(after_usage.1, after_edges.1 + 1);
}

/// The usage view is cached per store for the whole process, so the
/// fresh engine every front end builds per call still shares it.
#[tokio::test]
async fn usage_view_is_shared_across_engines_of_one_store() {
    use crate::retrieval::signals::{invalidate_usage_cache, lookup_counts};
    let (_tmp, store) = graph_fixture().await;
    let config = knobs(|c| c.retrieval.scoring.usage_weight = 0.1);
    let q = &graph_fixture_queries()[0];
    let before = lookup_counts().1;
    for _ in 0..3 {
        RetrievalEngine::new(store.clone(), config.clone())
            .query(q)
            .await
            .unwrap();
    }
    assert_eq!(lookup_counts().1, before + 1, "one read for three engines");
    invalidate_usage_cache(Some(&store));
    RetrievalEngine::new(store, config).query(q).await.unwrap();
    assert_eq!(lookup_counts().1, before + 2);
}

/// An expanded neighbour keeps its own doubts: its challenge and
/// `contradicts` penalties come off its decayed score, and its breakdown
/// still names the contradiction. And `total` counts each distinct
/// memory once, even one expansion pulled back from the cut tail.
#[tokio::test]
async fn expanded_neighbour_pays_its_own_penalties() {
    use crate::types::{Link, LinkKind, LinkOrigin, LinkStatus};
    let (_tmp, store) = graph_fixture().await;
    // G (live) now confirms a contradiction with D, and D is challenged.
    store
        .update_with(GG, |m| {
            let mut l = Link::new(GD, LinkKind::Contradicts, LinkOrigin::Human);
            l.status = LinkStatus::Confirmed;
            m.links.push(l);
            Ok(())
        })
        .await
        .unwrap();
    store
        .update_with(GD, |m| {
            m.status = crate::types::Status::Challenged;
            Ok(())
        })
        .await
        .unwrap();
    let challenge = EngramConfig::default()
        .retrieval
        .scoring
        .challenge_penalty
        .penalty_for(store.get(GD).await.unwrap().epistemic);
    let q = RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some("rotation".to_string()),
        max_results: Some(1),
        expand_links: Some(5),
        ..Default::default()
    };

    // Default knobs: the challenge penalty alone (it is always on).
    let engine = RetrievalEngine::new(store.clone(), EngramConfig::default());
    let r = engine.query(&q).await.unwrap();
    assert_eq!(r.memories[0].memory.id, GA);
    let hit = r.memories[0].score;
    let d = find(&r, GD).expect("D is reached through A");
    assert!(d.score_breakdown.contradicted_by.is_empty(), "knob off");
    let want = (hit * 0.5 - challenge).max(0.0);
    assert!((d.score - want).abs() < 1e-12, "{} vs {want}", d.score);
    // A (hit), C (a cut candidate, pulled back as a neighbour) and D:
    // three distinct memories, counted once each.
    let ids: std::collections::HashSet<&str> =
        r.memories.iter().map(|sm| sm.memory.id.as_str()).collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert_eq!(r.total, 3);

    // With the contradicts knob on, D also pays for G's contradiction,
    // and its breakdown says so. (A pays for B's, so it can fall below C;
    // keep both as hits.)
    let engine = RetrievalEngine::new(
        store,
        knobs(|c| c.retrieval.scoring.contradicts_penalty = 0.1),
    );
    let r = engine
        .query(&RetrievalQuery {
            max_results: Some(2),
            ..q
        })
        .await
        .unwrap();
    let hit = find(&r, GA).expect("A is a hit").score;
    let d = find(&r, GD).expect("D is still reached through A");
    assert_eq!(d.score_breakdown.via.as_ref().unwrap().from, GA);
    assert_eq!(d.score_breakdown.contradicted_by, vec![GG.to_string()]);
    assert_eq!(d.score_breakdown.contradicts_penalty, Some(0.1));
    let want = (hit * 0.5 - challenge - 0.1).max(0.0);
    assert!((d.score - want).abs() < 1e-12, "{} vs {want}", d.score);
}

/// The fan-in over-fetches each store past the caller's `max` and cuts
/// the merge back to it, so it passes `max` as the expansion parents:
/// a hit past it must not spend the budget.
#[tokio::test]
async fn expansion_parents_limit_which_hits_expand() {
    use crate::types::{Link, LinkKind, LinkOrigin, LinkStatus};
    let (_tmp, store) = graph_fixture().await;
    // E ("token refresh might race") gets a neighbour of its own: G.
    store
        .update_with(GE, |m| {
            let mut l = Link::new(GG, LinkKind::RelatedTo, LinkOrigin::Human);
            l.status = LinkStatus::Confirmed;
            m.links.push(l);
            Ok(())
        })
        .await
        .unwrap();
    let engine = RetrievalEngine::new(store, EngramConfig::default());
    let q = RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some("token".to_string()),
        max_results: Some(5),
        expand_links: Some(5),
        ..Default::default()
    };
    let all = engine.query(&q).await.unwrap();
    let g = find(&all, GG).expect("E's neighbour expands with every hit a parent");
    assert_eq!(g.score_breakdown.via.as_ref().unwrap().from, GE);

    let top = engine.query_expanding_from(&q, Some(1)).await.unwrap();
    let first = top.memories[0].memory.id.clone();
    assert_ne!(first, GE, "premise: E is not the top hit");
    assert!(find(&top, GG).is_none(), "E is past the parents limit");
    assert!(top
        .memories
        .iter()
        .filter_map(|sm| sm.score_breakdown.via.as_ref())
        .all(|v| v.from == first));

    let none = engine.query_expanding_from(&q, Some(0)).await.unwrap();
    assert!(none
        .memories
        .iter()
        .all(|sm| sm.score_breakdown.via.is_none()));
}

/// A neighbour the rank threshold dropped and expansion brought back is
/// shown, so it is no longer counted in `dropped_below_threshold`.
#[tokio::test]
async fn expansion_returned_neighbour_is_not_counted_as_dropped() {
    let (_tmp, store) = graph_fixture().await;
    let config = knobs(|c| c.retrieval.relevance_threshold = 0.3);
    let engine = RetrievalEngine::new(store, config);
    let q = RetrievalQuery {
        mode: RetrievalMode::Rank,
        logical: vec!["logging".to_string()],
        max_results: Some(10),
        expand_links: Some(0),
        ..Default::default()
    };
    let off = engine.query(&q).await.unwrap();
    assert!(find(&off, GG).is_some());
    assert!(
        find(&off, GD).is_none(),
        "premise: the threshold drops D (no tail at max 10)"
    );
    let on = engine
        .query(&RetrievalQuery {
            expand_links: Some(3),
            ..q.clone()
        })
        .await
        .unwrap();
    assert!(find(&on, GD).is_some_and(|d| d.score_breakdown.via.is_some()));
    assert_eq!(
        on.dropped_below_threshold,
        off.dropped_below_threshold - 1,
        "D is shown, not dropped"
    );
    assert_eq!(on.total, off.total + 1);
}
