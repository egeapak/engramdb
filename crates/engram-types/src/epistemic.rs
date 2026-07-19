//! Epistemic classification and validity conditions for memories.
//!
//! [`Epistemic`] captures *what kind of claim* a memory makes — orthogonal to
//! [`MemoryType`](super::MemoryType), which says what the memory is *about*.
//! A `hazard` can be a verified fact or an unconfirmed observation; a
//! `convention` is often a frozen decision. The class drives decay defaults,
//! situation-conditioned retrieval weighting, conflict-resolution policy, and
//! doctor verification.
//!
//! [`Validity`] makes a memory's *invalidation condition* first-class data:
//! what change would falsify it (a premise expiring, a watched path changing,
//! a task completing), and how far beyond its origin it generalizes.
//!
//! [`Situation`] names the querying agent's context (starting a session,
//! editing a file, debugging, making a design choice) so retrieval can
//! reweight epistemic classes accordingly. It lives here — in the `types`
//! foundation — because both core scoring and the CLI hook handlers name it.

use serde::{Deserialize, Serialize};

/// What KIND of claim a memory makes — orthogonal to `MemoryType` (what the
/// memory is ABOUT). Drives decay defaults, situation-conditioned retrieval
/// weighting, conflict-resolution policy, and doctor verification.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Epistemic {
    /// The serde-level fallback (see `Memory::epistemic` doc) — the real
    /// defaulting rule is `MemoryType::default_epistemic`.
    #[default]
    /// Structural fact about the code/tooling as it is. Verifiable against
    /// the repo; flips (rather than fades) when the referenced code changes.
    Fact,
    /// Empirical observation measured at a point in time. Environment-
    /// dependent; goes stale; generalizes with caution.
    Observation,
    /// Normative choice with a rationale. Valid while its premise holds;
    /// binding within its origin scope.
    Decision,
}

impl Epistemic {
    /// Stable lowercase name, matching the serde wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Epistemic::Fact => "fact",
            Epistemic::Observation => "observation",
            Epistemic::Decision => "decision",
        }
    }
}

impl std::fmt::Display for Epistemic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Epistemic {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "fact" => Ok(Epistemic::Fact),
            "observation" => Ok(Epistemic::Observation),
            "decision" => Ok(Epistemic::Decision),
            other => Err(format!(
                "invalid epistemic class '{other}' (expected: fact, observation, decision)"
            )),
        }
    }
}

/// How far beyond its origin a memory is claimed to hold.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Generality {
    /// Holds project-wide (default — matches all existing memories).
    #[default]
    Project,
    /// Binding only within its origin task; suppressed/advisory elsewhere.
    Task,
}

impl Generality {
    /// Stable lowercase name, matching the serde wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Generality::Project => "project",
            Generality::Task => "task",
        }
    }
}

impl std::fmt::Display for Generality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Generality {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "project" => Ok(Generality::Project),
            "task" => Ok(Generality::Task),
            other => Err(format!(
                "invalid generality '{other}' (expected: project, task)"
            )),
        }
    }
}

/// First-class invalidation condition: what would falsify this memory.
///
/// An all-empty `Validity` is meaningless; writers must emit `None` on the
/// memory instead (enforced via [`Validity::is_empty`] on the write path).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Validity {
    /// Free-text premise this memory depends on ("while we pin ort rc.12").
    /// Surfaced verbatim to agents so they can judge whether it still holds.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub premise: Option<String>,

    /// Paths/globs whose modification invalidates this memory. DISTINCT from
    /// `Memory::physical` (where the memory APPLIES, which drives scope
    /// scoring): a perf observation may apply to `src/retrieval/` but be
    /// invalidated by `Cargo.lock` changing. Carried in the index as the
    /// `watch_paths` column so the PostToolUse hook can match edited paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalidated_by: Vec<String>,

    /// Task/feature this memory was created for (free text, human-meaningful
    /// — NOT a session id; provenance already carries `session_id`).
    /// Presence means "review when this task completes"; with
    /// `generality: task` it also gates hook injection.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub origin_task: Option<String>,

    /// How far beyond its origin the memory is claimed to hold.
    #[serde(default)]
    pub generality: Generality,

    /// Memory IDs this memory was derived/consolidated from. When a listed
    /// memory is invalidated, doctor flags this one for review (one level
    /// only — no transitive propagation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_from: Vec<String>,
}

impl Validity {
    /// True when every field is unset/empty — such a `Validity` must not be
    /// persisted (the memory's `valid_while` should be `None` instead).
    ///
    /// `generality` alone does not make a `Validity` non-empty: `Project` is
    /// the default, and `Task` without an `origin_task` is meaningless.
    pub fn is_empty(&self) -> bool {
        self.premise.is_none()
            && self.invalidated_by.is_empty()
            && self.origin_task.is_none()
            && self.derived_from.is_empty()
    }
}

