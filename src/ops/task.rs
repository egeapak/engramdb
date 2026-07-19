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

/// True when the memory's decay is one of its DEFAULTS (type-diagonal or
/// off-diagonal class default) rather than an explicit user choice. Decay
/// has no `PartialEq`; compare the serialized form.
fn decay_is_default(memory: &crate::types::Memory) -> bool {
    let current = match &memory.decay {
        Some(d) => match serde_json::to_string(d) {
            Ok(s) => s,
            Err(_) => return false,
        },
        None => return true,
    };
    let candidates = [
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
pub async fn task_complete(store: &MemoryStore, name: &str) -> Result<TaskCompleteResult> {
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
                if !decay_is_default(memory) {
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

        let result = task_complete(&store, "feat-x").await.unwrap();
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

        let result = task_complete(&store, "feat-z").await.unwrap();
        assert_eq!(result.demoted, vec!["tc-obs".to_string()]);
        assert!(store.get("tc-dead").await.unwrap().decay.is_some());
    }
}
