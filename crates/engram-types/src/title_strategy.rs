//! Title-generation strategy selector.
//!
//! This enum is a *configuration value* (it is stored in `[title].strategy`),
//! so it lives in the `types` foundation rather than in the `title` module that
//! consumes it. Keeping it here lets `types::config` reference the default
//! without depending "upward" on the ONNX-backed `title` module. The `title`
//! module re-exports it for ergonomic `crate::title::TitleStrategy` access.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Strategy for automatic title generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleStrategy {
    /// Use keyword extraction (RAKE algorithm). Lightweight, no model needed.
    #[default]
    Keyword,
    /// Use T5-Small ONNX model for abstractive summarization.
    T5,
    /// Disable automatic title generation.
    None,
}

impl TitleStrategy {
    /// Parse a strategy name from a string.
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "keyword" | "keywords" | "rake" => Ok(Self::Keyword),
            "t5" | "t5-small" | "model" => Ok(Self::T5),
            "none" | "off" | "disabled" => Ok(Self::None),
            _ => anyhow::bail!(
                "Invalid title strategy '{}'. Valid options: keyword, t5, none",
                s
            ),
        }
    }
}
