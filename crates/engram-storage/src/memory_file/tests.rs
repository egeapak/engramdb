//! Comprehensive tests for the memory file trait system, versioned parsers/writers,
//! and migration paths.

use super::helpers::{parse_list_field, parse_score_field};
use super::*;
use engram_types::{Decay, Memory, MemoryType, Provenance, ProvenanceSource, Status, Visibility};

// ===========================================================================
// Test helpers
// ===========================================================================

fn sample_memory() -> Memory {
    Memory {
        id: "test-123".to_string(),
        type_: MemoryType::Hazard,
        epistemic: MemoryType::Hazard.default_epistemic(),
        valid_while: None,
        valid_from: None,
        invalidated_at: None,
        superseded_by: None,
        summary: "Test memory".to_string(),
        title: None,
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
        epistemic: MemoryType::Decision.default_epistemic(),
        valid_while: None,
        valid_from: None,
        invalidated_at: None,
        superseded_by: None,
        summary: "Full field test".to_string(),
        title: Some("Full Field Test Title".to_string()),
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

    // H1 title (title takes precedence over summary)
    assert!(output.contains("# Full Field Test Title"));
    // Summary preserved as bold field
    assert!(output.contains("**Summary:** Full field test"));

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

/// Files saved with Windows line endings (CRLF) — common when an editor or
/// `git core.autocrlf` rewrites the on-disk file — must parse identically to
/// the LF form. The writer emits LF; `split_frontmatter` trims the YAML block
/// and `parse_body_sections` uses `str::lines()`, both of which are line-ending
/// agnostic. This guards that property explicitly (it is the most likely
/// cross-platform parsing divergence).
#[test]
fn test_v2_parse_is_crlf_tolerant() {
    let memory = full_memory();
    let lf = write_memory_file(&memory).unwrap();
    assert!(
        lf.contains('\n') && !lf.contains('\r'),
        "writer must emit LF-only output"
    );

    let crlf = lf.replace('\n', "\r\n");
    let from_lf = parse_memory_file(&lf).unwrap();
    let from_crlf = parse_memory_file(&crlf).unwrap();

    assert_eq!(from_lf.id, from_crlf.id);
    assert_eq!(from_lf.type_, from_crlf.type_);
    assert_eq!(from_lf.summary, from_crlf.summary);
    assert_eq!(from_lf.content, from_crlf.content);
    assert_eq!(from_lf.details, from_crlf.details);
    assert_eq!(from_lf.physical, from_crlf.physical);
    assert_eq!(from_lf.logical, from_crlf.logical);
    assert_eq!(from_lf.tags, from_crlf.tags);
    assert_eq!(from_lf.criticality, from_crlf.criticality);
    assert_eq!(from_lf.visibility, from_crlf.visibility);
    assert_eq!(from_lf.provenance.source, from_crlf.provenance.source);
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
        Err(crate::error::StorageError::InvalidFormat(msg)) => {
            assert!(msg.contains("Missing frontmatter"));
        }
        _ => panic!("Expected InvalidFormat error for empty input"),
    }
}

#[test]
fn test_parse_memory_file_malformed_yaml_does_not_panic() {
    // Regression (found by `cargo fuzz run memory_file`): this input panicked
    // inside the old serde_yml/libyml scanner ("String join would overflow
    // memory bounds"), which aborts the process under panic=abort. After
    // moving to serde_yaml_ng it must surface as an error, never a panic.
    let content = std::str::from_utf8(b"---d\t\t\t\t\t\t\t\t\t\t\t\t\t\t  @---").unwrap();
    assert!(parse_memory_file(content).is_err());
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
        epistemic: MemoryType::Hazard.default_epistemic(),
        valid_while: None,
        valid_from: None,
        invalidated_at: None,
        superseded_by: None,
        summary: "Never call sync() outside a transaction".to_string(),
        title: None,
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

// ===========================================================================
// Title support tests
// ===========================================================================

#[test]
fn test_v2_roundtrip_with_title() {
    let mut memory = sample_memory();
    memory.title = Some("Database Decision".to_string());

    let written = V2Writer.write(&memory).unwrap();
    assert!(written.contains("title: Database Decision"));

    let reparsed = V2Parser.parse(&written).unwrap();
    assert_eq!(reparsed.title, Some("Database Decision".to_string()));
    assert_eq!(reparsed.summary, "Test memory");
}

#[test]
fn test_v2_roundtrip_without_title() {
    let memory = sample_memory();
    assert_eq!(memory.title, None);

    let written = V2Writer.write(&memory).unwrap();
    assert!(!written.contains("title:"));

    let reparsed = V2Parser.parse(&written).unwrap();
    assert_eq!(reparsed.title, None);
}

#[test]
fn test_v1_roundtrip_with_title() {
    let mut memory = sample_memory();
    memory.title = Some("My Title".to_string());

    let written = V1Writer.write(&memory).unwrap();
    assert!(written.contains("# My Title"));

    let reparsed = V1Parser.parse(&written).unwrap();
    assert_eq!(reparsed.title, Some("My Title".to_string()));
}

#[test]
fn test_v1_roundtrip_without_title() {
    let memory = sample_memory();
    let written = V1Writer.write(&memory).unwrap();
    assert!(!written.contains("\n# "));

    let reparsed = V1Parser.parse(&written).unwrap();
    assert_eq!(reparsed.title, None);
}

#[test]
fn test_title_survives_v1_to_v2_migration() {
    let mut memory = sample_memory();
    memory.title = Some("Important Title".to_string());

    // Write as V1
    let v1_content = V1Writer.write(&memory).unwrap();
    // Parse V1 → migrate to V2
    let from_v1 = V1Parser.parse(&v1_content).unwrap();
    let v2_content = V2Writer.write(&from_v1).unwrap();
    // Parse V2
    let final_memory = V2Parser.parse(&v2_content).unwrap();

    assert_eq!(final_memory.title, Some("Important Title".to_string()));
}

#[test]
fn test_title_survives_v2_to_v1_rollback() {
    let mut memory = sample_memory();
    memory.title = Some("Important Title".to_string());

    // Write as V2
    let v2_content = V2Writer.write(&memory).unwrap();
    // Parse V2 → rollback to V1
    let from_v2 = V2Parser.parse(&v2_content).unwrap();
    let v1_content = V1Writer.write(&from_v2).unwrap();
    // Parse V1
    let final_memory = V1Parser.parse(&v1_content).unwrap();

    assert_eq!(final_memory.title, Some("Important Title".to_string()));
}

#[test]
fn test_full_memory_title_roundtrip_v1_v2_v1() {
    let original = full_memory();
    assert_eq!(original.title, Some("Full Field Test Title".to_string()));

    // V1 → V2 → V1
    let v1 = V1Writer.write(&original).unwrap();
    let from_v1 = V1Parser.parse(&v1).unwrap();
    let v2 = V2Writer.write(&from_v1).unwrap();
    let from_v2 = V2Parser.parse(&v2).unwrap();
    let v1_back = V1Writer.write(&from_v2).unwrap();
    let final_memory = V1Parser.parse(&v1_back).unwrap();

    assert_eq!(final_memory.title, original.title);
}

// ===========================================================================
// Filename helper tests
// ===========================================================================

#[test]
fn test_slugify_basic() {
    assert_eq!(slugify("Use Snake Case"), "use-snake-case");
    assert_eq!(slugify("Hello, World!"), "hello-world");
    assert_eq!(slugify("  spaces  "), "spaces");
    assert_eq!(slugify("MixedCase_And-Stuff"), "mixedcase-and-stuff");
}

#[test]
fn test_slugify_truncation() {
    let long = "a".repeat(60);
    let slug = slugify(&long);
    assert!(slug.len() <= 50);
}

#[test]
fn test_slugify_truncation_at_word_boundary() {
    let title = "this is a title that is quite long and should be truncated at a word boundary";
    let slug = slugify(title);
    assert!(slug.len() <= 50);
    assert!(!slug.ends_with('-'));
}

/// Memory files can arrive with a cloned repo, so a frontmatter `id`
/// containing path separators or dot-dot must be rejected at parse time —
/// it would otherwise flow into `memory_filename` and compose a path
/// outside `.engramdb/memories/` on the next write.
#[test]
fn test_parse_rejects_path_hostile_ids() {
    let mem = Memory::new(MemoryType::Decision, "S", "C", Provenance::human());
    let good = write_memory_file(&mem).unwrap();
    assert!(parse_memory_file(&good).is_ok());

    for bad_id in ["../../../etc/x", "a/b", "a\\b", "", "x\u{0}y"] {
        let doctored = good.replace(&mem.id, bad_id);
        // Empty-string replace is a no-op; craft that case directly.
        let content = if bad_id.is_empty() {
            good.replace(&format!("id: {}", mem.id), "id: \"\"")
        } else {
            doctored
        };
        let result = parse_memory_file(&content);
        assert!(
            result.is_err(),
            "id {bad_id:?} must be rejected at parse time"
        );
    }
}

/// Regression: multibyte titles must not panic the truncation slice.
/// `is_alphanumeric()` keeps CJK/accented chars, so a long non-ASCII title
/// used to hit `&trimmed[..50]` at a non-char boundary and panic — reachable
/// from every create/update via `memory_filename`.
#[test]
fn test_slugify_multibyte_truncation_no_panic() {
    // 20 CJK chars = 60 bytes, no hyphens: byte 50 is mid-character.
    let cjk = "\u{5B58}".repeat(20); // 存
    let slug = slugify(&cjk);
    assert!(slug.len() <= 50);
    assert!(!slug.is_empty());

    // Mixed multibyte + separators still truncates on a boundary.
    let mixed = "caf\u{E9} ".repeat(12); // "café " x12 = 72 bytes
    let slug = slugify(&mixed);
    assert!(slug.len() <= 50);
    assert!(!slug.ends_with('-'));

    // Exhaustive-ish sweep around the boundary: 2- and 3-byte chars at
    // every length near 50 bytes must never panic.
    for n in 10..30 {
        let _ = slugify(&"\u{E9}".repeat(n)); // é (2 bytes)
        let _ = slugify(&"\u{4E2D}".repeat(n)); // 中 (3 bytes)
    }
}

#[test]
fn test_slugify_empty() {
    assert_eq!(slugify(""), "");
    assert_eq!(slugify("---"), "");
    assert_eq!(slugify("!!!"), "");
}

#[test]
fn test_memory_filename_with_title() {
    let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
    memory.title = Some("Use Snake Case".to_string());
    let filename = memory_filename(&memory);
    assert!(filename.starts_with("use-snake-case_"));
    assert!(filename.ends_with(".md"));
    assert!(filename.contains(&memory.id));
}

#[test]
fn test_memory_filename_without_title() {
    let memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
    let filename = memory_filename(&memory);
    assert_eq!(filename, format!("{}.md", memory.id));
}

#[test]
fn test_memory_filename_with_empty_slug_title() {
    let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
    memory.title = Some("---".to_string());
    let filename = memory_filename(&memory);
    assert_eq!(filename, format!("{}.md", memory.id));
}

#[test]
fn test_extract_id_from_stem_new_format() {
    let stem = "use-snake-case_019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8";
    assert_eq!(
        extract_id_from_stem(stem),
        "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8"
    );
}

#[test]
fn test_extract_id_from_stem_old_format() {
    let stem = "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8";
    assert_eq!(
        extract_id_from_stem(stem),
        "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8"
    );
}

#[test]
fn test_stem_matches_id_prefix_new_format() {
    let stem = "use-snake-case_019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8";
    assert!(stem_matches_id_prefix(stem, "019f0d3e"));
    assert!(stem_matches_id_prefix(
        stem,
        "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8"
    ));
    assert!(!stem_matches_id_prefix(stem, "aaaa"));
}

#[test]
fn test_stem_matches_id_prefix_old_format() {
    let stem = "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8";
    assert!(stem_matches_id_prefix(stem, "019f0d3e"));
    assert!(!stem_matches_id_prefix(stem, "use-snake"));
}

#[test]
fn test_parse_v1_without_title_backward_compat() {
    // Old V1 file without title field should parse with title = None
    let memory = parse_memory_file(sample_v1_content()).unwrap();
    assert_eq!(memory.title, None);
    assert_eq!(memory.content, "This is the content.");
}

// ===========================================================================
// Adversarial round-trip: content that looks like the format's own structure
// ===========================================================================

/// Assert that writing `memory` and re-parsing it preserves every field that
/// the V2 format round-trips, and that a second write is byte-identical
/// (the fuzz targets' fixed-point property).
fn assert_v2_roundtrip_exact(memory: &Memory) {
    let written = write_memory_file(memory).unwrap();
    let reparsed = parse_memory_file(&written).unwrap();

    assert_eq!(memory.id, reparsed.id);
    assert_eq!(memory.type_, reparsed.type_);
    assert_eq!(memory.title, reparsed.title);
    assert_eq!(memory.summary, reparsed.summary);
    assert_eq!(memory.content, reparsed.content, "content must round-trip");
    assert_eq!(memory.details, reparsed.details, "details must round-trip");
    assert_eq!(memory.physical, reparsed.physical);
    assert_eq!(memory.logical, reparsed.logical);
    assert_eq!(memory.tags, reparsed.tags);
    assert_eq!(memory.criticality, reparsed.criticality);
    assert_eq!(memory.confidence, reparsed.confidence);
    assert_eq!(memory.status, reparsed.status);
    assert_eq!(memory.visibility, reparsed.visibility);
    assert_eq!(memory.provenance.source, reparsed.provenance.source);
    assert_eq!(memory.provenance.reason, reparsed.provenance.reason);
    assert_eq!(memory.supersedes, reparsed.supersedes);
    assert_eq!(memory.created_at, reparsed.created_at);
    assert_eq!(memory.updated_at, reparsed.updated_at);
    assert_eq!(memory.accessed_at, reparsed.accessed_at);

    let rewritten = write_memory_file(&reparsed).unwrap();
    assert_eq!(written, rewritten, "write must be a fixed point");
}

#[test]
fn test_v2_roundtrip_content_with_structural_markdown() {
    let mut memory = sample_memory();
    memory.content = [
        "Intro line.",
        "# fake h1 that must stay in content",
        "## Scope",
        "- **Criticality:** 0.99",
        "- **Visibility:** personal",
        "## Content",
        "nested fake content section",
        "## Provenance",
        "- **Source:** agent",
        "<!-- engramdb",
        "visibility: personal",
        "-->",
        "<!--",
        "a plain html comment",
        "-->",
        "---",
        "> a blockquote line",
        ">",
        "\\## already-escaped-looking line",
        "\\\\## doubly-escaped-looking line",
        "\\not an escape",
        "tail line.",
    ]
    .join("\n");

    assert_v2_roundtrip_exact(&memory);

    // The adversarial content must not leak into structured fields.
    let reparsed = parse_memory_file(&write_memory_file(&memory).unwrap()).unwrap();
    assert_eq!(reparsed.summary, "Test memory");
    assert_eq!(reparsed.criticality, 0.5);
    assert_eq!(reparsed.visibility, Visibility::Shared);
    assert_eq!(reparsed.provenance.source, ProvenanceSource::Human);
}

#[test]
fn test_v2_roundtrip_details_with_structural_markdown() {
    let mut memory = sample_memory();
    memory.details = Some(
        [
            "## Content",
            "fake content override",
            "# another heading",
            "-->",
            "real detail text",
        ]
        .join("\n"),
    );

    assert_v2_roundtrip_exact(&memory);

    let reparsed = parse_memory_file(&write_memory_file(&memory).unwrap()).unwrap();
    assert_eq!(reparsed.content, "This is the content.");
}

#[test]
fn test_v2_roundtrip_content_with_summary_line() {
    // A `**Summary:** ...` line inside content must not override the real
    // summary (the parser only scans the preamble before the first section).
    let mut memory = sample_memory();
    memory.title = Some("Real Title".to_string());
    memory.content = "**Summary:** fake summary\nreal content".to_string();

    assert_v2_roundtrip_exact(&memory);
}

#[test]
fn test_v2_roundtrip_title_with_dashes_and_quotes() {
    for title in [
        "Weird --- \"quoted\" title",
        "--- starts with dashes",
        "---",
        "ends with dashes ---",
    ] {
        let mut memory = sample_memory();
        memory.title = Some(title.to_string());
        assert_v2_roundtrip_exact(&memory);
    }
}

#[test]
fn test_v2_roundtrip_content_with_dashes_lines() {
    let mut memory = sample_memory();
    memory.content = "before\n---\nafter".to_string();
    assert_v2_roundtrip_exact(&memory);
}

#[test]
fn test_v1_roundtrip_content_with_structural_markdown() {
    let writer = V1Writer;
    let parser = V1Parser;
    let mut memory = sample_memory();
    memory.content = "text\n## Details\nfake details\n# fake h1\nend".to_string();

    let written = writer.write(&memory).unwrap();
    let reparsed = parser.parse(&written).unwrap();
    assert_eq!(memory.content, reparsed.content);
    assert_eq!(memory.details, reparsed.details);
    assert_eq!(reparsed.title, None);
}

#[test]
fn test_split_frontmatter_ignores_inline_dashes() {
    // A line containing (but not being exactly) `---` is not a fence.
    let (frontmatter, body) = super::helpers::split_frontmatter(
        "---\nversion: 2\ntitle: a --- b\n--- not a fence\n---\nbody text\n",
    )
    .unwrap();
    assert!(frontmatter.contains("title: a --- b"));
    assert!(frontmatter.contains("--- not a fence"));
    assert_eq!(body, "body text\n");
}

// ===========================================================================
// Backward compatibility: old (unescaped) files
// ===========================================================================

#[test]
fn test_v2_old_unescaped_file_still_parses() {
    // Hand-built file in the pre-escaping format whose content contains
    // structural-looking lines. It was already mis-parsed by the old code;
    // the requirement here is that it still parses without error and the
    // writer-emitted (first) sections win over the content-embedded ones.
    let old = r#"---
version: 2
id: old-unescaped
type: decision
status: Active
---

# Old summary

## Content

Some text, then a fake section:

## Provenance

- **Source:** agent

## Scope

- **Criticality:** 0.7
- **Confidence:** 0.9

## Provenance

- **Source:** human
- **Created:** 2026-01-15T10:00:00Z
- **Updated:** 2026-01-15T10:00:00Z

<!-- engramdb
visibility: shared
accessed_at: "2026-01-15T10:00:00Z"
-->
"#;
    let memory = parse_memory_file(old).unwrap();
    assert_eq!(memory.id, "old-unescaped");
    assert_eq!(memory.summary, "Old summary");
    assert_eq!(memory.criticality, 0.7);
    assert_eq!(memory.confidence, 0.9);
    // First ## Provenance wins (the fake one embedded in content) — that is
    // the documented first-occurrence rule; new writes escape such lines.
    assert_eq!(memory.provenance.source, ProvenanceSource::Agent);

    // And the file can be rewritten + re-read losslessly from here on.
    let rewritten = write_memory_file(&memory).unwrap();
    let reparsed = parse_memory_file(&rewritten).unwrap();
    assert_eq!(memory.content, reparsed.content);
    assert_eq!(memory.criticality, reparsed.criticality);
}

#[test]
fn test_v2_duplicate_scope_section_first_wins() {
    let file = r#"---
version: 2
id: dup-scope
type: decision
status: Active
---

# Summary

## Content

content text

## Scope

- **Criticality:** 0.7
- **Confidence:** 0.9
- **Tags:** first

## Scope

- **Criticality:** 0.2
- **Confidence:** 0.1
- **Tags:** second

## Provenance

- **Source:** human
"#;
    let memory = parse_memory_file(file).unwrap();
    assert_eq!(memory.criticality, 0.7);
    assert_eq!(memory.confidence, 0.9);
    assert_eq!(memory.tags, vec!["first"]);
}

#[test]
fn test_v2_first_h1_wins_and_later_h1_stays_in_content() {
    let file = r#"---
version: 2
id: h1-test
type: decision
status: Active
---

# Real summary

## Content

before
# not the summary
after

## Scope

- **Criticality:** 0.5
"#;
    let memory = parse_memory_file(file).unwrap();
    assert_eq!(memory.summary, "Real summary");
    // In old files the embedded H1 used to overwrite the summary AND truncate
    // the content; now it stays where it was.
    assert_eq!(memory.content, "before\n# not the summary\nafter");
}

#[test]
fn test_v2_embedded_engramdb_block_last_marker_wins() {
    // Old unescaped file with an engramdb-looking block inside content; the
    // writer's real block at the end of the file must be the one parsed.
    let file = r#"---
version: 2
id: hidden-test
type: decision
status: Active
---

# Summary

## Content

content with an embedded block:
<!-- engramdb
visibility: personal
-->
more content

## Scope

- **Criticality:** 0.5

<!-- engramdb
visibility: shared
accessed_at: "2026-01-15T10:00:00Z"
-->
"#;
    let memory = parse_memory_file(file).unwrap();
    assert_eq!(memory.visibility, Visibility::Shared);
}

// ===========================================================================
// Epistemic fields (schema 0.3.0 era) — file-format behavior
// ===========================================================================

/// Byte-for-byte output of the pre-epistemic V2 writer for `sample_memory()`.
/// Captured on the last commit before the epistemic fields landed. The writer
/// only emits `epistemic` when off-diagonal and validity/window fields when
/// set, so a diagonal memory must keep producing EXACTLY these bytes — this is
/// the no-rewrite-churn guarantee for every existing store.
const GOLDEN_PRE_EPISTEMIC_SAMPLE: &str = r#"---
version: 2
id: test-123
type: hazard
status: Active
---

# Test memory

## Content

This is the content.

## Details

These are the details.

## Scope

- **Files:** `/`
- **Criticality:** 0.5
- **Confidence:** 0.8

## Provenance

- **Source:** human
- **Created:** 2026-01-15T10:00:00+00:00
- **Updated:** 2026-01-15T10:00:00+00:00

<!-- engramdb
visibility: Shared
accessed_at: 2026-01-15T10:00:00Z
decay:
  strategy: none
  floor: 0.5
-->
"#;

/// Same capture for `full_memory()` (title, personal visibility, decay with
/// half-life, supersedes — the busy path through the writer).
const GOLDEN_PRE_EPISTEMIC_FULL: &str = r#"---
version: 2
id: test-full
type: decision
status: Active
title: Full Field Test Title
---

# Full Field Test Title

**Summary:** Full field test

## Content

Content here.

## Details

Details here.

## Scope

- **Files:** `src/db/**`, `src/lib.rs`
- **Logical:** `infrastructure.database`
- **Tags:** database, transactions
- **Criticality:** 0.95
- **Confidence:** 1
- **Visibility:** personal

## Provenance

- **Source:** agent
- **Agent:** claude
- **Model:** claude-opus-4-6
- **Session:** sess-123
- **Reason:** Post-incident review
- **Created:** 2026-01-15T10:00:00+00:00
- **Updated:** 2026-01-15T10:00:00+00:00

<!-- engramdb
visibility: Personal
accessed_at: 2026-01-15T10:00:00Z
decay:
  strategy: exponential
  half_life:
  - 1209600
  - 0
  floor: 0.0
supersedes:
- old-id-1
-->
"#;

#[test]
fn test_v2_diagonal_memory_writes_pre_epistemic_bytes() {
    assert_eq!(
        write_memory_file(&sample_memory()).unwrap(),
        GOLDEN_PRE_EPISTEMIC_SAMPLE,
        "diagonal memory must round-trip byte-identically to the pre-epistemic writer"
    );
    assert_eq!(
        write_memory_file(&full_memory()).unwrap(),
        GOLDEN_PRE_EPISTEMIC_FULL,
        "full diagonal memory must round-trip byte-identically to the pre-epistemic writer"
    );
}

#[test]
fn test_v2_off_diagonal_epistemic_roundtrip() {
    use engram_types::Epistemic;

    // Hazard defaults to Fact; declare it an Observation (off-diagonal).
    let mut memory = sample_memory();
    memory.epistemic = Epistemic::Observation;

    let written = write_memory_file(&memory).unwrap();
    assert!(
        written.contains("epistemic: observation"),
        "off-diagonal class must be emitted in frontmatter:\n{written}"
    );

    let parsed = parse_memory_file(&written).unwrap();
    assert_eq!(parsed.epistemic, Epistemic::Observation);
}

#[test]
fn test_v2_parser_defaults_epistemic_from_type() {
    // A file with no `epistemic` key (i.e. every pre-epistemic file)
    // materializes the type-derived default, not the serde default (Fact).
    let file = "---\nversion: 2\nid: epi-default\ntype: debug\nstatus: Active\n---\n\n\
                # S\n\n## Content\n\nc\n";
    let parsed = parse_memory_file(file).unwrap();
    assert_eq!(parsed.epistemic, engram_types::Epistemic::Observation);
}

#[test]
fn test_v2_validity_and_window_fields_roundtrip() {
    use engram_types::{Generality, Validity};

    let mut memory = sample_memory();
    memory.valid_while = Some(Validity {
        premise: Some("while we pin ort rc.12".into()),
        invalidated_by: vec!["Cargo.lock".into()],
        origin_task: Some("epistemic-memory".into()),
        generality: Generality::Task,
        derived_from: vec!["src-1".into()],
    });
    memory.valid_from = Some("2026-01-10T00:00:00Z".parse().unwrap());
    memory.invalidated_at = Some("2026-02-01T00:00:00Z".parse().unwrap());
    memory.superseded_by = Some("succ-1".into());

    let written = write_memory_file(&memory).unwrap();
    let parsed = parse_memory_file(&written).unwrap();
    assert_eq!(parsed.valid_while, memory.valid_while);
    assert_eq!(parsed.valid_from, memory.valid_from);
    assert_eq!(parsed.invalidated_at, memory.invalidated_at);
    assert_eq!(parsed.superseded_by, memory.superseded_by);
}

#[test]
fn test_v2_empty_validity_normalized_to_none() {
    use engram_types::{Generality, Validity};

    // An all-empty Validity must not be persisted (write side)…
    let mut memory = sample_memory();
    memory.valid_while = Some(Validity::default());
    let written = write_memory_file(&memory).unwrap();
    assert!(
        !written.contains("valid_while"),
        "empty Validity must not be written:\n{written}"
    );
    assert_eq!(parse_memory_file(&written).unwrap().valid_while, None);

    // …and a hand-edited file carrying one (generality-only counts as empty)
    // is normalized away on read.
    let mut memory = sample_memory();
    memory.valid_while = Some(Validity {
        generality: Generality::Task,
        ..Default::default()
    });
    let written = write_memory_file(&memory).unwrap();
    assert_eq!(parse_memory_file(&written).unwrap().valid_while, None);
}

#[test]
fn test_v1_epistemic_defaults_from_type_not_serde() {
    // V1 frontmatter without an `epistemic` key: serde's field default is
    // Fact, but the authoritative default is type-derived — Debug must
    // materialize as Observation.
    let v1 = "---\nid: v1-epi\ntype: debug\nsummary: S\ncontent: \"\"\n\
              physical: []\nlogical: []\ncriticality: 0.5\nconfidence: 0.8\n\
              provenance:\n  source: human\nstatus: Active\nvisibility: Shared\n\
              created_at: 2026-01-15T10:00:00Z\nupdated_at: 2026-01-15T10:00:00Z\n\
              accessed_at: 2026-01-15T10:00:00Z\n---\n\n## Content\n\nc\n";
    let parsed = parse_memory_file(v1).unwrap();
    assert_eq!(parsed.epistemic, engram_types::Epistemic::Observation);

    // An explicit key is honored, even when it names the serde default.
    let v1_explicit = v1.replace("type: debug", "type: debug\nepistemic: fact");
    let parsed = parse_memory_file(&v1_explicit).unwrap();
    assert_eq!(parsed.epistemic, engram_types::Epistemic::Fact);
}
