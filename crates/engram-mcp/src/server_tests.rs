//! Tests for the MCP server surface.
//!
//! Extracted from an inline `mod tests` at the bottom of `server.rs` (which
//! had grown to ~60% of a 6,500-line file): same module (`server::tests`),
//! same `super::*` scope, just its own file so tool changes and test changes
//! stop contending on one file.

use super::*;
use engramdb::storage::InMemoryRegistry;
use engramdb::types::EmbeddingBackend;
use serde_json::json;
use tempfile::TempDir;

async fn setup() -> (TempDir, EngramDbServer) {
    let temp_dir = TempDir::new().unwrap();
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = EngramDbServer::new_with_registry(
        temp_dir.path().to_path_buf(),
        Some(EmbeddingBackend::Onnx),
        registry,
    );
    (temp_dir, server)
}

fn parse_ok(result: &Result<String, String>) -> serde_json::Value {
    let json_str = result.as_ref().expect("tool should succeed");
    serde_json::from_str(json_str).expect("should be valid JSON")
}

fn parse_err(result: &Result<String, String>) -> serde_json::Value {
    let json_str = result.as_ref().unwrap_err();
    serde_json::from_str(json_str).unwrap_or_else(|_| json!({"error": {"message": json_str}}))
}

/// Helper: build a QueryInput with all fields defaulted to None and the
/// given mode. Use with `..query_input(...)` in tests to override only
/// the fields that matter.
fn query_input(mode: &str) -> QueryInput {
    QueryInput {
        epistemic: None,
        situation: None,
        include_invalidated: None,
        mode: mode.to_string(),
        query: None,
        path: None,
        logical: None,
        types: None,
        tags: None,
        min_criticality: None,
        max_results: None,
        detail_level: None,
        include_expired: None,
        include_global: None,
        project: None,
    }
}

fn create_input(type_: &str, summary: &str, content: &str) -> CreateInput {
    CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        type_: type_.to_string(),
        content: content.to_string(),
        summary: summary.to_string(),
        details: None,
        physical: None,
        logical: None,
        tags: None,
        criticality: None,
        confidence: None,
        visibility: None,
        supersedes: None,
        decay_strategy: None,
        decay_half_life: None,
        decay_ttl: None,
        decay_floor: None,
        title: None,
        title_strategy: None,
        project: None,
    }
}

/// Helper: create a memory and return its ID.
async fn create_and_get_id(
    server: &EngramDbServer,
    type_: &str,
    summary: &str,
    content: &str,
) -> String {
    let result = server
        .memory_create(Parameters(create_input(type_, summary, content)))
        .await;
    let val = parse_ok(&result);
    val["id"].as_str().unwrap().to_string()
}

// -----------------------------------------------------------------------
// memory_create
// -----------------------------------------------------------------------

#[tokio::test]
async fn create_basic() {
    let (_dir, server) = setup().await;
    let result = server
        .memory_create(Parameters(create_input(
            "decision",
            "Use Rust",
            "We chose Rust for performance",
        )))
        .await;
    let val = parse_ok(&result);
    assert!(val["id"].is_string());
    assert_eq!(val["created"], true);
    assert_eq!(val["summary"], "Use Rust");
}

#[tokio::test]
async fn create_with_all_fields() {
    let (_dir, server) = setup().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        type_: "hazard".to_string(),
        content: "Race condition in cache".to_string(),
        summary: "Cache race".to_string(),
        details: Some("Detailed explanation".to_string()),
        physical: Some(vec!["src/cache.rs".to_string()]),
        logical: Some(vec!["caching.invalidation".to_string()]),
        tags: Some(vec!["perf".to_string(), "critical".to_string()]),
        criticality: Some(0.9),
        confidence: Some(0.7),
        visibility: Some("personal".to_string()),
        supersedes: Some(vec![]),
        decay_strategy: Some("exponential".to_string()),
        decay_half_life: Some(86400),
        decay_ttl: None,
        decay_floor: Some(0.1),
        title: None,
        title_strategy: None,
        project: None,
    };
    let result = server.memory_create(Parameters(input)).await;
    let val = parse_ok(&result);
    assert!(val["id"].is_string());
    assert_eq!(val["created"], true);
}

#[tokio::test]
async fn create_invalid_type() {
    let (_dir, server) = setup().await;
    let result = server
        .memory_create(Parameters(create_input("nonsense", "Bad", "Content")))
        .await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn create_criticality_out_of_range() {
    let (_dir, server) = setup().await;
    let mut input = create_input("decision", "Test", "Content");
    input.criticality = Some(2.0);
    let result = server.memory_create(Parameters(input)).await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn create_confidence_out_of_range() {
    let (_dir, server) = setup().await;
    let mut input = create_input("decision", "Test", "Content");
    input.confidence = Some(-0.1);
    let result = server.memory_create(Parameters(input)).await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn create_decay_floor_out_of_range() {
    let (_dir, server) = setup().await;
    let mut input = create_input("decision", "Test", "Content");
    input.decay_floor = Some(1.5);
    let result = server.memory_create(Parameters(input)).await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
}

// -----------------------------------------------------------------------
// memory_get
// -----------------------------------------------------------------------

#[tokio::test]
async fn get_existing() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(
        &server,
        "convention",
        "Use snake_case",
        "All names use snake_case",
    )
    .await;
    let result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["summary"], "Use snake_case");
    assert_eq!(val["content"], "All names use snake_case");
    assert_eq!(val["type"], "convention");
}

#[tokio::test]
async fn get_nonexistent() {
    let (_dir, server) = setup().await;
    // Need a store to exist first
    let _ = create_and_get_id(&server, "decision", "Setup", "Content").await;
    let result = server
        .memory_get(Parameters(GetInput {
            id: "nonexistent-id-1234".to_string(),
            project: None,
        }))
        .await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "MEMORY_NOT_FOUND");
}

#[tokio::test]
async fn get_by_prefix() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Prefix test", "Content").await;
    let prefix = &id[..8];
    let result = server
        .memory_get(Parameters(GetInput {
            id: prefix.to_string(),
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["summary"], "Prefix test");
}

// -----------------------------------------------------------------------
// memory_update
// -----------------------------------------------------------------------

#[tokio::test]
async fn update_summary() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Old summary", "Content").await;
    let result = server
        .memory_update(Parameters(UpdateInput {
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: None,
            clear_invalidated: None,
            id: id.clone(),
            summary: Some("New summary".to_string()),
            type_: None,
            content: None,
            details: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            visibility: None,
            title: None,
            status: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["updated"], true);

    let get_result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["summary"], "New summary");
}

#[tokio::test]
async fn update_type() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Summary", "Content").await;
    let result = server
        .memory_update(Parameters(UpdateInput {
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: None,
            clear_invalidated: None,
            id: id.clone(),
            type_: Some("hazard".to_string()),
            summary: None,
            content: None,
            details: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            visibility: None,
            title: None,
            status: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["updated"], true);

    let get_result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["type"], "hazard");
}

#[tokio::test]
async fn update_status() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Summary", "Content").await;
    let result = server
        .memory_update(Parameters(UpdateInput {
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: None,
            clear_invalidated: None,
            id: id.clone(),
            status: Some("challenged".to_string()),
            type_: None,
            summary: None,
            content: None,
            details: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            visibility: None,
            title: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["updated"], true);

    let get_result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["status"], "challenged");
}

#[tokio::test]
async fn update_tags_add_remove() {
    let (_dir, server) = setup().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        tags: Some(vec!["alpha".to_string(), "beta".to_string()]),
        ..create_input("decision", "Tagged", "Content")
    };
    let result = server.memory_create(Parameters(input)).await;
    let id = parse_ok(&result)["id"].as_str().unwrap().to_string();

    let result = server
        .memory_update(Parameters(UpdateInput {
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: None,
            clear_invalidated: None,
            id: id.clone(),
            tags_add: Some(vec!["gamma".to_string()]),
            tags_remove: Some(vec!["alpha".to_string()]),
            type_: None,
            summary: None,
            content: None,
            details: None,
            physical: None,
            logical: None,
            tags: None,
            criticality: None,
            confidence: None,
            visibility: None,
            title: None,
            status: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            project: None,
        }))
        .await;
    parse_ok(&result);

    let get_result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let get_val = parse_ok(&get_result);
    let tags: Vec<String> = get_val["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(tags.contains(&"beta".to_string()));
    assert!(tags.contains(&"gamma".to_string()));
    assert!(!tags.contains(&"alpha".to_string()));
}

#[tokio::test]
async fn update_criticality_validation() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Summary", "Content").await;
    let result = server
        .memory_update(Parameters(UpdateInput {
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: None,
            clear_invalidated: None,
            id,
            criticality: Some(2.0),
            type_: None,
            summary: None,
            content: None,
            details: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            confidence: None,
            visibility: None,
            title: None,
            status: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            project: None,
        }))
        .await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn update_decay_params() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Summary", "Content").await;
    let result = server
        .memory_update(Parameters(UpdateInput {
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: None,
            clear_invalidated: None,
            id,
            decay_strategy: Some("exponential".to_string()),
            decay_half_life: Some(3600),
            decay_floor: Some(0.2),
            type_: None,
            summary: None,
            content: None,
            details: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            visibility: None,
            title: None,
            status: None,
            supersedes: None,
            decay_ttl: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["updated"], true);
}

// -----------------------------------------------------------------------
// memory_delete
// -----------------------------------------------------------------------

#[tokio::test]
async fn delete_existing() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "To delete", "Content").await;
    let result = server
        .memory_delete(Parameters(DeleteInput {
            id: id.clone(),
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["deleted"], true);

    let get_result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let err_val = parse_err(&get_result);
    assert_eq!(err_val["error"]["code"], "MEMORY_NOT_FOUND");
}

#[tokio::test]
async fn delete_nonexistent() {
    let (_dir, server) = setup().await;
    // Ensure store exists
    let _ = create_and_get_id(&server, "decision", "Setup", "Content").await;
    let result = server
        .memory_delete(Parameters(DeleteInput {
            id: "nonexistent-id-5678".to_string(),
            project: None,
        }))
        .await;
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// memory_query — filter mode (formerly `search`)
// -----------------------------------------------------------------------

#[tokio::test]
async fn search_basic() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(
        &server,
        "decision",
        "Use Rust for speed",
        "Rust is fast and safe",
    )
    .await;
    let _ = create_and_get_id(
        &server,
        "convention",
        "snake_case naming",
        "Use snake_case everywhere",
    )
    .await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("Rust fast".to_string()),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(!val["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn search_with_type_filter() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "Decision mem", "Decision content").await;
    let _ = create_and_get_id(&server, "hazard", "Hazard mem", "Hazard content").await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("content".to_string()),
            types: Some(vec!["hazard".to_string()]),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    let memories = val["memories"].as_array().unwrap();
    for m in memories {
        assert_eq!(m["type"], "hazard");
    }
}

#[tokio::test]
async fn search_max_results() {
    let (_dir, server) = setup().await;
    for i in 0..5 {
        let _ = create_and_get_id(
            &server,
            "decision",
            &format!("Memory {}", i),
            &format!("Content about topic {}", i),
        )
        .await;
    }

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("topic".to_string()),
            max_results: Some(1),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["memories"].as_array().unwrap().len() <= 1);
}

#[tokio::test]
async fn search_low_similarity_query_does_not_boost_unrelated_memory() {
    // With embeddings enabled, even a nonsense query yields non-zero
    // semantic similarity, so filter-mode sufficiency passes for every
    // memory. The invariant we preserve is weaker than the old
    // `search()` "keyword-strict" contract: the returned memory should
    // rank below a keyword-matching one. We verify that here by
    // contrasting a keyword-matching query against the same corpus.
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "About Rust", "Rust content").await;

    let nonsense = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("xyzzy_nonexistent_term_9999".to_string()),
            ..query_input("filter")
        }))
        .await;
    let keyword = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("Rust".to_string()),
            ..query_input("filter")
        }))
        .await;

    let nonsense_val = parse_ok(&nonsense);
    let keyword_val = parse_ok(&keyword);

    // Keyword-matching query must return results.
    assert!(!keyword_val["memories"].as_array().unwrap().is_empty());

    // Even if the nonsense query returns the memory via semantic
    // similarity, the keyword query must score it higher.
    if let Some(ns_mem) = nonsense_val["memories"].as_array().unwrap().first() {
        let ns_score = ns_mem["score"].as_f64().unwrap();
        let kw_score = keyword_val["memories"][0]["score"].as_f64().unwrap();
        assert!(
            kw_score > ns_score,
            "keyword match ({}) should outrank nonsense semantic match ({})",
            kw_score,
            ns_score,
        );
    }
}

// -----------------------------------------------------------------------
// memory_query — rank mode (context-aware ranking, formerly `retrieve`)
// -----------------------------------------------------------------------

#[tokio::test]
async fn retrieve_by_path() {
    let (_dir, server) = setup().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        physical: Some(vec!["src/main.rs".to_string()]),
        criticality: Some(0.9),
        ..create_input("decision", "Main entry", "The main function starts here")
    };
    server.memory_create(Parameters(input)).await.unwrap();

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("src/main.rs".to_string()),
            ..query_input("rank")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(!val["memories"].as_array().unwrap().is_empty());
}

