use super::*;
use crate::storage::InMemoryRegistry;
use crate::types::{Provenance, Visibility};
use tempfile::TempDir;

async fn seeded_engine() -> (TempDir, RetrievalEngine) {
    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // One memory per class (types chosen so the class is the DIAGONAL
    // default — files stay pre-epistemic-shaped), plus one invalidated.
    for (id, type_) in [
        ("mem-fact", MemoryType::Hazard),
        ("mem-obs", MemoryType::Debug),
        ("mem-dec", MemoryType::Decision),
    ] {
        let mut m = Memory::new(type_, id, "content", Provenance::human());
        m.id = id.to_string();
        m.visibility = Visibility::Shared;
        m.criticality = 0.8;
        m.decay = Some(crate::types::Decay::none());
        store.create(&m).await.unwrap();
    }
    let mut gone = Memory::new(
        MemoryType::Hazard,
        "mem-invalidated",
        "content",
        Provenance::human(),
    );
    gone.id = "mem-invalidated".to_string();
    gone.visibility = Visibility::Shared;
    gone.criticality = 0.8;
    gone.decay = Some(crate::types::Decay::none());
    gone.invalidated_at = Some(Utc::now() - chrono::Duration::days(1));
    store.create(&gone).await.unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;
    (temp_dir, RetrievalEngine::new(store, config))
}

fn ids(result: &RetrievalResult) -> Vec<&str> {
    let mut v: Vec<&str> = result
        .memories
        .iter()
        .map(|m| m.memory.id.as_str())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn epistemic_hard_filter_narrows_both_modes() {
    let (_tmp, engine) = seeded_engine().await;

    // Rank mode.
    let result = engine
        .query(&RetrievalQuery {
            epistemic: Some(vec![Epistemic::Observation]),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ids(&result), vec!["mem-obs"]);

    // Two classes.
    let result = engine
        .query(&RetrievalQuery {
            epistemic: Some(vec![Epistemic::Observation, Epistemic::Decision]),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ids(&result), vec!["mem-dec", "mem-obs"]);

    // Filter mode (tags signal to satisfy Step 0) — class filter still
    // applies. Use a query that keyword-matches everything instead.
    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Filter,
            query: Some("content".to_string()),
            epistemic: Some(vec![Epistemic::Decision]),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ids(&result), vec!["mem-dec"]);
}

#[tokio::test]
async fn invalidated_excluded_by_default_included_on_optin() {
    let (_tmp, engine) = seeded_engine().await;

    // Default: the invalidated memory never surfaces.
    let result = engine.query(&RetrievalQuery::default()).await.unwrap();
    assert_eq!(ids(&result), vec!["mem-dec", "mem-fact", "mem-obs"]);

    // Opt-in: it is scored and returned like any other memory.
    let result = engine
        .query(&RetrievalQuery {
            include_invalidated: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        ids(&result),
        vec!["mem-dec", "mem-fact", "mem-invalidated", "mem-obs"]
    );

    // A future-dated (scheduled) invalidation is still valid now → included
    // by default, exactly like a future expires_at.
    let store = engine.store();
    store
        .update_with("mem-fact", |m| {
            m.invalidated_at = Some(Utc::now() + chrono::Duration::days(30));
            Ok(())
        })
        .await
        .unwrap();
    let result = engine.query(&RetrievalQuery::default()).await.unwrap();
    assert_eq!(ids(&result), vec!["mem-dec", "mem-fact", "mem-obs"]);
}

#[tokio::test]
async fn situation_reweights_rank_order() {
    let (_tmp, engine) = seeded_engine().await;

    // Neutral: all three active memories tie (same criticality/trust) and
    // every breakdown records a neutral multiplier.
    let neutral = engine.query(&RetrievalQuery::default()).await.unwrap();
    for m in &neutral.memories {
        assert_eq!(m.score_breakdown.situation_multiplier, 1.0);
    }

    // session_start: fact (1.0) > decision (0.92) > observation (0.8).
    let result = engine
        .query(&RetrievalQuery {
            situation: Some(Situation::SessionStart),
            ..Default::default()
        })
        .await
        .unwrap();
    let ordered: Vec<&str> = result
        .memories
        .iter()
        .map(|m| m.memory.id.as_str())
        .collect();
    assert_eq!(ordered, vec!["mem-fact", "mem-dec", "mem-obs"]);
    let mults: Vec<f64> = result
        .memories
        .iter()
        .map(|m| m.score_breakdown.situation_multiplier)
        .collect();
    assert!((mults[0] - 1.0).abs() < 1e-9);
    assert!((mults[1] - 0.92).abs() < 1e-9);
    assert!((mults[2] - 0.8).abs() < 1e-9);

    // debugging: observation on top.
    let result = engine
        .query(&RetrievalQuery {
            situation: Some(Situation::Debugging),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.memories[0].memory.id, "mem-obs");
}
