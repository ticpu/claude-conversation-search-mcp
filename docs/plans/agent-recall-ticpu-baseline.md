# ticpu v1.5.0 unchanged-upstream baseline

Date: 2026-07-30  
Repository: `laurynas-pliuskys/agent-recall`  
Upstream: `ticpu/claude-conversation-search-mcp`  
Commit: `730624b9694c6568934f10085643143915e9eba7`  
Binary identity: `claude-conversation-search 1.5.0`

## Isolation

All behavioral checks used synthetic Claude JSONL transcripts and an isolated
configuration/index rooted at `/tmp/agent-recall-phase3`. No real conversation
history, existing ticpu configuration, or user cache was read or changed.

The tracked checkout remained clean and identical to both `origin/master` and
`upstream/master` throughout the baseline.

## Toolchain and inherited CI checks

- `rustc 1.97.1 (8bab26f4f 2026-07-14)`
- `cargo 1.97.1 (c980f4866 2026-06-30)`
- `cargo fmt -- --check`: passed
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed
- `cargo build --release --locked`: passed
- `cargo test --release --locked`: 20 passed, 0 failed

## Indexing baseline

Initial full rebuild:

- 2 transcript files discovered
- 8 entries indexed (6 + 2)
- index stored only under `/tmp/agent-recall-phase3/cache`

Immediate repeat auto-index:

- reported `No files needed indexing`
- did not reparse unchanged transcript files

Incremental new-file update through MCP:

- 2 new transcript files were added after MCP startup
- passive search reported `index is stale (0 modified, 1 new files)` because the
  newest file was treated as the active session and excluded from stale counts
- `reindex` reported `0 stale + 2 new files reindexed`
- the new-session marker became searchable immediately

Incremental changed-file update through MCP:

- one indexed transcript was extended from 6 to 7 messages
- `reindex` reported `1 stale + 0 new files reindexed`
- the appended marker became searchable and the session showed 7 messages

## Retrieval baseline

The following behaviors passed:

- ordinary marker search returned the correct user message
- code-aware token search for `fix` matched `_fix_ssh_agent`
- tool-input marker search returned the serialized Bash input when tools were
  included
- tool-result marker search returned the indexed error result
- thinking marker search returned the thinking block when thinking was included
- asymmetric centered session navigation returned 1 message before and 2 after
  the selected full message UUID
- search result metadata included project, short session ID, message ID, role,
  timestamp, and interaction count

## MCP baseline

Initialization succeeded and advertised:

- server name `claude-search-mcp`
- server version `1.5.0`
- protocol version `2024-11-05`
- tool list change capability

`tools/list` returned six tools:

1. `search_conversations`
2. `reindex`
3. `get_session_messages`
4. `summarize_session`
5. `get_messages`
6. `respawn_server`

Search and incremental reindex tool calls both completed successfully.

## Confirmed upstream baseline defects

### B1. Standard initialized notification receives an error response

After a successful initialize request, sending
`notifications/initialized` produced an `Unknown method` JSON-RPC error with a
null ID. MCP notifications should not receive responses. This may cause noise or
compatibility problems with stricter MCP clients.

### B2. One modified transcript does not trigger the passive stale warning

With exactly one non-active indexed file whose size/mtime had changed, MCP search
returned results without a stale warning. The implementation warns only when
`stale_count > 1` or `new_count > 0`, so one modified file is silent.

### B3. Cached total-entry status inflates after reindexing a changed file

After indexing 8 entries, adding 4 entries in two new files, and replacing a
6-entry session with its 7-entry version, the current per-file counts summed to
13 but `index status` and cache metadata reported 19. The global counter adds the
replacement entries without subtracting the superseded file's prior entries.

The underlying session counts and search behavior remained correct in this
fixture; the defect affects reported global totals.

## Baseline conclusion

The unchanged ticpu engine is buildable and its primary search, parser, context,
incremental-indexing, passive-health, and MCP discovery paths work on the
representative fixture. B1-B3 are concrete upstream behaviors to preserve as
known baseline issues and fix deliberately after the mechanical rename rather
than mixing fixes into the baseline.
