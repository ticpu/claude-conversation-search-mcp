use super::*;
use crate::shared::indexer::SearchIndexer;
use crate::shared::models::{EntryBuilder, MessageType};
use chrono::Utc;
use tempfile::TempDir;

#[test]
fn test_get_session_messages_returns_all_indexed() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    // Create 100 messages for a session
    let session_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let entries: Vec<_> = (0..100)
        .map(|i| {
            let msg_type = if i % 2 == 0 {
                MessageType::User
            } else {
                MessageType::Assistant
            };
            EntryBuilder::new(&format!("uuid-{:04}", i), session_id)
                .message_type(msg_type)
                .content(&format!("Message {}", i))
                .sequence_num(i)
                .build()
        })
        .collect();

    // Index them
    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(entries, "test-source.jsonl")
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);

    // Retrieve with SearchEngine
    let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();
    let messages = engine
        .get_session_messages(session_id)
        .unwrap();

    assert_eq!(
        messages.len(),
        100,
        "Should retrieve all 100 indexed messages"
    );
}

#[test]
fn test_get_session_messages_with_short_id() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    let session_id = "12345678-abcd-efgh-ijkl-mnopqrstuvwx";
    let entries = vec![
        EntryBuilder::new("uuid-1", session_id)
            .content("Hello")
            .build(),
        EntryBuilder::new("uuid-2", session_id)
            .message_type(MessageType::Assistant)
            .content("Hi there")
            .sequence_num(1)
            .build(),
    ];

    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(entries, "test-source.jsonl")
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);

    let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

    // Test with short ID (first 8 chars)
    let messages = engine
        .get_session_messages("12345678")
        .unwrap();
    assert_eq!(
        messages.len(),
        2,
        "Should find messages with short session ID"
    );
}

