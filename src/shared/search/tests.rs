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
fn test_before_filter_excludes_later_docs() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();

    let session_id = "ffffffff-1111-2222-3333-444444444444";
    let old_ts = "2025-01-01T00:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    let new_ts = "2026-07-01T12:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();

    let new_content = "common ".repeat(20);
    let entries = vec![
        EntryBuilder::new("old-uuid", session_id)
            .content("common old doc")
            .timestamp(old_ts)
            .build(),
        EntryBuilder::new("new-uuid", session_id)
            .content(new_content.trim())
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
    let before = "2026-06-01T00:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    let results = engine
        .search(SearchQuery {
            text: "common".to_string(),
            limit: 1,
            before: Some(before),
            ..Default::default()
        })
        .unwrap();

    let uuids: Vec<_> = results
        .iter()
        .map(|r| {
            r.uuid
                .as_str()
        })
        .collect();
    assert_eq!(uuids, ["old-uuid"]);
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

/// Rewrites the format version in every file footer, as a newer tantivy would have written it.
fn stamp_future_format(index_path: &std::path::Path) -> usize {
    let current = format!(
        r#""index_format_version":{}"#,
        tantivy::INDEX_FORMAT_VERSION
    );
    let future = format!(
        r#""index_format_version":{}"#,
        tantivy::INDEX_FORMAT_VERSION + 1
    );
    assert_eq!(current.len(), future.len(), "footer length must not change");
    let mut stamped = 0;
    for file in std::fs::read_dir(index_path).unwrap() {
        let path = file
            .unwrap()
            .path();
        let mut bytes = std::fs::read(&path).unwrap();
        let Some(at) = bytes
            .windows(current.len())
            .rposition(|w| w == current.as_bytes())
        else {
            continue;
        };
        bytes[at..at + future.len()].copy_from_slice(future.as_bytes());
        std::fs::write(&path, bytes).unwrap();
        stamped += 1;
    }
    stamped
}

#[test]
fn incompatible_index_error_names_the_remedy() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path();
    let mut indexer = SearchIndexer::new(index_path).unwrap();
    indexer
        .index_conversations(
            vec![
                EntryBuilder::new("uuid-0", "session-1")
                    .content("hello")
                    .build(),
            ],
            "test-source.jsonl",
        )
        .unwrap();
    indexer
        .commit()
        .unwrap();
    drop(indexer);
    assert!(stamp_future_format(index_path) > 0);

    let err = match SearchEngine::new(index_path, HashMap::new()) {
        Ok(_) => panic!("a future index format must not open"),
        Err(e) => crate::shared::explain_index_error(e, index_path),
    };
    let message = format!("{err:#}");
    assert!(
        message.starts_with(&format!(
            "the search index at {} is in a format",
            index_path.display()
        )),
        "{message}"
    );
    assert!(message.contains("cache clear"), "{message}");
    assert!(message.contains("Index version unsupported"), "{message}");
}

#[test]
fn other_errors_pass_through_unexplained() {
    let err = crate::shared::explain_index_error(
        anyhow::anyhow!("disk full"),
        std::path::Path::new("/nowhere"),
    );
    assert_eq!(format!("{err:#}"), "disk full");
}

/// A user/assistant line as Claude Code writes it.
fn jsonl_line(session: &str, uuid: &str, kind: &str, minute: usize, text: &str) -> String {
    serde_json::json!({
        "uuid": uuid,
        "sessionId": session,
        "type": kind,
        "timestamp": format!("2026-03-01T10:{:02}:00Z", minute % 60),
        "cwd": "/home/user/GIT/proj",
        "message": {"role": kind, "content": text},
    })
    .to_string()
}

/// Writes `lines` to `path`, parses it with the real parser and indexes it
/// under its own path, as update_incremental does.
fn index_transcript(indexer: &mut SearchIndexer, path: &Path, lines: &[String]) {
    std::fs::create_dir_all(
        path.parent()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(path, lines.join("\n")).unwrap();
    let entries = crate::shared::parsers::JsonlParser::default()
        .parse_file(path)
        .unwrap();
    assert_eq!(entries.len(), lines.len(), "every line must parse");
    indexer
        .index_conversations(entries, &path.to_string_lossy())
        .unwrap();
}

fn transcript(session: &str, label: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|i| {
            let kind = if i % 2 == 0 { "user" } else { "assistant" };
            jsonl_line(
                session,
                &format!("{label}-{i:04}"),
                kind,
                i,
                &format!("{label} message {i}"),
            )
        })
        .collect()
}

