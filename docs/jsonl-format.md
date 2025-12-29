# Claude Code JSONL Format Specification

Analysis of Claude Code conversation storage format based on examination of 1,301 JSONL files (69,442 messages).

## File Types & Naming Patterns

### Session Files (UUID-based)
- Format: `{uuid}.jsonl` (UUID v4, 36 chars)
- Example: `42d186d0-3fe9-46e6-a861-87fb7a034236.jsonl`
- Primary conversation logs
- Size range: 5 lines to 1.7MB (avg 53 messages per file)

### Agent/Sidechain Files (Hash-based)
- Format: `agent-{hash}.jsonl` (8 char hex hash)
- Example: `agent-de30ae5d.jsonl`
- Separate transcripts for spawned agents/tasks
- All messages have `isSidechain: true` and `agentId: {hash}`
- Reference parent session via `sessionId`
- NOT merged with parent session files

## Message Types (type field)

| Type | Purpose | Indexable |
|------|---------|-----------|
| `user` | User input messages | Yes |
| `assistant` | Claude responses | Yes |
| `summary` | Session summaries with `leafUuid` anchor | Yes (metadata) |
| `file-history-snapshot` | File state tracking | No (empty metadata) |
| `queue-operation` | Internal queue events | No (administrative) |

## Message Structure

### User Message
```json
{
  "parentUuid": null | "uuid",
  "isSidechain": false,
  "userType": "external",
  "cwd": "/path/to/cwd",
  "sessionId": "uuid",
  "version": "2.0.xx",
  "gitBranch": "",
  "type": "user",
  "message": {
    "role": "user",
    "content": "The actual user input text"
  },
  "uuid": "unique-id",
  "timestamp": "2025-12-03T18:27:04.359Z",
  "thinkingMetadata": {
    "level": "high",
    "disabled": false,
    "triggers": []
  },
  "todos": []
}
```

### Assistant Message
```json
{
  "parentUuid": "uuid-of-parent",
  "isSidechain": false,
  "type": "assistant",
  "message": {
    "model": "claude-opus-4-5-20251101",
    "id": "msg_xxxxx",
    "type": "message",
    "role": "assistant",
    "content": [ /* Content blocks array */ ],
    "usage": { /* token metrics */ }
  },
  "uuid": "unique-id",
  "timestamp": "ISO-8601"
}
```

### Summary Message
```json
{
  "type": "summary",
  "summary": "Human-readable conversation title",
  "leafUuid": "uuid-of-last-message"
}
```

### File History Snapshot
```json
{
  "type": "file-history-snapshot",
  "messageId": "uuid",
  "snapshot": {
    "messageId": "same-uuid",
    "trackedFileBackups": {},
    "timestamp": "ISO-8601"
  },
  "isSnapshotUpdate": false
}
```

## Content Block Types (.message.content[])

| Type | Structure | Avg Size | Signal/Noise |
|------|-----------|----------|--------------|
| `text` | `{type:"text", text:"string"}` | variable | SIGNAL |
| `thinking` | `{type:"thinking", thinking:"text", signature:"bytes"}` | 225 chars | SIGNAL (reasoning) |
| `tool_use` | `{type:"tool_use", id:"str", name:"str", input:{...}}` | variable | SIGNAL (name only) |
| `tool_result` | `{type:"tool_result", content:"string", is_error:bool}` | 1KB-3.8MB | NOISE (file dumps) |

### Tool Use Distribution (typical session)
- Bash: 66%
- Edit: 17%
- Read: 11%
- Write: 3%
- WebFetch, TodoWrite, Task: < 2%

## UUID/ParentUUID Chain (Linked List)

Messages form a linked list via `uuid` and `parentUuid`:

```
[null] -> [aaa111] -> [bbb222] -> [ccc333] -> ...
parentUuid    uuid      parentUuid   uuid
```

### Chain Start (Initial Message)
```json
{"parentUuid": null, "uuid": "aaa111", "type": "user"}
```

### Session Resume (claude -r)
When resuming a session:
- New file created with same `sessionId`
- First messages in new file duplicate last messages from original (for context)
- Detection: Same UUID appears in multiple files

### Rollback/Sidechain (escape-escape)
```json
{"isSidechain": true, "parentUuid": "xyz", "uuid": "sidechain1"}
```
- `isSidechain: true` indicates fork/branch
- Creates separate execution path without deleting original

## Key Fields Reference

| Field | Purpose |
|-------|---------|
| `uuid` | Unique message identifier (PRIMARY KEY) |
| `parentUuid` | Pointer to previous message (null = start) |
| `sessionId` | Groups messages into logical conversation |
| `isSidechain` | true = rollback/agent branch |
| `agentId` | Present only in agent files |
| `timestamp` | ISO-8601 wall clock time |
| `cwd` | Working directory at message time |
| `version` | Claude Code version |
| `gitBranch` | Git branch if in repo |

## Deduplication Strategy

### Within Single File
- UUIDs are unique - no duplicates observed

### Across Files (Resumed Sessions)
- Same `uuid` appearing in different files indicates session resume
- Detection: UUID set intersection between files
- Strategy: Skip messages with already-indexed UUIDs

## Agent Files

Agent files are completely separate transcripts:

```json
{
  "parentUuid": null,           // NULL in agent file (new branch)
  "isSidechain": true,          // Always true
  "sessionId": "parent-uuid",   // Links to parent session
  "agentId": "de30ae5d",        // Identifies agent
  "type": "user",
  "message": {"role": "user", "content": "..."}
}
```

- NOT redundant with parent session
- Index separately
- Merge results in UI layer with context

## Special Cases

### API Errors
```json
{
  "isApiErrorMessage": true,
  "message": {
    "content": [{"type": "text", "text": "API Error: 401..."}]
  }
}
```

### Token Usage Metrics
All assistant messages include:
```json
"usage": {
  "input_tokens": 1000,
  "output_tokens": 2000,
  "cache_creation_input_tokens": 6502,
  "cache_read_input_tokens": 12598
}
```

## Indexing Recommendations

### Index (SIGNAL)
- `text` content blocks - full
- `thinking` blocks - full (valuable reasoning)
- `tool_use` - name field only, truncate input to 200 chars
- `summary` type messages

### Skip (NOISE)
- `tool_result` content - truncate to 500 chars, preserve `is_error` flag
- `file-history-snapshot` - no searchable content
- `queue-operation` - internal administrative

### Metadata to Preserve
- `uuid` - deduplication key
- `sessionId` - conversation grouping
- `parentUuid` - chain reconstruction
- `timestamp` - ordering
- `isSidechain` - fork indicator
- `agentId` - agent identification
- `cwd` - project context
