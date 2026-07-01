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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keyword_aliases() {
        for s in ["keyword", "keywords", "rake", "KEYWORD", "Rake"] {
            assert_eq!(TitleStrategy::parse(s).unwrap(), TitleStrategy::Keyword);
        }
    }

    #[test]
    fn parse_t5_aliases() {
        for s in ["t5", "t5-small", "model", "T5", "Model"] {
            assert_eq!(TitleStrategy::parse(s).unwrap(), TitleStrategy::T5);
        }
    }

    #[test]
    fn parse_none_aliases() {
        for s in ["none", "off", "disabled", "NONE", "Off"] {
            assert_eq!(TitleStrategy::parse(s).unwrap(), TitleStrategy::None);
        }
    }

    #[test]
    fn parse_invalid_is_error() {
        let err = TitleStrategy::parse("nonsense").unwrap_err().to_string();
        assert!(err.contains("Invalid title strategy"));
        assert!(err.contains("nonsense"));
    }

    #[test]
    fn default_is_keyword() {
        assert_eq!(TitleStrategy::default(), TitleStrategy::Keyword);
    }

    #[test]
    fn serde_uses_lowercase_names() {
        // `#[serde(rename_all = "lowercase")]` is what the `[title].strategy`
        // config field relies on; assert the on-disk spelling stays stable.
        assert_eq!(
            serde_json::to_string(&TitleStrategy::Keyword).unwrap(),
            "\"keyword\""
        );
        assert_eq!(serde_json::to_string(&TitleStrategy::T5).unwrap(), "\"t5\"");
        assert_eq!(
            serde_json::to_string(&TitleStrategy::None).unwrap(),
            "\"none\""
        );
    }

    #[test]
    fn serde_round_trip() {
        for strat in [
            TitleStrategy::Keyword,
            TitleStrategy::T5,
            TitleStrategy::None,
        ] {
            let json = serde_json::to_string(&strat).unwrap();
            let back: TitleStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(strat, back);
        }
    }
}
