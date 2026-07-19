//! Task lifecycle operations (§11.1–11.2).
//!
//! Tasks are free-text names carried in `valid_while.origin_task`. A session
//! declares its task (`task_current`), which the hooks use to un-suppress
//! that task's scoped memories; completing a task (`task_complete`) demotes
//! its task-scoped memories to a decaying curve so they fade unless promoted.

use crate::storage::{task_state, MemoryStore};
use crate::types::{Decay, Generality};
use anyhow::Result;
use chrono::Duration;

/// Result of a `task_current` call.
#[derive(Debug)]
pub struct TaskCurrentResult {
    pub session_id: String,
    /// The task now associated with the session (after any write).
    pub task: Option<String>,
}

/// Read or record the session→task association (§11.1). `task: None` reads
/// the current mapping; `Some(name)` records it.
pub fn task_current(
    project_dir: &std::path::Path,
    session_id: &str,
    task: Option<&str>,
) -> Result<TaskCurrentResult> {
    if let Some(name) = task {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("task name must not be empty");
        }
        task_state::set_current_task(project_dir, session_id, name)?;
    }
    Ok(TaskCurrentResult {
        session_id: session_id.to_string(),
        task: task_state::current_task(project_dir, session_id),
    })
}

/// Result of a `task_complete` run.
#[derive(Debug, Default)]
pub struct TaskCompleteResult {
    /// Task-scoped memories whose decay was flipped to the demotion curve.
    pub demoted: Vec<String>,
    /// Task-scoped memories left untouched because they carry an explicit
    /// (non-default) decay.
    pub kept_custom_decay: Vec<String>,
    /// `(id, summary)` of PROJECT-generality memories from this task —
    /// reported for review, never auto-demoted (§11.2).
    pub project_wide_notices: Vec<(String, String)>,
}

/// The demotion curve applied to task-scoped memories on completion: the
/// Intent curve (exponential, 14-day half-life). Demotion REPLACES the
/// curve — it never stacks with other penalties.
fn demotion_decay() -> Decay {
    Decay::exponential(Duration::days(14))
}

/// The class-appropriate default decay, honoring the `[epistemic]` config
/// overrides for the off-diagonal Observation curve (mirrors the create
/// path, which builds that curve from `observation_half_life_days` /
/// `observation_decay_floor`).
fn config_default_decay(
    type_: crate::types::MemoryType,
    epistemic: crate::types::Epistemic,
    cfg: &crate::types::EpistemicConfig,
) -> Option<Decay> {
    use crate::types::Epistemic;
    if epistemic == Epistemic::Observation && type_.default_epistemic() != Epistemic::Observation {
        Some(
            Decay::exponential(Duration::days(cfg.observation_half_life_days as i64))
                .with_floor(cfg.observation_decay_floor),
        )
    } else {
        crate::types::default_decay(type_, epistemic)
    }
}

/// True when the memory's decay is one of its DEFAULTS (type-diagonal,
/// off-diagonal class default, or the config-overridden observation curve)
/// rather than an explicit user choice. Decay has no `PartialEq`; compare
/// the serialized form.
fn decay_is_default(memory: &crate::types::Memory, cfg: &crate::types::EpistemicConfig) -> bool {
    let current = match &memory.decay {
        Some(d) => match serde_json::to_string(d) {
            Ok(s) => s,
            Err(_) => return false,
        },
        None => return true,
    };
    let candidates = [
        config_default_decay(memory.type_, memory.epistemic, cfg),
        crate::types::default_decay(memory.type_, memory.epistemic),
        memory.type_.default_decay(),
    ];
    candidates.iter().any(|c| {
        c.as_ref()
            .and_then(|d| serde_json::to_string(d).ok())
            .is_some_and(|s| s == current)
    })
}

