//! Shared parsing helpers for string-to-enum conversions.
//!
//! These functions are used by both CLI and MCP boundaries to convert
//! user-provided strings into typed enums.

use crate::retrieval::engine::DetailLevel;
use crate::types::{DecayStrategy, MemoryType, Status, Visibility};
use anyhow::{bail, Result};

/// Parse a string into a MemoryType enum.
pub fn parse_memory_type(s: &str) -> Result<MemoryType> {
    match s.to_lowercase().as_str() {
        "decision" => Ok(MemoryType::Decision),
        "convention" => Ok(MemoryType::Convention),
        "hazard" => Ok(MemoryType::Hazard),
        "context" => Ok(MemoryType::Context),
        "intent" => Ok(MemoryType::Intent),
        "relationship" => Ok(MemoryType::Relationship),
        "debug" => Ok(MemoryType::Debug),
        "preference" => Ok(MemoryType::Preference),
        _ => bail!(
            "Invalid memory type: {}. Valid types: decision, convention, hazard, context, intent, relationship, debug, preference",
            s
        ),
    }
}

/// Parse a string into a Visibility enum.
pub fn parse_visibility(s: &str) -> Result<Visibility> {
    match s.to_lowercase().as_str() {
        "shared" => Ok(Visibility::Shared),
        "personal" => Ok(Visibility::Personal),
        _ => bail!("Invalid visibility: {}. Valid values: shared, personal", s),
    }
}

/// Parse a string into a Status enum.
pub fn parse_status(s: &str) -> Result<Status> {
    match s.to_lowercase().as_str() {
        "active" => Ok(Status::Active),
        "needsreview" | "needs-review" | "needs_review" => Ok(Status::NeedsReview),
        "challenged" => Ok(Status::Challenged),
        _ => bail!(
            "Invalid status: {}. Valid values: active, needsreview, challenged",
            s
        ),
    }
}

/// Parse a string into a DecayStrategy enum.
pub fn parse_decay_strategy(s: &str) -> Result<DecayStrategy> {
    match s.to_lowercase().as_str() {
        "none" => Ok(DecayStrategy::None),
        "linear" => Ok(DecayStrategy::Linear),
        "exponential" => Ok(DecayStrategy::Exponential),
        "step" => Ok(DecayStrategy::Step),
        _ => bail!(
            "Invalid decay strategy: {}. Valid values: none, linear, exponential, step",
            s
        ),
    }
}

/// Validate that a score value is within the valid range [0.0, 1.0].
pub fn validate_score(value: f64, field_name: &str) -> Result<f64> {
    if !(0.0..=1.0).contains(&value) {
        bail!("{} must be between 0.0 and 1.0, got {}", field_name, value);
    }
    Ok(value)
}

/// Parse an optional list of memory-type strings into a type filter.
///
/// `None` or an empty list means "no type filter". Shared by the CLI
/// (`--type`, repeated) and the MCP `types` array so both boundaries parse
/// and reject identically.
pub fn parse_type_filter(types: Option<&[String]>) -> Result<Option<Vec<MemoryType>>> {
    match types {
        None | Some([]) => Ok(None),
        Some(list) => Ok(Some(
            list.iter()
                .map(|t| parse_memory_type(t))
                .collect::<Result<Vec<_>>>()?,
        )),
    }
}

/// Parse an optional detail-level string, defaulting to
/// [`DetailLevel::Content`] — the shared default for both front-ends.
pub fn parse_detail_level_or_default(s: Option<&str>) -> Result<DetailLevel> {
    match s {
        Some(s) => parse_detail_level(s),
        None => Ok(DetailLevel::Content),
    }
}

