//! Shared parsing helpers for string-to-enum conversions.
//!
//! These functions are used by both CLI and MCP boundaries to convert
//! user-provided strings into typed enums.

use crate::types::{MemoryType, Status, Visibility};
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
}