/// Mark a task finished (§11.2): every live memory with
/// `valid_while.origin_task == name` is processed under the per-project
/// write lock —
/// - `generality: task` → decay flipped to the demotion curve (unless the
///   memory carries an explicit user-set decay);
/// - `generality: project` → left untouched, reported as a notice
///   ("project-wide memory from a completed task — verify or demote").
pub async fn task_complete(
    store: &MemoryStore,
    name: &str,
    epistemic_cfg: &crate::types::EpistemicConfig,
) -> Result<TaskCompleteResult> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("task name must not be empty");
    }

    let ids = store.list_ids().await?;
    let loaded = store.get_batch(&ids).await?;
    let now = chrono::Utc::now();

    let mut result = TaskCompleteResult::default();
    for (id, memory) in &loaded {
        if memory.is_invalidated_at(now) {
            continue;
        }
        let Some(validity) = &memory.valid_while else {
            continue;
        };
        if validity.origin_task.as_deref() != Some(name) {
            continue;
        }

        match validity.generality {
            Generality::Project => {
                result
                    .project_wide_notices
                    .push((id.clone(), memory.summary.clone()));
            }
            Generality::Task => {
                if !decay_is_default(memory, epistemic_cfg) {
                    result.kept_custom_decay.push(id.clone());
                    continue;
                }
                let demoted = store
                    .update_with(id, |m| {
                        m.decay = Some(demotion_decay());
                        Ok(())
                    })
                    .await;
                match demoted {
                    Ok(_) => result.demoted.push(id.clone()),
                    Err(e) => {
                        tracing::warn!(memory_id = %id, "task_complete demotion failed: {e}");
                    }
                }
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{DecayStrategy, Epistemic, Memory, MemoryType, Provenance, Validity};
    use tempfile::TempDir;

    async fn setup() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        (tmp, store)
    }

    fn task_memory(id: &str, task: &str, generality: Generality) -> Memory {
        let mut m = Memory::new(MemoryType::Decision, id, "content", Provenance::human());
        m.id = id.to_string();
        m.valid_while = Some(Validity {
            origin_task: Some(task.to_string()),
            generality,
            ..Default::default()
        });
        m
    }

    #[test]
    fn task_current_reads_and_writes_mapping() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let read = task_current(dir, "sess-1", None).unwrap();
        assert_eq!(read.task, None);

        let written = task_current(dir, "sess-1", Some("epistemic")).unwrap();
        assert_eq!(written.task.as_deref(), Some("epistemic"));
        let read = task_current(dir, "sess-1", None).unwrap();
        assert_eq!(read.task.as_deref(), Some("epistemic"));

        assert!(task_current(dir, "sess-1", Some("  ")).is_err());
        assert!(task_current(dir, "", Some("t")).is_err());
    }

    #[tokio::test]
    async fn task_complete_demotes_task_scoped_only() {
        let (_t, store) = setup().await;

        store
            .create(&task_memory("tc-task", "feat-x", Generality::Task))
            .await
            .unwrap();
        store
            .create(&task_memory("tc-project", "feat-x", Generality::Project))
            .await
            .unwrap();
        store
            .create(&task_memory("tc-other", "feat-y", Generality::Task))
            .await
            .unwrap();
        // Custom decay must survive demotion untouched.
        let mut custom = task_memory("tc-custom", "feat-x", Generality::Task);
        custom.decay = Some(Decay::linear(Duration::days(3)).with_floor(0.4));
        store.create(&custom).await.unwrap();

        let result = task_complete(&store, "feat-x", &Default::default())
            .await
            .unwrap();
        assert_eq!(result.demoted, vec!["tc-task".to_string()]);
        assert_eq!(result.kept_custom_decay, vec!["tc-custom".to_string()]);
        assert_eq!(result.project_wide_notices.len(), 1);
        assert_eq!(result.project_wide_notices[0].0, "tc-project");

        // Demoted memory carries the 14d exponential curve.
        let m = store.get("tc-task").await.unwrap();
        let decay = m.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Exponential);
        assert_eq!(decay.half_life, Some(Duration::days(14)));

        // Untouched memories.
        let m = store.get("tc-other").await.unwrap();
        assert_eq!(m.decay.unwrap().strategy, DecayStrategy::None);
        let m = store.get("tc-custom").await.unwrap();
        assert_eq!(m.decay.unwrap().strategy, DecayStrategy::Linear);
        let m = store.get("tc-project").await.unwrap();
        assert_eq!(m.decay.unwrap().strategy, DecayStrategy::None);
    }

    #[tokio::test]
    async fn task_complete_skips_invalidated_and_off_diagonal_default() {
        let (_t, store) = setup().await;

        // Invalidated task memory: skipped entirely.
        let mut dead = task_memory("tc-dead", "feat-z", Generality::Task);
        dead.invalidated_at = Some(chrono::Utc::now() - Duration::days(1));
        store.create(&dead).await.unwrap();

        // Off-diagonal observation with its class-default decay: the class
        // default counts as "default" ⇒ demoted.
        let mut obs = task_memory("tc-obs", "feat-z", Generality::Task);
        obs.type_ = MemoryType::Hazard;
        obs.epistemic = Epistemic::Observation;
        obs.decay = crate::types::default_decay(MemoryType::Hazard, Epistemic::Observation);
        store.create(&obs).await.unwrap();

        let result = task_complete(&store, "feat-z", &Default::default())
            .await
            .unwrap();
        assert_eq!(result.demoted, vec!["tc-obs".to_string()]);
        assert!(store.get("tc-dead").await.unwrap().decay.is_some());
    }

    /// A config-overridden observation curve (create builds it from
    /// `[epistemic] observation_half_life_days`/`observation_decay_floor`)
    /// still counts as "default" and demotes — the override must not be
    /// mistaken for an explicit user decay.
    #[tokio::test]
    async fn task_complete_demotes_config_overridden_observation_curve() {
        let (_t, store) = setup().await;
        let cfg = crate::types::EpistemicConfig {
            observation_half_life_days: 60,
            observation_decay_floor: 0.25,
            ..Default::default()
        };

        let mut obs = task_memory("tc-cfg-obs", "feat-w", Generality::Task);
        obs.type_ = MemoryType::Hazard;
        obs.epistemic = Epistemic::Observation;
        // Exactly what create_memory builds under this config.
        obs.decay = Some(Decay::exponential(Duration::days(60)).with_floor(0.25));
        store.create(&obs).await.unwrap();

        let result = task_complete(&store, "feat-w", &cfg).await.unwrap();
        assert_eq!(result.demoted, vec!["tc-cfg-obs".to_string()]);
        let demoted = store.get("tc-cfg-obs").await.unwrap().decay.unwrap();
        assert_eq!(demoted.half_life, Some(Duration::days(14)));

        // Under the DEFAULT config the same curve is a custom choice: kept.
        let mut obs2 = task_memory("tc-cfg-obs2", "feat-w2", Generality::Task);
        obs2.type_ = MemoryType::Hazard;
        obs2.epistemic = Epistemic::Observation;
        obs2.decay = Some(Decay::exponential(Duration::days(60)).with_floor(0.25));
        store.create(&obs2).await.unwrap();
        let result = task_complete(&store, "feat-w2", &Default::default())
            .await
            .unwrap();
        assert_eq!(result.kept_custom_decay, vec!["tc-cfg-obs2".to_string()]);
    }
}