/// Regression: physical scopes are stored repo-relative, so an absolute
/// `path` (exactly what the tool description invites an agent to pass)
/// used to silently match nothing. The engine now relativizes absolute
/// paths against the project root, so absolute-under-root and relative
/// queries must return identical results.
#[tokio::test]
async fn query_filter_absolute_path_relativized_to_project() {
    let (dir, server) = setup().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        physical: Some(vec!["src/auth.rs".to_string()]),
        criticality: Some(0.9),
        ..create_input("hazard", "Auth hazard", "Never log raw tokens")
    };
    server.memory_create(Parameters(input)).await.unwrap();

    // Create the file on disk so canonicalization resolves it (mirrors a
    // real agent passing the path of the file it is editing).
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/auth.rs"), "// auth").unwrap();

    let ids_for = |val: &serde_json::Value| -> Vec<String> {
        val["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };

    // Baseline: repo-relative path (filter mode → path is a hard filter).
    let rel = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("src/auth.rs".to_string()),
            ..query_input("filter")
        }))
        .await;
    let rel_ids = ids_for(&parse_ok(&rel));
    assert_eq!(rel_ids.len(), 1, "relative path should match the memory");

    // Absolute path under the project root → identical results.
    let abs_path = dir.path().join("src/auth.rs");
    let abs = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some(abs_path.to_string_lossy().to_string()),
            ..query_input("filter")
        }))
        .await;
    let abs_ids = ids_for(&parse_ok(&abs));
    assert_eq!(
        abs_ids, rel_ids,
        "absolute path under the project root must match like the relative path"
    );
}

/// An absolute path NOT under the project root passes through unchanged:
/// it legitimately matches no repo-relative scope, so the query succeeds
/// with zero results (and must not panic or error).
#[tokio::test]
async fn query_filter_absolute_path_outside_project_matches_nothing() {
    let (_dir, server) = setup().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        physical: Some(vec!["src/auth.rs".to_string()]),
        criticality: Some(0.9),
        ..create_input("hazard", "Auth hazard", "Never log raw tokens")
    };
    server.memory_create(Parameters(input)).await.unwrap();

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("/definitely/elsewhere/src/auth.rs".to_string()),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(
        val["memories"].as_array().unwrap().is_empty(),
        "a path outside the project root must match nothing"
    );
}

#[tokio::test]
async fn retrieve_by_logical() {
    let (_dir, server) = setup().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        logical: Some(vec!["auth.login".to_string()]),
        physical: Some(vec!["src/auth/login.rs".to_string()]),
        criticality: Some(0.9),
        ..create_input("convention", "Login convention", "Always use OAuth2")
    };
    server.memory_create(Parameters(input)).await.unwrap();

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("src/auth/login.rs".to_string()),
            logical: Some(vec!["auth.login".to_string()]),
            ..query_input("rank")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(!val["memories"].as_array().unwrap().is_empty());
}

/// Filter-mode `logical` is a hierarchical filter, not exact string
/// equality: querying the domain `auth` must surface a memory scoped to
/// the subdomain `auth.oauth` (and the ancestor direction holds:
/// querying `auth.oauth` surfaces a memory scoped `auth`), while an
/// unrelated scope matches nothing. Mirrors the engine-level contract in
/// `retrieval::engine`.
#[tokio::test]
async fn query_filter_logical_is_hierarchical() {
    let (_dir, server) = setup().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        logical: Some(vec!["auth.oauth".to_string()]),
        criticality: Some(0.9),
        ..create_input("decision", "OAuth decision", "We use PKCE")
    };
    server.memory_create(Parameters(input)).await.unwrap();

    // Querying the parent domain matches the subdomain-scoped memory.
    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            logical: Some(vec!["auth".to_string()]),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(
        val["memories"].as_array().unwrap().len(),
        1,
        "query `auth` must match memory scoped `auth.oauth`: {val}"
    );

    // Querying a deeper scope matches the ancestor-scoped memory.
    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            logical: Some(vec!["auth.oauth.google".to_string()]),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(
        val["memories"].as_array().unwrap().len(),
        1,
        "query `auth.oauth.google` must match ancestor scope `auth.oauth`: {val}"
    );

    // Unrelated scope matches nothing.
    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            logical: Some(vec!["billing".to_string()]),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(
        val["memories"].as_array().unwrap().is_empty(),
        "query `billing` must not match `auth.oauth`: {val}"
    );
}

#[tokio::test]
async fn retrieve_detail_level_summary() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(
        &server,
        "decision",
        "Summary test",
        "Content for detail test",
    )
    .await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("/".to_string()),
            detail_level: Some("summary".to_string()),
            ..query_input("rank")
        }))
        .await;
    // Should succeed without error
    parse_ok(&result);
}

// Finding #9: detail_level is parsed case-insensitively, so a capitalized
// "Full" must also include details in the output. Before the fix,
// include_details used a case-sensitive `== "full"` compare and silently
// stripped details for any non-lowercase casing.
#[tokio::test]
async fn retrieve_detail_level_full_is_case_insensitive() {
    let (_dir, server) = setup().await;
    let mut input = create_input("decision", "Case test", "Body content");
    input.details = Some("These are the detailed notes".to_string());
    server
        .memory_create(Parameters(input))
        .await
        .expect("create should succeed");

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            // Filter mode + a query signal reliably returns the match.
            query: Some("Body content".to_string()),
            detail_level: Some("Full".to_string()), // capitalized on purpose
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(
        val["memories"].as_array().is_some_and(|m| !m.is_empty()),
        "expected the created memory to be returned: {val}"
    );
    // NEGATIVE (red before fix): capitalized "Full" dropped the details.
    // (Memory fields are flattened into each `memories[i]` object.)
    assert_eq!(
        val["memories"][0]["details"].as_str(),
        Some("These are the detailed notes"),
        "detail_level=Full (any case) must include details"
    );
}

#[tokio::test]
async fn retrieve_detail_level_invalid() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "Setup", "Content").await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            detail_level: Some("bogus".to_string()),
            ..query_input("rank")
        }))
        .await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn query_rejects_invalid_mode() {
    let (_dir, server) = setup().await;
    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("anything".to_string()),
            ..query_input("invalid_mode_name")
        }))
        .await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn query_filter_requires_a_signal() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "anything", "content").await;

    // No query/logical/path/tags — filter mode must reject.
    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            min_criticality: Some(0.5),
            ..query_input("filter")
        }))
        .await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "INTERNAL_ERROR");
    assert!(val["error"]["message"]
        .as_str()
        .unwrap()
        .contains("filter requires at least one"));
}

// -----------------------------------------------------------------------
// memory_list
// -----------------------------------------------------------------------

#[tokio::test]
async fn list_empty() {
    let (_dir, server) = setup().await;
    // Init the store by creating and immediately deleting a memory
    let id = create_and_get_id(&server, "decision", "Temp", "Temp").await;
    server
        .memory_delete(Parameters(DeleteInput { id, project: None }))
        .await
        .unwrap();

    let result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["total"], 0);
    assert_eq!(val["memories"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_all() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "First", "Content 1").await;
    let _ = create_and_get_id(&server, "hazard", "Second", "Content 2").await;
    let _ = create_and_get_id(&server, "convention", "Third", "Content 3").await;

    let result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["total"], 3);
    // §5.4: epistemic is always present in list output (type-derived here).
    for m in val["memories"].as_array().unwrap() {
        let epistemic = m["epistemic"].as_str().unwrap();
        match m["type"].as_str().unwrap() {
            "decision" => assert_eq!(epistemic, "decision"),
            "hazard" | "convention" => assert_eq!(epistemic, "fact"),
            other => panic!("unexpected type {other}"),
        }
    }
}

#[tokio::test]
async fn list_filter_by_type() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "Dec1", "Content").await;
    let _ = create_and_get_id(&server, "hazard", "Haz1", "Content").await;
    let _ = create_and_get_id(&server, "decision", "Dec2", "Content").await;

    let result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: Some(vec!["decision".to_string()]),
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["total"], 2);
    for m in val["memories"].as_array().unwrap() {
        assert_eq!(m["type"], "decision");
    }
}

#[tokio::test]
async fn list_sort_and_limit() {
    let (_dir, server) = setup().await;
    for i in 0..5 {
        let mut input = create_input("decision", &format!("Mem {}", i), "Content");
        input.criticality = Some(i as f64 * 0.2);
        server.memory_create(Parameters(input)).await.unwrap();
    }

    let result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: Some("criticality".to_string()),
            reverse: None,
            limit: Some(2),
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["total"], 2);
}

#[tokio::test]
async fn list_invalid_sort() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "Setup", "Content").await;

    let result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: Some("bogus".to_string()),
            reverse: None,
            limit: None,
            project: None,
        }))
        .await;
    let val = parse_err(&result);
    assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
}

// -----------------------------------------------------------------------
// memory_challenge + memory_review + memory_resolve
// -----------------------------------------------------------------------

#[tokio::test]
async fn challenge_memory() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Old decision", "Maybe wrong").await;
    let result = server
        .memory_challenge(Parameters(ChallengeInput {
            id: id.clone(),
            evidence: "Found contradicting evidence".to_string(),
            source_file: Some("src/test.rs".to_string()),
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["challenged"], true);

    let get_result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["status"], "challenged");
}

#[tokio::test]
async fn review_shows_challenged() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Reviewed decision", "Content").await;
    server
        .memory_challenge(Parameters(ChallengeInput {
            id,
            evidence: "Evidence".to_string(),
            source_file: None,
            project: None,
        }))
        .await
        .unwrap();

    let result = server
        .memory_review(Parameters(ReviewInput {
            scope: None,
            max_results: None,
            type_: None,
            challenged_only: Some(true),
            stale_only: None,
            stale_after_days: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["total"].as_u64().unwrap() > 0);
    for m in val["memories"].as_array().unwrap() {
        assert_eq!(m["status"], "challenged");
    }
}

#[tokio::test]
async fn review_with_type_filter() {
    let (_dir, server) = setup().await;
    let id1 = create_and_get_id(&server, "decision", "Dec challenged", "Content").await;
    let id2 = create_and_get_id(&server, "hazard", "Haz challenged", "Content").await;
    for id in [&id1, &id2] {
        server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "Evidence".to_string(),
                source_file: None,
                project: None,
            }))
            .await
            .unwrap();
    }

    let result = server
        .memory_review(Parameters(ReviewInput {
            scope: None,
            max_results: None,
            type_: Some("decision".to_string()),
            challenged_only: Some(true),
            stale_only: None,
            stale_after_days: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    for m in val["memories"].as_array().unwrap() {
        assert_eq!(m["type"], "decision");
    }
}

/// The `review` tool defaults its recency window to `[review].recency_days`
/// (90) when the caller omits `stale_after_days`, echoes the effective window
/// back, and does NOT surface freshly-created active memories (they are well
/// within the window) — so the recency arm nudges about *stale* memories rather
/// than flooding review with every active one.
#[tokio::test]
async fn review_recency_defaults_and_excludes_fresh() {
    let (_dir, server) = setup().await;
    // A brand-new active memory: within the 90-day window, must not surface.
    let _id = create_and_get_id(&server, "decision", "Fresh decision", "Content").await;

    let result = server
        .memory_review(Parameters(ReviewInput {
            scope: None,
            max_results: None,
            type_: None,
            challenged_only: None,
            stale_only: None,
            stale_after_days: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    // The effective window is echoed from config (default 90).
    assert_eq!(val["recency_days"], 90);
    // The fresh active memory is not stale, and nothing is flagged, so review
    // is empty.
    assert_eq!(val["total"], 0);
}

/// Passing `stale_after_days: 0` disables the recency arm (a 0-day window would
/// otherwise flag every active memory). The window is echoed as 0 and no active
/// memory surfaces.
#[tokio::test]
async fn review_recency_zero_disables() {
    let (_dir, server) = setup().await;
    let _id = create_and_get_id(&server, "decision", "Some decision", "Content").await;

    let result = server
        .memory_review(Parameters(ReviewInput {
            scope: None,
            max_results: None,
            type_: None,
            challenged_only: None,
            stale_only: None,
            stale_after_days: Some(0),
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["recency_days"], 0);
    assert_eq!(val["total"], 0);
}

#[tokio::test]
async fn resolve_keep() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Keep me", "Content").await;
    server
        .memory_challenge(Parameters(ChallengeInput {
            id: id.clone(),
            evidence: "Maybe wrong".to_string(),
            source_file: None,
            project: None,
        }))
        .await
        .unwrap();

    let result = server
        .memory_resolve(Parameters(ResolveInput {
            superseded_by: None,
            id: id.clone(),
            action: "keep".to_string(),
            updated_content: None,
            updated_summary: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["resolved"], true);
    assert_eq!(val["action"], "keep");

    let get_result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["status"], "active");
}

#[tokio::test]
async fn resolve_delete() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "decision", "Delete me", "Content").await;
    server
        .memory_challenge(Parameters(ChallengeInput {
            id: id.clone(),
            evidence: "Definitely wrong".to_string(),
            source_file: None,
            project: None,
        }))
        .await
        .unwrap();

    let result = server
        .memory_resolve(Parameters(ResolveInput {
            superseded_by: None,
            id: id.clone(),
            action: "delete".to_string(),
            updated_content: None,
            updated_summary: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["resolved"], true);
    assert_eq!(val["action"], "delete");

    let get_result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    assert!(get_result.is_err());
}

// -----------------------------------------------------------------------
// memory_stats
// -----------------------------------------------------------------------

#[tokio::test]
async fn stats_empty() {
    let (_dir, server) = setup().await;
    // Init store
    let id = create_and_get_id(&server, "decision", "Temp", "Temp").await;
    server
        .memory_delete(Parameters(DeleteInput { id, project: None }))
        .await
        .unwrap();

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["total"], 0);
}

#[tokio::test]
async fn stats_populated() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "Dec1", "Content").await;
    let _ = create_and_get_id(&server, "decision", "Dec2", "Content").await;
    let _ = create_and_get_id(&server, "hazard", "Haz1", "Content").await;

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["total"], 3);
    assert_eq!(val["by_type"]["decision"], 2);
    assert_eq!(val["by_type"]["hazard"], 1);
}

// -----------------------------------------------------------------------
// memory_gc
// -----------------------------------------------------------------------

#[tokio::test]
async fn gc_dry_run() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "Keep me", "Content").await;

    let result = server
        .memory_gc(Parameters(GcInput {
            dry_run: Some(true),
            threshold: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["dry_run"], true);

    // Memory should still be there
    let list_result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: None,
        }))
        .await;
    let list_val = parse_ok(&list_result);
    assert_eq!(list_val["total"], 1);
}

