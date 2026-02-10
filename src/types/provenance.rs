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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provenance_constructors() {
        // Test human()
        let human_prov = Provenance::human();
        assert_eq!(human_prov.source, ProvenanceSource::Human);
        assert_eq!(human_prov.agent_id, None);

        // Test agent()
        let agent_prov = Provenance::agent("a1");
        assert_eq!(agent_prov.source, ProvenanceSource::Agent);
        assert_eq!(agent_prov.agent_id, Some("a1".to_string()));

        // Test inferred()
        let inferred_prov = Provenance::inferred();
        assert_eq!(inferred_prov.source, ProvenanceSource::Inferred);
        assert_eq!(inferred_prov.agent_id, None);

        // Test imported()
        let imported_prov = Provenance::imported();
        assert_eq!(imported_prov.source, ProvenanceSource::Imported);
        assert_eq!(imported_prov.agent_id, None);
    }

    #[test]
    fn test_provenance_builder_chain() {
        let prov = Provenance::agent("a1")
            .with_model("gpt")
            .with_session("s1")
            .with_reason("r");

        assert_eq!(prov.source, ProvenanceSource::Agent);
        assert_eq!(prov.agent_id, Some("a1".to_string()));
        assert_eq!(prov.model, Some("gpt".to_string()));
        assert_eq!(prov.session_id, Some("s1".to_string()));
        assert_eq!(prov.reason, Some("r".to_string()));
    }

    #[test]
    fn test_provenance_serde_roundtrip() {
        let original = Provenance::agent("test-agent")
            .with_model("claude-opus")
            .with_session("session-123")
            .with_reason("test reason");

        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Provenance = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.source, original.source);
        assert_eq!(deserialized.agent_id, original.agent_id);
        assert_eq!(deserialized.model, original.model);
        assert_eq!(deserialized.session_id, original.session_id);
        assert_eq!(deserialized.reason, original.reason);
    }
}
