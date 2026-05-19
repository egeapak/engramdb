//! Natural Language Inference (NLI) module for contradiction detection.
//!
//! This module provides an NLI provider abstraction and an ONNX-based implementation
//! that classifies sentence pairs as entailment, neutral, or contradiction.
//! It is used to automatically detect contradictions between new and existing memories.

pub mod onnx;

pub use onnx::{
    NliModelSpec, OnnxNliProvider, DEFAULT_NLI_MODEL, NLI_DEBERTA_XSMALL, NLI_DEBERTA_XSMALL_Q,
};

use anyhow::Result;
use async_trait::async_trait;
use std::fmt;

/// NLI classification label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NliLabel {
    Entailment,
    Neutral,
    Contradiction,
}

impl fmt::Display for NliLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NliLabel::Entailment => write!(f, "entailment"),
            NliLabel::Neutral => write!(f, "neutral"),
            NliLabel::Contradiction => write!(f, "contradiction"),
        }
    }
}

/// Result of classifying a sentence pair via NLI.
#[derive(Debug, Clone)]
pub struct NliResult {
    /// The dominant label (highest probability)
    pub label: NliLabel,
    /// Probability of entailment
    pub entailment: f32,
    /// Probability of neutral
    pub neutral: f32,
    /// Probability of contradiction
    pub contradiction: f32,
}

impl NliResult {
    /// Create an NliResult from the three class probabilities.
    ///
    /// Automatically determines the dominant label.
    pub fn from_probs(entailment: f32, neutral: f32, contradiction: f32) -> Self {
        let label = if entailment >= neutral && entailment >= contradiction {
            NliLabel::Entailment
        } else if neutral >= entailment && neutral >= contradiction {
            NliLabel::Neutral
        } else {
            NliLabel::Contradiction
        };
        Self {
            label,
            entailment,
            neutral,
            contradiction,
        }
    }
}

/// Trait for NLI providers.
///
/// Implementations classify sentence pairs into entailment, neutral, or contradiction.
/// Providers should be thread-safe (Send + Sync) for concurrent usage.
#[async_trait]
pub trait NliProvider: Send + Sync {
    /// Classify a single premise-hypothesis pair.
    async fn classify(&self, premise: &str, hypothesis: &str) -> Result<NliResult>;

    /// Classify multiple premise-hypothesis pairs in batch.
    async fn classify_batch(&self, pairs: &[(&str, &str)]) -> Result<Vec<NliResult>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nli_result_from_probs_entailment() {
        let result = NliResult::from_probs(0.8, 0.1, 0.1);
        assert_eq!(result.label, NliLabel::Entailment);
        assert_eq!(result.entailment, 0.8);
    }

    #[test]
    fn test_nli_result_from_probs_neutral() {
        let result = NliResult::from_probs(0.1, 0.8, 0.1);
        assert_eq!(result.label, NliLabel::Neutral);
        assert_eq!(result.neutral, 0.8);
    }

    #[test]
    fn test_nli_result_from_probs_contradiction() {
        let result = NliResult::from_probs(0.1, 0.1, 0.8);
        assert_eq!(result.label, NliLabel::Contradiction);
        assert_eq!(result.contradiction, 0.8);
    }

    #[test]
    fn test_nli_label_display() {
        assert_eq!(NliLabel::Entailment.to_string(), "entailment");
        assert_eq!(NliLabel::Neutral.to_string(), "neutral");
        assert_eq!(NliLabel::Contradiction.to_string(), "contradiction");
    }
}