/// The querying agent's situation, used to reweight epistemic classes during
/// retrieval (see `[retrieval.scoring.situation]` config). `None` situation
/// means a neutral multiplier of 1.0 — existing queries are unaffected.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Situation {
    /// Session is starting: static facts and project-wide decisions matter
    /// most (set by the SessionStart hook).
    SessionStart,
    /// About to modify a file: decisions and hazards binding on it dominate
    /// (set by the PreToolUse hook).
    FileEdit,
    /// Investigating a failure: observations rank highest (agent-declared).
    Debugging,
    /// Weighing a design choice: prior decisions and their rationale
    /// dominate (agent-declared).
    DesignChoice,
}

impl Situation {
    /// Stable snake_case name, matching the serde wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Situation::SessionStart => "session_start",
            Situation::FileEdit => "file_edit",
            Situation::Debugging => "debugging",
            Situation::DesignChoice => "design_choice",
        }
    }
}

impl std::fmt::Display for Situation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Situation {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "session_start" => Ok(Situation::SessionStart),
            "file_edit" => Ok(Situation::FileEdit),
            "debugging" => Ok(Situation::Debugging),
            "design_choice" => Ok(Situation::DesignChoice),
            other => Err(format!(
                "invalid situation '{other}' (expected: session_start, file_edit, debugging, design_choice)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epistemic_serde_lowercase() {
        assert_eq!(serde_json::to_string(&Epistemic::Fact).unwrap(), "\"fact\"");
        assert_eq!(
            serde_json::to_string(&Epistemic::Observation).unwrap(),
            "\"observation\""
        );
        assert_eq!(
            serde_json::to_string(&Epistemic::Decision).unwrap(),
            "\"decision\""
        );
        let e: Epistemic = serde_json::from_str("\"observation\"").unwrap();
        assert_eq!(e, Epistemic::Observation);
    }

    #[test]
    fn test_situation_serde_snake_case() {
        // Wire values are snake_case: design_choice, not designchoice.
        assert_eq!(
            serde_json::to_string(&Situation::DesignChoice).unwrap(),
            "\"design_choice\""
        );
        assert_eq!(
            serde_json::to_string(&Situation::SessionStart).unwrap(),
            "\"session_start\""
        );
        assert_eq!(
            serde_json::to_string(&Situation::FileEdit).unwrap(),
            "\"file_edit\""
        );
        let s: Situation = serde_json::from_str("\"design_choice\"").unwrap();
        assert_eq!(s, Situation::DesignChoice);
    }

    #[test]
    fn test_from_str_roundtrip_and_errors() {
        for e in [Epistemic::Fact, Epistemic::Observation, Epistemic::Decision] {
            assert_eq!(e.as_str().parse::<Epistemic>().unwrap(), e);
        }
        for g in [Generality::Project, Generality::Task] {
            assert_eq!(g.as_str().parse::<Generality>().unwrap(), g);
        }
        for s in [
            Situation::SessionStart,
            Situation::FileEdit,
            Situation::Debugging,
            Situation::DesignChoice,
        ] {
            assert_eq!(s.as_str().parse::<Situation>().unwrap(), s);
        }
        assert!("bogus".parse::<Epistemic>().is_err());
        assert!("bogus".parse::<Generality>().is_err());
        assert!("bogus".parse::<Situation>().is_err());
    }

    #[test]
    fn test_validity_is_empty() {
        assert!(Validity::default().is_empty());

        // generality alone (even Task) does not make it non-empty
        let g_only = Validity {
            generality: Generality::Task,
            ..Default::default()
        };
        assert!(g_only.is_empty());

        let premise = Validity {
            premise: Some("while we pin ort rc.12".into()),
            ..Default::default()
        };
        assert!(!premise.is_empty());

        let watch = Validity {
            invalidated_by: vec!["Cargo.lock".into()],
            ..Default::default()
        };
        assert!(!watch.is_empty());

        let task = Validity {
            origin_task: Some("demo".into()),
            ..Default::default()
        };
        assert!(!task.is_empty());

        let derived = Validity {
            derived_from: vec!["abc".into()],
            ..Default::default()
        };
        assert!(!derived.is_empty());
    }

    #[test]
    fn test_validity_serde_skips_empty_fields() {
        let v = Validity {
            premise: Some("p".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&v).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("premise"));
        assert!(!obj.contains_key("invalidated_by"));
        assert!(!obj.contains_key("origin_task"));
        assert!(!obj.contains_key("derived_from"));
        // generality serializes (it has a value), defaulting on read
        let back: Validity = serde_json::from_value(json).unwrap();
        assert_eq!(back, v);
    }
}
