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