#[tokio::test]
async fn gc_confirm() {
    let (_dir, server) = setup().await;
    // Create a memory with very low criticality
    let mut input = create_input("debug", "Low priority debug", "Ephemeral content");
    input.criticality = Some(0.01);
    input.confidence = Some(0.01);
    server.memory_create(Parameters(input)).await.unwrap();

    // GC with a high threshold to ensure it catches the low-criticality memory
    let result = server
        .memory_gc(Parameters(GcInput {
            dry_run: Some(false),
            threshold: Some(0.99),
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["dry_run"], false);
}

// -----------------------------------------------------------------------
// memory_reindex
// -----------------------------------------------------------------------

#[tokio::test]
async fn reindex_basic() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "To reindex", "Content").await;

    let result = server
        .memory_reindex(Parameters(ReindexInput {
            embeddings_only: None,
            index_only: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["indexed"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn reindex_index_only() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "To reindex", "Content").await;

    let result = server
        .memory_reindex(Parameters(ReindexInput {
            embeddings_only: None,
            index_only: Some(true),
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["indexed"].as_u64().unwrap() >= 1);
}

/// Regression for PR-blocker bug #2: `reindex` is the remediation path
/// for an embedding model mismatch, so it must keep working even under
/// the strict `error` policy — where `build_engine_for` (the enforcing
/// chokepoint) deliberately refuses. Before the fix, `memory_reindex`
/// did `build_engine_for(...).ok()` → `None` in `error` mode and
/// silently ran an index-only reindex, reporting `embedded: 0` as
/// success while leaving the store unfixable.
#[tokio::test]
async fn reindex_re_embeds_in_error_mode_despite_mismatch() {
    let (dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "To reindex", "Content").await;

    // Force an unambiguous model mismatch.
    let store = server.open_store_for(None).await.unwrap();
    store
        .set_embedding_fingerprint(engramdb::storage::EmbeddingFingerprint {
            model: "onnx/bogus-old-model".to_string(),
            dimensions: 384,
        })
        .await
        .unwrap();

    // Strictest policy: refuse degraded embedding work.
    let mut config = engramdb::types::EngramConfig::default();
    config.embeddings.reindex_on_model_change = engramdb::types::ReindexOnModelChange::Error;
    tokio::fs::write(
        dir.path().join(".engramdb").join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .await
    .unwrap();

    // The enforcing chokepoint must refuse on the mismatch. The gate can
    // only fire when a live embedding provider loaded (no live `model_id()`
    // ⇒ nothing to compare against the stored fingerprint) — and under
    // parallel/instrumented runs the in-process ONNX load transiently
    // fails. That is the documented model-load race, not a gating bug:
    // skip instead of failing, mirroring the other model-dependent tests.
    match server.build_engine_for(None).await {
        Err(_) => {} // gate fired as required
        Ok(engine) => {
            if !engine.embeddings_available() {
                eprintln!("skipping: embedding provider unavailable (model-load race)");
                return;
            }
            panic!("error mode must gate the normal embedding path");
        }
    }
    // ...but the remediation builder must bypass the gate...
    assert!(
        server.assemble_engine_for(None).await.is_ok(),
        "reindex's engine builder must not be gated by error mode"
    );

    // ...and `reindex` must actually re-embed, not silently no-op.
    let result = server
        .memory_reindex(Parameters(ReindexInput {
            embeddings_only: Some(true),
            index_only: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert!(
        val["embedded"].as_u64().unwrap() >= 1,
        "reindex must re-embed in error mode (bug #2); got {val}"
    );

    // The store is consistent again (fingerprint re-stamped, not bogus).
    let fp = store.embedding_fingerprint().await.unwrap().unwrap();
    assert_ne!(fp.model, "onnx/bogus-old-model");
}

// -----------------------------------------------------------------------
// memory_compress_candidates
// -----------------------------------------------------------------------

#[tokio::test]
async fn compress_candidates_basic() {
    let (_dir, server) = setup().await;
    let _ = create_and_get_id(&server, "decision", "Candidate", "Content").await;

    let result = server
        .memory_compress_candidates(Parameters(CompressCandidatesInput {
            scope: None,
            threshold: None,
            project: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["candidates"].is_array());
    assert!(val["total"].is_number());
}

// -----------------------------------------------------------------------
// Cross-project: resolve_dir
// -----------------------------------------------------------------------

/// Helper: set up two projects (A = server default, B = cross-project target)
/// with a shared registry that knows about both.
async fn setup_cross_project() -> (TempDir, TempDir, EngramDbServer) {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    let project_id_b = engramdb::storage::project_id::compute_project_id(dir_b.path());

    let registry = InMemoryRegistry::new();
    // Register project B so resolve_dir can find it
    registry.update(dir_b.path(), &project_id_b).await.unwrap();

    let registry: Arc<dyn RegistryBackend> = Arc::new(registry);
    let server = EngramDbServer::new_with_registry(
        dir_a.path().to_path_buf(),
        Some(EmbeddingBackend::Onnx),
        registry,
    );

    // Init project B's store so cross-project opens work
    MemoryStore::init(dir_b.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    (dir_a, dir_b, server)
}

#[tokio::test]
async fn resolve_dir_none_returns_self_dir() {
    let (_dir, server) = setup().await;
    let resolved = server.resolve_dir(None).await.unwrap();
    assert_eq!(resolved, server.dir);
}

#[tokio::test]
async fn resolve_dir_valid_project_id() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_id_b = engramdb::storage::project_id::compute_project_id(dir_b.path());

    let resolved = server.resolve_dir(Some(&project_id_b)).await.unwrap();
    // The registry stores canonicalized paths
    let expected = dir_b.path().canonicalize().unwrap();
    assert_eq!(resolved, expected);
}

#[tokio::test]
async fn resolve_dir_valid_path() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let path_str = dir_b.path().to_string_lossy().to_string();

    let resolved = server.resolve_dir(Some(&path_str)).await.unwrap();
    let expected = dir_b.path().canonicalize().unwrap();
    assert_eq!(resolved, expected);
}

#[tokio::test]
async fn resolve_dir_unregistered_project_id() {
    let (_dir, server) = setup().await;
    let result = server.resolve_dir(Some("abcdef0123456789")).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("PROJECT_NOT_FOUND"), "got: {}", err);
}

#[tokio::test]
async fn resolve_dir_unregistered_path() {
    let (_dir, server) = setup().await;
    let unregistered = TempDir::new().unwrap();
    let path_str = unregistered.path().to_string_lossy().to_string();

    let result = server.resolve_dir(Some(&path_str)).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("PROJECT_NOT_FOUND"), "got: {}", err);
}

#[tokio::test]
async fn resolve_dir_ambiguous_hex_treated_as_id() {
    let (_dir, server) = setup().await;
    // 16-char hex should be treated as project ID, not path
    let result = server.resolve_dir(Some("0123456789abcdef")).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("PROJECT_NOT_FOUND"), "got: {}", err);
}

#[tokio::test]
async fn resolve_dir_relative_path_rejected() {
    let (_dir, server) = setup().await;
    let result = server.resolve_dir(Some("relative/path")).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("VALIDATION_ERROR"), "got: {}", err);
}

#[tokio::test]
async fn resolve_dir_nonexistent_path() {
    let (_dir, server) = setup().await;
    let result = server
        .resolve_dir(Some("/tmp/nonexistent_engramdb_test_dir_12345"))
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("PROJECT_NOT_FOUND"), "got: {}", err);
}

// -----------------------------------------------------------------------
// Cross-project: integration tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn cross_project_create_and_get() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create a memory in project B from server anchored at A
    let mut input = create_input("decision", "Cross-project decision", "Stored in B");
    input.project = Some(project_b.clone());
    let result = server.memory_create(Parameters(input)).await;
    let val = parse_ok(&result);
    let id = val["id"].as_str().unwrap().to_string();
    assert!(val["created"].as_bool().unwrap());

    // Get it back via project override
    let get_result = server
        .memory_get(Parameters(GetInput {
            id: id.clone(),
            project: Some(project_b.clone()),
        }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["summary"], "Cross-project decision");
    assert_eq!(get_val["content"], "Stored in B");

    // Verify it's NOT in project A
    let get_from_a = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    assert!(get_from_a.is_err());
}

#[tokio::test]
async fn cross_project_search() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create memories in project B
    let mut input = create_input(
        "convention",
        "Use snake_case in B",
        "Convention for project B",
    );
    input.project = Some(project_b.clone());
    server.memory_create(Parameters(input)).await.unwrap();

    // Search from server A targeting project B
    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("snake_case".to_string()),
            project: Some(project_b),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["total"].as_u64().unwrap() > 0);
    assert_eq!(val["memories"][0]["summary"], "Use snake_case in B");
}

#[tokio::test]
async fn cross_project_delete() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create in B
    let mut input = create_input("debug", "To delete from B", "Temp content");
    input.project = Some(project_b.clone());
    let result = server.memory_create(Parameters(input)).await;
    let id = parse_ok(&result)["id"].as_str().unwrap().to_string();

    // Delete from B via server A
    let del_result = server
        .memory_delete(Parameters(DeleteInput {
            id: id.clone(),
            project: Some(project_b.clone()),
        }))
        .await;
    let del_val = parse_ok(&del_result);
    assert!(del_val["deleted"].as_bool().unwrap());

    // Confirm gone from B
    let get_result = server
        .memory_get(Parameters(GetInput {
            id,
            project: Some(project_b),
        }))
        .await;
    assert!(get_result.is_err());
}

// -----------------------------------------------------------------------
// Cross-project write gate ([security].allow_cross_project_writes)
// -----------------------------------------------------------------------

/// Write a `[security]` config into a project's `.engramdb/config.toml`.
async fn write_security_config(dir: &std::path::Path, allow_cross_project_writes: bool) {
    let engramdb_dir = dir.join(".engramdb");
    tokio::fs::create_dir_all(&engramdb_dir).await.unwrap();
    let toml = format!("[security]\nallow_cross_project_writes = {allow_cross_project_writes}\n");
    tokio::fs::write(engramdb_dir.join("config.toml"), toml)
        .await
        .unwrap();
}

/// Build an `UpdateInput` with only `id`/`project` set (all else `None`).
fn update_input(id: &str, project: Option<String>) -> UpdateInput {
    UpdateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        clear_validity: None,
        clear_invalidated: None,
        id: id.to_string(),
        type_: None,
        content: Some("changed".to_string()),
        summary: None,
        details: None,
        physical: None,
        logical: None,
        tags: None,
        tags_add: None,
        tags_remove: None,
        criticality: None,
        confidence: None,
        visibility: None,
        title: None,
        status: None,
        supersedes: None,
        decay_strategy: None,
        decay_half_life: None,
        decay_ttl: None,
        decay_floor: None,
        project,
    }
}

/// Default config (gate on): the helper does not block a write to a different
/// registered project — by project id or by path.
#[tokio::test]
async fn cross_project_write_gate_allows_by_default() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b_path = dir_b.path().to_string_lossy().to_string();
    let project_id_b = engramdb::storage::project_id::compute_project_id(dir_b.path());

    // No [security] section => default true => not blocked.
    server
        .check_cross_project_write(Some(&project_b_path))
        .await
        .expect("gate should allow by default (path)");
    server
        .check_cross_project_write(Some(&project_id_b))
        .await
        .expect("gate should allow by default (id)");

    // And a real cross-project create actually succeeds under the default.
    let mut input = create_input("decision", "Allowed by default", "In B");
    input.project = Some(project_b_path);
    let result = server.memory_create(Parameters(input)).await;
    assert!(parse_ok(&result)["created"].as_bool().unwrap());
}

/// Gate off: create/update/delete/challenge targeting a DIFFERENT registered
/// project are all rejected with the VALIDATION_ERROR gate error, before any
/// store/model work.
#[tokio::test]
async fn cross_project_write_gate_rejects_when_disabled() {
    let (dir_a, dir_b, server) = setup_cross_project().await;
    write_security_config(dir_a.path(), false).await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    let assert_blocked = |result: &Result<String, String>| {
        let err = result.as_ref().unwrap_err();
        assert!(err.contains("VALIDATION_ERROR"), "got: {err}");
        assert!(
            err.contains("allow_cross_project_writes"),
            "gate message expected, got: {err}"
        );
    };

    // create
    let mut c = create_input("decision", "Blocked", "In B");
    c.project = Some(project_b.clone());
    assert_blocked(&server.memory_create(Parameters(c)).await);

    // delete (id need not exist — the gate rejects before opening the store)
    let del = server
        .memory_delete(Parameters(DeleteInput {
            id: "deadbeefdeadbeef".to_string(),
            project: Some(project_b.clone()),
        }))
        .await;
    assert_blocked(&del);

    // update
    let upd = server
        .memory_update(Parameters(update_input(
            "deadbeefdeadbeef",
            Some(project_b.clone()),
        )))
        .await;
    assert_blocked(&upd);

    // challenge
    let ch = server
        .memory_challenge(Parameters(ChallengeInput {
            id: "deadbeefdeadbeef".to_string(),
            evidence: "contradiction".to_string(),
            source_file: None,
            project: Some(project_b),
        }))
        .await;
    assert_blocked(&ch);
}

