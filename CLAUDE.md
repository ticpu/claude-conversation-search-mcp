# Claude Code Project Instructions

## Output Philosophy

This tool is designed for **Claude to search its own conversation history**. Output must be optimized for AI consumption:

**Dense & Information-Rich**:
- Maximum useful data per line, minimal decoration
- No ASCII art, banners, or decorative separators
- Use `…` (single char) not `...` (3 chars) for truncation
- Collapse whitespace in previews

**Hierarchical Format**:
```
N. 📁 ~/path 🗒️ session_id (M msgs) 💬 msg_uuid
🎟️rust,api,error
   User: context before…
»  AI: matched content…
   User: context after…
```
- `📁` project path (hyperlink to directory)
- `🗒️` session UUID (hyperlink to jsonl file)
- `💬` message UUID
- `🎟️` tags (technologies, languages, error flag)
- `»` marks the matched message

**grep -C Style Context**:
- `-C N` shows N messages before/after match
- Filters noise (tool_result dumps, warmup messages via `is_displayable()`)
- Deduplicates by session

**Terminal Hyperlinks**:
- OSC 8 hyperlinks when terminal supports it (detected via DA1 query)
- `HYPERLINKS=0` to disable, `HYPERLINKS=1` to force enable

## Project Architecture

- `src/main.rs` - Entry point, clap subcommand routing
- `src/cli/` - CLI commands
- `src/mcp/` - MCP server (server.rs, stats_analyzer.rs)
- `src/shared/` - Shared modules (cache, search, indexer, models)

## Design Decisions

**summarize_session pattern**: Returns Task tool instructions instead of doing work itself. Avoids polluting MCP tool descriptions with complex instructions. The haiku agent spawned by Task reads these instructions.

**Token estimation**: `HAIKU_CONTEXT_WINDOW * CONTEXT_SAFETY_MARGIN` (200k * 0.75 = 150k) determines when to warn about large sessions.

**is_displayable() filter**: Centralized in `SearchResult` to filter Warmup messages and non-User/Assistant/Summary types. Used by search, session viewing, and summarization.

**Prefix matching for session IDs**: `get_session_messages` accepts short session IDs (first 8 chars) for convenience.

## CLI/MCP Feature Parity

CLI and MCP must share the same output formatting code in `src/shared/`. The only difference: MCP assumes non-TTY (no terminal hyperlinks). When adding features:
- `-A`, `-B`, `-C` context switches must exist in both
- Limits and filters should have equivalent options
- New formatting goes in shared module, not duplicated

## MCP Tool Schema Conventions

- Don't repeat default values in descriptions when `"default": N` is set in schema
- Use grep-style `-A`, `-B`, `-C` for context parameters (familiar to developers)
- Keep descriptions terse - schema metadata speaks for itself

## Debugging MCP Tools

MCP servers communicate via JSON-RPC over stdio. Use `debug: true` parameter on search tools to see:
- Raw JSON arguments received
- Parameter parsing results
- Filtering logic details

## Testing

Test changes before committing. No need to install - use the built binary directly:

```bash
cargo build --release
./target/release/claude-conversation-search session <session_id>
./target/release/claude-conversation-search search "query"
```

## Pre-commit Checklist

1. `cargo test`
2. `cargo clippy --fix --allow-dirty`
3. `cargo fmt`

All warnings must be resolved. Remove unused code instead of suppressing.

## Release Process

1. Update version in `Cargo.toml`
2. Run `cargo clippy -- -D warnings` (CI uses `-D warnings`)
3. Commit: `git commit -m "bump: Version X.Y.Z"`
4. Push and wait for CI: `gh run list -L1 --json databaseId -q '.[0].databaseId' | xargs gh run watch --exit-status`
5. Tag: `git tag -as vX.Y.Z` (annotated + signed)
6. Push tag: `git push --tags`

Release workflow (`.github/workflows/release.yml`) triggers on version tags and builds binaries.
