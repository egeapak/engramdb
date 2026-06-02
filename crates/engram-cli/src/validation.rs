//! Input validation utilities for CLI commands.
//!
//! Re-exports shared validation from the ops layer.

pub use engramdb::ops::parsing::validate_score;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_score_valid_zero() {
        assert!(validate_score(0.0, "test").is_ok());
    }

    #[test]
    fn test_validate_score_valid_half() {
        assert_eq!(validate_score(0.5, "test").unwrap(), 0.5);
    }

    #[test]
    fn test_validate_score_valid_one() {
        assert!(validate_score(1.0, "test").is_ok());
    }

    #[test]
    fn test_validate_score_too_high() {
        let err = validate_score(1.5, "criticality").unwrap_err();
        assert!(err.to_string().contains("0.0 and 1.0"));
    }

    #[test]
    fn test_validate_score_negative() {
        let err = validate_score(-0.1, "confidence").unwrap_err();
        assert!(err.to_string().contains("0.0 and 1.0"));
    }

    #[test]
    fn test_validate_score_way_too_high() {
        assert!(validate_score(2.0, "criticality").is_err());
    }

    #[test]
    fn test_validate_score_boundary_just_above() {
        let err = validate_score(1.0000001, "x").unwrap_err();
        assert!(err.to_string().contains("0.0 and 1.0"));
    }

    #[test]
    fn test_validate_score_boundary_just_below() {
        let err = validate_score(-0.0000001, "x").unwrap_err();
        assert!(err.to_string().contains("0.0 and 1.0"));
    }

    #[test]
    fn test_validate_score_nan() {
        let err = validate_score(f64::NAN, "x").unwrap_err();
        assert!(err.to_string().contains("0.0 and 1.0"));
    }

    #[test]
    fn test_validate_score_infinity() {
        let err = validate_score(f64::INFINITY, "x").unwrap_err();
        assert!(err.to_string().contains("0.0 and 1.0"));
    }
}
