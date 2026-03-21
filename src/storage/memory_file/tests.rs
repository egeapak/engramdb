//! Comprehensive tests for the memory file trait system, versioned parsers/writers,
//! and migration paths.

use super::helpers::{parse_list_field, parse_score_field};
use super::*;
use crate::types::{Decay, Memory, MemoryType, Provenance, ProvenanceSource, Status, Visibility};

// ===========================================================================
// Test helpers
// ===========================================================================

fn sample_memory() -> Memory {
    Memory {
        id: "test-123".to_string(),
        type_: MemoryType::Hazard,
        summary: "Test memory".to_string(),
        content: "This is the content.".to_string(),
        details: Some("These are the details.".to_string()),
        physical: vec!["/".to_string()],
        logical: vec![],
        tags: vec![],
        criticality: 0.5,
        decay: Some(Decay::none_with_floor(0.5)),
        provenance: Provenance::human(),
        confidence: 0.8,
        supersedes: vec![],
        status: Status::Active,
        visibility: Visibility::Shared,
        challenges: vec![],
        verified_at: None,
        created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
        updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
        accessed_at: "2026-01-15T10:00:00Z".parse().unwrap(),
        expires_at: None,
    }
}

fn full_memory() -> Memory {
    Memory {
        id: "test-full".to_string(),
        type_: MemoryType::Decision,
        summary: "Full field test".to_string(),
        content: "Content here.".to_string(),
        details: Some("Details here.".to_string()),
        physical: vec!["src/db/**".to_string(), "src/lib.rs".to_string()],
        logical: vec!["infrastructure.database".to_string()],
        tags: vec!["database".to_string(), "transactions".to_string()],
        criticality: 0.95,
        decay: Some(Decay::exponential(chrono::Duration::days(14))),
        provenance: Provenance::agent("claude")
            .with_model("claude-opus-4-6")
            .with_session("sess-123")
            .with_reason("Post-incident review"),
        confidence: 1.0,
        supersedes: vec!["old-id-1".to_string()],
        status: Status::Active,
        visibility: Visibility::Personal,
        challenges: vec![],
        verified_at: None,
        created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
        updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
        accessed_at: "2026-01-15T10:00:00Z".parse().unwrap(),
        expires_at: None,
    }
}

fn sample_v1_content() -> &'static str {
    r#"---
id: test-123
type: hazard
summary: Test memory
content: ""
physical:
  - "/"
logical: []
tags: []
criticality: 0.5
provenance:
  source: human
confidence: 0.8
supersedes: []
status: Active
visibility: Shared
challenges: []
created_at: "2026-01-15T10:00:00Z"
updated_at: "2026-01-15T10:00:00Z"
accessed_at: "2026-01-15T10:00:00Z"
---

## Content

This is the content.

## Details

These are the details.
"#
}

fn sample_v1_full_content() -> &'static str {
    r#"---
id: migrate-me
type: convention
summary: Use snake_case
content: ""
physical:
  - "src/**"
logical:
  - "code.style"
tags:
  - naming
criticality: 0.7
provenance:
  source: human
confidence: 0.9
supersedes: []
status: Active
visibility: Shared
challenges: []
created_at: "2026-01-15T10:00:00Z"
updated_at: "2026-01-15T10:00:00Z"
accessed_at: "2026-01-15T10:00:00Z"
---

## Content

Always use snake_case for function names.
"#
}

// ===========================================================================
// Trait API tests
// ===========================================================================

#[test]
fn test_parser_trait_version() {
    let v1 = V1Parser;
    assert_eq!(v1.version(), None);

    let v2 = V2Parser;
    assert_eq!(v2.version(), Some(2));
}

#[test]
fn test_writer_trait_version() {
    let v1 = V1Writer;
    assert_eq!(v1.version(), None);

    let v2 = V2Writer;
    assert_eq!(v2.version(), Some(2));
}

#[test]
fn test_parser_for_version_dispatch() {
    let legacy = parser_for_version(None);
    assert_eq!(legacy.version(), None);

    let v2 = parser_for_version(Some(2));
    assert_eq!(v2.version(), Some(2));

    // Unknown versions fall back to V1
    let unknown = parser_for_version(Some(999));
    assert_eq!(unknown.version(), None);
}