// ---------------------------------------------------------------------------
// Promotion (§11.3)
// ---------------------------------------------------------------------------

/// Report from one promotion pass.
#[derive(Debug, Default)]
pub struct PromotionReport {
    /// `(id, summary, distinct later sessions)` — met the threshold; promote
    /// manually (suggestion mode, the default).
    pub suggestions: Vec<(String, String, usize)>,
    /// Ids auto-promoted (only with `[epistemic] auto_promote = true`).
    pub promoted: Vec<String>,
}

/// Count, per memory id, the DISTINCT sessions whose retrievals returned it
/// (from the §11.3 telemetry rows). Pure so the counting is testable without
/// LanceDB.
pub fn count_retrieval_sessions(
    events: &[crate::telemetry::EventRow],
) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    use crate::telemetry::EventType;
    let mut map: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for ev in events {
        if ev.event_type != EventType::Retrieval {
            continue;
        }
        let Some(sid) = ev.session_id.as_deref().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(ids_json) = ev.memory_ids.as_deref() else {
            continue;
        };
        let Ok(ids) = serde_json::from_str::<Vec<String>>(ids_json) else {
            continue;
        };
        for id in ids {
            map.entry(id).or_default().insert(sid.to_string());
        }
    }
    map
}

/// §11.3 re-confirmation → promotion: task-bound memories retrieved in ≥
/// `promotion_min_sessions` DISTINCT sessions (excluding their origin
/// session) are suggested for promotion — or, behind
/// `[epistemic] auto_promote`, promoted: `origin_task` cleared,
/// `generality: project`, decay reset to the diagonal default,
/// `verified_at` stamped. Promotion never retypes the memory.
pub async fn promote_reconfirmed_memories(
    store: &MemoryStore,
    config: &crate::types::EngramConfig,
) -> Result<PromotionReport> {
    let min_sessions = config.epistemic.promotion_min_sessions as usize;
    if min_sessions == 0 {
        return Ok(PromotionReport::default());
    }

    let events = crate::telemetry::persistence::load_recent(&store.project_id, 50_000).await?;
    let sessions_by_memory = count_retrieval_sessions(&events);
    if sessions_by_memory.is_empty() {
        return Ok(PromotionReport::default());
    }

    let ids = store.list_ids().await?;
    let loaded = store.get_batch(&ids).await?;
    let now = chrono::Utc::now();

    let mut report = PromotionReport::default();
    for (id, memory) in &loaded {
        if memory.is_invalidated_at(now) {
            continue;
        }
        let Some(validity) = &memory.valid_while else {
            continue;
        };
        if validity.origin_task.is_none() {
            continue; // only task-bound memories promote
        }
        let Some(sessions) = sessions_by_memory.get(id) else {
            continue;
        };
        // Exclude the origin session: re-confirmation means OTHER sessions
        // found it useful.
        let later_sessions = sessions
            .iter()
            .filter(|s| memory.provenance.session_id.as_deref() != Some(s.as_str()))
            .count();
        if later_sessions < min_sessions {
            continue;
        }

        if config.epistemic.auto_promote {
            // Class-appropriate reset honoring the [epistemic] observation
            // overrides — computed outside the closure from the loaded copy.
            let reset_decay =
                config_default_decay(memory.type_, memory.epistemic, &config.epistemic);
            let promoted = store
                .update_with(id, move |m| {
                    if let Some(v) = &mut m.valid_while {
                        v.origin_task = None;
                        v.generality = crate::types::Generality::Project;
                    }
                    m.decay = reset_decay.clone();
                    m.verified_at = Some(chrono::Utc::now());
                    Ok(())
                })
                .await;
            match promoted {
                Ok(_) => report.promoted.push(id.clone()),
                Err(e) => tracing::warn!(memory_id = %id, "auto-promotion failed: {e}"),
            }
        } else {
            report
                .suggestions
                .push((id.clone(), memory.summary.clone(), later_sessions));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod promotion_tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::telemetry::{EventRow, EventType};
    use crate::types::{EngramConfig, Generality, Memory, MemoryType, Provenance, Validity};
    use chrono::Utc;
    use tempfile::TempDir;

    fn retrieval_event(sid: &str, ids: &[&str]) -> EventRow {
        EventRow {
            ts: Utc::now(),
            event_type: EventType::Retrieval,
            tool: None,
            stage: None,
            duration_ms: None,
            success: None,
            hit: None,
            retrieval_quality: None,
            session_id: Some(sid.to_string()),
            memory_ids: serde_json::to_string(&ids.to_vec()).ok(),
        }
    }

    #[test]
    fn count_retrieval_sessions_distinct_per_memory() {
        let events = vec![
            retrieval_event("s1", &["m1", "m2"]),
            retrieval_event("s1", &["m1"]), // same session again: not distinct
            retrieval_event("s2", &["m1"]),
            retrieval_event("s3", &["m1"]),
            // Non-retrieval rows and anonymous rows are ignored.
            EventRow {
                ts: Utc::now(),
                event_type: EventType::QueryOutcome,
                tool: None,
                stage: None,
                duration_ms: None,
                success: None,
                hit: Some(true),
                retrieval_quality: Some("full".into()),
                session_id: Some("s9".into()),
                memory_ids: None,
            },
            EventRow {
                session_id: None,
                ..retrieval_event("x", &["m2"])
            },
        ];
        let map = count_retrieval_sessions(&events);
        assert_eq!(map.get("m1").unwrap().len(), 3);
        assert_eq!(map.get("m2").unwrap().len(), 1);
    }

    async fn store_with_task_memory(auto: bool) -> (TempDir, MemoryStore, EngramConfig) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mut m = Memory::new(
            MemoryType::Decision,
            "Task decision worth promoting",
            "c",
            Provenance::human(),
        );
        m.id = "promo-1".into();
        m.provenance.session_id = Some("origin-sess".into());
        m.valid_while = Some(Validity {
            origin_task: Some("feat-x".into()),
            generality: Generality::Task,
            ..Default::default()
        });
        store.create(&m).await.unwrap();
        let mut config = EngramConfig::default();
        config.epistemic.auto_promote = auto;
        (tmp, store, config)
    }

    #[tokio::test]
    async fn promotion_suggests_at_threshold_excluding_origin_session() {
        let (_t, store, config) = store_with_task_memory(false).await;

        // Persist retrieval rows: origin session + 3 later sessions.
        let events: Vec<EventRow> = ["origin-sess", "s1", "s2", "s3"]
            .iter()
            .map(|sid| retrieval_event(sid, &["promo-1"]))
            .collect();
        crate::telemetry::persistence::append_events(&store.project_id, &events, None).await;

        let report = promote_reconfirmed_memories(&store, &config).await.unwrap();
        assert_eq!(report.suggestions.len(), 1);
        let (id, _, sessions) = &report.suggestions[0];
        assert_eq!(id, "promo-1");
        assert_eq!(*sessions, 3, "origin session must not count");
        assert!(report.promoted.is_empty(), "suggestion mode never mutates");
        // Memory untouched.
        let m = store.get("promo-1").await.unwrap();
        assert_eq!(
            m.valid_while.unwrap().origin_task.as_deref(),
            Some("feat-x")
        );
    }

    #[tokio::test]
    async fn promotion_below_threshold_is_silent_and_auto_promotes_at_threshold() {
        let (_t, store, mut config) = store_with_task_memory(true).await;

        // Only 2 later sessions (< default 3): nothing happens.
        let events: Vec<EventRow> = ["s1", "s2"]
            .iter()
            .map(|sid| retrieval_event(sid, &["promo-1"]))
            .collect();
        crate::telemetry::persistence::append_events(&store.project_id, &events, None).await;
        let report = promote_reconfirmed_memories(&store, &config).await.unwrap();
        assert!(report.suggestions.is_empty() && report.promoted.is_empty());

        // Third distinct session crosses the threshold → auto-promoted.
        crate::telemetry::persistence::append_events(
            &store.project_id,
            &[retrieval_event("s3", &["promo-1"])],
            None,
        )
        .await;
        let report = promote_reconfirmed_memories(&store, &config).await.unwrap();
        assert_eq!(report.promoted, vec!["promo-1".to_string()]);

        let m = store.get("promo-1").await.unwrap();
        // Clearing origin_task left an all-empty Validity, which the write
        // path normalizes to None — exactly the promoted (project-wide,
        // unconditional) state.
        assert_eq!(m.valid_while, None);
        assert!(m.verified_at.is_some());
        assert_eq!(m.type_, MemoryType::Decision, "promotion never retypes");

        // Idempotent: no longer task-bound, so a second pass is silent.
        config.epistemic.auto_promote = true;
        let report = promote_reconfirmed_memories(&store, &config).await.unwrap();
        assert!(report.promoted.is_empty());
    }
}