#[test]
fn test_project_filter_with_full_path() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    let session_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let entries = vec![
        EntryBuilder::new("uuid-1", session_id)
            .content("hello world")
            .project("my-cool-project", "/home/user/GIT/my-cool-project")
            .build(),
        EntryBuilder::new("uuid-2", session_id)
            .message_type(MessageType::Assistant)
            .content("hi there")
            .sequence_num(1)
            .project("my-cool-project", "/home/user/GIT/my-cool-project")
            .build(),
        EntryBuilder::new("uuid-3", session_id)
            .content("other stuff")
            .sequence_num(2)
            .project("other-project", "/home/user/GIT/other-project")
            .build(),
    ];

    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(entries, "test-source.jsonl")
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);

    let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

    // Filter by full path (how users pass --project)
    let results = engine
        .search(SearchQuery {
            text: "hello".to_string(),
            limit: 10,
            project_filter: Some("/home/user/GIT/my-cool-project".to_string()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        results.len(),
        1,
        "Should find 1 result with full path project filter"
    );
    assert_eq!(results[0].uuid, "uuid-1");

    // Filter by short project name
    let results = engine
        .search(SearchQuery {
            text: "hello".to_string(),
            limit: 10,
            project_filter: Some("my-cool-project".to_string()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        results.len(),
        1,
        "Should find 1 result with short project name filter"
    );

    // Filter should exclude non-matching projects
    let results = engine
        .search(SearchQuery {
            text: "hello".to_string(),
            limit: 10,
            project_filter: Some("other-project".to_string()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(results.len(), 0, "Should find 0 results for wrong project");
}

#[test]
fn test_session_filter_with_full_uuid() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    let session_a = "aaaaaaaa-1111-2222-3333-444444444444";
    let session_b = "bbbbbbbb-5555-6666-7777-888888888888";
    let entries = vec![
        EntryBuilder::new("uuid-1", session_a)
            .content("hello world")
            .build(),
        EntryBuilder::new("uuid-2", session_b)
            .content("hello world")
            .build(),
    ];

    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(entries, "test-source.jsonl")
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);

    let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

    // Full session ID
    let results = engine
        .search(SearchQuery {
            text: "hello".to_string(),
            limit: 10,
            session_filter: Some(session_a.to_string()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(results.len(), 1, "Should find 1 result for session A");
    assert_eq!(results[0].uuid, "uuid-1");

    // Short prefix
    let results = engine
        .search(SearchQuery {
            text: "hello".to_string(),
            limit: 10,
            session_filter: Some("aaaaaaaa".to_string()),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        results.len(),
        1,
        "Should find 1 result with short session prefix"
    );
    assert_eq!(results[0].uuid, "uuid-1");
}

#[test]
fn test_get_session_messages_by_prefix() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    let session_id = "aabbccdd-1122-3344-5566-778899001122";
    let entries = vec![
        EntryBuilder::new("uuid-1", session_id)
            .content("first")
            .build(),
        EntryBuilder::new("uuid-2", session_id)
            .message_type(MessageType::Assistant)
            .content("second")
            .sequence_num(1)
            .build(),
        EntryBuilder::new("uuid-3", session_id)
            .content("third")
            .sequence_num(2)
            .build(),
    ];

    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(entries, "test-source.jsonl")
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);

    let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

    // Full ID
    let messages = engine
        .get_session_messages(session_id)
        .unwrap();
    assert_eq!(messages.len(), 3);

    // Short prefix
    let messages = engine
        .get_session_messages("aabbccdd")
        .unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Should find all messages with short prefix"
    );

    // Non-matching prefix
    let messages = engine
        .get_session_messages("xxxxxxxx")
        .unwrap();
    assert_eq!(
        messages.len(),
        0,
        "Should find no messages for wrong prefix"
    );
}

#[test]
fn test_displayable_count_matches_retrieval() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    let session_id = "testtest-1234-5678-abcd-ef0123456789";
    let entries = vec![
        EntryBuilder::new("uuid-1", session_id)
            .content("User message")
            .build(),
        EntryBuilder::new("uuid-2", session_id)
            .message_type(MessageType::Assistant)
            .content("Assistant message")
            .sequence_num(1)
            .build(),
        EntryBuilder::new("uuid-3", session_id)
            .message_type(MessageType::System)
            .content("System message")
            .sequence_num(2)
            .build(),
        EntryBuilder::new("uuid-4", session_id)
            .message_type(MessageType::Summary)
            .content("Summary")
            .sequence_num(3)
            .build(),
        EntryBuilder::new("uuid-5", session_id)
            .content("Warmup")
            .sequence_num(4)
            .build(),
    ];

    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(entries, "test-source.jsonl")
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);

    let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();
    let messages = engine
        .get_session_messages(session_id)
        .unwrap();

    // Count displayable
    let displayable_count = messages
        .iter()
        .filter(|m| m.is_displayable())
        .count();
    // User, Assistant, Summary are displayable; System is not; "Warmup" content filtered
    assert_eq!(
        displayable_count, 3,
        "Should have 3 displayable messages (User, Assistant, Summary)"
    );
}

#[test]
fn test_date_filter_pushed_into_query() {
    // Old doc has high term frequency for "common"; new doc has one occurrence.
    // Without the date filter in the query, top-1 by BM25 fetches only the old doc
    // and the post-filter drops it, returning nothing.
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    let session_id = "dddddddd-1111-2222-3333-444444444444";
    let old_ts = "2025-01-01T00:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    let new_ts = "2026-07-01T12:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();

    let old_content = "common ".repeat(20);
    let entries = vec![
        EntryBuilder::new("old-uuid", session_id)
            .content(old_content.trim())
            .timestamp(old_ts)
            .build(),
        EntryBuilder::new("new-uuid", session_id)
            .content("common recent doc")
            .sequence_num(1)
            .timestamp(new_ts)
            .build(),
    ];

    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(entries, "test-source.jsonl")
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);

    let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

    // After filter set to 2026-06-01 — old doc must be excluded from the corpus.
    let after = "2026-06-01T00:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    let results = engine
        .search(SearchQuery {
            text: "common".to_string(),
            limit: 1,
            after: Some(after),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(
        results.len(),
        1,
        "Date filter in query must surface the recent doc even with limit 1; got: {:?}",
        results
            .iter()
            .map(|r| (&r.uuid, r.timestamp))
            .collect::<Vec<_>>()
    );
    assert_eq!(results[0].uuid, "new-uuid");
}

#[test]
fn test_date_sort_in_collector() {
    // Without collector-level sort, DateDesc limit=1 could return the high-BM25 old doc.
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    let session_id = "eeeeeeee-5555-6666-7777-888888888888";
    let old_ts = "2025-03-01T00:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    let new_ts = "2026-07-10T08:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();

    let old_content = "sort ".repeat(15);
    let entries = vec![
        EntryBuilder::new("old-sort", session_id)
            .content(old_content.trim())
            .timestamp(old_ts)
            .build(),
        EntryBuilder::new("new-sort", session_id)
            .content("sort newer")
            .sequence_num(1)
            .timestamp(new_ts)
            .build(),
    ];

    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(entries, "test-source.jsonl")
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);

    let engine = SearchEngine::new(index_path, HashMap::new()).unwrap();

    let results = engine
        .search(SearchQuery {
            text: "sort".to_string(),
            limit: 1,
            sort_by: SortOrder::DateDesc,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(results.len(), 1, "Should return exactly 1 result");
    assert_eq!(
        results[0].uuid, "new-sort",
        "DateDesc limit=1 must return the newest doc, not the highest-BM25 one"
    );
}