/// Gate off: the session's OWN project (`project = None`) and the shared
/// global store (`project = "global"`) are always allowed.
#[tokio::test]
async fn cross_project_write_gate_allows_own_and_global_when_disabled() {
    let (dir_a, _dir_b, server) = setup_cross_project().await;
    write_security_config(dir_a.path(), false).await;

    server
        .check_cross_project_write(None)
        .await
        .expect("own project (None) must always be allowed");
    server
        .check_cross_project_write(Some("global"))
        .await
        .expect("global store must always be allowed");
}

/// Gate off: targeting the session's OWN project by explicit id/path resolves
/// to the session's own root id and is NOT treated as cross-project. This is
/// the same resolution path a linked worktree of the session's own project
/// takes (its id resolves to the main project's id via
/// `resolve_root_project_id`), so worktree parity is covered here without a
/// real git worktree.
#[tokio::test]
async fn cross_project_write_gate_allows_own_project_by_id_when_disabled() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let project_id_a = engramdb::storage::project_id::compute_project_id(dir_a.path());
    let project_id_b = engramdb::storage::project_id::compute_project_id(dir_b.path());

    let registry = InMemoryRegistry::new();
    registry.update(dir_a.path(), &project_id_a).await.unwrap();
    registry.update(dir_b.path(), &project_id_b).await.unwrap();
    let registry: Arc<dyn RegistryBackend> = Arc::new(registry);

    let server = EngramDbServer::new_with_registry(
        dir_a.path().to_path_buf(),
        Some(EmbeddingBackend::Onnx),
        registry,
    );
    write_security_config(dir_a.path(), false).await;

    // Own project referenced explicitly by id/path: allowed.
    server
        .check_cross_project_write(Some(&project_id_a))
        .await
        .expect("own project by id must be allowed even when gate is off");
    let own_path = dir_a.path().to_string_lossy().to_string();
    server
        .check_cross_project_write(Some(&own_path))
        .await
        .expect("own project by path must be allowed even when gate is off");

    // A different registered project: rejected.
    let result = server.check_cross_project_write(Some(&project_id_b)).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("VALIDATION_ERROR"));
}

#[tokio::test]
async fn cross_project_stats() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create a memory in B
    let mut input = create_input("hazard", "Hazard in B", "Watch out");
    input.project = Some(project_b.clone());
    server.memory_create(Parameters(input)).await.unwrap();

    // Stats for B from server A
    let result = server
        .memory_stats(Parameters(StatsInput {
            project: Some(project_b),
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["total"], 1);
    assert_eq!(val["by_type"]["hazard"], 1);
}

#[tokio::test]
async fn cross_project_uninitialized_store_errors() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    let project_id_b = engramdb::storage::project_id::compute_project_id(dir_b.path());

    let registry = InMemoryRegistry::new();
    registry.update(dir_b.path(), &project_id_b).await.unwrap();

    let registry: Arc<dyn RegistryBackend> = Arc::new(registry);
    let server = EngramDbServer::new_with_registry(
        dir_a.path().to_path_buf(),
        Some(EmbeddingBackend::Onnx),
        registry,
    );

    // Do NOT init project B — it should fail with StoreNotInitialized
    let project_b = dir_b.path().to_string_lossy().to_string();
    let result = server
        .memory_stats(Parameters(StatsInput {
            project: Some(project_b),
            all_projects: None,
        }))
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("STORE_NOT_INITIALIZED"),
        "Expected STORE_NOT_INITIALIZED, got: {}",
        err
    );
}

#[tokio::test]
async fn default_behavior_preserved() {
    let (_dir, server) = setup().await;
    // Create without project override — should work as before
    let id = create_and_get_id(&server, "decision", "Default project", "Content").await;
    let result = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["summary"], "Default project");
}

#[tokio::test]
async fn cross_project_update() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create in B
    let mut input = create_input("decision", "Original summary", "Original content");
    input.project = Some(project_b.clone());
    let result = server.memory_create(Parameters(input)).await;
    let id = parse_ok(&result)["id"].as_str().unwrap().to_string();

    // Update in B from server A
    let update_result = server
        .memory_update(Parameters(UpdateInput {
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: None,
            clear_invalidated: None,
            id: id.clone(),
            summary: Some("Updated summary".to_string()),
            content: Some("Updated content".to_string()),
            type_: None,
            details: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            visibility: None,
            title: None,
            status: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            project: Some(project_b.clone()),
        }))
        .await;
    let update_val = parse_ok(&update_result);
    assert!(update_val["updated"].as_bool().unwrap());

    // Verify update landed in B
    let get_result = server
        .memory_get(Parameters(GetInput {
            id,
            project: Some(project_b),
        }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["summary"], "Updated summary");
    assert_eq!(get_val["content"], "Updated content");
}

#[tokio::test]
async fn cross_project_write_isolation() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create memories in A
    create_and_get_id(&server, "decision", "A memory 1", "Content A1").await;
    create_and_get_id(&server, "convention", "A memory 2", "Content A2").await;

    // Get A's count
    let stats_a_before = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let count_a_before = parse_ok(&stats_a_before)["total"].as_u64().unwrap();
    assert_eq!(count_a_before, 2);

    // Write to B
    let mut input = create_input("hazard", "B hazard", "B content");
    input.project = Some(project_b.clone());
    server.memory_create(Parameters(input)).await.unwrap();

    // Delete from B
    let mut input2 = create_input("debug", "B debug to delete", "B temp");
    input2.project = Some(project_b.clone());
    let result = server.memory_create(Parameters(input2)).await;
    let id_b = parse_ok(&result)["id"].as_str().unwrap().to_string();
    server
        .memory_delete(Parameters(DeleteInput {
            id: id_b,
            project: Some(project_b),
        }))
        .await
        .unwrap();

    // Verify A is completely unaffected
    let stats_a_after = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let count_a_after = parse_ok(&stats_a_after)["total"].as_u64().unwrap();
    assert_eq!(count_a_after, count_a_before);
}

#[tokio::test]
async fn cross_project_via_project_id() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_id_b = engramdb::storage::project_id::compute_project_id(dir_b.path());

    // Create using project ID instead of path
    let mut input = create_input("convention", "Via project ID", "Created by ID");
    input.project = Some(project_id_b.clone());
    let result = server.memory_create(Parameters(input)).await;
    let val = parse_ok(&result);
    let id = val["id"].as_str().unwrap().to_string();
    assert!(val["created"].as_bool().unwrap());

    // Get back via project ID
    let get_result = server
        .memory_get(Parameters(GetInput {
            id: id.clone(),
            project: Some(project_id_b),
        }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["summary"], "Via project ID");

    // Verify not in A
    let get_from_a = server
        .memory_get(Parameters(GetInput { id, project: None }))
        .await;
    assert!(get_from_a.is_err());
}

#[tokio::test]
async fn cross_project_doctor() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create a memory in B so the store has data
    let mut input = create_input("context", "Doctor test", "Health check");
    input.project = Some(project_b.clone());
    server.memory_create(Parameters(input)).await.unwrap();

    // Run doctor on B from server A
    let result = server
        .memory_doctor(Parameters(DoctorInput {
            fix: None,
            project: Some(project_b),
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["healthy"].as_bool().unwrap());
    assert_eq!(val["on_disk"], 1);
}

#[tokio::test]
async fn cross_project_list() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create memories in B
    let mut input1 = create_input("decision", "B decision", "Content");
    input1.project = Some(project_b.clone());
    server.memory_create(Parameters(input1)).await.unwrap();

    let mut input2 = create_input("hazard", "B hazard", "Content");
    input2.project = Some(project_b.clone());
    server.memory_create(Parameters(input2)).await.unwrap();

    // List from A targeting B
    let result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: Some(project_b),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["total"], 2);

    // List A — should have nothing
    let result_a = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: None,
        }))
        .await;
    let val_a = parse_ok(&result_a);
    assert_eq!(val_a["total"], 0);
}

#[tokio::test]
async fn cross_project_challenge_and_review() {
    let (_dir_a, dir_b, server) = setup_cross_project().await;
    let project_b = dir_b.path().to_string_lossy().to_string();

    // Create in B
    let mut input = create_input("decision", "Questionable decision", "Maybe wrong");
    input.project = Some(project_b.clone());
    let result = server.memory_create(Parameters(input)).await;
    let id = parse_ok(&result)["id"].as_str().unwrap().to_string();

    // Challenge from A targeting B
    let challenge_result = server
        .memory_challenge(Parameters(ChallengeInput {
            id: id.clone(),
            evidence: "New evidence contradicts this".to_string(),
            source_file: None,
            project: Some(project_b.clone()),
        }))
        .await;
    let challenge_val = parse_ok(&challenge_result);
    assert!(challenge_val["challenged"].as_bool().unwrap());

    // Review B from A — should show the challenged memory
    let review_result = server
        .memory_review(Parameters(ReviewInput {
            scope: None,
            max_results: None,
            type_: None,
            challenged_only: Some(true),
            stale_only: None,
            stale_after_days: None,
            project: Some(project_b),
        }))
        .await;
    let review_val = parse_ok(&review_result);
    assert_eq!(review_val["total"], 1);
    assert_eq!(review_val["memories"][0]["id"], id);
}

#[tokio::test]
async fn cross_project_create_on_uninitialized_errors() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    let project_id_b = engramdb::storage::project_id::compute_project_id(dir_b.path());
    let registry = InMemoryRegistry::new();
    registry.update(dir_b.path(), &project_id_b).await.unwrap();

    let registry: Arc<dyn RegistryBackend> = Arc::new(registry);
    let server = EngramDbServer::new_with_registry(
        dir_a.path().to_path_buf(),
        Some(EmbeddingBackend::Onnx),
        registry,
    );

    // Try to create in uninitialized B — should fail, NOT auto-init
    let project_b = dir_b.path().to_string_lossy().to_string();
    let mut input = create_input("decision", "Should fail", "No auto-init");
    input.project = Some(project_b);
    let result = server.memory_create(Parameters(input)).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("STORE_NOT_INITIALIZED"),
        "Expected STORE_NOT_INITIALIZED, got: {}",
        err
    );

    // Verify B was NOT auto-initialized
    assert!(!dir_b.path().join(".engramdb").exists());
}

// =======================================================================
// Global memory tests — feature parity with project-scoped memories
// =======================================================================

/// Handle returned by [`setup_global`]. Bundles the per-test `TempDir`
/// with a process-wide lock guard so that `let (_dir, server) = ...`
/// call sites get both test isolation and the expected `TempDir`
/// lifetime without threading extra values through every test body.
///
/// The lock serializes all tests that touch the global store (see
/// [`engramdb::storage::test_support`] for background) and clears the
/// on-disk global layout before each test.
#[allow(dead_code)]
struct GlobalSetupHandle {
    dir: TempDir,
    _lock: engramdb::storage::test_support::GlobalTestLock,
}

impl GlobalSetupHandle {
    #[allow(dead_code)]
    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

/// Setup for global tests.
///
/// Under `cargo test` (one process, parallel tests) the global store's
/// on-disk layout is shared, so we serialize via
/// `acquire_global_test_lock` and wipe the global dir per test. Under
/// `cargo nextest` (one process per test) the lock is effectively
/// free — each test still sees a clean slate.
async fn setup_global() -> (GlobalSetupHandle, EngramDbServer) {
    let lock = engramdb::storage::test_support::acquire_global_test_lock().await;
    MemoryStore::init_global().await.unwrap();
    let (dir, server) = setup().await;
    (GlobalSetupHandle { dir, _lock: lock }, server)
}

fn global_project() -> Option<String> {
    Some("global".to_string())
}

fn create_global_input(type_: &str, summary: &str, content: &str) -> CreateInput {
    CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        project: global_project(),
        ..create_input(type_, summary, content)
    }
}

async fn create_global_and_get_id(
    server: &EngramDbServer,
    type_: &str,
    summary: &str,
    content: &str,
) -> String {
    let result = server
        .memory_create(Parameters(create_global_input(type_, summary, content)))
        .await;
    let val = parse_ok(&result);
    val["id"].as_str().unwrap().to_string()
}

// --- Global CRUD ---

#[tokio::test]
async fn global_create_basic() {
    let (_dir, server) = setup_global().await;
    let result = server
        .memory_create(Parameters(create_global_input(
            "decision",
            "Global preference",
            "I prefer tabs over spaces",
        )))
        .await;
    let val = parse_ok(&result);
    assert!(val["id"].is_string());
    assert_eq!(val["created"], true);
    assert_eq!(val["summary"], "Global preference");
}

#[tokio::test]
async fn global_create_with_all_fields() {
    let (_dir, server) = setup_global().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        type_: "convention".to_string(),
        content: "Always use semantic versioning".to_string(),
        summary: "Use semver".to_string(),
        details: Some("Major.Minor.Patch format".to_string()),
        physical: Some(vec!["**/Cargo.toml".to_string()]),
        logical: Some(vec!["versioning".to_string()]),
        tags: Some(vec!["global".to_string(), "convention".to_string()]),
        criticality: Some(0.9),
        confidence: Some(0.95),
        visibility: Some("shared".to_string()),
        supersedes: Some(vec![]),
        decay_strategy: None,
        decay_half_life: None,
        decay_ttl: None,
        decay_floor: None,
        title: Some("Semver Convention".to_string()),
        title_strategy: None,
        project: global_project(),
    };
    let result = server.memory_create(Parameters(input)).await;
    let val = parse_ok(&result);
    assert!(val["id"].is_string());
    assert_eq!(val["created"], true);
}

