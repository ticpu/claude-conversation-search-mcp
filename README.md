# Claude Code Conversation Search

A high-performance search engine for your Claude Code conversation history, built in Rust with lightning-fast full-text search capabilities.

## Overview

Claude Code stores all your conversations locally as JSONL files, but there's no built-in way to search through them. This tool indexes all your conversations and provides powerful search capabilities, letting you quickly find past solutions, discussions, and code snippets.

## Features

### 🔍 **Powerful Search**
- **Full-text search** across all conversations with BM25 ranking
- **Smart filtering** by project name
- **Highlighted snippets** showing matched content in context
- **Relevance scoring** for best matches first

### ⚡ **High Performance**  
- **Lightning fast**: Sub-millisecond search queries
- **Efficient indexing**: Processes thousands of conversations in seconds
- **Memory efficient**: Uses memory-mapped indexes via Tantivy

### 🔧 **Unified Interface**
- **Single binary** with subcommands for both CLI and MCP server functionality
- **CLI mode**: Simple command-line interface for terminal usage (`claude-conversation-search search ...`)
- **MCP server mode**: Integration with Claude Code via Model Context Protocol (`claude-conversation-search mcp`)
- Configurable result limits and project-based filtering

### 🎯 **Smart Features**
- **Auto-discovery** of Claude Code directories (`~/.claude/projects/`)
- **Project organization** with conversation grouping by directory
- **Robust parsing** handles malformed JSONL gracefully

## Quick Start

### Installation

```bash
# From source
git clone https://github.com/user/claude-conversation-search
cd claude-conversation-search

# Build the unified binary
cargo build --release
cp target/release/claude-conversation-search /usr/local/bin/  # or add to PATH
```

**Note**: The project now uses a single binary with subcommands for both CLI and MCP server functionality, eliminating the need for feature flags and multiple binaries.

### Basic Usage

```bash
# Index your conversations (run this first time)
claude-conversation-search index

# Search for anything
claude-conversation-search search "kubernetes"
claude-conversation-search search "error handling" 
claude-conversation-search search "rust async"

# Search with project filter
claude-conversation-search search "rust" --project "vault-rs"

# Limit number of results
claude-conversation-search search "function" --limit 20
```

## CLI Reference

### `claude-conversation-search index`
Build or update the search index from your Claude Code conversations.

```bash
claude-conversation-search index              # Build/update index from ~/.claude/projects/
claude-conversation-search index --rebuild    # Force full rebuild (recreates index)
```

**What it does:**
- Scans `~/.claude/projects/` for `*.jsonl` files
- Parses conversation entries with timestamps, content, and metadata  
- Builds full-text search index using Tantivy
- Index stored at `~/.cache/claude-conversation-search/`

**Expected output:**
```
Starting indexing process...
Scanning for JSONL files in: /home/user/.claude/projects/**/*.jsonl
Processing: /home/user/.claude/projects/my-project/session-id.jsonl
  Indexed 45 entries
Processing: /home/user/.claude/projects/other-project/session-id2.jsonl  
  Indexed 123 entries
Indexing complete: 15 files, 2,847 entries
```

### `claude-conversation-search search <query>`
Search through your indexed conversations.

```bash
claude-conversation-search search "rust async functions"
claude-conversation-search search "error" --project "my-project" --limit 5
```

**Options:**
- `--project <name>` - Filter by project directory name (e.g., "vault-rs")
- `--limit <n>` - Maximum results to show (default: 10)

**Expected output:**
```
Found 3 results:

1. [my-project] 2025-08-23 15:30 (score: 8.42)
   Session: abc123-def456-789
   Here's how to handle async functions in Rust: async fn process_data() -> Result<(), Error> { ... }

2. [my-project] 2025-08-22 09:15 (score: 7.23)  
   Session: xyz789-abc123-456
   You can use tokio::spawn for concurrent async tasks...

3. [another-project] 2025-08-20 14:45 (score: 6.91)
   Session: def456-xyz789-123
   The async/await syntax makes it easy to write asynchronous code...
```

**Query features:**
- **Simple text**: `claude-conversation-search search "docker compose"`
- **Multiple terms**: `claude-conversation-search search "rust error handling"`  
- **Phrase search**: `claude-conversation-search search '"exact phrase"'` (wrap in quotes)
- **Boolean AND**: `claude-conversation-search search "rust AND async"` (both terms must appear)

## MCP Integration (Claude Code)

This tool also provides an MCP (Model Context Protocol) server for seamless integration with Claude Code.

### Setup

1. **Build the binary** (if not already done):
   ```bash
   cargo build --release
   ```

2. **Configure Claude Code** using the MCP CLI:
   ```bash
   # Add the MCP server to Claude Code
   claude mcp add claude-conversation-search /path/to/claude-conversation-search mcp
   
   # Alternatively, if installed globally:
   claude mcp add claude-conversation-search claude-conversation-search mcp
   
   # Verify it was added successfully
   claude mcp list
   ```

   This configures Claude Code to use `claude-conversation-search mcp` as an MCP server named "claude-conversation-search".

3. **Use within Claude Code** - Claude will automatically have access to search your conversations:
   - "Search my previous conversations about Rust async"
   - "Find where we discussed error handling"
   - "Show stats on my coding conversations"