#[test]
fn test_writer_for_version_dispatch() {
    let legacy = writer_for_version(None);
    assert_eq!(legacy.version(), None);

    let v2 = writer_for_version(Some(2));
    assert_eq!(v2.version(), Some(2));
}

#[test]
fn test_latest_writer_is_v2() {
    let w = latest_writer();
    assert_eq!(w.version(), Some(CURRENT_FORMAT_VERSION));
}

// ===========================================================================
// Version detection
// ===========================================================================

#[test]
fn test_detect_format_version_legacy() {
    assert_eq!(detect_format_version(sample_v1_content()), None);
}

#[test]
fn test_detect_format_version_v2() {
    let v2 = r#"---
version: 2
id: test-123
type: hazard
status: Active
---

# Test memory
"#;
    assert_eq!(detect_format_version(v2), Some(2));
}

#[test]
fn test_detect_format_version_empty() {
    assert_eq!(detect_format_version(""), None);
    assert_eq!(detect_format_version("no frontmatter"), None);
}

// ===========================================================================
// V1 Parser tests
// ===========================================================================

#[test]
fn test_v1_parser_basic() {
    let parser = V1Parser;
    let memory = parser.parse(sample_v1_content()).unwrap();

    assert_eq!(memory.id, "test-123");
    assert_eq!(memory.type_, MemoryType::Hazard);
    assert_eq!(memory.summary, "Test memory");
    assert_eq!(memory.content, "This is the content.");
    assert_eq!(memory.details.as_deref(), Some("These are the details."));
    assert_eq!(memory.criticality, 0.5);
    assert_eq!(memory.confidence, 0.8);
    assert_eq!(memory.status, Status::Active);
    assert_eq!(memory.visibility, Visibility::Shared);
}

#[test]
fn test_v1_parser_full_fields() {
    let parser = V1Parser;
    let memory = parser.parse(sample_v1_full_content()).unwrap();

    assert_eq!(memory.id, "migrate-me");
    assert_eq!(memory.summary, "Use snake_case");
    assert_eq!(memory.content, "Always use snake_case for function names.");
    assert_eq!(memory.physical, vec!["src/**"]);
    assert_eq!(memory.logical, vec!["code.style"]);
    assert_eq!(memory.tags, vec!["naming"]);
    assert_eq!(memory.criticality, 0.7);
    assert_eq!(memory.confidence, 0.9);
}

#[test]
fn test_v1_parser_missing_frontmatter() {
    let parser = V1Parser;
    let result = parser.parse("no frontmatter here");
    assert!(result.is_err());
}

// ===========================================================================
// V1 Writer tests
// ===========================================================================

#[test]
fn test_v1_writer_basic() {
    let writer = V1Writer;
    let memory = sample_memory();
    let output = writer.write(&memory).unwrap();

    assert!(output.starts_with("---\n"));
    assert!(output.contains("id: test-123"));
    assert!(output.contains("type: hazard"));
    assert!(output.contains("summary: Test memory"));
    assert!(output.contains("## Content"));
    assert!(output.contains("This is the content."));
    assert!(output.contains("## Details"));
    assert!(output.contains("These are the details."));
    // V1 should NOT have a version field
    assert!(!output.contains("version:"));
}

#[test]
fn test_v1_writer_no_details() {
    let writer = V1Writer;
    let mut memory = sample_memory();
    memory.details = None;

    let output = writer.write(&memory).unwrap();
    assert!(output.contains("## Content"));
    assert!(!output.contains("## Details"));
}

// ===========================================================================
// V1 roundtrip
// ===========================================================================

#[test]
fn test_v1_roundtrip() {
    let writer = V1Writer;
    let parser = V1Parser;
    let memory = sample_memory();

    let written = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&written).unwrap();

    assert_eq!(memory.id, reparsed.id);
    assert_eq!(memory.type_, reparsed.type_);
    assert_eq!(memory.summary, reparsed.summary);
    assert_eq!(memory.content, reparsed.content);
    assert_eq!(memory.details, reparsed.details);
    assert_eq!(memory.status, reparsed.status);
    assert_eq!(memory.visibility, reparsed.visibility);
    assert_eq!(memory.criticality, reparsed.criticality);
    assert_eq!(memory.confidence, reparsed.confidence);
    assert_eq!(memory.physical, reparsed.physical);
    assert_eq!(memory.provenance.source, reparsed.provenance.source);
}

