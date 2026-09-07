pub(crate) mod schema;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::PathBuf;
use tracing::error;

use super::protocol::{tool_err, tool_ok};
use super::server::{McpServer, clear_index_dir};
use crate::shared::path_utils::{discover_jsonl_files, globally_active_session_jsonl};
use crate::shared::session_view::{self, SessionViewOpts, Window};
use crate::shared::{self, CacheManager, DisplayOptions, SearchQuery, SortOrder, get_config};

const HAIKU_CONTEXT_WINDOW: usize = 200_000;
const CONTEXT_SAFETY_MARGIN: f64 = 0.75;

/// Extract Vec<String> from JSON array value
fn json_strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Parsed arguments for the search_conversations tool.
/// `after`/`before` are kept as raw strings: date parsing failures are
/// reported as tool content (isError), not a protocol-level error, so
/// parsing them stays in the caller alongside that response building.
struct SearchArgs {
    query_text: String,
    debug_mode: bool,
    project_filter: Option<String>,
    session_filter: Option<String>,
    context_before: usize,
    context_after: usize,
    exclude_projects: Vec<String>,
    exclude_patterns: Vec<String>,
    limit: usize,
    sort_by: SortOrder,
    after: Option<String>,
    before: Option<String>,
    include: Vec<String>,
    truncate_length: usize,
}

impl SearchArgs {
    fn from_json(args: &Value) -> Result<SearchArgs> {
        let query_text = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?
            .to_string();

        let debug_mode = args
            .get("debug")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let project_filter = args
            .get("project")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let session_filter = args
            .get("session")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Parse grep-style context: -C (both), -B (before), -A (after)
        let context_c = args
            .get("-C")
            .and_then(|v| v.as_u64())
            .unwrap_or(2);
        let context_before = args
            .get("-B")
            .and_then(|v| v.as_u64())
            .unwrap_or(context_c) as usize;
        let context_after = args
            .get("-A")
            .and_then(|v| v.as_u64())
            .unwrap_or(context_c) as usize;

        let exclude_projects = json_strings(args.get("exclude_projects"));

        let exclude_patterns: Vec<String> = match args.get("exclude_patterns") {
            Some(v) if v.is_array() => json_strings(Some(v)),
            Some(v) => {
                let s = v
                    .as_str()
                    .ok_or_else(|| {
                        anyhow::anyhow!("'exclude_patterns' must be an array of strings")
                    })?;
                serde_json::from_str::<Vec<String>>(s)
                    .with_context(|| format!("parsing 'exclude_patterns' from '{s}'"))?
            }
            None => Vec::new(),
        };

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let sort_by = match args
            .get("sort_by")
            .and_then(|v| v.as_str())
            .unwrap_or("relevance")
        {
            "date_desc" => SortOrder::DateDesc,
            "date_asc" => SortOrder::DateAsc,
            _ => SortOrder::Relevance,
        };

        let after = args
            .get("after")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let before = args
            .get("before")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let include = json_strings(args.get("include"));

        let truncate_length = args
            .get("truncate_length")
            .and_then(|v| v.as_u64())
            .unwrap_or(300) as usize;

        Ok(SearchArgs {
            query_text,
            debug_mode,
            project_filter,
            session_filter,
            context_before,
            context_after,
            exclude_projects,
            exclude_patterns,
            limit,
            sort_by,
            after,
            before,
            include,
            truncate_length,
        })
    }
}

/// Active-session file plus staleness counts against the on-disk index.
/// Excludes the active session from the check itself (it is always being
/// written to), found via `globally_active_session_jsonl` since the MCP
/// server's cwd is fixed at startup and does not track the caller's
/// current project.
struct StalenessProbe {
    current_session_file: Option<PathBuf>,
    health: shared::FileHealthCounts,
}

fn probe_staleness(config: &shared::Config) -> Result<StalenessProbe> {
    let all_files = discover_jsonl_files()?;
    let current_session_file = globally_active_session_jsonl();
    let current_session_file_ref = current_session_file.as_deref();
    let files_for_stale_check: Vec<_> = all_files
        .iter()
        .filter(|f| Some(f.as_path()) != current_session_file_ref)
        .cloned()
        .collect();

    let cache = CacheManager::new(&config.get_cache_dir()?)?;
    let health = cache.quick_health_check(&files_for_stale_check);

    Ok(StalenessProbe {
        current_session_file,
        health,
    })
}

/// Merge configured and request-supplied exclude patterns and compile them,
/// refusing an invalid pattern rather than searching unfiltered.
fn compile_excludes(
    config: &shared::Config,
    extra: &[String],
) -> Result<(Vec<String>, Vec<regex::Regex>)> {
    let mut all_exclude_patterns = config
        .search
        .exclude_patterns
        .clone();
    all_exclude_patterns.extend(
        extra
            .iter()
            .cloned(),
    );
    let exclude_regexes = shared::compile_exclude_patterns(&all_exclude_patterns)?;
    Ok((all_exclude_patterns, exclude_regexes))
}

