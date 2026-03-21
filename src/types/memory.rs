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

use super::{Challenge, Decay, Provenance};

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
        if let Some(expires_at) = self.expires_at {
            Utc::now() > expires_at
        } else {
            false
        }
    }

    /// Check if the memory is active (not expired, not challenged)
    pub fn is_active(&self) -> bool {
        !self.is_expired() && self.status == Status::Active
    }
}

/// Partial update struct for modifying existing memories
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub type_: Option<MemoryType>,

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
    use crate::types::DecayStrategy;

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