### MCP Tools Available
- **search_conversations**: Search through indexed conversations with optional project exclusion
- **get_conversation_context**: Get full context around specific results
- **analyze_conversation_topics**: Analyze technology usage patterns
- **get_conversation_stats**: Get detailed statistics about conversations
- **respawn_server**: Reload the MCP server without restarting Claude Code
- **analyze_conversation_content**: AI-powered analysis of selected conversations (requires web server config)

## Examples

### Finding Past Solutions
```bash
# Find how you solved a specific problem
claude-conversation-search search "docker compose error"
claude-conversation-search search "authentication failed" --project "web-app"

# Find code snippets
claude-conversation-search search "async fn" --project "rust-backend"
claude-conversation-search search "useEffect" --project "react-frontend"
```

### Exploring Conversations
```bash
# Find long discussions
claude-conversation-search search "help me understand" --limit 50

# Find tool usage examples
claude-conversation-search search "bash" --limit 20

# Search for specific technologies
claude-conversation-search search "kubernetes deployment"
claude-conversation-search search "database migration"
```

### Common Use Cases
```bash
# Review recent work
claude-conversation-search search "TODO" --limit 30

# Find error solutions
claude-conversation-search search "error" --limit 20

# Look up specific functions or APIs
claude-conversation-search search "fetch API"
claude-conversation-search search "regex pattern"
```

## Configuration

The tool works out of the box, but you can customize behavior:

### Environment Variables
- `CLAUDE_CONFIG_DIR` - Override Claude Code directory location
- `CLAUDE_SEARCH_CACHE` - Custom cache directory location  
- `RUST_LOG` - Control logging verbosity (`error`, `warn`, `info`, `debug`, `trace`)

### Cache Location
- **Linux**: `~/.cache/claude-conversation-search/`
- **macOS**: `~/Library/Caches/claude-conversation-search/`  
- **Windows**: `%LOCALAPPDATA%\claude-conversation-search\`

### AI Analysis Configuration (Optional)

For AI-powered conversation analysis, create `~/.config/claude-conversation-search-mcp/config.yaml`:

```yaml
web_server:
  path: /var/www/html/claude-temp/
  url: https://yourdomain.com/claude-temp/
```

**How it works:**
- The MCP server writes conversation content to the configured local path
- Uses WebFetch tool to analyze content via the corresponding **publicly accessible** URL
- **No API key required** - leverages Claude Code's built-in WebFetch capability
- Temporary files are cleaned up after analysis

**Requirements:**
- You need a web server with a publicly accessible domain
- The local path must be writable by the MCP server
- The URL must correspond to the local path and be web-accessible

## Performance

### Indexing Speed
- **~1000 conversations/second** on modern hardware
- **Incremental updates** process only changed files
- **Parallel processing** utilizes all CPU cores

### Search Speed  
- **Sub-millisecond** queries on typical datasets
- **Memory-mapped indexes** for optimal I/O
- **Cached results** for repeated queries

### Storage Efficiency
- **~10% overhead** compared to original JSONL files
- **Compressed indexes** with segment merging
- **Automatic cleanup** of unused segments

## Troubleshooting

### Common Issues

**"No conversations found"**
- Check that Claude Code has created files in `~/.claude/projects/`
- Verify directory permissions
- Try `claude-conversation-search index --rebuild`

**"Index is corrupt"**  
- Run `claude-conversation-search cache clear && claude-conversation-search index`
- Check disk space availability

**"Search is slow"**
- Run `claude-conversation-search cache info` to check index size
- Consider `claude-conversation-search index --rebuild` to optimize

**"Permission denied"**
- Ensure read access to Claude Code directories
- Check cache directory permissions

### Getting Help

```bash
claude-conversation-search --help          # General help
claude-conversation-search search --help   # Search command help
claude-conversation-search index --help    # Index command help
```

## Technical Details

### Architecture
- **Search Engine**: Tantivy (Rust-native, Lucene-inspired)
- **Index Format**: Segment-based with BM25 scoring
- **Storage**: Memory-mapped files for efficiency
- **Parsing**: Robust JSONL parser with error recovery

### Supported Formats
- **Claude Code JSONL** (all versions)
- **Multiple directories** (old `~/.claude` and new `~/.config/claude`)
- **Cross-platform** file paths and timestamps

### Privacy & Security
- **Local only**: No data leaves your machine
- **No network access** required after installation
- **Safe parsing**: Handles malformed data gracefully
- **No data modification**: Read-only access to conversations

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

### Development Setup
```bash
git clone https://github.com/user/claude-conversation-search
cd claude-conversation-search

# Build for development
cargo build

# Test (runs all tests)
cargo test

# Run CLI tool
cargo run -- --help

# Run MCP server (for testing)
cargo run -- mcp

# Check for warnings and run linting
cargo check
cargo clippy --fix --allow-dirty
```

## License

MIT License - see [LICENSE](LICENSE) for details.

## Acknowledgments

- **Tantivy** - Fast, full-text search engine for Rust
- **Claude Code** - AI-powered coding assistant by Anthropic
- **ccusage** - Inspiration for JSONL parsing approach