#[tokio::test]
async fn global_get() {
    let (_dir, server) = setup_global().await;
    let id = create_global_and_get_id(
        &server,
        "preference",
        "Dark mode pref",
        "Always use dark mode in editors",
    )
    .await;

    let result = server
        .memory_get(Parameters(GetInput {
            id: id.clone(),
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["id"], id);
    assert_eq!(val["summary"], "Dark mode pref");
    assert_eq!(val["content"], "Always use dark mode in editors");
}

#[tokio::test]
async fn global_update() {
    let (_dir, server) = setup_global().await;
    let id = create_global_and_get_id(
        &server,
        "convention",
        "Commit convention",
        "Use conventional commits",
    )
    .await;

    let result = server
        .memory_update(Parameters(UpdateInput {
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: None,
            clear_invalidated: None,
            id: id.clone(),
            type_: None,
            content: Some("Use conventional commits with scope".to_string()),
            summary: Some("Commit convention v2".to_string()),
            details: None,
            physical: None,
            logical: None,
            tags: Some(vec!["git".to_string()]),
            tags_add: None,
            tags_remove: None,
            criticality: Some(0.8),
            confidence: None,
            visibility: None,
            title: None,
            status: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["updated"], true);

    // Verify the update
    let get_result = server
        .memory_get(Parameters(GetInput {
            id,
            project: global_project(),
        }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["summary"], "Commit convention v2");
    assert_eq!(get_val["content"], "Use conventional commits with scope");
    assert!(get_val["tags"].as_array().unwrap().contains(&json!("git")));
}

#[tokio::test]
async fn global_delete() {
    let (_dir, server) = setup_global().await;
    let id = create_global_and_get_id(
        &server,
        "decision",
        "To delete globally",
        "Global content to remove",
    )
    .await;

    let result = server
        .memory_delete(Parameters(DeleteInput {
            id: id.clone(),
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["deleted"], true);

    // Verify it's gone
    let get_result = server
        .memory_get(Parameters(GetInput {
            id,
            project: global_project(),
        }))
        .await;
    let err_val = parse_err(&get_result);
    assert_eq!(err_val["error"]["code"], "MEMORY_NOT_FOUND");
}

// --- Global isolation ---

#[tokio::test]
async fn global_memories_isolated_from_project() {
    let (_dir, server) = setup_global().await;

    // Create in global store
    let global_id = create_global_and_get_id(
        &server,
        "preference",
        "Global only memory",
        "This lives only in global",
    )
    .await;

    // Create in project store
    let project_id = create_and_get_id(
        &server,
        "decision",
        "Project only memory",
        "Project content",
    )
    .await;

    // Global memory NOT visible in project
    let result = server
        .memory_get(Parameters(GetInput {
            id: global_id.clone(),
            project: None,
        }))
        .await;
    assert!(result.is_err());

    // Project memory NOT visible in global
    let result = server
        .memory_get(Parameters(GetInput {
            id: project_id,
            project: global_project(),
        }))
        .await;
    assert!(result.is_err());

    // Global memory IS visible with project="global"
    let result = server
        .memory_get(Parameters(GetInput {
            id: global_id,
            project: global_project(),
        }))
        .await;
    assert!(result.is_ok());
}

// --- Global retrieve & search ---

#[tokio::test]
async fn global_retrieve() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(
        &server,
        "convention",
        "Always lint code",
        "Run linters before committing",
    )
    .await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("/".to_string()),
            project: global_project(),
            ..query_input("rank")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(!val["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn global_retrieve_with_semantic_query() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(
        &server,
        "convention",
        "Error handling convention",
        "Always use Result types for error handling in Rust",
    )
    .await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("error handling".to_string()),
            project: global_project(),
            ..query_input("rank")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(!val["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn global_search() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(
        &server,
        "preference",
        "Editor font preference",
        "Use JetBrains Mono font in all editors",
    )
    .await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("JetBrains Mono".to_string()),
            project: global_project(),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    assert!(!val["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn global_search_with_type_filter() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(
        &server,
        "convention",
        "Convention in global",
        "Convention content",
    )
    .await;
    create_global_and_get_id(&server, "hazard", "Hazard in global", "Hazard content").await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("content".to_string()),
            types: Some(vec!["hazard".to_string()]),
            project: global_project(),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    let memories = val["memories"].as_array().unwrap();
    for m in memories {
        assert_eq!(m["type"], "hazard");
    }
}

// --- include_global merges results ---

#[tokio::test]
async fn include_global_in_retrieve() {
    let (_dir, server) = setup_global().await;

    // Create a global memory
    create_global_and_get_id(
        &server,
        "convention",
        "Global lint convention",
        "Always run clippy before committing Rust code",
    )
    .await;

    // Create a project memory
    create_and_get_id(
        &server,
        "decision",
        "Project-specific decision",
        "Use tokio runtime",
    )
    .await;

    // Retrieve with include_global=true should include both
    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("/".to_string()),
            max_results: Some(20),
            include_global: Some(true),
            ..query_input("rank")
        }))
        .await;
    let val = parse_ok(&result);
    let memories = val["memories"].as_array().unwrap();
    let summaries: Vec<&str> = memories
        .iter()
        .filter_map(|m| m["summary"].as_str())
        .collect();
    assert!(
        summaries.contains(&"Global lint convention"),
        "Expected global memory in results, got: {:?}",
        summaries
    );
    assert!(
        summaries.contains(&"Project-specific decision"),
        "Expected project memory in results, got: {:?}",
        summaries
    );
}

#[tokio::test]
async fn include_global_in_search() {
    let (_dir, server) = setup_global().await;

    create_global_and_get_id(
        &server,
        "preference",
        "Global search test memory",
        "This memory is global for search merge test",
    )
    .await;

    create_and_get_id(
        &server,
        "decision",
        "Project search test memory",
        "This memory is project-scoped for search merge test",
    )
    .await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            query: Some("search test memory".to_string()),
            max_results: Some(20),
            include_global: Some(true),
            ..query_input("filter")
        }))
        .await;
    let val = parse_ok(&result);
    let memories = val["memories"].as_array().unwrap();
    let summaries: Vec<&str> = memories
        .iter()
        .filter_map(|m| m["summary"].as_str())
        .collect();
    assert!(
        summaries.contains(&"Global search test memory"),
        "Expected global memory in search results, got: {:?}",
        summaries
    );
    assert!(
        summaries.contains(&"Project search test memory"),
        "Expected project memory in search results, got: {:?}",
        summaries
    );
}

#[tokio::test]
async fn include_global_false_excludes_global() {
    let (_dir, server) = setup_global().await;

    create_global_and_get_id(
        &server,
        "convention",
        "Global only for exclusion test",
        "Should not appear when include_global=false",
    )
    .await;

    create_and_get_id(
        &server,
        "decision",
        "Project only for exclusion test",
        "Should appear",
    )
    .await;

    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("/".to_string()),
            max_results: Some(20),
            include_global: Some(false),
            ..query_input("rank")
        }))
        .await;
    let val = parse_ok(&result);
    let memories = val["memories"].as_array().unwrap();
    let summaries: Vec<&str> = memories
        .iter()
        .filter_map(|m| m["summary"].as_str())
        .collect();
    assert!(
        !summaries.contains(&"Global only for exclusion test"),
        "Global memory should NOT appear when include_global=false, got: {:?}",
        summaries
    );
}

#[tokio::test]
async fn include_global_default_excludes_global() {
    let (_dir, server) = setup_global().await;

    create_global_and_get_id(
        &server,
        "convention",
        "Global default exclusion test",
        "Should not appear when include_global is omitted",
    )
    .await;

    create_and_get_id(
        &server,
        "decision",
        "Project default exclusion test",
        "Should appear",
    )
    .await;

    // include_global defaults to None (false)
    let result = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            path: Some("/".to_string()),
            max_results: Some(20),
            ..query_input("rank")
        }))
        .await;
    let val = parse_ok(&result);
    let memories = val["memories"].as_array().unwrap();
    let summaries: Vec<&str> = memories
        .iter()
        .filter_map(|m| m["summary"].as_str())
        .collect();
    assert!(
        !summaries.contains(&"Global default exclusion test"),
        "Global memory should NOT appear by default, got: {:?}",
        summaries
    );
}

// --- Global challenge / review / resolve ---

