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

### 🔧 **Dual Interface**
- **CLI tool**: Simple command-line interface for terminal usage
- **MCP server**: Integration with Claude Code via Model Context Protocol
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

# Build CLI tool only (recommended for command-line usage)
cargo build --release --bin claude-search --features cli --no-default-features
cp target/release/claude-search /usr/local/bin/  # or add to PATH

# Or build MCP server for Claude Code integration
cargo build --release --bin claude-search-mcp --features mcp --no-default-features

# Or build both (default)
cargo build --release
```

**Note**: This is a multi-binary project with separate CLI and MCP components. Building with specific features avoids dead code warnings.

### Basic Usage

```bash
# Index your conversations (run this first time)
claude-search index

# Search for anything
claude-search search "kubernetes"
claude-search search "error handling" 
claude-search search "rust async"

# Search with project filter
claude-search search "rust" --project "vault-rs"

# Limit number of results
claude-search search "function" --limit 20
```

## CLI Reference

### `claude-search index`
Build or update the search index from your Claude Code conversations.

```bash
claude-search index              # Build/update index from ~/.claude/projects/
claude-search index --rebuild    # Force full rebuild (recreates index)
```

**What it does:**
- Scans `~/.claude/projects/` for `*.jsonl` files
- Parses conversation entries with timestamps, content, and metadata  
- Builds full-text search index using Tantivy
- Index stored at `~/.cache/claude-search/`

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

### `claude-search search <query>`
Search through your indexed conversations.

```bash
claude-search search "rust async functions"
claude-search search "error" --project "my-project" --limit 5
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
- **Simple text**: `claude-search search "docker compose"`
- **Multiple terms**: `claude-search search "rust error handling"`  
- **Phrase search**: `claude-search search '"exact phrase"'` (wrap in quotes)
- **Boolean AND**: `claude-search search "rust AND async"` (both terms must appear)

## MCP Integration (Claude Code)

This tool also provides an MCP (Model Context Protocol) server for seamless integration with Claude Code.

### Setup

1. **Build the MCP server**:
   ```bash
   cargo build --release --bin claude-search-mcp --features mcp --no-default-features
   ```

2. **Configure Claude Code** by adding to your MCP settings:
   ```json
   {
     "claude-search": {
       "command": "/path/to/claude-search-mcp",
       "args": []
     }
   }
   ```

3. **Use within Claude Code** - Claude will automatically have access to search your conversations:
   - "Search my previous conversations about Rust async"
   - "Find where we discussed error handling"
   - "Show stats on my coding conversations"

### MCP Tools Available
- **search_conversations**: Search through indexed conversations
- **get_conversation_context**: Get full context around specific results
- **analyze_conversation_topics**: Analyze technology usage patterns
- **get_conversation_stats**: Get detailed statistics about conversations

## Examples

### Finding Past Solutions
```bash
# Find how you solved a specific problem
claude-search search "docker compose error"
claude-search search "authentication failed" --project "web-app"

# Find code snippets
claude-search search "async fn" --project "rust-backend"
claude-search search "useEffect" --project "react-frontend"
```

### Exploring Conversations
```bash
# Find long discussions
claude-search search "help me understand" --limit 50

# Find tool usage examples
claude-search search "bash" --limit 20

# Search for specific technologies
claude-search search "kubernetes deployment"
claude-search search "database migration"
```

### Common Use Cases
```bash
# Review recent work
claude-search search "TODO" --limit 30

# Find error solutions
claude-search search "error" --limit 20

# Look up specific functions or APIs
claude-search search "fetch API"
claude-search search "regex pattern"
```

## Configuration

The tool works out of the box, but you can customize behavior:

### Environment Variables
- `CLAUDE_CONFIG_DIR` - Override Claude Code directory location
- `CLAUDE_SEARCH_CACHE` - Custom cache directory location  
- `RUST_LOG` - Control logging verbosity (`error`, `warn`, `info`, `debug`, `trace`)

### Cache Location
- **Linux**: `~/.cache/claude-search/`
- **macOS**: `~/Library/Caches/claude-search/`  
- **Windows**: `%LOCALAPPDATA%\claude-search\`

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
- Try `claude-search index --rebuild`

**"Index is corrupt"**  
- Run `claude-search cache clear && claude-search index`
- Check disk space availability

**"Search is slow"**
- Run `claude-search cache info` to check index size
- Consider `claude-search index --rebuild` to optimize

**"Permission denied"**
- Ensure read access to Claude Code directories
- Check cache directory permissions

### Getting Help

```bash
claude-search --help          # General help
claude-search search --help   # Search command help
claude-search index --help    # Index command help
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
cargo run --bin claude-search --features cli -- --help

# Run MCP server (for testing)
cargo run --bin claude-search-mcp --features mcp

# Check for warnings (build each binary individually)
cargo check --bin claude-search --features cli --no-default-features
cargo check --bin claude-search-mcp --features mcp --no-default-features
```

## License

MIT License - see [LICENSE](LICENSE) for details.

## Acknowledgments

- **Tantivy** - Fast, full-text search engine for Rust
- **Claude Code** - AI-powered coding assistant by Anthropic
- **ccusage** - Inspiration for JSONL parsing approach