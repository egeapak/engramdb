//! Challenge struct for disputing memory validity.
//!
//! This module defines the [`Challenge`] struct, which represents a dispute
//! or contradiction to a memory's validity. When a challenge is added to a
//! memory, its status changes to Challenged, triggering human review.
//!
//! Challenges include a timestamp, optional agent ID, evidence explaining
//! the contradiction, and an optional source file that conflicts with the memory.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A challenge to a memory's validity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Challenge {
    /// When the challenge was created
    pub timestamp: DateTime<Utc>,

    /// Agent or user who issued the challenge
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Evidence or reason for the challenge
    pub evidence: String,

    /// Source file that contradicts this memory (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

impl Challenge {
    /// Create a new challenge with the given evidence
    pub fn new(evidence: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            agent_id: None,
            evidence: evidence.into(),
            source_file: None,
        }
    }

    /// Set the agent_id field
    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Set the source_file field
    pub fn with_source_file(mut self, source_file: impl Into<String>) -> Self {
        self.source_file = Some(source_file.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_challenge_new() {
        let challenge = Challenge::new("evidence");

        assert_eq!(challenge.evidence, "evidence");
        assert_eq!(challenge.agent_id, None);
        assert_eq!(challenge.source_file, None);
        // Verify timestamp is approximately now (within 1 second)
        let now = Utc::now();
        let diff = (now - challenge.timestamp).num_seconds().abs();
        assert!(diff < 1);
    }

    #[test]
    fn test_challenge_builder() {
        let challenge = Challenge::new("evidence")
            .with_agent("a1")
            .with_source_file("f.rs");

        assert_eq!(challenge.evidence, "evidence");
        assert_eq!(challenge.agent_id, Some("a1".to_string()));
        assert_eq!(challenge.source_file, Some("f.rs".to_string()));
    }
}