#[tokio::test]
async fn global_challenge_and_review() {
    let (_dir, server) = setup_global().await;
    let id = create_global_and_get_id(
        &server,
        "convention",
        "Challengeable convention",
        "Use semicolons in JS",
    )
    .await;

    // Challenge
    let result = server
        .memory_challenge(Parameters(ChallengeInput {
            id: id.clone(),
            evidence: "Modern JS style guides recommend no semicolons".to_string(),
            source_file: Some(".eslintrc".to_string()),
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["challenged"], true);

    // Review should list challenged memory
    let result = server
        .memory_review(Parameters(ReviewInput {
            scope: None,
            max_results: None,
            type_: None,
            challenged_only: Some(true),
            stale_only: None,
            stale_after_days: None,
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    let memories = val["memories"].as_array().unwrap();
    assert!(
        memories.iter().any(|m| m["id"] == id),
        "Challenged global memory should appear in review"
    );
}

#[tokio::test]
async fn global_resolve_keep() {
    let (_dir, server) = setup_global().await;
    let id = create_global_and_get_id(
        &server,
        "convention",
        "Resolve test",
        "Convention to resolve",
    )
    .await;

    // Challenge first
    server
        .memory_challenge(Parameters(ChallengeInput {
            id: id.clone(),
            evidence: "Might be wrong".to_string(),
            source_file: None,
            project: global_project(),
        }))
        .await
        .unwrap();

    // Resolve by keeping
    let result = server
        .memory_resolve(Parameters(ResolveInput {
            superseded_by: None,
            id: id.clone(),
            action: "keep".to_string(),
            updated_content: None,
            updated_summary: None,
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["resolved"], true);
    assert_eq!(val["action"], "keep");

    // Verify it's back to active
    let get_result = server
        .memory_get(Parameters(GetInput {
            id,
            project: global_project(),
        }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["status"], "active");
}

#[tokio::test]
async fn global_resolve_update() {
    let (_dir, server) = setup_global().await;
    let id = create_global_and_get_id(
        &server,
        "convention",
        "To resolve with update",
        "Original content",
    )
    .await;

    server
        .memory_challenge(Parameters(ChallengeInput {
            id: id.clone(),
            evidence: "Needs update".to_string(),
            source_file: None,
            project: global_project(),
        }))
        .await
        .unwrap();

    let result = server
        .memory_resolve(Parameters(ResolveInput {
            superseded_by: None,
            id: id.clone(),
            action: "update".to_string(),
            updated_content: Some("Updated content after resolve".to_string()),
            updated_summary: Some("Resolved convention".to_string()),
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["resolved"], true);

    let get_val = parse_ok(
        &server
            .memory_get(Parameters(GetInput {
                id,
                project: global_project(),
            }))
            .await,
    );
    assert_eq!(get_val["summary"], "Resolved convention");
    assert_eq!(get_val["content"], "Updated content after resolve");
}

#[tokio::test]
async fn global_resolve_delete() {
    let (_dir, server) = setup_global().await;
    let id = create_global_and_get_id(
        &server,
        "decision",
        "To resolve-delete",
        "Will be removed by resolve",
    )
    .await;

    server
        .memory_challenge(Parameters(ChallengeInput {
            id: id.clone(),
            evidence: "Should be deleted".to_string(),
            source_file: None,
            project: global_project(),
        }))
        .await
        .unwrap();

    let result = server
        .memory_resolve(Parameters(ResolveInput {
            superseded_by: None,
            id: id.clone(),
            action: "delete".to_string(),
            updated_content: None,
            updated_summary: None,
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["resolved"], true);

    // Should be gone
    let get_result = server
        .memory_get(Parameters(GetInput {
            id,
            project: global_project(),
        }))
        .await;
    assert!(get_result.is_err());
}

// --- Global list, stats, doctor, reindex, gc ---

#[tokio::test]
async fn global_list() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(
        &server,
        "decision",
        "Listed global decision",
        "Decision content",
    )
    .await;
    create_global_and_get_id(
        &server,
        "convention",
        "Listed global convention",
        "Convention content",
    )
    .await;

    let result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["total"].as_u64().unwrap() >= 2);
}

#[tokio::test]
async fn global_list_with_type_filter() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(&server, "decision", "G decision", "Decision content").await;
    create_global_and_get_id(&server, "hazard", "G hazard", "Hazard content").await;

    let result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: Some(vec!["hazard".to_string()]),
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    let memories = val["memories"].as_array().unwrap();
    for m in memories {
        assert_eq!(m["type"], "hazard");
    }
}

#[tokio::test]
async fn global_stats() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(&server, "decision", "Stat decision", "Content").await;
    create_global_and_get_id(&server, "hazard", "Stat hazard", "Content").await;

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: global_project(),
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["total"].as_u64().unwrap() >= 2);
    assert!(val["by_type"]["decision"].as_u64().unwrap() >= 1);
    assert!(val["by_type"]["hazard"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn global_doctor() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(&server, "decision", "Doctor test", "Content").await;

    let result = server
        .memory_doctor(Parameters(DoctorInput {
            fix: None,
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["healthy"], true);
    assert!(val["indexed"].as_u64().unwrap() >= 1);
    assert!(val["on_disk"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn global_reindex() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(&server, "decision", "Reindex test", "Content to reindex").await;

    let result = server
        .memory_reindex(Parameters(ReindexInput {
            embeddings_only: None,
            index_only: Some(true),
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["indexed"].as_u64().unwrap() >= 1);

    // Memory should still be accessible after reindex
    let list_result = server
        .memory_list(Parameters(ListInput {
            epistemic: None,
            include_invalidated: None,
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: None,
            reverse: None,
            limit: None,
            project: global_project(),
        }))
        .await;
    let list_val = parse_ok(&list_result);
    assert!(list_val["total"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn global_gc_dry_run() {
    let (_dir, server) = setup_global().await;
    create_global_and_get_id(&server, "decision", "GC test", "Content").await;

    let result = server
        .memory_gc(Parameters(GcInput {
            dry_run: Some(true),
            threshold: None,
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["dry_run"], true);
}

// --- Global compress ---

#[tokio::test]
async fn global_compress_candidates() {
    let (_dir, server) = setup_global().await;

    // Create low-criticality global memories
    let input1 = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        criticality: Some(0.1),
        ..create_global_input("context", "Low crit 1", "Low criticality global context 1")
    };
    server.memory_create(Parameters(input1)).await.unwrap();

    let input2 = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        criticality: Some(0.1),
        ..create_global_input("context", "Low crit 2", "Low criticality global context 2")
    };
    server.memory_create(Parameters(input2)).await.unwrap();

    let result = server
        .memory_compress_candidates(Parameters(CompressCandidatesInput {
            scope: None,
            threshold: Some(0.3),
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["total"].as_u64().unwrap() >= 2);
}

#[tokio::test]
async fn global_compress_apply() {
    let (_dir, server) = setup_global().await;

    let id1 = create_global_and_get_id(
        &server,
        "context",
        "Compress source 1",
        "Global context to compress A",
    )
    .await;
    let id2 = create_global_and_get_id(
        &server,
        "context",
        "Compress source 2",
        "Global context to compress B",
    )
    .await;

    let result = server
        .memory_compress_apply(Parameters(CompressApplyInput {
            source_ids: vec![id1.clone(), id2.clone()],
            summary: "Compressed global ctx".to_string(),
            content: "Combined global context A and B".to_string(),
            scope: None,
            tags: Some(vec!["compressed".to_string()]),
            project: global_project(),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["applied"], true);
    assert_eq!(val["superseded_count"], 2);
    assert!(val["new_id"].is_string());

    // New memory should be accessible
    let new_id = val["new_id"].as_str().unwrap();
    let get_result = server
        .memory_get(Parameters(GetInput {
            id: new_id.to_string(),
            project: global_project(),
        }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["summary"], "Compressed global ctx");
}

// --- Global with all memory types ---

#[tokio::test]
async fn global_all_memory_types() {
    let (_dir, server) = setup_global().await;
    let types = [
        "decision",
        "convention",
        "hazard",
        "context",
        "intent",
        "relationship",
        "debug",
        "preference",
    ];

    for t in &types {
        let result = server
            .memory_create(Parameters(create_global_input(
                t,
                &format!("Global {} memory", t),
                &format!("Content for global {}", t),
            )))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["created"], true, "Failed to create global {} memory", t);
    }

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: global_project(),
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);
    assert!(val["total"].as_u64().unwrap() >= types.len() as u64);
}

// --- Global with personal visibility ---

#[tokio::test]
async fn global_personal_visibility() {
    let (_dir, server) = setup_global().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        visibility: Some("personal".to_string()),
        ..create_global_input(
            "preference",
            "Personal global pref",
            "My personal preference stored globally",
        )
    };
    let result = server.memory_create(Parameters(input)).await;
    let val = parse_ok(&result);
    let id = val["id"].as_str().unwrap();

    let get_result = server
        .memory_get(Parameters(GetInput {
            id: id.to_string(),
            project: global_project(),
        }))
        .await;
    let get_val = parse_ok(&get_result);
    assert_eq!(get_val["summary"], "Personal global pref");
}

// --- Global retrieve detail levels ---

#[tokio::test]
async fn global_retrieve_detail_levels() {
    let (_dir, server) = setup_global().await;
    let input = CreateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        details: Some("Extended details for this global memory".to_string()),
        ..create_global_input(
            "decision",
            "Detail level test",
            "Content for detail level test",
        )
    };
    server.memory_create(Parameters(input)).await.unwrap();

    for level in &["summary", "content", "full"] {
        let result = server
            .memory_query(Parameters(QueryInput {
                epistemic: None,
                situation: None,
                include_invalidated: None,
                path: Some("/".to_string()),
                detail_level: Some(level.to_string()),
                project: global_project(),
                ..query_input("rank")
            }))
            .await;
        let val = parse_ok(&result);
        assert!(
            !val["memories"].as_array().unwrap().is_empty(),
            "detail_level={} returned no memories",
            level
        );
    }
}

// --- Global auto-init ---

#[tokio::test]
async fn global_auto_initializes() {
    // Unlike non-default project stores, global should auto-init
    let (_dir, server) = setup_global().await;

    // This should succeed even if global store wasn't explicitly initialized
    let result = server
        .memory_create(Parameters(create_global_input(
            "decision",
            "Auto-init test",
            "Testing auto-initialization",
        )))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["created"], true);
}

// =========================================================================
// Worktree / project hierarchy tests
// =========================================================================

/// Build a fake linked-worktree layout under `root`:
///
///   <root>/main/.git/                 (main .git dir)
///   <root>/main/.git/worktrees/wt/    (per-worktree gitdir)
///   <root>/wt/.git                    (file: `gitdir: <abs path>`)
///
/// Returns (canonicalized main path, canonicalized worktree path).
fn make_fake_worktree_mcp(root: &std::path::Path) -> (PathBuf, PathBuf) {
    let main = root.join("main");
    let wt = root.join("wt");
    let wt_gitdir = main.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(main.join(".git")).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::create_dir_all(&wt_gitdir).unwrap();
    std::fs::write(wt_gitdir.join("commondir"), "../..").unwrap();
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", wt_gitdir.display()),
    )
    .unwrap();
    (main.canonicalize().unwrap(), wt.canonicalize().unwrap())
}

fn new_server_at(dir: &std::path::Path, registry: Arc<dyn RegistryBackend>) -> EngramDbServer {
    EngramDbServer::new_with_registry(dir.to_path_buf(), Some(EmbeddingBackend::Onnx), registry)
}

#[tokio::test]
async fn effective_dir_in_worktree_resolves_to_main() {
    let tmp = TempDir::new().unwrap();
    let (main, wt) = make_fake_worktree_mcp(tmp.path());
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(&wt, registry);
    assert_eq!(server.dir, wt);
    assert_eq!(server.effective_dir, main);
}

#[tokio::test]
async fn effective_dir_for_non_worktree_equals_dir() {
    let tmp = TempDir::new().unwrap();
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(tmp.path(), registry);
    assert_eq!(server.dir, server.effective_dir);
}

#[tokio::test]
async fn ensure_hierarchy_noop_for_non_worktree() {
    let tmp = TempDir::new().unwrap();
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(tmp.path(), registry.clone());
    server
        .ensure_hierarchy()
        .await
        .expect("ensure_hierarchy should succeed");
    // No registration happens in a non-worktree; store is only init'd on
    // the first actual memory operation.
    let loaded = registry.load().await.unwrap();
    assert!(loaded.projects.is_empty());
    assert!(!tmp.path().join(".engramdb").exists());
}

#[tokio::test]
async fn ensure_hierarchy_auto_inits_main_and_registers_worktree() {
    let tmp = TempDir::new().unwrap();
    let (main, wt) = make_fake_worktree_mcp(tmp.path());
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(&wt, registry.clone());

    server.ensure_hierarchy().await.unwrap();

    // Main project got initialized.
    assert!(main.join(".engramdb").exists());
    // Worktree did NOT get its own .engramdb/.
    assert!(!wt.join(".engramdb").exists());

    // Registry contains both with the child's parent set to the main id.
    let reg = registry.load().await.unwrap();
    let main_id = engramdb::storage::project_id::compute_project_id(&main);
    let wt_id = engramdb::storage::project_id::compute_project_id(&wt);
    let main_entry = reg
        .projects
        .iter()
        .find(|e| e.project_id == main_id)
        .expect("main project registered");
    assert_eq!(main_entry.parent_project_id, None);
    let wt_entry = reg
        .projects
        .iter()
        .find(|e| e.project_id == wt_id)
        .expect("worktree registered");
    assert_eq!(
        wt_entry.parent_project_id.as_deref(),
        Some(main_id.as_str())
    );
}

#[tokio::test]
async fn ensure_hierarchy_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let (main, wt) = make_fake_worktree_mcp(tmp.path());
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(&wt, registry.clone());

    server.ensure_hierarchy().await.unwrap();
    server.ensure_hierarchy().await.unwrap();
    server.ensure_hierarchy().await.unwrap();

    // Still exactly two entries after repeated calls.
    let reg = registry.load().await.unwrap();
    assert_eq!(reg.projects.len(), 2);
    let wt_id = engramdb::storage::project_id::compute_project_id(&wt);
    let main_id = engramdb::storage::project_id::compute_project_id(&main);
    let wt_entry = reg.projects.iter().find(|e| e.project_id == wt_id).unwrap();
    assert_eq!(
        wt_entry.parent_project_id.as_deref(),
        Some(main_id.as_str())
    );
}

#[tokio::test]
async fn ensure_hierarchy_skips_init_when_main_already_initialized() {
    let tmp = TempDir::new().unwrap();
    let (main, wt) = make_fake_worktree_mcp(tmp.path());
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());

    // Pre-initialize the main project.
    MemoryStore::init(&main, registry.as_ref()).await.unwrap();

    let server = new_server_at(&wt, registry.clone());
    server.ensure_hierarchy().await.unwrap();

    // Main still exists; worktree registered with parent link.
    assert!(main.join(".engramdb").exists());
    let reg = registry.load().await.unwrap();
    assert_eq!(reg.projects.len(), 2);
    let wt_id = engramdb::storage::project_id::compute_project_id(&wt);
    let main_id = engramdb::storage::project_id::compute_project_id(&main);
    let wt_entry = reg.projects.iter().find(|e| e.project_id == wt_id).unwrap();
    assert_eq!(
        wt_entry.parent_project_id.as_deref(),
        Some(main_id.as_str())
    );
}

#[tokio::test]
async fn resolve_dir_none_in_worktree_returns_main_path() {
    let tmp = TempDir::new().unwrap();
    let (main, wt) = make_fake_worktree_mcp(tmp.path());
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(&wt, registry.clone());
    server.ensure_hierarchy().await.unwrap();

    let resolved = server.resolve_dir(None).await.unwrap();
    assert_eq!(resolved, main);
}

#[tokio::test]
async fn resolve_dir_with_worktree_path_swaps_to_main() {
    let tmp = TempDir::new().unwrap();
    let (main, wt) = make_fake_worktree_mcp(tmp.path());
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(&wt, registry.clone());
    server.ensure_hierarchy().await.unwrap();

    let wt_str = wt.to_string_lossy().to_string();
    let resolved = server.resolve_dir(Some(&wt_str)).await.unwrap();
    assert_eq!(resolved, main);
}

#[tokio::test]
async fn resolve_dir_with_worktree_id_follows_parent_chain() {
    let tmp = TempDir::new().unwrap();
    let (main, wt) = make_fake_worktree_mcp(tmp.path());
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(&wt, registry.clone());
    server.ensure_hierarchy().await.unwrap();

    let wt_id = engramdb::storage::project_id::compute_project_id(&wt);
    let resolved = server.resolve_dir(Some(&wt_id)).await.unwrap();
    assert_eq!(resolved, main);
}

#[tokio::test]
async fn memory_create_in_worktree_writes_to_main_store() {
    let tmp = TempDir::new().unwrap();
    let (main, wt) = make_fake_worktree_mcp(tmp.path());
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(&wt, registry.clone());
    server.ensure_hierarchy().await.unwrap();

    // Create a memory via the MCP handler while running in the worktree.
    let res = server
        .memory_create(Parameters(create_input(
            "decision",
            "From worktree",
            "Created while running in linked worktree",
        )))
        .await;
    let val = parse_ok(&res);
    assert_eq!(val["created"], true);

    // The memory should live under the MAIN project's store, not the worktree.
    let main_store = MemoryStore::open(&main).await.unwrap();
    let summaries = main_store.list_summary().await.unwrap();
    assert_eq!(summaries.len(), 1, "memory should be in main project");
    let mem = main_store.get(&summaries[0].id).await.unwrap();
    assert_eq!(mem.summary, "From worktree");

    // The worktree still has no .engramdb/.
    assert!(!wt.join(".engramdb").exists());
}

// -----------------------------------------------------------------------
// projects_list / projects_info / projects_link / projects_unlink
// -----------------------------------------------------------------------

async fn setup_two_registered_projects() -> (TempDir, TempDir, EngramDbServer, String, String) {
    let parent_tmp = TempDir::new().unwrap();
    let child_tmp = TempDir::new().unwrap();
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());

    let parent_store = MemoryStore::init(parent_tmp.path(), registry.as_ref())
        .await
        .unwrap();
    let child_store = MemoryStore::init(child_tmp.path(), registry.as_ref())
        .await
        .unwrap();
    let parent_id = parent_store.project_id.clone();
    let child_id = child_store.project_id.clone();

    let server = EngramDbServer::new_with_registry(
        parent_tmp.path().to_path_buf(),
        Some(EmbeddingBackend::Onnx),
        Arc::clone(&registry),
    );
    (parent_tmp, child_tmp, server, parent_id, child_id)
}

#[tokio::test]
async fn projects_list_shows_registered_projects() {
    let (_p, _c, server, parent_id, child_id) = setup_two_registered_projects().await;
    let result = server.projects_list().await;
    let val = parse_ok(&result);
    let arr = val.as_array().unwrap();
    let ids: Vec<String> = arr
        .iter()
        .map(|e| e["project_id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&parent_id), "parent must be in list");
    assert!(ids.contains(&child_id), "child must be in list");
}

#[tokio::test]
async fn projects_info_current_project() {
    let (_p, _c, server, parent_id, _child_id) = setup_two_registered_projects().await;
    let result = server
        .projects_info(Parameters(ProjectsInfoInput { project: None }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["project_id"], parent_id);
    assert!(val["memory_count"].is_number());
}

#[tokio::test]
async fn projects_info_by_id() {
    let (_p, _c, server, _parent_id, child_id) = setup_two_registered_projects().await;
    let result = server
        .projects_info(Parameters(ProjectsInfoInput {
            project: Some(child_id.clone()),
        }))
        .await;
    let val = parse_ok(&result);
    assert_eq!(val["project_id"], child_id);
}

#[tokio::test]
async fn projects_link_then_unlink_roundtrip() {
    let (_p, _c, server, parent_id, child_id) = setup_two_registered_projects().await;

    let link_res = server
        .projects_link(Parameters(ProjectsLinkInput {
            child: child_id.clone(),
            parent: parent_id.clone(),
        }))
        .await;
    let val = parse_ok(&link_res);
    assert_eq!(val["linked"], true);

    // projects_list should now reflect the parent_project_id on the child.
    let list = parse_ok(&server.projects_list().await);
    let child_entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["project_id"] == child_id)
        .unwrap();
    assert_eq!(child_entry["parent_project_id"], parent_id);

    let unlink_res = server
        .projects_unlink(Parameters(ProjectsUnlinkInput {
            project_id: child_id.clone(),
        }))
        .await;
    let val = parse_ok(&unlink_res);
    assert_eq!(val["unlinked"], true);

    let list = parse_ok(&server.projects_list().await);
    let child_entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["project_id"] == child_id)
        .unwrap();
    assert!(child_entry.get("parent_project_id").is_none());
}

#[tokio::test]
async fn projects_link_rejects_self() {
    let (_p, _c, server, parent_id, _child_id) = setup_two_registered_projects().await;
    let result = server
        .projects_link(Parameters(ProjectsLinkInput {
            child: parent_id.clone(),
            parent: parent_id,
        }))
        .await;
    let err = parse_err(&result);
    assert_eq!(err["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn projects_link_rejects_cycle() {
    let (_p, _c, server, parent_id, child_id) = setup_two_registered_projects().await;

    // child → parent
    server
        .projects_link(Parameters(ProjectsLinkInput {
            child: child_id.clone(),
            parent: parent_id.clone(),
        }))
        .await
        .unwrap();

    // Reversing would create a cycle.
    let result = server
        .projects_link(Parameters(ProjectsLinkInput {
            child: parent_id,
            parent: child_id,
        }))
        .await;
    let err = parse_err(&result);
    assert_eq!(err["error"]["code"], "VALIDATION_ERROR");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("cycle"));
}

// -----------------------------------------------------------------------
// Runtime telemetry overlay on `stats`
// -----------------------------------------------------------------------

#[tokio::test]
async fn stats_includes_runtime_fields_after_calls() {
    let (_dir, server) = setup().await;

    // Drive a few tool calls. Two creates and one query that should
    // succeed against the memories we just inserted (filter mode picks
    // up keyword matches).
    let _id1 = create_and_get_id(&server, "decision", "Snake case", "We use snake_case").await;
    let _id2 = create_and_get_id(&server, "convention", "Tabs", "We use tabs").await;

    let _ = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            mode: "filter".to_string(),
            query: Some("snake_case".to_string()),
            ..query_input("filter")
        }))
        .await;

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);

    // Existing static fields are still there.
    assert!(val.get("total").is_some(), "static `total` preserved");
    assert!(val.get("by_type").is_some(), "static `by_type` preserved");
    assert!(
        val.get("avg_criticality").is_some(),
        "static `avg_criticality` preserved"
    );

    // New runtime fields are present.
    assert!(val.get("since").is_some(), "runtime `since` added");
    assert!(
        val.get("project_id").is_some(),
        "runtime `project_id` added"
    );
    let usage = &val["usage"];
    assert_eq!(usage["by_tool"]["create"], 2);
    // The stats call itself is counted (in-flight, but the scope hasn't
    // dropped yet inside this handler — so just assert >= 1 query).
    assert!(usage["by_tool"]["query"].as_u64().unwrap() >= 1);
    let queries = &val["queries"];
    assert!(queries["total"].as_u64().unwrap() >= 1);
    assert!(queries["hit_rate"].as_f64().unwrap() >= 0.0);
    assert!(
        val["timings_ms"]["tool"]["create"]["count"]
            .as_u64()
            .unwrap()
            >= 2
    );

    // by_project map is omitted unless requested.
    assert!(
        val.get("by_project").is_none(),
        "by_project absent unless all_projects=true"
    );
}

#[tokio::test]
async fn stats_all_projects_returns_breakdown() {
    let (_dir, server) = setup().await;
    let _id = create_and_get_id(&server, "decision", "X", "We chose X").await;

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: Some(true),
        }))
        .await;
    let val = parse_ok(&result);
    assert!(
        val.get("by_project").is_some(),
        "by_project present with all_projects=true"
    );
    assert!(
        !val["by_project"].as_object().unwrap().is_empty(),
        "at least one project recorded"
    );
}

#[tokio::test]
async fn stats_records_zero_results_and_quality() {
    let (_dir, server) = setup().await;

    // Issue a query with nothing in the store; embeddings unavailable in
    // tests without ONNX setup, so this should land in the
    // `no_query_signals` quality bucket and count as zero-result.
    let _ = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            mode: "rank".to_string(),
            query: Some("nonexistent gobbledygook".to_string()),
            ..query_input("rank")
        }))
        .await;

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);
    let queries = &val["queries"];
    assert!(queries["total"].as_u64().unwrap() >= 1);
    // zero_results == total when there were no hits in the empty store
    assert!(queries["zero_results"].as_u64().unwrap() >= 1);
    assert!((queries["hit_rate"].as_f64().unwrap()) <= 1.0);
}

#[tokio::test]
async fn stats_reports_session_id_and_unique_sessions() {
    let (_dir, server) = setup().await;
    // Drive a couple of calls so the session is seen by telemetry.
    let _ = create_and_get_id(&server, "decision", "Test", "Session test").await;
    let _ = server
        .memory_query(Parameters(QueryInput {
            epistemic: None,
            situation: None,
            include_invalidated: None,
            mode: "rank".to_string(),
            query: Some("anything".to_string()),
            ..query_input("rank")
        }))
        .await;

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);

    // Server.session_id() must be non-empty and exactly one unique
    // session must be observed for the project.
    assert!(!server.session_id().is_empty());
    assert_eq!(val["usage"]["unique_sessions"], 1);
}

#[tokio::test]
async fn stats_reports_followups_for_same_session_queries() {
    let (_dir, server) = setup().await;
    // Three back-to-back queries from the same server (=same session_id).
    // First one is not a followup; the other two arrive within the
    // 60s default followup window → followups == 2.
    for _ in 0..3 {
        let _ = server
            .memory_query(Parameters(QueryInput {
                epistemic: None,
                situation: None,
                include_invalidated: None,
                mode: "rank".to_string(),
                query: Some("hello".to_string()),
                ..query_input("rank")
            }))
            .await;
    }

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);
    let q = &val["queries"];
    assert_eq!(q["total"].as_u64().unwrap(), 3);
    assert_eq!(q["followups"].as_u64().unwrap(), 2);
    assert!(q["followup_rate"].as_f64().unwrap() > 0.6);
}

/// Test gap covered: a tool call that returns Err must show up under
/// `errors_by_tool` (the RAII guard records on Drop without
/// `mark_success`).
#[tokio::test]
async fn stats_records_tool_errors() {
    let (_dir, server) = setup().await;
    // `memory_get` against a non-existent ID returns MemoryNotFound.
    let _ = server
        .memory_get(Parameters(GetInput {
            id: "this-id-does-not-exist".to_string(),
            project: None,
        }))
        .await;

    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: None,
        }))
        .await;
    let val = parse_ok(&result);
    let errors = &val["usage"]["errors_by_tool"];
    assert_eq!(
        errors["get"].as_u64().unwrap_or(0),
        1,
        "errored memory_get must appear in errors_by_tool"
    );
}

