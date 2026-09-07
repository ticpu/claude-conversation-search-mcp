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
- `src/cli/` - CLI commands: `args.rs` (clap types), `commands.rs` (dispatch, cache/install), `index.rs`, `search.rs`, `session.rs`, `stats.rs` (topics + stats), `summary.rs`
- `src/mcp/` - MCP server: `protocol.rs` (JSON-RPC loop), `server.rs`, `tools/mod.rs` (tool bodies), `tools/schema.rs` (tool list + JSON schemas)
- `src/shared/` - Shared between CLI and MCP: `cache.rs`, `cache_stats.rs`, `config.rs`, `format.rs` (search result rendering), `indexer.rs`, `lock.rs`, `metadata.rs` (tag extraction), `models.rs`, `parsers/` (JSONL parsing), `path_utils.rs`, `search/` (query + engine), `session_view.rs`, `terminal.rs`
- `src/shared/path_utils.rs` - All `.claude/` filesystem concerns: `projects_dir()`, `project_dir_name()`, `session_jsonl_path()`, `discover_jsonl_files()`, `find_session_jsonl()` (prefix match), `globally_active_session_jsonl()`. This is the single source of truth for Claude directory layout. Do not duplicate this logic elsewhere.

## Design Decisions

**summarize_session pattern**: The MCP tool returns Task tool instructions instead of doing work itself, avoiding complex instructions baked into the MCP tool description. The haiku agent spawned by Task reads these instructions. The CLI `summary` command is unrelated: it renders the session directly and pipes it into `claude --print --model haiku` in a jailed empty directory (`src/cli/summary.rs`).

**Token estimation**: `HAIKU_CONTEXT_WINDOW * CONTEXT_SAFETY_MARGIN` (200k * 0.75 = 150k) determines when `summarize_session` warns that a session may need multiple agents.

**is_displayable() filter**: On both `SearchResult` and `ConversationEntry` (`src/shared/models.rs`) to filter non-User/Assistant/Summary message types and literal `Warmup` messages. Shared by search formatting and `session_view::render`.

**Session viewing is unified**: `src/shared/session_view.rs` backs both the CLI `session` command and the MCP `get_session_messages` tool. `load_session` reads the source JSONL directly via `JsonlParser::with_full_content()` for untruncated content, falling back to the Tantivy index (pre-truncated) only when no JSONL file is found; the render footer marks that fallback. `find_session_jsonl` (in `path_utils`) accepts a session ID prefix, so both frontends get prefix matching for free.

**Active session exclusion**: MCP `search_conversations` uses `globally_active_session_jsonl()` (most-recently-modified JSONL under `.claude/projects/`) rather than a cwd walk, since the MCP server's cwd is fixed at startup and does not track the caller's active project. That file is excluded from the staleness check (it's always being written), and its session is also excluded from search results unless the caller passes `include: ["current_session"]`. The CLI has no equivalent: it is invoked per-command and never applies this exclusion.

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

MCP servers communicate via JSON-RPC over stdio. `search_conversations` takes a `debug: true`
argument that prepends a line with the parsed query, `-B`/`-A`, limit, and the exclude
projects/patterns actually applied. No other tool has a debug argument.

## Testing

No need to install to test a change - use the built binary directly:

```bash
cargo build --release
./target/release/claude-conversation-search session <session_id>
./target/release/claude-conversation-search search "query"
```

## Pre-commit Hook

`hooks/pre-commit` is the gate and the only description of what committing requires: it refuses
`#[allow(dead_code)]` in staged `.rs` files, then runs `cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --release`, in that
order. Install with:

```bash
bash hooks/install.sh
```

Before committing, run `cargo clippy --fix --allow-dirty --message-format=short && cargo fmt --all`
and let the hook be the gate rather than re-running each check separately.

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

**Cargo.lock Policy**: ignored on master. The release procedure commits it on a detached child of the tagged master commit, so the tag carries the lockfile that `--locked` release builds and the AUR PKGBUILD pin, and master never does. Never stage `Cargo.lock` on a branch.