/// A main transcript and one subagent transcript sharing a sessionId, both
/// numbered from sequence 0, with `needle` in message 5 of each.
fn main_and_subagent(tmp: &TempDir) -> (PathBuf, PathBuf, &'static str) {
    let session = "a0a0a0a0-1111-2222-3333-444444444444";
    let project = tmp
        .path()
        .join("projects")
        .join("-home-user-GIT-proj");
    let main = project.join(format!("{session}.jsonl"));
    let agent = project
        .join(session)
        .join("subagents")
        .join("agent-abc123.jsonl");
    let index_path = tmp
        .path()
        .join("index");
    let mut indexer = SearchIndexer::new(&index_path).unwrap();
    let mut main_lines = transcript(session, "main", 10);
    main_lines[5] = jsonl_line(session, "main-0005", "assistant", 5, "main needlemain");
    let mut agent_lines = transcript(session, "agent", 7);
    agent_lines[5] = jsonl_line(session, "agent-0005", "assistant", 5, "agent needleagent");
    index_transcript(&mut indexer, &main, &main_lines);
    index_transcript(&mut indexer, &agent, &agent_lines);
    indexer
        .commit()
        .unwrap();
    (index_path, main, session)
}

fn context_uuids(r: &SearchResultWithContext) -> Vec<&str> {
    r.context_messages
        .iter()
        .map(|m| {
            m.uuid
                .as_str()
        })
        .collect()
}

#[test]
fn context_stays_inside_the_hit_transcript() {
    let tmp = TempDir::new().unwrap();
    let (index_path, _, _) = main_and_subagent(&tmp);
    let engine = SearchEngine::new(&index_path, HashMap::new()).unwrap();

    let query = |text: &str| SearchQuery {
        text: text.to_string(),
        limit: 10,
        ..Default::default()
    };
    let hits = engine
        .search_with_context(query("needlemain"), 2, 2)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        context_uuids(&hits[0]),
        [
            "main-0003",
            "main-0004",
            "main-0005",
            "main-0006",
            "main-0007"
        ]
    );
    assert_eq!(hits[0].match_index, 2);
    assert_eq!(hits[0].total_session_messages, 10, "main transcript only");

    let hits = engine
        .search_with_context(query("needleagent"), 2, 2)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        context_uuids(&hits[0]),
        ["agent-0003", "agent-0004", "agent-0005", "agent-0006"]
    );
    assert_eq!(hits[0].match_index, 2);
    assert_eq!(
        hits[0].total_session_messages, 10,
        "a subagent hit counts its main transcript"
    );
}

#[test]
fn session_messages_list_main_transcript_first_in_file_order() {
    let tmp = TempDir::new().unwrap();
    let (index_path, _, session) = main_and_subagent(&tmp);
    let engine = SearchEngine::new(&index_path, HashMap::new()).unwrap();

    let messages = engine
        .get_session_messages(session)
        .unwrap();
    let uuids: Vec<&str> = messages
        .iter()
        .map(|m| {
            m.uuid
                .as_str()
        })
        .collect();
    let expected: Vec<String> = (0..10)
        .map(|i| format!("main-{i:04}"))
        .chain((0..7).map(|i| format!("agent-{i:04}")))
        .collect();
    assert_eq!(uuids, expected);
}

#[test]
fn session_messages_are_not_capped() {
    let tmp = TempDir::new().unwrap();
    let session = "b0b0b0b0-1111-2222-3333-444444444444";
    let index_path = tmp
        .path()
        .join("index");
    let mut indexer = SearchIndexer::new(&index_path).unwrap();
    index_transcript(
        &mut indexer,
        &tmp.path()
            .join("p")
            .join(format!("{session}.jsonl")),
        &transcript(session, "m", 6000),
    );
    indexer
        .commit()
        .unwrap();
    let engine = SearchEngine::new(&index_path, HashMap::new()).unwrap();

    let messages = engine
        .get_session_messages(session)
        .unwrap();
    assert_eq!(messages.len(), 6000);
    assert!(
        messages
            .windows(2)
            .all(|w| w[0].sequence_num + 1 == w[1].sequence_num)
    );
}
