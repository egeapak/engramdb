//! Memory struct and related enums (MemoryType, Status, Visibility).
//!
//! This module defines the core [`Memory`] struct, which represents a single piece
//! of knowledge stored in EngramDB. Each memory has:
//! - Content (summary, content, optional details)
//! - Scope (physical paths, logical domains)
//! - Metadata (criticality, confidence, timestamps)
//! - Provenance tracking (who/what created it)
//! - Decay configuration (how relevance decreases over time)
//! - Challenges (validity disputes)
//!
//! The [`MemoryUpdate`] struct provides partial updates to existing memories.
//! Memories can be Active, NeedsReview, or Challenged, and can be Shared or Personal.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Challenge, Decay, Epistemic, Provenance, Validity};

/// Type of memory - categorizes the kind of knowledge stored
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    /// Architectural or design decision
    Decision,
    /// Coding convention or pattern
    Convention,
    /// Known bug, footgun, or warning
    Hazard,
    /// Background context about the codebase
    Context,
    /// In-flight refactor or planned change
    Intent,
    /// Relationship between components
    Relationship,
    /// Debugging note or investigation result
    Debug,
    /// User or agent preference
    Preference,
}

impl MemoryType {
    /// Get the default decay strategy for this memory type
    pub fn default_decay(&self) -> Option<Decay> {
        match self {
            MemoryType::Decision
            | MemoryType::Convention
            | MemoryType::Context
            | MemoryType::Relationship
            | MemoryType::Preference => Some(Decay::none()),
            MemoryType::Hazard => Some(Decay::none_with_floor(0.5)),
            MemoryType::Intent => Some(Decay::exponential(Duration::days(14))),
            MemoryType::Debug => Some(Decay::exponential(Duration::days(30))),
        }
    }

    /// Default epistemic class when the author doesn't specify one.
    ///
    /// This mapping is the backward-compatibility anchor: memory files that
    /// predate the `epistemic` field materialize to exactly these values, so
    /// it is effectively frozen — changing it changes the behavior of
    /// existing stores. (Hazard → Fact is deliberate: a footgun is
    /// verifiable against the repo, and the Fact class preserves Hazard's
    /// never-forget floor-0.5 decay via the diagonal rule in
    /// [`default_decay`].)
    pub fn default_epistemic(&self) -> Epistemic {
        match self {
            MemoryType::Context
            | MemoryType::Convention
            | MemoryType::Relationship
            | MemoryType::Hazard => Epistemic::Fact,
            MemoryType::Debug => Epistemic::Observation,
            MemoryType::Decision | MemoryType::Intent | MemoryType::Preference => {
                Epistemic::Decision
            }
        }
    }
}

/// Default decay for a (type, epistemic class) pair.
///
/// INVARIANT (diagonal): `default_decay(t, t.default_epistemic())` is
/// byte-identical to `t.default_decay()` — every memory whose class was
/// defaulted from its type decays exactly as it did before the epistemic
/// axis existed. A paired test asserts this for all types.
///
/// Off-diagonal, the declared class wins over the type default. The
/// Observation constants (90d half-life, 0.2 floor) are the built-in
/// defaults; the create path substitutes `[epistemic]` config values when
/// set (`observation_half_life_days` / `observation_decay_floor`) — config
/// resolution happens in `ops::create`, keeping this function pure.
pub fn default_decay(type_: MemoryType, epistemic: Epistemic) -> Option<Decay> {
    if epistemic == type_.default_epistemic() {
        return type_.default_decay();
    }
    match epistemic {
        Epistemic::Observation => Some(Decay::exponential(Duration::days(90)).with_floor(0.2)),
        // Facts flip when the code changes; they don't fade with time.
        Epistemic::Fact => Some(Decay::none()),
        // Decisions are premise-bound, not time-bound.
        Epistemic::Decision => type_.default_decay(),
    }
}

/// Status of a memory
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Status {
    /// Memory is active and valid
    Active,
    /// Memory needs human review
    NeedsReview,
    /// Memory has been challenged
    Challenged,
}