/// Test gap covered: registry-level tools (`projects_list/link/unlink`)
/// bucket under `__system__`, not the launching project.
#[tokio::test]
async fn stats_buckets_projects_list_under_system() {
    let (_dir, server) = setup().await;
    let _ = server.projects_list().await;

    // Snapshot with all_projects to see the by_project breakdown.
    let result = server
        .memory_stats(Parameters(StatsInput {
            project: None,
            all_projects: Some(true),
        }))
        .await;
    let val = parse_ok(&result);
    let bp = &val["by_project"];
    assert!(
        bp["__system__"].is_object(),
        "projects_list bucketed under __system__: got {}",
        bp
    );
    assert_eq!(
        bp["__system__"]["usage"]["by_tool"]["projects_list"]
            .as_u64()
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn get_info_instructions_includes_reflection_nudge() {
    let tmp = TempDir::new().unwrap();
    let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
    let server = new_server_at(tmp.path(), registry);
    let info = server.get_info();
    let instructions = info.instructions.expect("server exposes instructions");
    assert!(
        instructions.contains("When you finish"),
        "instructions should nudge end-of-task reflection, got: {instructions}"
    );
    assert!(instructions.contains("reflect"));
    // The MCP-aware variant should explicitly push the MCP tools
    // ("challenge" only appears in the reflection nudge, not the base
    // instructions), unlike the MCP-agnostic SessionStart hook copy.
    assert!(
        instructions.contains("challenge"),
        "MCP instructions nudge should reference MCP tools, got: {instructions}"
    );
}

// =================================================================
// Prompt and resource transport coverage.
//
// `get_prompt` (CRAP 182, was 0% covered) and `read_resource` (CRAP
// 90, was 0% covered) live behind `ServerHandler` trait impls — they
// can't be called directly from a test because their `_context`
// parameter is `RequestContext<RoleServer>` and the `Peer` inside
// requires a real transport. The standard rmcp pattern (used in the
// upstream `tests/test_message_protocol.rs`) is to wire a duplex
// transport in process: spawn the server on one end, drive a tiny
// `()` client on the other, and round-trip the request.
// =================================================================

async fn duplex_serve(
    server: EngramDbServer,
) -> (
    rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server_handle: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
        let svc = server.serve(server_io).await?;
        svc.waiting().await?;
        Ok(())
    });
    let client = rmcp::serve_client((), client_io)
        .await
        .expect("client must hand-shake against the in-process server");
    (client, server_handle)
}