/// Parse a string into a DetailLevel enum.
pub fn parse_detail_level(s: &str) -> Result<DetailLevel> {
    match s.to_lowercase().as_str() {
        "summary" => Ok(DetailLevel::Summary),
        "content" => Ok(DetailLevel::Content),
        "full" => Ok(DetailLevel::Full),
        _ => bail!(
            "Invalid detail level: {}. Must be summary, content, or full",
            s
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_memory_type_valid() {
        assert_eq!(parse_memory_type("decision").unwrap(), MemoryType::Decision);
        assert_eq!(
            parse_memory_type("convention").unwrap(),
            MemoryType::Convention
        );
        assert_eq!(parse_memory_type("hazard").unwrap(), MemoryType::Hazard);
        assert_eq!(parse_memory_type("context").unwrap(), MemoryType::Context);
        assert_eq!(parse_memory_type("intent").unwrap(), MemoryType::Intent);
        assert_eq!(
            parse_memory_type("relationship").unwrap(),
            MemoryType::Relationship
        );
        assert_eq!(parse_memory_type("debug").unwrap(), MemoryType::Debug);
        assert_eq!(
            parse_memory_type("preference").unwrap(),
            MemoryType::Preference
        );
    }

    #[test]
    fn test_parse_memory_type_case_insensitive() {
        assert_eq!(parse_memory_type("Decision").unwrap(), MemoryType::Decision);
        assert_eq!(parse_memory_type("HAZARD").unwrap(), MemoryType::Hazard);
    }

    #[test]
    fn test_parse_memory_type_invalid() {
        assert!(parse_memory_type("invalid").is_err());
        assert!(parse_memory_type("").is_err());
    }

    #[test]
    fn test_parse_visibility_valid() {
        assert_eq!(parse_visibility("shared").unwrap(), Visibility::Shared);
        assert_eq!(parse_visibility("personal").unwrap(), Visibility::Personal);
    }

    #[test]
    fn test_parse_visibility_case_insensitive() {
        assert_eq!(parse_visibility("Shared").unwrap(), Visibility::Shared);
        assert_eq!(parse_visibility("PERSONAL").unwrap(), Visibility::Personal);
    }

    #[test]
    fn test_parse_visibility_invalid() {
        assert!(parse_visibility("invalid").is_err());
    }

    #[test]
    fn test_parse_status_valid() {
        assert_eq!(parse_status("active").unwrap(), Status::Active);
        assert_eq!(parse_status("needsreview").unwrap(), Status::NeedsReview);
        assert_eq!(parse_status("needs-review").unwrap(), Status::NeedsReview);
        assert_eq!(parse_status("needs_review").unwrap(), Status::NeedsReview);
        assert_eq!(parse_status("challenged").unwrap(), Status::Challenged);
    }

    #[test]
    fn test_parse_status_invalid() {
        assert!(parse_status("invalid").is_err());
    }

    #[test]
    fn test_parse_decay_strategy_valid() {
        assert_eq!(parse_decay_strategy("none").unwrap(), DecayStrategy::None);
        assert_eq!(
            parse_decay_strategy("linear").unwrap(),
            DecayStrategy::Linear
        );
        assert_eq!(
            parse_decay_strategy("exponential").unwrap(),
            DecayStrategy::Exponential
        );
        assert_eq!(parse_decay_strategy("step").unwrap(), DecayStrategy::Step);
    }

    #[test]
    fn test_parse_decay_strategy_case_insensitive() {
        assert_eq!(parse_decay_strategy("None").unwrap(), DecayStrategy::None);
        assert_eq!(
            parse_decay_strategy("LINEAR").unwrap(),
            DecayStrategy::Linear
        );
        assert_eq!(
            parse_decay_strategy("Exponential").unwrap(),
            DecayStrategy::Exponential
        );
    }

    #[test]
    fn test_parse_decay_strategy_invalid() {
        assert!(parse_decay_strategy("invalid").is_err());
        assert!(parse_decay_strategy("").is_err());
    }

    #[test]
    fn test_validate_score_valid() {
        assert_eq!(validate_score(0.0, "test").unwrap(), 0.0);
        assert_eq!(validate_score(0.5, "test").unwrap(), 0.5);
        assert_eq!(validate_score(1.0, "test").unwrap(), 1.0);
    }

    #[test]
    fn test_validate_score_invalid() {
        assert!(validate_score(1.5, "criticality").is_err());
        assert!(validate_score(-0.1, "confidence").is_err());
        assert!(validate_score(f64::NAN, "x").is_err());
        assert!(validate_score(f64::INFINITY, "x").is_err());
    }

    #[test]
    fn test_parse_detail_level_valid() {
        assert_eq!(parse_detail_level("summary").unwrap(), DetailLevel::Summary);
        assert_eq!(parse_detail_level("content").unwrap(), DetailLevel::Content);
        assert_eq!(parse_detail_level("full").unwrap(), DetailLevel::Full);
    }

    #[test]
    fn test_parse_detail_level_case_insensitive() {
        assert_eq!(parse_detail_level("Summary").unwrap(), DetailLevel::Summary);
        assert_eq!(parse_detail_level("FULL").unwrap(), DetailLevel::Full);
    }

    #[test]
    fn test_parse_detail_level_invalid() {
        assert!(parse_detail_level("invalid").is_err());
        assert!(parse_detail_level("").is_err());
    }
}
