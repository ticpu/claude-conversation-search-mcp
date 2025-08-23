# Claude Code MCP Integration

This document explains how to integrate the Claude Search tool with Claude Code using the Model Context Protocol (MCP).

## Installation Steps

### 1. Build the MCP Server

First, build the release version of the MCP server:

```bash
cargo build --release --bin claude-search-mcp
```

### 2. Configure Claude Code

Add the following configuration to your Claude Code MCP configuration file. The location depends on your system:

- **macOS**: `~/Library/Application Support/Claude/claude_code_config.json`
- **Linux**: `~/.config/claude/claude_code_config.json`
- **Windows**: `%APPDATA%\Claude\claude_code_config.json`

```json
{
  "mcpServers": {
    "claude-search": {
      "command": "/full/path/to/claude-code-conversation-search/target/release/claude-search-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "claude_search=info"
      }
    }
  }
}
```

**Important**: Replace `/full/path/to/` with the actual absolute path to this project directory.

### 3. Restart Claude Code

After adding the configuration, restart Claude Code to load the MCP server.

## Available Tools

Once configured, Claude Code will have access to these conversation search tools:

### 🔍 `search_conversations`
Search through your Claude Code conversation history with rich metadata.

**Parameters:**
- `query` (required): Search text. Supports field syntax like `session_id:abc123`
- `project` (optional): Filter by project name
- `limit` (optional): Max results (default: 10)

**Example usage:**
- "Search for conversations about rust programming"
- "Find discussions about docker in the backend project"
- "Show me error-related conversations from last week"

### 📋 `get_conversation_context`
Get detailed context for a specific conversation session.

**Parameters:**
- `session_id` (required): Session ID to retrieve
- `include_content` (optional): Include full message content vs snippets

**Example usage:**
- "Show me the full context for session abc123"
- "Get conversation details for the session about database migration"

### 📊 `analyze_conversation_topics`
Analyze technology topics and patterns across conversations.

**Parameters:**
- `project` (optional): Filter by project name
- `limit` (optional): Max topics per category (default: 20)

**Example usage:**
- "What technologies am I discussing most often?"
- "Show me the most common programming languages in my conversations"

### 📈 `get_conversation_stats`
Get detailed statistics about your conversation data.

**Parameters:**
- `project` (optional): Filter by project name

**Example usage:**
- "Show me my conversation statistics"
- "How many conversations involve code errors?"

## Features

- **🚀 Auto-indexing**: Automatically indexes conversations on first use
- **🏷️ Rich Metadata**: Detects technologies, languages, tools, and errors
- **🎯 Smart Search**: Field-specific queries and project filtering  
- **📊 Visual Output**: Emoji-tagged results for quick scanning
- **⚡ Fast Search**: Tantivy-powered full-text search with BM25 ranking
- **📈 Analytics**: Topic analysis and conversation patterns

## Troubleshooting

### MCP Server Not Starting
1. Check that the binary exists and is executable
2. Verify the absolute path in the configuration
3. Check Claude Code's MCP logs for error messages

### No Results Found
1. Ensure you have Claude Code conversation files in `~/.claude/projects/`
2. Run the CLI tool first to test: `./target/release/claude-search search "test"`
3. Check that auto-indexing completed successfully

### Performance Issues
1. The first search may take longer as it builds the index
2. Subsequent searches should be very fast
3. Clear and rebuild index if needed: `./target/release/claude-search cache clear`

## Advanced Usage

### Field-Specific Searches
- `session_id:abc123` - Find specific session
- `project:myproject` - Filter by project
- `technologies:rust` - Find conversations about Rust

### Search Tips
- Use quotes for exact phrases: `"error handling"`
- Combine terms: `rust docker deployment`
- Use wildcards in session IDs: `session_id:abc*`

## Development

### Testing the MCP Server
Run the test script to verify functionality:

```bash
python3 test_mcp.py
```

### Manual Testing
Test the MCP server directly:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | ./target/release/claude-search-mcp
```

### Logs
MCP server logs are written to stderr to avoid interfering with JSON-RPC communication. Set `RUST_LOG=claude_search=debug` for verbose logging.

## Integration Benefits

- **🎯 Contextual Search**: Find relevant past conversations while coding
- **📚 Knowledge Base**: Your conversation history becomes a searchable knowledge base
- **🔄 Seamless Workflow**: Access conversation context without leaving Claude Code  
- **🧠 Better Context**: Claude can reference past discussions and solutions
- **📊 Usage Insights**: Understand your coding patterns and technology usage

## License

This MCP integration follows the same license as the main project.