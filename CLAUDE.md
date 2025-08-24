# Claude Code Project Instructions

## Development Notes

- Prefer `cargo check` over `cargo build` when just checking for compilation errors - it's much quicker
- Use `cargo build` only when you need the actual binary
- **Multi-binary project**: To avoid dead code warnings, build each binary with only its features: `cargo check --bin claude-search --features cli --no-default-features` or `cargo check --bin claude-search-mcp --features mcp --no-default-features`
- **IMPORTANT**: Running `claude-search index` creates massive Tantivy logging output (thousands of lines). Always redirect both stdout and stderr to a file or `/dev/null`. Never read its output directly in the terminal.
  - Correct: `./target/release/claude-search index >/dev/null 2>&1`
  - Wrong: `./target/release/claude-search index` (will flood terminal)

## Pre-commit Checklist

Before any git commit, run these commands in order until there's no output:

1. `cargo test` - Run all tests
2. `cargo clippy --fix --allow-dirty` - Fix clippy warnings automatically
3. `cargo fmt` - Format code consistently

**Important:** 
- Fix all warnings properly - do NOT use underscore prefixes (`_var_name`) to hide unused variables
- Remove unused code instead of suppressing warnings
- All clippy warnings must be resolved before committing