#[test]
fn test_v1_roundtrip_full_fields() {
    let writer = V1Writer;
    let parser = V1Parser;
    let memory = full_memory();

    let written = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&written).unwrap();

    assert_eq!(memory.id, reparsed.id);
    assert_eq!(memory.physical, reparsed.physical);
    assert_eq!(memory.logical, reparsed.logical);
    assert_eq!(memory.tags, reparsed.tags);
    assert_eq!(memory.criticality, reparsed.criticality);
    assert_eq!(memory.confidence, reparsed.confidence);
    assert_eq!(memory.visibility, reparsed.visibility);
    assert_eq!(memory.provenance.source, reparsed.provenance.source);
    assert_eq!(memory.provenance.agent_id, reparsed.provenance.agent_id);
    assert_eq!(memory.supersedes, reparsed.supersedes);
}

// ===========================================================================
// V2 Parser tests
// ===========================================================================

#[test]
fn test_v2_parser_from_writer() {
    let writer = V2Writer;
    let parser = V2Parser;
    let memory = sample_memory();

    let written = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&written).unwrap();

    assert_eq!(memory.id, reparsed.id);
    assert_eq!(memory.type_, reparsed.type_);
    assert_eq!(memory.summary, reparsed.summary);
    assert_eq!(memory.content, reparsed.content);
    assert_eq!(memory.details, reparsed.details);
    assert_eq!(memory.status, reparsed.status);
    assert_eq!(memory.visibility, reparsed.visibility);
    assert_eq!(memory.criticality, reparsed.criticality);
    assert_eq!(memory.confidence, reparsed.confidence);
}

#[test]
fn test_v2_parser_missing_frontmatter() {
    let parser = V2Parser;
    let result = parser.parse("\n## Content\n\nNo frontmatter here.\n");
    assert!(result.is_err());
}

// ===========================================================================
// V2 Writer tests
// ===========================================================================

#[test]
fn test_v2_writer_format_structure() {
    let writer = V2Writer;
    let memory = full_memory();
    let output = writer.write(&memory).unwrap();

    // Frontmatter
    assert!(output.contains("version: 2"));
    assert!(output.contains("id: test-full"));
    assert!(output.contains("type: decision"));
    assert!(output.contains("status: Active"));

    // H1 summary
    assert!(output.contains("# Full field test"));

    // Sections
    assert!(output.contains("## Content"));
    assert!(output.contains("## Details"));
    assert!(output.contains("## Scope"));
    assert!(output.contains("## Provenance"));

    // Scope fields
    assert!(output.contains("- **Files:** `src/db/**`, `src/lib.rs`"));
    assert!(output.contains("- **Logical:** `infrastructure.database`"));
    assert!(output.contains("- **Tags:** database, transactions"));
    assert!(output.contains("- **Criticality:** 0.95"));
    assert!(output.contains("- **Confidence:** 1"));
    assert!(output.contains("- **Visibility:** personal"));

    // Provenance fields
    assert!(output.contains("- **Source:** agent"));
    assert!(output.contains("- **Agent:** claude"));
    assert!(output.contains("- **Model:** claude-opus-4-6"));
    assert!(output.contains("- **Session:** sess-123"));
    assert!(output.contains("- **Reason:** Post-incident review"));

    // Hidden meta
    assert!(output.contains("<!-- engramdb"));
}

#[test]
fn test_v2_writer_no_details() {
    let writer = V2Writer;
    let mut memory = sample_memory();
    memory.details = None;

    let output = writer.write(&memory).unwrap();
    assert!(output.contains("## Content"));
    assert!(!output.contains("## Details"));
}

#[test]
fn test_v2_writer_has_version_field() {
    let writer = V2Writer;
    let memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
    let written = writer.write(&memory).unwrap();
    assert!(written.contains("version: 2"));
    assert_eq!(
        detect_format_version(&written),
        Some(CURRENT_FORMAT_VERSION)
    );
}

// ===========================================================================
// V2 roundtrip
// ===========================================================================

#[test]
fn test_v2_roundtrip() {
    let writer = V2Writer;
    let parser = V2Parser;
    let memory = sample_memory();

    let written = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&written).unwrap();

    assert_eq!(memory.id, reparsed.id);
    assert_eq!(memory.type_, reparsed.type_);
    assert_eq!(memory.summary, reparsed.summary);
    assert_eq!(memory.content, reparsed.content);
    assert_eq!(memory.details, reparsed.details);
    assert_eq!(memory.status, reparsed.status);
    assert_eq!(memory.visibility, reparsed.visibility);
    assert_eq!(memory.criticality, reparsed.criticality);
    assert_eq!(memory.confidence, reparsed.confidence);
    assert_eq!(memory.physical, reparsed.physical);
    assert_eq!(memory.provenance.source, reparsed.provenance.source);
}

