//! Trust and confidence weighting based on provenance source
//!
//! This module maps memory provenance sources to trust weights, reflecting the
//! reliability and confidence level of information from different sources.
//!
//! # Default Trust Weights
//!
//! - **Human**: 1.0 - Highest trust for human-provided information
//! - **Agent**: 0.85 - High trust for agent-generated content
//! - **Imported**: 0.7 - Moderate trust for imported data
//! - **Inferred**: 0.6 - Lower trust for inferred/derived information
//!
//! # Usage
//!
//! Trust weights can be customized via configuration using [`trust_weight_from_config`],
//! or use the defaults via [`trust_weight`].

use crate::types::{ProvenanceSource, TrustWeights};

/// Get the default trust weight for a provenance source.
///
/// # Default Weights
/// - Human: 1.0
/// - Agent: 0.85
/// - Imported: 0.7
/// - Inferred: 0.6
///
/// # Arguments
/// * `source` - The provenance source
///
/// # Returns
/// Trust weight from 0.0 to 1.0
pub fn trust_weight(source: ProvenanceSource) -> f64 {
    match source {
        ProvenanceSource::Human => 1.0,
        ProvenanceSource::Agent => 0.85,
        ProvenanceSource::Imported => 0.7,
        ProvenanceSource::Inferred => 0.6,
    }
}

/// Get the trust weight for a provenance source using configurable weights.
///
/// # Arguments
/// * `source` - The provenance source
/// * `weights` - Configuration with custom trust weights
///
/// # Returns
/// Trust weight from 0.0 to 1.0
pub fn trust_weight_from_config(source: ProvenanceSource, weights: &TrustWeights) -> f64 {
    match source {
        ProvenanceSource::Human => weights.human,
        ProvenanceSource::Agent => weights.agent,
        ProvenanceSource::Imported => weights.imported,
        ProvenanceSource::Inferred => weights.inferred,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_trust_weights() {
        assert_eq!(trust_weight(ProvenanceSource::Human), 1.0);
        assert_eq!(trust_weight(ProvenanceSource::Agent), 0.85);
        assert_eq!(trust_weight(ProvenanceSource::Imported), 0.7);
        assert_eq!(trust_weight(ProvenanceSource::Inferred), 0.6);
    }

    #[test]
    fn test_trust_weight_from_config() {
        let weights = TrustWeights {
            human: 1.0,
            agent: 0.9,
            imported: 0.75,
            inferred: 0.5,
        };

        assert_eq!(
            trust_weight_from_config(ProvenanceSource::Human, &weights),
            1.0
        );
        assert_eq!(
            trust_weight_from_config(ProvenanceSource::Agent, &weights),
            0.9
        );
        assert_eq!(
            trust_weight_from_config(ProvenanceSource::Imported, &weights),
            0.75
        );
        assert_eq!(
            trust_weight_from_config(ProvenanceSource::Inferred, &weights),
            0.5
        );
    }

    #[test]
    fn test_trust_weight_from_default_config() {
        let weights = TrustWeights::default();

        assert_eq!(
            trust_weight_from_config(ProvenanceSource::Human, &weights),
            1.0
        );
        assert_eq!(
            trust_weight_from_config(ProvenanceSource::Agent, &weights),
            0.9
        );
        assert_eq!(
            trust_weight_from_config(ProvenanceSource::Imported, &weights),
            0.7
        );
        assert_eq!(
            trust_weight_from_config(ProvenanceSource::Inferred, &weights),
            0.6
        );
    }
}