/// `get_prompt("memory-session-start")` against an empty store →
/// fallback "No relevant memories found." branch (server.rs:2342)
/// + the standard prompt template.
#[tokio::test]
async fn get_prompt_session_start_empty_store_returns_fallback() {
    let (_dir, server) = setup().await;
    let (client, server_handle) = duplex_serve(server).await;

    let result = client
        .peer()
        .get_prompt(GetPromptRequestParams::new("memory-session-start"))
        .await
        .expect("get_prompt must succeed");

    assert_eq!(
        result.description.as_deref(),
        Some("Session start briefing")
    );
    assert_eq!(result.messages.len(), 1);
    let text = match &result.messages[0].content {
        PromptMessageContent::Text { text } => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    assert!(text.contains("EngramDB"), "missing EngramDB header: {text}");
    assert!(
        text.contains("No relevant memories found."),
        "empty-store fallback missing: {text}"
    );

    // Clean shutdown so the join handle resolves.
    client.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
}

/// `get_prompt("memory-session-end")` against an empty store → the
/// `compute_stats` Ok branch with zero memories and zero
/// review_count.
#[tokio::test]
async fn get_prompt_session_end_reports_zero_memory_store() {
    let (_dir, server) = setup().await;
    let (client, server_handle) = duplex_serve(server).await;

    let result = client
        .peer()
        .get_prompt(GetPromptRequestParams::new("memory-session-end"))
        .await
        .expect("get_prompt must succeed");

    assert_eq!(result.description.as_deref(), Some("Session end review"));
    let text = match &result.messages[0].content {
        PromptMessageContent::Text { text } => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    assert!(
        text.contains("Current store has 0 memories"),
        "stats line missing: {text}"
    );
    assert!(
        text.contains("create"),
        "session-end template body missing: {text}"
    );
    assert!(
        text.contains("origin_task") && text.contains("generality: task"),
        "task-scoped guidance line (spec \u{a7}16.2) missing: {text}"
    );

    client.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
}

/// Legacy/pre-epistemic categorization: memories get a type-derived class
/// but no enrichment metadata. The session-end prompt must surface the gap
/// count so agents enrich as they touch memories (and stay silent when
/// nothing gaps).
#[tokio::test]
async fn get_prompt_session_end_surfaces_enrichment_gaps() {
    let (_dir, server) = setup().await;

    // No memories: no gap line.
    let (client, server_handle) = duplex_serve(server).await;
    let result = client
        .peer()
        .get_prompt(GetPromptRequestParams::new("memory-session-end"))
        .await
        .expect("get_prompt must succeed");
    let text = match &result.messages[0].content {
        PromptMessageContent::Text { text } => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    assert!(
        !text.contains("lack a recorded premise"),
        "empty store must not nag: {text}"
    );
    client.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;

    // A legacy-shaped decision (class defaulted, no premise) triggers it.
    let (_dir2, server2) = setup().await;
    let _ = create_and_get_id(
        &server2,
        "decision",
        "Legacy decision",
        "no premise recorded",
    )
    .await;
    let (client2, server_handle2) = duplex_serve(server2).await;
    let result = client2
        .peer()
        .get_prompt(GetPromptRequestParams::new("memory-session-end"))
        .await
        .expect("get_prompt must succeed");
    let text = match &result.messages[0].content {
        PromptMessageContent::Text { text } => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    assert!(
        text.contains("1 decision(s) lack a recorded premise"),
        "gap line missing: {text}"
    );
    client2.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle2).await;
}

/// Unknown prompt name → `invalid_params` error branch
/// (server.rs:2397-2401).
#[tokio::test]
async fn get_prompt_with_unknown_name_returns_error() {
    let (_dir, server) = setup().await;
    let (client, server_handle) = duplex_serve(server).await;

    let err = client
        .peer()
        .get_prompt(GetPromptRequestParams::new("this-prompt-does-not-exist"))
        .await
        .expect_err("unknown prompt must error");
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("unknown")
            || msg.to_lowercase().contains("not found")
            || msg.to_lowercase().contains("invalid"),
        "unexpected error message: {msg}"
    );

    client.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
}

/// `read_resource("memory://index")` → list_filterable branch.
/// Empty store → empty JSON array in the contents text.
#[tokio::test]
async fn read_resource_memory_index_returns_serialized_filterables() {
    let (_dir, server) = setup().await;
    let (client, server_handle) = duplex_serve(server).await;

    let result = client
        .peer()
        .read_resource(ReadResourceRequestParams::new("memory://index"))
        .await
        .expect("read_resource must succeed");

    assert_eq!(result.contents.len(), 1);
    let text = match &result.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("expected text contents, got {other:?}"),
    };
    let parsed: serde_json::Value =
        serde_json::from_str(&text).expect("contents must be JSON array");
    assert!(parsed.is_array(), "expected array, got {parsed}");
    assert_eq!(parsed.as_array().unwrap().len(), 0);

    client.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
}

/// `read_resource("memory://context/<path>")` → build_engine + query
/// branch (server.rs:2227-2263). Empty store → empty memories array.
#[tokio::test]
async fn read_resource_memory_context_returns_serialized_query_result() {
    let (_dir, server) = setup().await;
    let (client, server_handle) = duplex_serve(server).await;

    let result = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(
            "memory://context/src/lib.rs",
        ))
        .await
        .expect("read_resource must succeed");

    let text = match &result.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("expected text contents, got {other:?}"),
    };
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(parsed.is_array(), "expected array, got {parsed}");

    client.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
}

/// Unknown URI → `invalid_params` error branch (server.rs:2264-2268).
#[tokio::test]
async fn read_resource_with_unknown_uri_returns_error() {
    let (_dir, server) = setup().await;
    let (client, server_handle) = duplex_serve(server).await;

    let err = client
        .peer()
        .read_resource(ReadResourceRequestParams::new("memory://nope/whatever"))
        .await
        .expect_err("unknown URI must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("Unknown resource URI") || msg.to_lowercase().contains("invalid"),
        "unexpected error message: {msg}"
    );

    client.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
}

/// Sanity check: list_prompts returns the two prompts the server
/// claims to support (server.rs:2273-2302). This also exercises the
/// list_prompts handler which had no direct test.
#[tokio::test]
async fn list_prompts_returns_both_session_prompts() {
    let (_dir, server) = setup().await;
    let (client, server_handle) = duplex_serve(server).await;

    let result = client
        .peer()
        .list_prompts(None)
        .await
        .expect("list_prompts must succeed");
    let names: std::collections::HashSet<String> =
        result.prompts.iter().map(|p| p.name.clone()).collect();
    assert!(names.contains("memory-session-start"));
    assert!(names.contains("memory-session-end"));

    client.cancel().await.ok();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
}

// -----------------------------------------------------------------------
// Epistemic surfaces (I5a): create/update fields, query filters, verify,
// resolve invalidate, output tagging
// -----------------------------------------------------------------------

#[tokio::test]
async fn create_with_epistemic_fields_roundtrips_in_get() {
    let (_dir, server) = setup().await;
    let mut input = create_input("hazard", "Off-diagonal hazard", "content");
    input.epistemic = Some("observation".to_string());
    input.premise = Some("while ort is pinned".to_string());
    input.invalidated_by = Some(vec!["Cargo.lock".to_string()]);
    input.origin_task = Some("epistemic-memory".to_string());
    input.generality = Some("task".to_string());
    let id = parse_ok(&server.memory_create(Parameters(input)).await)["id"]
        .as_str()
        .unwrap()
        .to_string();

    let got = parse_ok(
        &server
            .memory_get(Parameters(GetInput {
                id: id.clone(),
                project: None,
            }))
            .await,
    );
    assert_eq!(got["epistemic"], "observation");
    assert_eq!(got["valid_while"]["premise"], "while ort is pinned");
    assert_eq!(got["valid_while"]["invalidated_by"][0], "Cargo.lock");
    assert_eq!(got["valid_while"]["origin_task"], "epistemic-memory");
    assert_eq!(got["valid_while"]["generality"], "task");
    assert!(
        got.get("invalidated_at").is_none(),
        "live memory has no window end"
    );

    // Diagonal create: epistemic still always present in output (§5.4).
    let id2 = create_and_get_id(&server, "decision", "Diagonal decision", "c").await;
    let got2 = parse_ok(
        &server
            .memory_get(Parameters(GetInput {
                id: id2,
                project: None,
            }))
            .await,
    );
    assert_eq!(got2["epistemic"], "decision");
    assert!(got2.get("valid_while").is_none());
}

#[tokio::test]
async fn create_rejects_invalid_epistemic_inputs() {
    let (_dir, server) = setup().await;
    let mut input = create_input("decision", "Bad class", "c");
    input.epistemic = Some("vibes".to_string());
    let err = parse_err(&server.memory_create(Parameters(input)).await);
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("invalid epistemic class"));

    let mut input = create_input("decision", "Bad ts", "c");
    input.valid_from = Some("not-a-date".to_string());
    let err = parse_err(&server.memory_create(Parameters(input)).await);
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("invalid valid_from"));
}

#[tokio::test]
async fn query_epistemic_filter_and_breakdown_expose_situation() {
    let (_dir, server) = setup().await;
    create_and_get_id(&server, "debug", "An observation memory", "c").await;
    create_and_get_id(&server, "hazard", "A fact memory", "c").await;

    let mut q = query_input("rank");
    q.epistemic = Some(vec!["observation".to_string()]);
    let result = parse_ok(&server.memory_query(Parameters(q)).await);
    let memories = result["memories"].as_array().unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0]["epistemic"], "observation");

    // Situation is threaded and observable in the breakdown (§7.1).
    let mut q = query_input("rank");
    q.situation = Some("debugging".to_string());
    let result = parse_ok(&server.memory_query(Parameters(q)).await);
    for m in result["memories"].as_array().unwrap() {
        let mult = m["score_breakdown"]["situation_multiplier"]
            .as_f64()
            .unwrap();
        assert!(mult > 0.0 && mult <= 1.0);
        // debugging × observation = 1.0; debugging × fact = 0.6+0.4*0.6=0.84
        if m["epistemic"] == "observation" {
            assert!((mult - 1.0).abs() < 1e-9);
        }
    }

    // Invalid situation rejected.
    let mut q = query_input("rank");
    q.situation = Some("panicking".to_string());
    let err = parse_err(&server.memory_query(Parameters(q)).await);
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("invalid situation"));
}

#[tokio::test]
async fn resolve_invalidate_closes_window_and_query_excludes_by_default() {
    let (_dir, server) = setup().await;
    let old_id = create_and_get_id(&server, "decision", "Old decision", "c").await;
    let new_id = create_and_get_id(&server, "decision", "New decision", "c").await;

    let result = parse_ok(
        &server
            .memory_resolve(Parameters(ResolveInput {
                superseded_by: Some(new_id.clone()),
                id: old_id.clone(),
                action: "invalidate".to_string(),
                updated_content: None,
                updated_summary: None,
                project: None,
            }))
            .await,
    );
    assert_eq!(result["action"], "invalidate");

    // get shows the closed window + reverse link.
    let got = parse_ok(
        &server
            .memory_get(Parameters(GetInput {
                id: old_id.clone(),
                project: None,
            }))
            .await,
    );
    assert!(got.get("invalidated_at").is_some());
    assert_eq!(got["superseded_by"], new_id);

    // Default query excludes it; include_invalidated brings it back.
    let result = parse_ok(&server.memory_query(Parameters(query_input("rank"))).await);
    let ids: Vec<&str> = result["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&old_id.as_str()));
    assert!(ids.contains(&new_id.as_str()));

    let mut q = query_input("rank");
    q.include_invalidated = Some(true);
    let result = parse_ok(&server.memory_query(Parameters(q)).await);
    let ids: Vec<&str> = result["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&old_id.as_str()));

    // List mirrors the default exclusion + opt-in, tagging invalidated_at.
    let result = parse_ok(
        &server
            .memory_list(Parameters(ListInput {
                epistemic: None,
                include_invalidated: None,
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: None,
            }))
            .await,
    );
    assert_eq!(result["memories"].as_array().unwrap().len(), 1);
    let result = parse_ok(
        &server
            .memory_list(Parameters(ListInput {
                epistemic: None,
                include_invalidated: Some(true),
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: None,
            }))
            .await,
    );
    let listed = result["memories"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed
        .iter()
        .any(|m| m["id"] == old_id.as_str() && m.get("invalidated_at").is_some()));
}

#[tokio::test]
async fn verify_tool_stamps_and_update_reopens() {
    let (_dir, server) = setup().await;
    let id = create_and_get_id(&server, "hazard", "Verifiable fact", "c").await;

    let result = parse_ok(
        &server
            .memory_verify(Parameters(VerifyInput {
                id: id.clone(),
                project: None,
            }))
            .await,
    );
    assert_eq!(result["verified"], true);
    assert_eq!(result["review_cleared"], false);

    // Invalidate then reopen via update's clear_invalidated.
    parse_ok(
        &server
            .memory_resolve(Parameters(ResolveInput {
                superseded_by: None,
                id: id.clone(),
                action: "invalidate".to_string(),
                updated_content: None,
                updated_summary: None,
                project: None,
            }))
            .await,
    );
    let mut update = UpdateInput {
        epistemic: None,
        premise: None,
        invalidated_by: None,
        origin_task: None,
        generality: None,
        valid_from: None,
        clear_validity: None,
        clear_invalidated: Some(true),
        id: id.clone(),
        type_: None,
        content: None,
        summary: None,
        details: None,
        physical: None,
        logical: None,
        tags: None,
        tags_add: None,
        tags_remove: None,
        criticality: None,
        confidence: None,
        visibility: None,
        title: None,
        status: None,
        supersedes: None,
        decay_strategy: None,
        decay_half_life: None,
        decay_ttl: None,
        decay_floor: None,
        project: None,
    };
    update.clear_invalidated = Some(true);
    parse_ok(&server.memory_update(Parameters(update)).await);

    let got = parse_ok(
        &server
            .memory_get(Parameters(GetInput {
                id: id.clone(),
                project: None,
            }))
            .await,
    );
    assert!(got.get("invalidated_at").is_none(), "window reopened");
    assert!(got.get("superseded_by").is_none());
}

// -----------------------------------------------------------------------
// Task tools (I5b)
// -----------------------------------------------------------------------

#[tokio::test]
async fn task_current_and_complete_roundtrip() {
    let (_dir, server) = setup().await;
    // Initialize the store first (task state lives under .engramdb/).
    create_and_get_id(&server, "context", "Bootstrap memory", "c").await;

    // Read with no declaration.
    let result = parse_ok(
        &server
            .memory_task_current(Parameters(TaskCurrentInput {
                task: None,
                project: None,
            }))
            .await,
    );
    assert!(result["task"].is_null());

    // Declare, then read back.
    let result = parse_ok(
        &server
            .memory_task_current(Parameters(TaskCurrentInput {
                task: Some("feat-x".to_string()),
                project: None,
            }))
            .await,
    );
    assert_eq!(result["task"], "feat-x");

    // Create a task-scoped memory for that task and complete it.
    let mut input = create_input("decision", "Task-bound decision", "c");
    input.origin_task = Some("feat-x".to_string());
    input.generality = Some("task".to_string());
    let id = parse_ok(&server.memory_create(Parameters(input)).await)["id"]
        .as_str()
        .unwrap()
        .to_string();

    let result = parse_ok(
        &server
            .memory_task_complete(Parameters(TaskCompleteInput {
                task: "feat-x".to_string(),
                project: None,
            }))
            .await,
    );
    assert_eq!(result["demoted"][0], id);

    // Empty task name is rejected.
    let err = parse_err(
        &server
            .memory_task_complete(Parameters(TaskCompleteInput {
                task: "  ".to_string(),
                project: None,
            }))
            .await,
    );
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("must not be empty"));
}