#[test]
fn test_v2_roundtrip_full_fields() {
    let writer = V2Writer;
    let parser = V2Parser;
    let memory = full_memory();

    let written = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&written).unwrap();

    assert_eq!(memory.id, reparsed.id);
    assert_eq!(memory.summary, reparsed.summary);
    assert_eq!(memory.physical, reparsed.physical);
    assert_eq!(memory.logical, reparsed.logical);
    assert_eq!(memory.tags, reparsed.tags);
    assert_eq!(memory.criticality, reparsed.criticality);
    assert_eq!(memory.confidence, reparsed.confidence);
    assert_eq!(memory.visibility, reparsed.visibility);
    assert_eq!(memory.provenance.source, reparsed.provenance.source);
    assert_eq!(memory.provenance.agent_id, reparsed.provenance.agent_id);
    assert_eq!(memory.provenance.model, reparsed.provenance.model);
    assert_eq!(memory.provenance.session_id, reparsed.provenance.session_id);
    assert_eq!(memory.provenance.reason, reparsed.provenance.reason);
    assert_eq!(memory.supersedes, reparsed.supersedes);
}

#[test]
fn test_v2_roundtrip_multiline_content() {
    let writer = V2Writer;
    let parser = V2Parser;
    let mut memory = sample_memory();
    memory.content =
        "First paragraph here.\n\nSecond paragraph here.\n\nThird paragraph here.".to_string();

    let written = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&written).unwrap();

    assert!(reparsed.content.contains("First paragraph"));
    assert!(reparsed.content.contains("Second paragraph"));
    assert!(reparsed.content.contains("Third paragraph"));
    assert!(reparsed.content.contains("\n\n"));
}

#[test]
fn test_v2_roundtrip_empty_content() {
    let writer = V2Writer;
    let parser = V2Parser;
    let mut memory = sample_memory();
    memory.content = String::new();

    let written = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&written).unwrap();
    assert_eq!(reparsed.content, "");
}

#[test]
fn test_v2_personal_visibility_roundtrip() {
    let writer = V2Writer;
    let parser = V2Parser;
    let mut memory = sample_memory();
    memory.visibility = Visibility::Personal;

    let written = writer.write(&memory).unwrap();
    assert!(written.contains("**Visibility:** personal"));

    let reparsed = parser.parse(&written).unwrap();
    assert_eq!(reparsed.visibility, Visibility::Personal);
}

// ===========================================================================
// Migration path: V1 → V2
// ===========================================================================

#[test]
fn test_migration_v1_to_v2_basic() {
    let v1_content = sample_v1_content();

    // Detect as legacy
    assert_eq!(detect_format_version(v1_content), None);

    // Parse with V1
    let parser = V1Parser;
    let memory = parser.parse(v1_content).unwrap();

    // Write with V2
    let writer = V2Writer;
    let v2_content = writer.write(&memory).unwrap();

    // Detect as v2
    assert_eq!(detect_format_version(&v2_content), Some(2));

    // Parse with V2 and verify fields
    let v2_parser = V2Parser;
    let reparsed = v2_parser.parse(&v2_content).unwrap();
    assert_eq!(reparsed.id, "test-123");
    assert_eq!(reparsed.type_, MemoryType::Hazard);
    assert_eq!(reparsed.summary, "Test memory");
    assert_eq!(reparsed.content, "This is the content.");
    assert_eq!(reparsed.details.as_deref(), Some("These are the details."));
    assert_eq!(reparsed.criticality, 0.5);
    assert_eq!(reparsed.confidence, 0.8);
    assert_eq!(reparsed.status, Status::Active);
}

