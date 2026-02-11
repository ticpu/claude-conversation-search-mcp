# Claude Code Conversation Search

**CLI + MCP tool for searching Claude Code conversation history.**

A single binary that works two ways:
- **CLI**: Search your conversations from the terminal (`claude-conversation-search search "rust async"`)
- **MCP Server**: Lets Claude search its own history during sessions (the only tool that does this!)

Other tools (claude-history-explorer, claude-code-history-viewer, etc.) are *viewers* - you browse manually. This tool indexes everything with Tantivy/BM25 and gives both you AND Claude direct search access.

![Screenshot](docs/screenshot.png)

*Claude searching its own history to understand why a function was added, then jumping to the exact message with `center_on` and `-B/-A` context.*

## Perfect For Heavy Claude Code Users

If you work across **dozens of projects**, you know the pain:
- "I solved this exact problem last month... but which project?"
- "What was that regex pattern I used for parsing logs?"
- "How did I configure that Docker setup?"

This tool indexes **all your conversations across all projects** and lets Claude search them instantly. No more digging through folders or re-explaining context.

> **Warning**: Claude Code auto-deletes old conversations! Check `~/.claude/settings.json` for `cleanupPeriodDays` - this deletes conversations older than N days (0 = immediate deletion!). Set it to `999999999` to keep your history.

## Why This Tool?

| Feature | This Tool | Other Tools |
|---------|-----------|-------------|
| Claude can search its own history | ✓ MCP integration | ✗ Manual browsing only |
| Cross-project search | ✓ All projects indexed | ✗ Per-project only |
| Full-text search | ✓ Tantivy/BM25 | Some have regex |
| Jump to specific message | ✓ `center_on` + `-B/-A` context | ✗ |
| Smart content filtering | ✓ Skips tool_result noise | ✗ Index everything |
| Passive staleness detection | ✓ Warns when index outdated | ✗ |

## Overview

Claude Code stores conversations as JSONL files in `~/.claude/projects/`. This tool indexes them with smart filtering (skips file dumps, keeps reasoning) and exposes search via MCP so Claude can find relevant past conversations during your session.

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
- **Smart content filtering**: Indexes text/thinking blocks, skips tool_result file dumps (noise reduction)
- **UUID-based deduplication**: Handles session resume and rollbacks gracefully
- **Passive health monitoring**: Warns when index is stale, offers reindex tool
- **Robust parsing** handles malformed JSONL gracefully

## Quick Start

### One-Line Install

```bash
git clone https://github.com/ticpu/claude-conversation-search-mcp
cd claude-conversation-search-mcp
cargo run --release -- install
```

The installer registers the binary with Claude Code MCP.

Verify: `claude mcp list` should show `claude-conversation-search`.

### Manual Installation

```bash
cargo build --release
cp target/release/claude-conversation-search ~/.local/bin/
claude mcp add claude-conversation-search ~/.local/bin/claude-conversation-search mcp
```

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
- **search_conversations**: Full-text search with `-C`/`-B`/`-A` context (grep-style). Shows timestamps, session IDs, 🎟️ tags.
- **get_session_messages**: Paginated session content. Use `center_on` + `-B`/`-A` to jump to a specific message.
- **get_messages**: Fetch full content of specific messages by UUID (from 💬 in search results).
- **summarize_session**: Returns Task instructions for haiku-powered summarization of large sessions.
- **reindex**: Update index when results seem incomplete.
- **respawn_server**: Reload MCP server after rebuilding.

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

### Config File

`~/.config/claude-conversation-search-mcp/config.yaml`:

```yaml
limits:
  per_file_chars: 150000        # Max chars indexed per JSONL file
  tool_result_max_chars: 2000   # Max chars kept from tool_result content
  tool_input_max_chars: 200     # Max chars kept from tool_use input

search:
  exclude_patterns: []          # Regex patterns to exclude from results

index:
  auto_index_on_startup: true
  writer_heap_mb: 50
```

Changing `tool_result_max_chars` or `tool_input_max_chars` requires a reindex (`claude-conversation-search index rebuild`).

### Cache Location

- **Linux**: `~/.cache/claude-conversation-search/`
- **macOS**: `~/Library/Caches/claude-conversation-search/`
- **Windows**: `%LOCALAPPDATA%\claude-conversation-search\`

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