type DateBounds = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Parse the optional after/before bounds. A malformed date is reported to
/// the caller as tool content (isError), not a protocol-level error, so the
/// caller decides how to surface this function's `Err`.
fn parse_date_bounds(search_args: &SearchArgs) -> Result<DateBounds> {
    Ok((
        shared::parse_date_opt(
            search_args
                .after
                .as_deref(),
        )?,
        shared::parse_date_opt(
            search_args
                .before
                .as_deref(),
        )?,
    ))
}

/// Render the tool response body: optional debug line, exclude/staleness
/// notes, then the compact result list.
fn render_search_response(
    raw_args: &Value,
    search_args: &SearchArgs,
    all_exclude_patterns: &[String],
    health: &shared::FileHealthCounts,
    filtered: &[shared::SearchResultWithContext],
    display_opts: &DisplayOptions,
) -> String {
    let mut output = String::new();

    if search_args.debug_mode {
        output.push_str(&format!(
            "DEBUG: query={:?}, -B={}, -A={}, limit={}, exclude_projects={:?}, patterns={:?}\n\n",
            raw_args.get("query"),
            search_args.context_before,
            search_args.context_after,
            search_args.limit,
            search_args.exclude_projects,
            all_exclude_patterns
        ));
    }

    if !search_args
        .exclude_projects
        .is_empty()
        || !all_exclude_patterns.is_empty()
    {
        output.push_str(&format!(
            "Excluding: {} projects, {} patterns\n",
            search_args
                .exclude_projects
                .len(),
            all_exclude_patterns.len()
        ));
    }

    if health.stale > 0 || health.new_files > 0 {
        output.push_str(&format!(
            "Note: index is stale ({} modified, {} new files). Call reindex tool for fresher results.\n",
            health.stale, health.new_files
        ));
    }

    if filtered.is_empty() {
        output.push_str("No results found.\n");
    } else {
        for (i, result) in filtered
            .iter()
            .enumerate()
        {
            output.push_str(&result.format_compact_with_options(i, display_opts));
            if i < filtered.len() - 1 {
                output.push('\n');
            }
        }
        if filtered.len() == search_args.limit {
            output.push_str(&format!("\n+more: limit={}\n", search_args.limit));
        }
    }

    output
}

impl McpServer {
    pub(crate) async fn tool_search_conversations(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let search_args = SearchArgs::from_json(&args)?;

        let config = get_config();
        let probe = probe_staleness(config)?;

        let (all_exclude_patterns, exclude_regexes) =
            match compile_excludes(config, &search_args.exclude_patterns) {
                Ok(v) => v,
                Err(e) => return tool_err(format!("{e:#}")),
            };

        let (after, before) = match parse_date_bounds(&search_args) {
            Ok(bounds) => bounds,
            Err(e) => return tool_err(e.to_string()),
        };

        let display_opts = DisplayOptions {
            include_thinking: search_args
                .include
                .contains(&"thinking".to_string()),
            include_tools: search_args
                .include
                .contains(&"tools".to_string()),
            truncate_length: search_args.truncate_length,
        };

        let include_current_session = search_args
            .include
            .contains(&"current_session".to_string());

        // Get current session ID from file detected earlier
        let current_session_id: Option<String> = if !include_current_session {
            probe
                .current_session_file
                .as_ref()
                .and_then(|p| {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
        } else {
            None
        };

        let query = SearchQuery {
            text: search_args
                .query_text
                .clone(),
            project_filter: search_args
                .project_filter
                .clone(),
            session_filter: search_args
                .session_filter
                .clone(),
            limit: search_args.limit * 3,
            sort_by: search_args
                .sort_by
                .clone(),
            after,
            before,
        };

        let search_engine = &self.search_engine;
        let results_with_context = search_engine.search_with_context(
            query,
            search_args.context_before,
            search_args.context_after,
        )?;

        let filtered = shared::apply_search_filters(
            results_with_context,
            &shared::ResultFilter {
                exclude_projects: search_args
                    .exclude_projects
                    .clone(),
                exclude_regexes,
                active_session: current_session_id.clone(),
                limit: search_args.limit,
            },
        );

        tool_ok(render_search_response(
            &args,
            &search_args,
            &all_exclude_patterns,
            &probe.health,
            &filtered,
            &display_opts,
        ))
    }

    pub(crate) async fn tool_get_session_messages(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        let context_c = args
            .get("-C")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);
        let opts = SessionViewOpts {
            session_id: session_id.to_string(),
            truncate_length: args
                .get("truncate_length")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
            window: Window {
                center: args
                    .get("center_on")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                before: args
                    .get("-B")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(context_c) as usize,
                after: args
                    .get("-A")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(context_c) as usize,
                offset: args
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize,
                limit: args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50) as usize,
            },
        };

        let (entries, source) = session_view::load_session(&self.cache_dir, session_id)?;
        if entries.is_empty() {
            return tool_err(format!("No messages found for session {}", session_id));
        }

        let display = DisplayOptions {
            include_thinking: true,
            include_tools: true,
            truncate_length: opts.truncate_length,
        };

        tool_ok(session_view::render(&entries, &source, &opts, &display))
    }