#[test]
fn test_migration_v1_to_v2_full_fields() {
    let v1_content = sample_v1_full_content();

    // Parse V1
    let memory = V1Parser.parse(v1_content).unwrap();
    assert_eq!(memory.id, "migrate-me");
    assert_eq!(memory.physical, vec!["src/**"]);
    assert_eq!(memory.logical, vec!["code.style"]);
    assert_eq!(memory.tags, vec!["naming"]);

    // Write V2
    let v2_content = V2Writer.write(&memory).unwrap();
    assert_eq!(detect_format_version(&v2_content), Some(2));

    // Parse V2 and verify all fields survived
    let reparsed = V2Parser.parse(&v2_content).unwrap();
    assert_eq!(reparsed.id, "migrate-me");
    assert_eq!(reparsed.summary, "Use snake_case");
    assert_eq!(
        reparsed.content,
        "Always use snake_case for function names."
    );
    assert_eq!(reparsed.criticality, 0.7);
    assert_eq!(reparsed.confidence, 0.9);
    assert_eq!(reparsed.physical, vec!["src/**"]);
    assert_eq!(reparsed.logical, vec!["code.style"]);
    assert_eq!(reparsed.tags, vec!["naming"]);
    assert_eq!(reparsed.provenance.source, ProvenanceSource::Human);
}

#[test]
fn test_migration_v1_to_v2_via_dispatch() {
    // This simulates how the migrate command works:
    // detect version → get parser → parse → write with latest writer
    let v1_content = sample_v1_content();

    let version = detect_format_version(v1_content);
    assert_eq!(version, None);

    let parser = parser_for_version(version);
    let memory = parser.parse(v1_content).unwrap();

    let writer = latest_writer();
    let v2_content = writer.write(&memory).unwrap();

    assert_eq!(
        detect_format_version(&v2_content),
        Some(CURRENT_FORMAT_VERSION)
    );

    // Verify the migrated content can be parsed by auto-detection
    let reparsed = parse_memory_file(&v2_content).unwrap();
    assert_eq!(reparsed.id, "test-123");
    assert_eq!(reparsed.content, "This is the content.");
}

// ===========================================================================
// Migration path: V2 → V2 (idempotent)
// ===========================================================================

#[test]
fn test_migration_v2_to_v2_idempotent() {
    let writer = V2Writer;
    let parser = V2Parser;
    let memory = full_memory();

    let v2_first = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&v2_first).unwrap();
    let v2_second = writer.write(&reparsed).unwrap();

    // Content should be identical (idempotent)
    assert_eq!(v2_first, v2_second);
}

// ===========================================================================
// Auto-detection dispatch (parse_memory_file)
// ===========================================================================

#[test]
fn test_parse_memory_file_auto_detects_v1() {
    let memory = parse_memory_file(sample_v1_content()).unwrap();
    assert_eq!(memory.id, "test-123");
    assert_eq!(memory.content, "This is the content.");
}

#[test]
fn test_parse_memory_file_auto_detects_v2() {
    let original = full_memory();
    let v2_content = V2Writer.write(&original).unwrap();

    let memory = parse_memory_file(&v2_content).unwrap();
    assert_eq!(memory.id, "test-full");
    assert_eq!(memory.content, "Content here.");
    assert_eq!(memory.visibility, Visibility::Personal);
}

#[test]
fn test_parse_memory_file_missing_frontmatter() {
    let result = parse_memory_file("");
    assert!(result.is_err());

    let result = parse_memory_file("no frontmatter here");
    assert!(result.is_err());
}

#[test]
fn test_parse_memory_file_empty_input() {
    let result = parse_memory_file("");
    assert!(result.is_err());
    match result {
        Err(crate::storage::error::StorageError::InvalidFormat(msg)) => {
            assert!(msg.contains("Missing frontmatter"));
        }
        _ => panic!("Expected InvalidFormat error for empty input"),
    }
}

// ===========================================================================
// write_memory_file convenience function
// ===========================================================================

#[test]
fn test_write_memory_file_uses_latest() {
    let memory = sample_memory();
    let output = write_memory_file(&memory).unwrap();
    assert_eq!(detect_format_version(&output), Some(CURRENT_FORMAT_VERSION));
}

// ===========================================================================
// Helper function tests
// ===========================================================================

#[test]
fn test_parse_score_field() {
    assert_eq!(
        parse_score_field("**Criticality:** 0.95 | **Confidence:** 1.0", "Criticality"),
        Some(0.95)
    );
    assert_eq!(
        parse_score_field("**Criticality:** 0.95 | **Confidence:** 1.0", "Confidence"),
        Some(1.0)
    );
    assert_eq!(parse_score_field("no scores here", "Criticality"), None);
}

