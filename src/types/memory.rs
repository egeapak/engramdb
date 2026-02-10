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
    pub visibility: Option<Visibility>,

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
        if let Some(visibility) = self.visibility {
            memory.visibility = visibility;
        }
        if let Some(expires_at) = self.expires_at {
            memory.expires_at = Some(expires_at);
        }

        memory.mark_updated();
    }
}
