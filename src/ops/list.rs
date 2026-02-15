//! List memories with filtering, sorting, and limiting.

use crate::ops::parsing::{parse_memory_type, parse_status};
use crate::storage::lance_index::IndexFilterable;
use crate::storage::MemoryStore;
use crate::types::MemoryType;
use anyhow::{anyhow, Result};

/// Sort field for list operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Criticality,
    Created,
    Updated,
    Type,
}

/// Parse a string into a SortField.
pub fn parse_sort_field(s: &str) -> Result<SortField> {
    match s.to_lowercase().as_str() {
        "criticality" | "relevance" => Ok(SortField::Criticality),
        "created" => Ok(SortField::Created),
        "updated" => Ok(SortField::Updated),
        "type" => Ok(SortField::Type),
        _ => Err(anyhow!(
            "Invalid sort field: {}. Valid options are: criticality, relevance, created, updated, type",
            s
        )),
    }
}

/// Parameters for listing memories.
pub struct ListParams {
    pub types: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub status: Option<String>,
    pub scope: Option<String>,
    pub sort_field: SortField,
    pub reverse: bool,
    pub limit: Option<usize>,
}

/// List memories with optional filtering, sorting, and limiting.
///
/// Returns index entries (lightweight summaries) rather than full memory data.
pub async fn list_memories(
    store: &MemoryStore,
    params: &ListParams,
) -> Result<Vec<IndexFilterable>> {
    let mut entries = store.list_filterable().await?;

    // Apply type filter
    if let Some(ref type_strs) = params.types {
        if !type_strs.is_empty() {
            let types: Vec<MemoryType> = type_strs
                .iter()
                .map(|s| parse_memory_type(s))
                .collect::<Result<Vec<_>>>()?;
            entries.retain(|e| types.contains(&e.type_));
        }
    }

    // Apply tags filter (OR logic)
    if let Some(ref tags) = params.tags {
        if !tags.is_empty() {
            entries.retain(|e| tags.iter().any(|tag| e.tags.contains(tag)));
        }
    }

    // Apply status filter
    if let Some(ref status_str) = params.status {
        let status = parse_status(status_str)?;
        entries.retain(|e| e.status == status);
    }

    // Apply scope filter (matches physical or logical scopes)
    if let Some(ref scope) = params.scope {
        entries.retain(|e| {
            e.physical.iter().any(|p| p.contains(scope.as_str()))
                || e.logical.iter().any(|l| l.contains(scope.as_str()))
        });
    }

    // Apply sorting
    match params.sort_field {
        SortField::Criticality => {
            entries.sort_by(|a, b| b.criticality.partial_cmp(&a.criticality).unwrap());
        }
        SortField::Created => {
            entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        }
        SortField::Updated => {
            entries.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        }
        SortField::Type => {
            entries.sort_by(|a, b| format!("{:?}", a.type_).cmp(&format!("{:?}", b.type_)));
        }
    }

    // Apply reverse if requested
    if params.reverse {
        entries.reverse();
    }

    // Apply limit
    if let Some(max) = params.limit {
        entries.truncate(max);
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sort_field_valid() {
        assert_eq!(
            parse_sort_field("criticality").unwrap(),
            SortField::Criticality
        );
        assert_eq!(
            parse_sort_field("relevance").unwrap(),
            SortField::Criticality
        );
        assert_eq!(parse_sort_field("created").unwrap(), SortField::Created);
        assert_eq!(parse_sort_field("updated").unwrap(), SortField::Updated);
        assert_eq!(parse_sort_field("type").unwrap(), SortField::Type);
    }

    #[test]
    fn test_parse_sort_field_invalid() {
        assert!(parse_sort_field("invalid").is_err());
    }
}