/// Visibility level of a memory
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Visibility {
    /// Shared across all agents/sessions
    Shared,
    /// Personal to a specific agent/session
    Personal,
}

/// Core Memory struct - represents a single piece of knowledge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Unique identifier (UUID v7 - time-sortable)
    pub id: String,

    /// Type of memory
    #[serde(rename = "type")]
    pub type_: MemoryType,

    /// Epistemic class — what KIND of claim this memory makes (orthogonal to
    /// `type_`, which says what it is ABOUT). Non-optional in the domain
    /// model; the memory-file parsers default it from
    /// `type_.default_epistemic()` for files that predate the field. (The
    /// serde default — `Fact` — only covers exotic JSON paths that bypass
    /// the file parsers; it is not the authoritative defaulting rule.)
    #[serde(default)]
    pub epistemic: Epistemic,

    /// Invalidation condition. `None` = no declared falsifier.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub valid_while: Option<Validity>,

    /// Valid-time start: when the claim became true in the world. `None` ⇒
    /// `created_at` (the overwhelmingly common case; set explicitly only to
    /// backdate).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub valid_from: Option<DateTime<Utc>>,

    /// Valid-time end: when the claim stopped holding. `None` ⇒ still valid.
    /// Setting this CLOSES the validity window — the memory is retained on
    /// disk and queryable via `include_invalidated`, but excluded from
    /// default retrieval. Never set by deletion.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub invalidated_at: Option<DateTime<Utc>>,

    /// Id of the memory that superseded this one, when the window was closed
    /// by supersession (ADR-style reverse link of `supersedes`). `None` when
    /// closed by `resolve invalidate` without a successor.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub superseded_by: Option<String>,

    /// Brief summary (≤100 chars)
    pub summary: String,

    /// Optional short title (a few words) for human-readable filenames.
    /// When present, the memory file is named `<slug>_<uuid>.md` instead of `<uuid>.md`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Main content (~500 tokens)
    pub content: String,

    /// Extended details (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,

    /// Physical scope - file paths or globs
    pub physical: Vec<String>,

    /// Logical scope - dot-notation domains
    pub logical: Vec<String>,

    /// Searchable tags
    #[serde(default)]
    pub tags: Vec<String>,

    /// Criticality score (0.0 to 1.0)
    pub criticality: f64,

    /// Decay configuration (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay: Option<Decay>,

    /// Provenance tracking
    pub provenance: Provenance,

    /// Confidence in this memory (0.0 to 1.0)
    pub confidence: f64,

    /// IDs of memories this supersedes
    #[serde(default)]
    pub supersedes: Vec<String>,

    /// Current status
    pub status: Status,

    /// Visibility level
    pub visibility: Visibility,

    /// Challenges to this memory's validity
    #[serde(default)]
    pub challenges: Vec<Challenge>,

    /// Timestamp of last verification/resolution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,

    /// Last access timestamp
    pub accessed_at: DateTime<Utc>,

    /// Optional expiration timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl Memory {
    /// Create a new Memory with sensible defaults
    pub fn new(
        type_: MemoryType,
        summary: impl Into<String>,
        content: impl Into<String>,
        provenance: Provenance,
    ) -> Self {
        let now = Utc::now();
        let id = Uuid::now_v7().to_string();

        Self {
            id,
            type_,
            epistemic: type_.default_epistemic(),
            valid_while: None,
            valid_from: None,
            invalidated_at: None,
            superseded_by: None,
            summary: summary.into(),
            title: None,
            content: content.into(),
            details: None,
            physical: vec!["/".to_string()],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            decay: type_.default_decay(),
            provenance,
            confidence: 0.8,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: now,
            updated_at: now,
            accessed_at: now,
            expires_at: None,
        }
    }

    /// Update the accessed_at timestamp
    pub fn touch(&mut self) {
        self.accessed_at = Utc::now();
    }

    /// Mark the memory as updated
    pub fn mark_updated(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Add a challenge to this memory
    pub fn add_challenge(&mut self, challenge: Challenge) {
        self.challenges.push(challenge);
        self.status = Status::Challenged;
        self.mark_updated();
    }

    /// Check if the memory has expired
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(Utc::now())
    }

    /// Check if the memory has expired as of `now`.
    ///
    /// Parameterized variant of [`Self::is_expired`] so callers that score a
    /// whole result set against one timestamp (e.g. the retrieval engine)
    /// make a consistent decision for every memory in the batch.
    pub fn is_expired_at(&self, now: chrono::DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expires_at| now > expires_at)
    }

    /// Check if the memory's validity window is closed as of `now`.
    ///
    /// A future-dated `invalidated_at` (scheduled invalidation) is NOT yet
    /// invalidated — mirroring `is_expired_at` semantics.
    pub fn is_invalidated_at(&self, now: chrono::DateTime<Utc>) -> bool {
        self.invalidated_at.is_some_and(|t| now >= t)
    }

    /// Check if the memory's validity window is closed as of the current time.
    pub fn is_invalidated(&self) -> bool {
        self.is_invalidated_at(Utc::now())
    }

    /// Check if the memory is active (not expired, not invalidated, not challenged)
    pub fn is_active(&self) -> bool {
        !self.is_expired() && !self.is_invalidated() && self.status == Status::Active
    }
}

