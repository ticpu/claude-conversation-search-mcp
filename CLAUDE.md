# Claude Code Project Instructions

## Project Architecture

**Single Binary with Subcommands**: The project uses a unified architecture:
- `src/main.rs` - Main entry point with clap subcommand routing
- `src/cli/` - CLI-specific code (commands.rs, etc.)
- `src/mcp/` - MCP server and related functionality (server.rs, conversation_aggregator.rs, etc.)
- `src/shared/` - Shared modules used by both CLI and MCP (cache, search, indexer, models, etc.)

**Command Structure**:
- `claude-search index` - Build/update search index
- `claude-search search <query>` - Search conversations  
- `claude-search topics` - Show technology topics
- `claude-search stats` - Show conversation statistics
- `claude-search session <id>` - View specific session
- `claude-search cache info|clear` - Cache management
- `claude-search mcp` - Run as MCP server

## Development Notes

- Prefer `cargo check` over `cargo build` when just checking for compilation errors - it's much quicker
- Use `cargo build` only when you need the actual binary
- **Single Binary Architecture**: Uses subcommands (`claude-search mcp`, `claude-search search`, etc.) eliminating dead code warnings and complexity from multiple binaries
- All modules use standard `crate::shared` imports - no feature flags or path-based imports needed

## Special Notes

- AI Analysis Feature uses WebFetch approach with config at `~/.config/claude-search-mcp/config.yaml`
- To test MCP changes: use the `respawn_server` MCP tool available in Claude Code conversations

## Debugging MCP Tools

Since MCP servers communicate via JSON-RPC over stdio, traditional debugging methods (println!, logging) interfere with the protocol. Instead:

**Use the debug parameter**: All search tools support a `debug: true` parameter that shows:
- Raw JSON arguments received
- Parameter parsing results  
- Filtering logic details
- Result counts at each stage

Example usage:
```
mcp__claude-conversation-search__search_conversations(query="test", exclude_patterns=[".*temp.*"], debug=true)
```

This outputs detailed debugging information directly in the search results, allowing you to troubleshoot parameter parsing and filtering logic.

## Pre-commit Checklist

1. `cargo test` - Run all tests
2. `cargo clippy --fix --allow-dirty` - Fix clippy warnings automatically  
3. `cargo fmt` - Format code consistently

- All clippy warnings must be resolved before committing.
- Remove unused code instead of suppressing warnings.
- Fix all warnings properly - do NOT use underscore prefixes (`_var_name`) to hide unused variables

- Try not to use super:: and crate:: all around the file, prefer importing something more specific.
