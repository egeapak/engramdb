use serde::{Deserialize, Serialize};

/// Source of a memory (who/what created it)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProvenanceSource {
    /// Created by a human developer
    Human,
    /// Created by an AI agent
    Agent,
    /// Inferred from code analysis
    Inferred,
    /// Imported from external source
    Imported,
}

/// Provenance tracking for a memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// Source type
    pub source: ProvenanceSource,

    /// Agent identifier (e.g., "claude-opus-4", "cursor")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Model identifier (e.g., "claude-opus-4-6")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Session identifier for tracking related memories
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Why this memory was created (optional explanation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Provenance {
    /// Create a new Provenance with the specified source
    pub fn new(source: ProvenanceSource) -> Self {
        Self {
            source,
            agent_id: None,
            model: None,
            session_id: None,
            reason: None,
        }
    }

    /// Create a human-sourced provenance
    pub fn human() -> Self {
        Self::new(ProvenanceSource::Human)
    }

    /// Create an agent-sourced provenance
    pub fn agent(agent_id: impl Into<String>) -> Self {
        Self {
            source: ProvenanceSource::Agent,
            agent_id: Some(agent_id.into()),
            model: None,
            session_id: None,
            reason: None,
        }
    }

    /// Create an inferred provenance
    pub fn inferred() -> Self {
        Self::new(ProvenanceSource::Inferred)
    }

    /// Create an imported provenance
    pub fn imported() -> Self {
        Self::new(ProvenanceSource::Imported)
    }

    /// Set the model field
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the session_id field
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set the reason field
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}