#[test]
fn test_parse_list_field() {
    let text = "- **Files:** `src/db/**`, `src/lib.rs`\n- **Tags:** database, transactions";
    assert_eq!(
        parse_list_field(text, "Files"),
        vec!["src/db/**", "src/lib.rs"]
    );
    assert_eq!(
        parse_list_field(text, "Tags"),
        vec!["database", "transactions"]
    );
    assert!(parse_list_field(text, "Logical").is_empty());
}

// ===========================================================================
// Human-readable output verification
// ===========================================================================

#[test]
fn test_v2_output_format_readable() {
    let memory = Memory {
        id: "0195a3b7-8c4d-7e2f-a1b3-9d4e5f6a7b8c".to_string(),
        type_: MemoryType::Hazard,
        summary: "Never call sync() outside a transaction".to_string(),
        content: "The sync() method acquires a write lock.".to_string(),
        details: Some("Incident on 2026-01-14.".to_string()),
        physical: vec!["src/db/**".to_string()],
        logical: vec!["infrastructure.database".to_string()],
        tags: vec![
            "database".to_string(),
            "transactions".to_string(),
            "deadlock".to_string(),
        ],
        criticality: 0.95,
        decay: Some(Decay::none_with_floor(0.5)),
        provenance: Provenance::human().with_reason("Post-incident review of INC-2026-003"),
        confidence: 1.0,
        supersedes: vec![],
        status: Status::Active,
        visibility: Visibility::Shared,
        challenges: vec![],
        verified_at: None,
        created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
        updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
        accessed_at: "2026-02-08T14:30:00Z".parse().unwrap(),
        expires_at: None,
    };

    let written = V2Writer.write(&memory).unwrap();

    assert!(written.contains("# Never call sync() outside a transaction"));
    assert!(written.contains("- **Criticality:** 0.95"));
    assert!(written.contains("- **Confidence:** 1"));
    assert!(written.contains("## Content"));
    assert!(written.contains("## Details"));
    assert!(written.contains("## Scope"));
    assert!(written.contains("- **Files:** `src/db/**`"));
    assert!(written.contains("- **Tags:** database, transactions, deadlock"));
    assert!(written.contains("## Provenance"));
    assert!(written.contains("- **Source:** human"));
    assert!(written.contains("- **Reason:** Post-incident review of INC-2026-003"));
    assert!(written.contains("<!-- engramdb"));

    // Verify no ugly YAML dump
    assert!(!written.contains("content: \"\""));
    assert!(!written.contains("supersedes: []"));
}

// ===========================================================================
// Cross-format migration: write V1, migrate to V2, verify identical data
// ===========================================================================

#[test]
fn test_cross_format_full_migration_cycle() {
    let original = full_memory();

    // Write as V1
    let v1_content = V1Writer.write(&original).unwrap();
    assert_eq!(detect_format_version(&v1_content), None);

    // Parse V1
    let from_v1 = V1Parser.parse(&v1_content).unwrap();

    // Write as V2
    let v2_content = V2Writer.write(&from_v1).unwrap();
    assert_eq!(detect_format_version(&v2_content), Some(2));

    // Parse V2
    let from_v2 = V2Parser.parse(&v2_content).unwrap();

    // Core fields must match
    assert_eq!(original.id, from_v2.id);
    assert_eq!(original.type_, from_v2.type_);
    assert_eq!(original.summary, from_v2.summary);
    assert_eq!(original.content, from_v2.content);
    assert_eq!(original.details, from_v2.details);
    assert_eq!(original.physical, from_v2.physical);
    assert_eq!(original.logical, from_v2.logical);
    assert_eq!(original.tags, from_v2.tags);
    assert_eq!(original.criticality, from_v2.criticality);
    assert_eq!(original.confidence, from_v2.confidence);
    assert_eq!(original.status, from_v2.status);
    assert_eq!(original.visibility, from_v2.visibility);
    assert_eq!(original.provenance.source, from_v2.provenance.source);
    assert_eq!(original.provenance.agent_id, from_v2.provenance.agent_id);
    assert_eq!(original.provenance.model, from_v2.provenance.model);
    assert_eq!(
        original.provenance.session_id,
        from_v2.provenance.session_id
    );
    assert_eq!(original.provenance.reason, from_v2.provenance.reason);
    assert_eq!(original.supersedes, from_v2.supersedes);
}

