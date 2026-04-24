//! Filtering logic for memory retrieval.
//!
//! This module provides efficient filtering of memory index entries before loading
//! full memory data. Filters are applied at the index level to minimize disk I/O
//! and improve retrieval performance.

use crate::scope::physical;
use crate::storage::{IndexFilterable, IndexForFiltering};
use crate::types::MemoryType;
use chrono::{DateTime, Utc};

/// Trait for index entries that can be filtered by `apply_index_filters`.
///
/// Implemented by both [`IndexFilterable`] (12 columns) and
/// [`IndexForFiltering`] (7 columns), allowing the retrieval pipeline
/// to use a lighter projection without changing filter logic.
pub trait Filterable {
    fn id(&self) -> &str;
    fn type_(&self) -> MemoryType;
    fn tags(&self) -> &[String];
    fn physical(&self) -> &[String];
    fn criticality(&self) -> f64;
    fn expires_at(&self) -> Option<DateTime<Utc>>;
}

impl Filterable for IndexFilterable {
    fn id(&self) -> &str {
        &self.id
    }
    fn type_(&self) -> MemoryType {
        self.type_
    }
    fn tags(&self) -> &[String] {
        &self.tags
    }
    fn physical(&self) -> &[String] {
        &self.physical
    }
    fn criticality(&self) -> f64 {
        self.criticality
    }
    fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }
}

impl Filterable for IndexForFiltering {
    fn id(&self) -> &str {
        &self.id
    }
    fn type_(&self) -> MemoryType {
        self.type_
    }
    fn tags(&self) -> &[String] {
        &self.tags
    }
    fn physical(&self) -> &[String] {
        &self.physical
    }
    fn criticality(&self) -> f64 {
        self.criticality
    }
    fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }
}

/// Search filters for restricting retrieval results.
///
/// Logical scope is intentionally absent: it is a scoring signal, not a
/// filter. See [`crate::scope::logical::proximity`].
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    /// Filter by memory type
    pub types: Option<Vec<MemoryType>>,

    /// Filter by tags (OR logic - memory must have at least one matching tag)
    pub tags: Option<Vec<String>>,

    /// Filter by physical scope (file path)
    pub physical: Option<String>,

    /// Minimum criticality threshold
    pub min_criticality: Option<f64>,
}

/// Apply filters to a list of index entries.
///
/// Generic over any type implementing [`Filterable`], so it works with both
/// the full `IndexFilterable` (12 columns) and the lightweight
/// `IndexForFiltering` (6 columns).
///
/// # Arguments
/// * `entries` - The index entries to filter
/// * `filters` - The filter criteria to apply
///
/// # Returns
/// Filtered list of index entries
pub fn apply_index_filters<T: Filterable>(entries: Vec<T>, filters: &SearchFilters) -> Vec<T> {
    entries
        .into_iter()
        .filter(|entry| {
            // Filter by type
            if let Some(ref types) = filters.types {
                if !types.contains(&entry.type_()) {
                    return false;
                }
            }

            // Filter by tags (OR logic - at least one tag must match)
            if let Some(ref filter_tags) = filters.tags {
                if !filter_tags.iter().any(|tag| entry.tags().contains(tag)) {
                    return false;
                }
            }

            // Filter by physical scope
            if let Some(ref physical_path) = filters.physical {
                if !physical::matches(entry.physical(), physical_path) {
                    return false;
                }
            }

            // Filter by minimum criticality
            if let Some(min_crit) = filters.min_criticality {
                if entry.criticality() < min_crit {
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
    use crate::types::{Status, Visibility};
    use chrono::Utc;

    fn create_test_entry(
        id: &str,
        type_: MemoryType,
        tags: Vec<String>,
        physical: Vec<String>,
        logical: Vec<String>,
        criticality: f64,
    ) -> IndexFilterable {
        IndexFilterable {
            id: id.to_string(),
            type_,
            summary: "Test summary".to_string(),
            physical,
            logical,
            tags,
            criticality,
            status: Status::Active,
            visibility: Visibility::Shared,
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
    fn test_logical_scope_is_not_a_filter() {
        // Logical scope must not exclude memories; it is a scoring signal
        // applied by the retrieval engine. apply_index_filters must
        // completely ignore logical values stored on index entries.
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
                vec!["database".to_string()],
                0.7,
            ),
        ];

        let filters = SearchFilters::default();
        let filtered = apply_index_filters(entries, &filters);
        assert_eq!(filtered.len(), 2);
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
        };

        let filtered = apply_index_filters(entries, &filters);
        assert_eq!(filtered.len(), 1); // Only entry 1 matches all criteria
        assert_eq!(filtered[0].id, "1");
    }
}
