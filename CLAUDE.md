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
- `src/shared/path_utils.rs` - All `.claude/` filesystem concerns: `projects_dir()`, `project_dir_name()`, `session_jsonl_path()`, `discover_jsonl_files()`, `active_session_jsonl()`. This is the single source of truth for Claude directory layout. Do not duplicate this logic elsewhere.

## Design Decisions

**summarize_session pattern**: Returns Task tool instructions instead of doing work itself. Avoids polluting MCP tool descriptions with complex instructions. The haiku agent spawned by Task reads these instructions.

**Token estimation**: `HAIKU_CONTEXT_WINDOW * CONTEXT_SAFETY_MARGIN` (200k * 0.75 = 150k) determines when to warn about large sessions.

**is_displayable() filter**: On both `SearchResult` and `ConversationEntry` to filter Warmup messages and non-User/Assistant/Summary types. Used by search, session viewing, and summarization.

**JSONL-first session reading**: `session` CLI and MCP `get_session_messages` read directly from the source JSONL file via `JsonlParser::with_full_content()` to get untruncated content. Falls back to Tantivy index only when the JSONL file is not found. The index content is pre-truncated during parsing (tool inputs/results).

**Prefix matching for session IDs**: `get_session_messages` accepts short session IDs (first 8 chars) for convenience.

**Active session exclusion**: MCP `search_conversations` excludes the currently-written JSONL from stale checks via `active_session_jsonl(cwd)` in `path_utils`. This walks up from the process cwd to find the matching `.claude/projects/<dir>/` entry, same algorithm as `claude-session-uuid`.

**Derived package dependencies**: the `.deb`'s `Depends:` is read off the binaries in the build container, never written into `packaging/control`. The glibc floor moves with the base image, and a floor set too low installs cleanly then dies at exec on a symbol version. The `DT_NEEDED` soname list is derived the same way and mapped to packages; an unrecognized soname fails the build rather than shipping an under-declared package.

**`Install` is a user action, not a packaging one**: the `Install` subcommand registers the MCP server into the invoking user's Claude Code config. It never runs from a maintainer script. The package ships the binary, shell completions and a copyright file, nothing else.

**File-granularity index replacement**: `update_incremental` deletes and re-adds documents per source JSONL file (via exact match on the raw `source_file` STRING field), not per session. Main session files and subagent transcripts (`<uuid>/subagents/**/agent-*.jsonl`) share the same `sessionId`; deleting by session wiped all sibling files' documents whenever any one file was re-indexed.

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

1. `cargo fmt --all` (required - pre-commit hook rejects unformatted code)
2. `cargo clippy --fix --allow-dirty`
3. `cargo test`

All warnings must be resolved. Remove unused code instead of suppressing.
Always run `cargo fmt --all` before `git commit` - the pre-commit hook checks formatting and will reject the commit if code is not formatted.

## Pre-commit Hook

A pre-commit hook lives in `hooks/pre-commit` (runs fmt, clippy, tests). Install with:

```bash
bash hooks/install.sh
```

## Packaging

`make deb` builds `claude-conversation-search_<version>_{amd64,arm64}.deb`. One container pass
cross-compiles both Linux triples and writes the binaries, the per-arch glibc floor, the per-arch
derived dependency list and the three shell completions into `dist/`. That directory is the only
join point: the deb recipe runs natively and reads nothing else from the build.

`packaging/control` is a template. The Makefile rewrites its `Version:`, `Architecture:` and
`Depends:` lines; the placeholders are self-describing so an unsubstituted control file is obvious.

## Release Process

The procedure lives in `.claude/commands/release.md` and is the only copy — a second numbered list
here went stale and contradicted it on the lockfile step and the commit subject.

Release workflow (`.github/workflows/release.yml`) triggers on version tags and builds binaries and
`.deb` packages. The packages are what https://apt.ticpu.net ingests, so a release missing them
leaves the archive on the previous version.

**Cargo.lock Policy**: Excluded from .gitignore and committed only on releases for reproducible builds. `.gitattributes` configures `merge=union` to avoid spurious merge conflicts. Do not stage `Cargo.lock` outside of the release process.