    pub(crate) async fn tool_summarize_session(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        // Get session stats for size estimation
        let search_engine = &self.search_engine;
        let messages = search_engine.get_session_messages(session_id)?;
        let msg_count = messages.len();
        let total_chars: usize = messages
            .iter()
            .map(|m| {
                m.content
                    .len()
            })
            .sum();
        let approx_tokens = total_chars / 4; // rough estimate: ~4 chars per token

        let safe_limit = (HAIKU_CONTEXT_WINDOW as f64 * CONTEXT_SAFETY_MARGIN) as usize;
        let size_note = if approx_tokens > safe_limit {
            " (large - may need multiple agents)"
        } else {
            ""
        };

        let output = format!(
            r#"Session {session_id}: {msg_count} messages, ~{approx_tokens} tokens{size_note}

Task(
  subagent_type: "general-purpose",
  model: "haiku",
  prompt: "Summarize session {session_id}:
1. Call get_session_messages(session_id=\"{session_id}\")
2. If output ends with '+more: offset=N', call again with that offset
3. Repeat until no '+more' appears
4. Return a concise summary: topic, key decisions, outcome"
)"#
        );

        tool_ok(output)
    }

    pub(crate) async fn tool_get_messages(&self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let ids = json_strings(args.get("ids"));

        if ids.is_empty() {
            return tool_err("No message IDs provided");
        }

        let search_engine = &self.search_engine;
        let messages = search_engine.get_messages_by_uuid(&ids)?;

        if messages.is_empty() {
            return tool_ok("No messages found for provided IDs");
        }

        let mut output = String::new();
        for msg in &messages {
            output.push_str(&format!(
                "💬 {} 📅 {} [{}]\n{}\n\n",
                &msg.uuid[..8.min(
                    msg.uuid
                        .len()
                )],
                msg.timestamp
                    .format("%Y-%m-%d %H:%M"),
                msg.message_type,
                msg.content
            ));
        }

        tool_ok(output)
    }

    #[cfg(unix)]
    pub(crate) async fn tool_respawn(&self) -> Result<Value> {
        // Try to find the release binary first, fallback to current_exe
        let current_dir = std::env::current_dir().context("getting the current directory")?;

        let release_path = current_dir.join("target/release/claude-conversation-search");
        let exe_path = if release_path.exists() {
            release_path
        } else {
            std::env::current_exe().context("getting the current executable path")?
        };

        // Schedule respawn after a short delay to allow response to be sent
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Replace current process with new instance using exec
            let args: Vec<String> = std::env::args().collect();
            let err = exec::execvp(&exe_path, &args);
            error!("Failed to exec with {}: {}", exe_path.display(), err);
        });

        tool_ok("Respawning MCP server...")
    }

    #[cfg(windows)]
    pub(crate) async fn tool_respawn(&self) -> Result<Value> {
        tool_err("respawn_server is not supported on Windows")
    }

    pub(crate) async fn tool_reindex(&mut self, args: Option<Value>) -> Result<Value> {
        let args = args.unwrap_or_default();
        let full_rebuild = args
            .get("full")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let all_files = discover_jsonl_files()?;

        let _lock = match crate::shared::ExclusiveIndexAccess::acquire() {
            Ok(lock) => lock,
            Err(e) if crate::shared::is_index_busy(&e) => {
                return tool_err("Another process is indexing, reindex was not run");
            }
            Err(e) => {
                return tool_err(format!("Reindex could not lock the index: {e:#}"));
            }
        };

        let result = if full_rebuild {
            if self
                .cache_dir
                .exists()
            {
                clear_index_dir(&self.cache_dir)?;
            }
            let mut indexer = crate::shared::SearchIndexer::new(&self.cache_dir)?;
            let mut cache = crate::shared::CacheManager::new(&self.cache_dir)?;
            cache.update_incremental(&mut indexer, all_files)?;
            self.search_engine = crate::shared::SearchEngine::from_cache(&self.cache_dir, &cache)?;
            "Full rebuild complete".to_string()
        } else {
            // Incremental update
            let mut indexer = crate::shared::SearchIndexer::open(&self.cache_dir)?;
            let mut cache = crate::shared::CacheManager::new(&self.cache_dir)?;
            let health = cache.quick_health_check(&all_files);
            cache.update_incremental(&mut indexer, all_files)?;
            self.search_engine = crate::shared::SearchEngine::from_cache(&self.cache_dir, &cache)?;
            format!(
                "Incremental update: {} stale + {} new files reindexed",
                health.stale, health.new_files
            )
        };
        tool_ok(result)
    }
}
