//! Filtering logic for memory retrieval.
//!
//! This module provides efficient filtering of memory index entries before loading
//! full memory data. Filters are applied at the index level to minimize disk I/O
//! and improve retrieval performance.

use crate::scope::physical;
use crate::storage::IndexEntry;
use crate::types::MemoryType;

/// Search filters for restricting retrieval results.
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    /// Filter by memory type
    pub types: Option<Vec<MemoryType>>,

    /// Filter by tags (OR logic - memory must have at least one matching tag)
    pub tags: Option<Vec<String>>,

    /// Filter by physical scope (file path)
    pub physical: Option<String>,

    /// Filter by logical scope
    pub logical: Option<String>,

    /// Minimum criticality threshold
    pub min_criticality: Option<f64>,
}

/// Apply filters to a list of index entries
///
/// # Arguments
/// * `entries` - The index entries to filter
/// * `filters` - The filter criteria to apply
///
/// # Returns
/// Filtered list of index entries
pub fn apply_index_filters(entries: Vec<IndexEntry>, filters: &SearchFilters) -> Vec<IndexEntry> {
    entries
        .into_iter()
        .filter(|entry| {
            // Filter by type
            if let Some(ref types) = filters.types {
                if !types.contains(&entry.type_) {
                    return false;
                }
            }

            // Filter by tags (OR logic - at least one tag must match)
            if let Some(ref filter_tags) = filters.tags {
                if !filter_tags.iter().any(|tag| entry.tags.contains(tag)) {
                    return false;
                }
            }

            // Filter by physical scope
            if let Some(ref physical_path) = filters.physical {
                if !physical::matches(&entry.physical, physical_path) {
                    return false;
                }
            }

            // Filter by logical scope (check if any entry logical scope matches)
            if let Some(ref logical_scope) = filters.logical {
                if !entry.logical.iter().any(|scope| scope == logical_scope) {
                    return false;
                }
            }

            // Filter by minimum criticality
            if let Some(min_crit) = filters.min_criticality {
                if entry.criticality < min_crit {
                    return false;
                }
            }

            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ProvenanceSource, Status};
    use chrono::Utc;

    fn create_test_entry(
        id: &str,
        type_: MemoryType,
        tags: Vec<String>,
        physical: Vec<String>,
        logical: Vec<String>,
        criticality: f64,
    ) -> IndexEntry {
        IndexEntry {
            id: id.to_string(),
            type_,
            summary: "Test summary".to_string(),
            physical,
            logical,
            tags,
            criticality,
            confidence: 0.8,
            provenance_source: ProvenanceSource::Human,
            status: Status::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            expires_at: None,
        }
    }

    #[test]
    fn test_no_filters() {
        let entries = vec![
            create_test_entry("1", MemoryType::Decision, vec![], vec![], vec![], 0.5),
            create_test_entry("2", MemoryType::Convention, vec![], vec![], vec![], 0.7),
        ];

        let filters = SearchFilters::default();
        let filtered = apply_index_filters(entries.clone(), &filters);

        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_by_type() {
        let entries = vec![
            create_test_entry("1", MemoryType::Decision, vec![], vec![], vec![], 0.5),
            create_test_entry("2", MemoryType::Convention, vec![], vec![], vec![], 0.7),
            create_test_entry("3", MemoryType::Hazard, vec![], vec![], vec![], 0.6),
        ];

        let filters = SearchFilters {
            types: Some(vec![MemoryType::Decision, MemoryType::Convention]),
            ..Default::default()
        };

        let filtered = apply_index_filters(entries, &filters);
        assert_eq!(filtered.len(), 2);
        assert!(filtered
            .iter()
            .all(|e| e.type_ == MemoryType::Decision || e.type_ == MemoryType::Convention));
    }

    #[test]
    fn test_filter_by_tags_or_logic() {
        let entries = vec![
            create_test_entry(
                "1",
                MemoryType::Decision,
                vec!["auth".to_string()],
                vec![],
                vec![],
                0.5,
            ),
            create_test_entry(
                "2",
                MemoryType::Convention,
                vec!["api".to_string(), "rest".to_string()],
                vec![],
                vec![],
                0.7,
            ),
            create_test_entry(
                "3",
                MemoryType::Hazard,
                vec!["database".to_string()],
                vec![],
                vec![],
                0.6,
            ),
        ];

        let filters = SearchFilters {
            tags: Some(vec!["auth".to_string(), "rest".to_string()]),
            ..Default::default()
        };

        let filtered = apply_index_filters(entries, &filters);
        assert_eq!(filtered.len(), 2); // Entries 1 and 2
    }

    #[test]
    fn test_filter_by_physical_scope() {
        let entries = vec![
            create_test_entry(
                "1",
                MemoryType::Decision,
                vec![],
                vec!["src/api/**".to_string()],
                vec![],
                0.5,
            ),
            create_test_entry(
                "2",
                MemoryType::Convention,
                vec![],
                vec!["src/db/**".to_string()],
                vec![],
                0.7,
            ),
            create_test_entry(
                "3",
                MemoryType::Hazard,
                vec![],
                vec!["/".to_string()],
                vec![],
                0.6,
            ),
        ];

        let filters = SearchFilters {
            physical: Some("src/api/auth/handlers.rs".to_string()),
            ..Default::default()
        };

        let filtered = apply_index_filters(entries, &filters);
        assert_eq!(filtered.len(), 2); // Entries 1 and 3 (api/** and /)
    }

    #[test]
    fn test_filter_by_logical_scope() {
        let entries = vec![
            create_test_entry(
                "1",
                MemoryType::Decision,
                vec![],
                vec![],
                vec!["auth".to_string()],
                0.5,
            ),
            create_test_entry(
                "2",
                MemoryType::Convention,
                vec![],
                vec![],
                vec!["api".to_string(), "auth".to_string()],
                0.7,
            ),
            create_test_entry(
                "3",
                MemoryType::Hazard,
                vec![],
                vec![],
                vec!["database".to_string()],
                0.6,
            ),
        ];

        let filters = SearchFilters {
            logical: Some("auth".to_string()),
            ..Default::default()
        };

        let filtered = apply_index_filters(entries, &filters);
        assert_eq!(filtered.len(), 2); // Entries 1 and 2
    }

    #[test]
    fn test_filter_by_min_criticality() {
        let entries = vec![
            create_test_entry("1", MemoryType::Decision, vec![], vec![], vec![], 0.3),
            create_test_entry("2", MemoryType::Convention, vec![], vec![], vec![], 0.7),
            create_test_entry("3", MemoryType::Hazard, vec![], vec![], vec![], 0.9),
        ];

        let filters = SearchFilters {
            min_criticality: Some(0.6),
            ..Default::default()
        };

        let filtered = apply_index_filters(entries, &filters);
        assert_eq!(filtered.len(), 2); // Entries 2 and 3
    }

    #[test]
    fn test_multiple_filters() {
        let entries = vec![
            create_test_entry(
                "1",
                MemoryType::Decision,
                vec!["auth".to_string()],
                vec!["src/api/**".to_string()],
                vec![],
                0.8,
            ),
            create_test_entry(
                "2",
                MemoryType::Convention,
                vec!["auth".to_string()],
                vec!["src/db/**".to_string()],
                vec![],
                0.7,
            ),
            create_test_entry(
                "3",
                MemoryType::Decision,
                vec!["database".to_string()],
                vec!["src/api/**".to_string()],
                vec![],
                0.9,
            ),
        ];

        let filters = SearchFilters {
            types: Some(vec![MemoryType::Decision]),
            tags: Some(vec!["auth".to_string()]),
            physical: Some("src/api/handlers.rs".to_string()),
            min_criticality: Some(0.75),
            ..Default::default()
        };

        let filtered = apply_index_filters(entries, &filters);
        assert_eq!(filtered.len(), 1); // Only entry 1 matches all criteria
        assert_eq!(filtered[0].id, "1");
    }
}
