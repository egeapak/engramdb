//! Integration tests for the title feature.
//!
//! Tests the full PR checklist items:
//! - Backward compat: existing files without titles load correctly
//! - Explicit title creates slug filename
//! - Auto-generation creates keyword-based title
//! - title_strategy: none produces uuid-only filename
//! - Title update changes filename on disk

#[cfg(test)]
mod title_integration {
    use engramdb::ops::{create_memory, CreateParams};
    use engramdb::storage::{InMemoryRegistry, MemoryStore};
    use engramdb::title::TitleStrategy;
    use engramdb::types::{MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    async fn setup() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        (temp_dir, store)
    }

    fn test_params() -> CreateParams {
        CreateParams {
            type_: MemoryType::Decision,
            content: "Use PostgreSQL for the database backend instead of SQLite".to_string(),
            summary: "Database backend choice".to_string(),
            title: None,
            physical: vec!["/".to_string()],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            confidence: 0.8,
            details: None,
            visibility: Visibility::Shared,
            provenance: Provenance::human(),
            supersedes: vec![],
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            title_strategy: TitleStrategy::None,
            embed_async: false,
        }
    }

    // -----------------------------------------------------------------------
    // 1. Backward compatibility: existing files without titles load correctly
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_backward_compat_old_format_files_load() {
        let (temp_dir, store) = setup().await;

        // Create a memory with no title and title_strategy: None (old-style uuid-only filename)
        let mut params = test_params();
        params.title = None;
        params.title_strategy = TitleStrategy::None;

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();

        // Verify the file on disk has uuid-only format
        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        let entries: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], format!("{}.md", id));

        // Verify it loads back correctly
        let loaded = store.get(&id).await.unwrap();
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.title, None);
        assert_eq!(loaded.summary, "Database backend choice");
    }

    #[tokio::test]
    async fn test_backward_compat_prefix_match_old_format() {
        let (_temp_dir, store) = setup().await;

        let mut params = test_params();
        params.title = None;
        params.title_strategy = TitleStrategy::None;

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();
        let prefix = &id[..8];

        // Prefix match should work for old-format files
        let loaded = store.get(prefix).await.unwrap();
        assert_eq!(loaded.id, id);
    }

    // -----------------------------------------------------------------------
    // 2. Explicit title creates slug filename
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_explicit_title_creates_slug_filename() {
        let (temp_dir, store) = setup().await;

        let mut params = test_params();
        params.title = Some("My Database Title".to_string());
        params.title_strategy = TitleStrategy::None; // shouldn't matter, explicit title takes precedence

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();

        // Check filename on disk
        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        let entries: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        assert_eq!(entries.len(), 1);
        let filename = &entries[0];
        assert!(
            filename.starts_with("my-database-title_"),
            "Expected slug prefix, got: {}",
            filename
        );
        assert!(
            filename.contains(&id),
            "Filename should contain the UUID: {}",
            filename
        );
        assert!(filename.ends_with(".md"));

        // Verify title is preserved in memory
        let loaded = store.get(&id).await.unwrap();
        assert_eq!(loaded.title, Some("My Database Title".to_string()));
    }

    #[tokio::test]
    async fn test_explicit_title_prefix_match_with_slug() {
        let (_temp_dir, store) = setup().await;

        let mut params = test_params();
        params.title = Some("Prefix Match Test".to_string());

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();
        let prefix = &id[..8];

        // Prefix match should work even with slug-based filenames
        let loaded = store.get(prefix).await.unwrap();
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.title, Some("Prefix Match Test".to_string()));
    }

    // -----------------------------------------------------------------------
    // 3. Auto-generation creates keyword-based title
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_auto_generation_keyword_strategy() {
        let (temp_dir, store) = setup().await;

        let mut params = test_params();
        params.title = None; // No explicit title
        params.title_strategy = TitleStrategy::Keyword; // Auto-generate via keywords
        params.summary = "Database backend choice for production".to_string();

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();

        let loaded = store.get(&id).await.unwrap();

        // Title should have been auto-generated
        assert!(
            loaded.title.is_some(),
            "Keyword strategy should auto-generate a title"
        );

        let title = loaded.title.unwrap();
        assert!(
            !title.is_empty(),
            "Auto-generated title should not be empty"
        );
        assert!(
            title.split_whitespace().count() <= 4,
            "Auto-generated title should be a few words, got: '{}'",
            title
        );

        // Filename should contain slug
        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        let entries: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        assert_eq!(entries.len(), 1);
        let filename = &entries[0];
        assert!(
            filename.contains('_'),
            "Auto-generated title should produce slug_uuid.md filename: {}",
            filename
        );
    }

    #[tokio::test]
    async fn test_explicit_title_overrides_auto_generation() {
        let (_temp_dir, store) = setup().await;

        let mut params = test_params();
        params.title = Some("Explicit Title".to_string());
        params.title_strategy = TitleStrategy::Keyword; // Would auto-gen, but explicit takes priority

        let result = create_memory(&store, params, None).await.unwrap();
        let loaded = store.get(&result.id).await.unwrap();

        assert_eq!(
            loaded.title,
            Some("Explicit Title".to_string()),
            "Explicit title should not be overridden by auto-generation"
        );
    }

    // -----------------------------------------------------------------------
    // 4. title_strategy: none produces uuid-only filename
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_title_strategy_none_produces_uuid_only_filename() {
        let (temp_dir, store) = setup().await;

        let mut params = test_params();
        params.title = None;
        params.title_strategy = TitleStrategy::None;

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();

        // Verify uuid-only filename
        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        let entries: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0],
            format!("{}.md", id),
            "title_strategy: None should produce uuid-only filename"
        );

        // Verify no title on the memory
        let loaded = store.get(&id).await.unwrap();
        assert_eq!(loaded.title, None);
    }

    // -----------------------------------------------------------------------
    // 5. Title update changes filename on disk
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_title_update_changes_filename() {
        let (temp_dir, store) = setup().await;

        // Create with no title
        let mut params = test_params();
        params.title = None;
        params.title_strategy = TitleStrategy::None;

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();

        let memories_dir = temp_dir.path().join(".engramdb").join("memories");

        // Initial filename should be uuid-only
        let initial_entries: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(initial_entries[0], format!("{}.md", id));

        // Update with a title
        let update_params = engramdb::ops::UpdateParams {
            type_: None,
            summary: None,
            content: None,
            title: Some("New Title".to_string()),
            details: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            status: None,
            visibility: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            embed_async: false,
        };

        engramdb::ops::update_memory(&store, &id, update_params, None)
            .await
            .unwrap();

        // Filename should now have slug
        let updated_entries: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        assert_eq!(updated_entries.len(), 1);
        let new_filename = &updated_entries[0];
        assert!(
            new_filename.starts_with("new-title_"),
            "Updated filename should have slug prefix, got: {}",
            new_filename
        );
        assert!(new_filename.contains(&id));

        // Verify memory loads correctly with the new filename
        let loaded = store.get(&id).await.unwrap();
        assert_eq!(loaded.title, Some("New Title".to_string()));
    }

    #[tokio::test]
    async fn test_title_update_from_one_title_to_another_changes_filename() {
        let (temp_dir, store) = setup().await;

        // Create with initial title
        let mut params = test_params();
        params.title = Some("First Title".to_string());

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();

        let memories_dir = temp_dir.path().join(".engramdb").join("memories");

        // Verify initial filename
        let initial: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(initial[0].starts_with("first-title_"));

        // Update to different title
        let update_params = engramdb::ops::UpdateParams {
            type_: None,
            summary: None,
            content: None,
            title: Some("Second Title".to_string()),
            details: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            status: None,
            visibility: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            embed_async: false,
        };

        engramdb::ops::update_memory(&store, &id, update_params, None)
            .await
            .unwrap();

        // Filename should change to new slug
        let updated: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        assert_eq!(updated.len(), 1, "Should still be exactly one file");
        assert!(
            updated[0].starts_with("second-title_"),
            "Filename should reflect updated title, got: {}",
            updated[0]
        );

        // Verify memory loads
        let loaded = store.get(&id).await.unwrap();
        assert_eq!(loaded.title, Some("Second Title".to_string()));
    }

    // -----------------------------------------------------------------------
    // Title in markdown body (H1 heading)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_title_appears_in_markdown_body() {
        let (temp_dir, store) = setup().await;

        let mut params = test_params();
        params.title = Some("My Important Decision".to_string());

        let result = create_memory(&store, params, None).await.unwrap();
        let id = result.id.clone();

        // Read the raw file content
        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        let entries: Vec<_> = std::fs::read_dir(&memories_dir)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();

        let file_content = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(
            file_content.contains("# My Important Decision"),
            "File should contain H1 title heading"
        );
        assert!(
            file_content.contains("## Content"),
            "File should still have ## Content section"
        );

        // Verify roundtrip
        let loaded = store.get(&id).await.unwrap();
        assert_eq!(loaded.title, Some("My Important Decision".to_string()));
    }
}