// ===========================================================================
// Migration rollback: V1 → V2 → V1 roundtrip
// ===========================================================================

#[test]
fn test_migration_v1_to_v2_rollback_to_v1() {
    let original = full_memory();

    // Step 1: Write original as V1
    let v1_original = V1Writer.write(&original).unwrap();
    assert_eq!(detect_format_version(&v1_original), None);

    // Step 2: Migrate V1 → V2 (simulate `engramdb migrate`)
    let from_v1 = V1Parser.parse(&v1_original).unwrap();
    let v2_content = V2Writer.write(&from_v1).unwrap();
    assert_eq!(detect_format_version(&v2_content), Some(2));

    // Step 3: Rollback V2 → V1 (simulate `engramdb rollback --target-version 1`)
    let from_v2 = V2Parser.parse(&v2_content).unwrap();
    let v1_rolled_back = V1Writer.write(&from_v2).unwrap();
    assert_eq!(detect_format_version(&v1_rolled_back), None);

    // Step 4: Parse the rolled-back V1 and verify all fields match the original
    let final_memory = V1Parser.parse(&v1_rolled_back).unwrap();

    assert_eq!(original.id, final_memory.id);
    assert_eq!(original.type_, final_memory.type_);
    assert_eq!(original.summary, final_memory.summary);
    assert_eq!(original.content, final_memory.content);
    assert_eq!(original.details, final_memory.details);
    assert_eq!(original.physical, final_memory.physical);
    assert_eq!(original.logical, final_memory.logical);
    assert_eq!(original.tags, final_memory.tags);
    assert_eq!(original.criticality, final_memory.criticality);
    assert_eq!(original.confidence, final_memory.confidence);
    assert_eq!(original.status, final_memory.status);
    assert_eq!(original.visibility, final_memory.visibility);
    assert_eq!(original.provenance.source, final_memory.provenance.source);
    assert_eq!(
        original.provenance.agent_id,
        final_memory.provenance.agent_id
    );
    assert_eq!(original.provenance.model, final_memory.provenance.model);
    assert_eq!(
        original.provenance.session_id,
        final_memory.provenance.session_id
    );
    assert_eq!(original.provenance.reason, final_memory.provenance.reason);
    assert_eq!(original.supersedes, final_memory.supersedes);
}

#[test]
fn test_migration_v1_to_v2_rollback_to_v1_basic() {
    // Use the sample V1 content string (not generated by V1Writer)
    let v1_content = sample_v1_content();

    // Migrate to V2
    let memory = V1Parser.parse(v1_content).unwrap();
    let v2_content = V2Writer.write(&memory).unwrap();

    // Rollback to V1
    let from_v2 = V2Parser.parse(&v2_content).unwrap();
    let v1_rolled_back = V1Writer.write(&from_v2).unwrap();

    // Verify fields survived the full cycle
    let final_memory = V1Parser.parse(&v1_rolled_back).unwrap();
    assert_eq!(final_memory.id, "test-123");
    assert_eq!(final_memory.type_, MemoryType::Hazard);
    assert_eq!(final_memory.summary, "Test memory");
    assert_eq!(final_memory.content, "This is the content.");
    assert_eq!(
        final_memory.details.as_deref(),
        Some("These are the details.")
    );
    assert_eq!(final_memory.criticality, 0.5);
    assert_eq!(final_memory.confidence, 0.8);
    assert_eq!(final_memory.status, Status::Active);
    assert_eq!(final_memory.visibility, Visibility::Shared);
}

#[test]
fn test_rollback_via_writer_for_version() {
    // Simulate rollback using the dispatch API
    let original = full_memory();

    // Migrate forward
    let v2_content = latest_writer().write(&original).unwrap();
    assert_eq!(detect_format_version(&v2_content), Some(2));

    // Rollback using writer_for_version
    let memory = parse_memory_file(&v2_content).unwrap();
    let v1_writer = writer_for_version(None); // target: V1
    let v1_content = v1_writer.write(&memory).unwrap();
    assert_eq!(detect_format_version(&v1_content), None);

    // Verify data integrity
    let final_memory = parse_memory_file(&v1_content).unwrap();
    assert_eq!(original.id, final_memory.id);
    assert_eq!(original.content, final_memory.content);
    assert_eq!(original.summary, final_memory.summary);
    assert_eq!(original.physical, final_memory.physical);
}