/// Partial update struct for modifying existing memories
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub type_: Option<MemoryType>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub epistemic: Option<Epistemic>,

    /// `Some(validity)` replaces `valid_while`; an all-empty `Validity`
    /// clears it to `None` on the memory (see `apply_to`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_while: Option<Validity>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<DateTime<Utc>>,

    /// `Some(ts)` closes the validity window at `ts`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalidated_at: Option<DateTime<Utc>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,

    /// When true, clears `invalidated_at` AND `superseded_by` — the §2.4
    /// reopening surface. Applied after the setter fields above, so a single
    /// update cannot both set and clear.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_invalidated: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub criticality: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay: Option<Decay>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenges: Option<Vec<Challenge>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl MemoryUpdate {
    /// Create a new empty update
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply this update to a memory
    pub fn apply_to(&self, memory: &mut Memory) {
        if let Some(type_) = self.type_ {
            memory.type_ = type_;
        }
        if let Some(epistemic) = self.epistemic {
            memory.epistemic = epistemic;
        }
        if let Some(ref valid_while) = self.valid_while {
            // An all-empty Validity clears the field: `is_empty` is the
            // write-path guard that keeps meaningless Validity off disk.
            memory.valid_while = if valid_while.is_empty() {
                None
            } else {
                Some(valid_while.clone())
            };
        }
        if let Some(valid_from) = self.valid_from {
            memory.valid_from = Some(valid_from);
        }
        if let Some(invalidated_at) = self.invalidated_at {
            memory.invalidated_at = Some(invalidated_at);
        }
        if let Some(ref superseded_by) = self.superseded_by {
            memory.superseded_by = Some(superseded_by.clone());
        }
        if self.clear_invalidated {
            memory.invalidated_at = None;
            memory.superseded_by = None;
        }
        if let Some(ref summary) = self.summary {
            memory.summary = summary.clone();
        }
        if let Some(ref title) = self.title {
            memory.title = Some(title.clone());
        }
        if let Some(ref content) = self.content {
            memory.content = content.clone();
        }
        if let Some(ref details) = self.details {
            memory.details = Some(details.clone());
        }
        if let Some(ref physical) = self.physical {
            memory.physical = physical.clone();
        }
        if let Some(ref logical) = self.logical {
            memory.logical = logical.clone();
        }
        if let Some(ref tags) = self.tags {
            memory.tags = tags.clone();
        }
        if let Some(criticality) = self.criticality {
            memory.criticality = criticality;
        }
        if let Some(ref decay) = self.decay {
            memory.decay = Some(decay.clone());
        }
        if let Some(ref provenance) = self.provenance {
            memory.provenance = provenance.clone();
        }
        if let Some(confidence) = self.confidence {
            memory.confidence = confidence;
        }
        if let Some(ref supersedes) = self.supersedes {
            memory.supersedes = supersedes.clone();
        }
        if let Some(status) = self.status {
            memory.status = status;
        }
        if let Some(ref challenges) = self.challenges {
            memory.challenges = challenges.clone();
        }
        if let Some(visibility) = self.visibility {
            memory.visibility = visibility;
        }
        if let Some(verified_at) = self.verified_at {
            memory.verified_at = Some(verified_at);
        }
        if let Some(expires_at) = self.expires_at {
            memory.expires_at = Some(expires_at);
        }

        memory.mark_updated();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DecayStrategy;

    #[test]
    fn test_memory_new_defaults() {
        let memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );

        assert_eq!(memory.criticality, 0.5);
        assert_eq!(memory.confidence, 0.8);
        assert_eq!(memory.physical, vec!["/".to_string()]);
        assert_eq!(memory.status, Status::Active);
        assert_eq!(memory.visibility, Visibility::Shared);
        assert!(memory.tags.is_empty());
        assert!(memory.logical.is_empty());
        assert!(memory.supersedes.is_empty());
        assert!(memory.challenges.is_empty());
    }

    #[test]
    fn test_memory_type_default_decay() {
        assert_eq!(
            MemoryType::Decision.default_decay().unwrap().strategy,
            DecayStrategy::None
        );
        assert_eq!(
            MemoryType::Convention.default_decay().unwrap().strategy,
            DecayStrategy::None
        );
        assert_eq!(
            MemoryType::Context.default_decay().unwrap().strategy,
            DecayStrategy::None
        );
        assert_eq!(
            MemoryType::Relationship.default_decay().unwrap().strategy,
            DecayStrategy::None
        );
        assert_eq!(
            MemoryType::Preference.default_decay().unwrap().strategy,
            DecayStrategy::None
        );

        let hazard_decay = MemoryType::Hazard.default_decay().unwrap();
        assert_eq!(hazard_decay.strategy, DecayStrategy::None);
        assert_eq!(hazard_decay.floor, 0.5);

        let intent_decay = MemoryType::Intent.default_decay().unwrap();
        assert_eq!(intent_decay.strategy, DecayStrategy::Exponential);
        assert_eq!(intent_decay.half_life, Some(Duration::days(14)));

        let debug_decay = MemoryType::Debug.default_decay().unwrap();
        assert_eq!(debug_decay.strategy, DecayStrategy::Exponential);
        assert_eq!(debug_decay.half_life, Some(Duration::days(30)));
    }

    #[test]
    fn test_memory_touch() {
        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        let original_access = memory.accessed_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        memory.touch();

        assert!(memory.accessed_at > original_access);
    }

    #[test]
    fn test_memory_add_challenge() {
        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        let original_updated = memory.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        let challenge = Challenge::new("Evidence of contradiction");
        memory.add_challenge(challenge);

        assert_eq!(memory.challenges.len(), 1);
        assert_eq!(memory.status, Status::Challenged);
        assert!(memory.updated_at > original_updated);
    }

    #[test]
    fn test_memory_is_expired_none() {
        let memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());

        assert!(!memory.is_expired());
    }

    #[test]
    fn test_memory_is_expired_future() {
        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        memory.expires_at = Some(Utc::now() + Duration::days(1));

        assert!(!memory.is_expired());
    }

    #[test]
    fn test_memory_is_expired_past() {
        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        memory.expires_at = Some(Utc::now() - Duration::days(1));

        assert!(memory.is_expired());
    }

    #[test]
    fn test_memory_is_active() {
        // Active and not expired
        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        assert!(memory.is_active());

        // Challenged status
        memory.status = Status::Challenged;
        assert!(!memory.is_active());

        // Expired
        memory.status = Status::Active;
        memory.expires_at = Some(Utc::now() - Duration::days(1));
        assert!(!memory.is_active());
    }

    #[test]
    fn test_memory_update_apply_partial() {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Original summary",
            "Original content",
            Provenance::human(),
        );
        let original_content = memory.content.clone();
        let original_updated = memory.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut update = MemoryUpdate::new();
        update.summary = Some("New summary".to_string());
        update.apply_to(&mut memory);

        assert_eq!(memory.summary, "New summary");
        assert_eq!(memory.content, original_content);
        assert!(memory.updated_at > original_updated);
    }

    #[test]
    fn test_memory_update_apply_all_fields() {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Original",
            "Original",
            Provenance::human(),
        );

        let update = MemoryUpdate {
            type_: Some(MemoryType::Convention),
            epistemic: None,
            valid_while: None,
            valid_from: None,
            invalidated_at: None,
            superseded_by: None,
            clear_invalidated: false,
            summary: Some("New summary".to_string()),
            title: None,
            content: Some("New content".to_string()),
            details: Some("New details".to_string()),
            physical: Some(vec!["/src/main.rs".to_string()]),
            logical: Some(vec!["app.core".to_string()]),
            tags: Some(vec!["tag1".to_string()]),
            criticality: Some(0.9),
            decay: Some(Decay::exponential(Duration::days(7))),
            provenance: Some(Provenance::agent("test-agent")),
            confidence: Some(0.95),
            supersedes: Some(vec!["old-id".to_string()]),
            status: Some(Status::NeedsReview),
            challenges: None,
            visibility: Some(Visibility::Personal),
            verified_at: Some(Utc::now()),
            expires_at: Some(Utc::now() + Duration::days(30)),
        };

        update.apply_to(&mut memory);

        assert_eq!(memory.type_, MemoryType::Convention);
        assert_eq!(memory.summary, "New summary");
        assert_eq!(memory.content, "New content");
        assert_eq!(memory.details, Some("New details".to_string()));
        assert_eq!(memory.physical, vec!["/src/main.rs".to_string()]);
        assert_eq!(memory.logical, vec!["app.core".to_string()]);
        assert_eq!(memory.tags, vec!["tag1".to_string()]);
        assert_eq!(memory.criticality, 0.9);
        assert_eq!(memory.confidence, 0.95);
        assert_eq!(memory.supersedes, vec!["old-id".to_string()]);
        assert_eq!(memory.status, Status::NeedsReview);
        assert_eq!(memory.visibility, Visibility::Personal);
        assert!(memory.expires_at.is_some());
    }

    #[test]
    fn test_memory_verified_at_default_is_none() {
        let memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );
        assert!(memory.verified_at.is_none());
    }

    #[test]
    fn test_memory_verified_at_serialization_roundtrip() {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );
        let now = Utc::now();
        memory.verified_at = Some(now);

        let json = serde_json::to_string(&memory).unwrap();
        let deserialized: Memory = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.verified_at, Some(now));
        assert_eq!(deserialized.summary, "Test summary");
        assert_eq!(deserialized.content, "Test content");
    }

    #[test]
    fn test_memory_update_apply_challenges() {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Original",
            "Original",
            Provenance::human(),
        );
        assert!(memory.challenges.is_empty());

        let challenges = vec![
            Challenge::new("First evidence"),
            Challenge::new("Second evidence"),
        ];
        let mut update = MemoryUpdate::new();
        update.challenges = Some(challenges);
        update.status = Some(Status::Challenged);
        update.apply_to(&mut memory);

        assert_eq!(memory.challenges.len(), 2);
        assert_eq!(memory.challenges[0].evidence, "First evidence");
        assert_eq!(memory.challenges[1].evidence, "Second evidence");
        assert_eq!(memory.status, Status::Challenged);
    }

    #[test]
    fn test_memory_update_none_challenges_preserves_existing() {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Original",
            "Original",
            Provenance::human(),
        );
        memory.add_challenge(Challenge::new("Existing challenge"));

        let update = MemoryUpdate::new(); // all fields None
        update.apply_to(&mut memory);

        // challenges should be untouched
        assert_eq!(memory.challenges.len(), 1);
        assert_eq!(memory.challenges[0].evidence, "Existing challenge");
    }

    #[test]
    fn test_default_epistemic_mapping() {
        use crate::Epistemic;
        assert_eq!(MemoryType::Context.default_epistemic(), Epistemic::Fact);
        assert_eq!(MemoryType::Convention.default_epistemic(), Epistemic::Fact);
        assert_eq!(
            MemoryType::Relationship.default_epistemic(),
            Epistemic::Fact
        );
        // Hazard → Fact is deliberate (D3): preserves the never-forget
        // floor-0.5 decay via the diagonal rule.
        assert_eq!(MemoryType::Hazard.default_epistemic(), Epistemic::Fact);
        assert_eq!(
            MemoryType::Debug.default_epistemic(),
            Epistemic::Observation
        );
        assert_eq!(
            MemoryType::Decision.default_epistemic(),
            Epistemic::Decision
        );
        assert_eq!(MemoryType::Intent.default_epistemic(), Epistemic::Decision);
        assert_eq!(
            MemoryType::Preference.default_epistemic(),
            Epistemic::Decision
        );
    }

    #[test]
    fn test_default_decay_diagonal_invariant() {
        // For every type, the two-arg default with the type's own default
        // class must be byte-identical to the one-arg default — this is what
        // makes the epistemic migration a behavioral no-op.
        use crate::default_decay;
        for t in [
            MemoryType::Decision,
            MemoryType::Convention,
            MemoryType::Hazard,
            MemoryType::Context,
            MemoryType::Intent,
            MemoryType::Relationship,
            MemoryType::Debug,
            MemoryType::Preference,
        ] {
            let diagonal = default_decay(t, t.default_epistemic());
            let legacy = t.default_decay();
            match (diagonal, legacy) {
                (Some(a), Some(b)) => {
                    assert_eq!(a.strategy, b.strategy, "strategy drift for {t:?}");
                    assert_eq!(a.half_life, b.half_life, "half_life drift for {t:?}");
                    assert_eq!(a.ttl, b.ttl, "ttl drift for {t:?}");
                    assert_eq!(a.floor, b.floor, "floor drift for {t:?}");
                }
                (None, None) => {}
                (a, b) => panic!("diagonal decay mismatch for {t:?}: {a:?} vs {b:?}"),
            }
        }
    }

    #[test]
    fn test_default_decay_off_diagonal() {
        use crate::{default_decay, Epistemic};

        // Convention (diagonal Fact) declared as Observation: staleness decay.
        let obs = default_decay(MemoryType::Convention, Epistemic::Observation).unwrap();
        assert_eq!(obs.strategy, DecayStrategy::Exponential);
        assert_eq!(obs.half_life, Some(Duration::days(90)));
        assert_eq!(obs.floor, 0.2);

        // Debug (diagonal Observation) declared as Fact: no decay.
        let fact = default_decay(MemoryType::Debug, Epistemic::Fact).unwrap();
        assert_eq!(fact.strategy, DecayStrategy::None);

        // Convention declared as Decision: falls back to the type default.
        let dec = default_decay(MemoryType::Convention, Epistemic::Decision).unwrap();
        assert_eq!(dec.strategy, DecayStrategy::None);
        // Debug declared as Decision keeps Debug's own exponential default.
        let dbg_dec = default_decay(MemoryType::Debug, Epistemic::Decision).unwrap();
        assert_eq!(dbg_dec.strategy, DecayStrategy::Exponential);
        assert_eq!(dbg_dec.half_life, Some(Duration::days(30)));
    }

    #[test]
    fn test_memory_new_epistemic_defaults() {
        for t in [
            MemoryType::Decision,
            MemoryType::Hazard,
            MemoryType::Debug,
            MemoryType::Convention,
        ] {
            let memory = Memory::new(t, "s", "c", Provenance::human());
            assert_eq!(memory.epistemic, t.default_epistemic());
            assert!(memory.valid_while.is_none());
            assert!(memory.valid_from.is_none());
            assert!(memory.invalidated_at.is_none());
            assert!(memory.superseded_by.is_none());
        }
    }

    #[test]
    fn test_memory_update_apply_epistemic_and_validity() {
        use crate::{Epistemic, Generality, Validity};
        let mut memory = Memory::new(MemoryType::Convention, "s", "c", Provenance::human());
        assert_eq!(memory.epistemic, Epistemic::Fact);

        // Set class + validity
        let mut update = MemoryUpdate::new();
        update.epistemic = Some(Epistemic::Decision);
        update.valid_while = Some(Validity {
            premise: Some("while rate limits exist".into()),
            origin_task: Some("demo".into()),
            generality: Generality::Task,
            ..Default::default()
        });
        update.apply_to(&mut memory);
        assert_eq!(memory.epistemic, Epistemic::Decision);
        let vw = memory.valid_while.as_ref().unwrap();
        assert_eq!(vw.origin_task.as_deref(), Some("demo"));
        assert_eq!(vw.generality, Generality::Task);

        // All-empty Validity clears the field
        let mut clear = MemoryUpdate::new();
        clear.valid_while = Some(Validity::default());
        clear.apply_to(&mut memory);
        assert!(memory.valid_while.is_none());

        // None preserves
        let noop = MemoryUpdate::new();
        noop.apply_to(&mut memory);
        assert_eq!(memory.epistemic, Epistemic::Decision);
    }

    #[test]
    fn test_memory_update_invalidate_and_reopen() {
        let mut memory = Memory::new(MemoryType::Decision, "s", "c", Provenance::human());
        let ts = Utc::now() - Duration::hours(1);

        let mut invalidate = MemoryUpdate::new();
        invalidate.invalidated_at = Some(ts);
        invalidate.superseded_by = Some("winner-id".to_string());
        invalidate.apply_to(&mut memory);
        assert!(memory.is_invalidated());
        assert_eq!(memory.superseded_by.as_deref(), Some("winner-id"));

        // Reopening clears both fields
        let mut reopen = MemoryUpdate::new();
        reopen.clear_invalidated = true;
        reopen.apply_to(&mut memory);
        assert!(!memory.is_invalidated());
        assert!(memory.invalidated_at.is_none());
        assert!(memory.superseded_by.is_none());
    }

    #[test]
    fn test_is_active_invalidated() {
        let mut memory = Memory::new(MemoryType::Decision, "s", "c", Provenance::human());
        assert!(memory.is_active());

        // Past invalidation ⇒ inactive
        memory.invalidated_at = Some(Utc::now() - Duration::days(1));
        assert!(memory.is_invalidated());
        assert!(!memory.is_active());

        // Future-dated invalidation (scheduled) ⇒ still active now
        memory.invalidated_at = Some(Utc::now() + Duration::days(1));
        assert!(!memory.is_invalidated());
        assert!(memory.is_active());

        // None ⇒ unchanged behavior
        memory.invalidated_at = None;
        assert!(memory.is_active());
    }

    #[test]
    fn test_memory_new_title_is_none() {
        let memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );
        assert_eq!(memory.title, None);
    }

    #[test]
    fn test_memory_update_apply_title() {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Original",
            "Original",
            Provenance::human(),
        );
        assert_eq!(memory.title, None);

        let mut update = MemoryUpdate::new();
        update.title = Some("My Title".to_string());
        update.apply_to(&mut memory);

        assert_eq!(memory.title, Some("My Title".to_string()));
    }

    #[test]
    fn test_memory_update_none_title_preserves_existing() {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Original",
            "Original",
            Provenance::human(),
        );
        memory.title = Some("Existing Title".to_string());

        let update = MemoryUpdate::new(); // all fields None
        update.apply_to(&mut memory);

        assert_eq!(memory.title, Some("Existing Title".to_string()));
    }
}
