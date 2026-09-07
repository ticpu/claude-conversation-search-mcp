use crate::cli::index;
use crate::shared::session_view::{self, SessionViewOpts, Window};
use crate::shared::{self, CacheManager, DisplayOptions, SearchEngine, SearchQuery, SortOrder};
use anyhow::Result;
use chrono::Utc;
use clap::{Subcommand, ValueEnum};
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

#[derive(Subcommand)]
pub enum CliCommands {
    /// Index management
    Index {
        #[command(subcommand)]
        action: Option<IndexAction>,
    },
    /// Search conversations (auto-indexes if needed)
    Search {
        /// Search query
        query: String,
        /// Filter by project
        #[arg(long)]
        project: Option<String>,
        /// Filter by session ID (prefix match)
        #[arg(long)]
        session: Option<String>,
        /// Results limit
        #[arg(long, default_value = "10")]
        limit: usize,
        /// Context lines before and after match (like grep -C)
        #[arg(short = 'C', default_value = "2")]
        context: usize,
        /// Context lines before match (like grep -B)
        #[arg(short = 'B')]
        ctx_before: Option<usize>,
        /// Context lines after match (like grep -A)
        #[arg(short = 'A')]
        ctx_after: Option<usize>,
        /// Exclude projects by name
        #[arg(long)]
        exclude_project: Vec<String>,
        /// Exclude results matching regex patterns
        #[arg(long)]
        exclude_pattern: Vec<String>,
        /// Sort order
        #[arg(long, value_enum, default_value = "relevance")]
        sort: SortArg,
        /// Results after date (YYYY-MM-DD or ISO 8601)
        #[arg(long)]
        after: Option<String>,
        /// Results before date (YYYY-MM-DD or ISO 8601)
        #[arg(long)]
        before: Option<String>,
        /// Include extra content types
        #[arg(long, value_enum)]
        include: Vec<IncludeArg>,
        /// Characters shown per message (0 = full content)
        #[arg(long, default_value = "300")]
        truncate: usize,
    },
    /// Show technology topics and their usage across conversations
    Topics {
        /// Filter by project
        #[arg(long)]
        project: Option<String>,
        /// Results limit
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Show detailed cache and conversation statistics
    Stats {
        /// Filter by project
        #[arg(long)]
        project: Option<String>,
    },
    /// View specific session conversations
    Session {
        /// Session ID to view
        session_id: String,
        /// Show full content (not just snippets)
        #[arg(long)]
        full: bool,
        /// Center on a message UUID (prefix match)
        #[arg(long)]
        center: Option<String>,
        /// Context messages before and after center (like grep -C)
        #[arg(short = 'C', default_value = "5")]
        context: usize,
        /// Context messages before center (like grep -B)
        #[arg(short = 'B')]
        before: Option<usize>,
        /// Context messages after center (like grep -A)
        #[arg(short = 'A')]
        after: Option<usize>,
        /// Characters shown per message (0 = full content)
        #[arg(long, default_value = "200")]
        truncate: usize,
        /// Skip this many messages before displaying
        #[arg(long, default_value = "0")]
        offset: usize,
        /// Messages shown (0 = all)
        #[arg(long, default_value = "0")]
        limit: usize,
    },
    /// Summarize a session using Claude (runs in jailed empty dir)
    Summary {
        /// Session ID to summarize
        session_id: String,
    },
    /// Cache management
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    /// Run as MCP server
    Mcp,
    /// Register with Claude MCP
    Install {
        /// Use project scope instead of user scope
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand)]
pub enum CacheAction {
    /// Show cache statistics
    Info,
    /// Clear cache and rebuild
    Clear,
}

#[derive(ValueEnum, Clone, Default)]
pub enum SortArg {
    #[default]
    Relevance,
    DateDesc,
    DateAsc,
}

#[derive(ValueEnum, Clone, PartialEq)]
pub enum IncludeArg {
    Thinking,
    Tools,
}

impl From<SortArg> for SortOrder {
    fn from(s: SortArg) -> Self {
        match s {
            SortArg::Relevance => SortOrder::Relevance,
            SortArg::DateDesc => SortOrder::DateDesc,
            SortArg::DateAsc => SortOrder::DateAsc,
        }
    }
}

#[derive(Subcommand, Default)]
pub enum IndexAction {
    /// Show index status and statistics (default)
    #[default]
    Status,
    /// Force full rebuild of the index
    Rebuild,
    /// Clean up deleted entries from index
    Vacuum,
}

pub fn setup_logging(verbose: u8) {
    let level = match verbose {
        0 => Level::ERROR,
        1 => Level::WARN,
        2 => Level::INFO,
        _ => Level::DEBUG,
    };

    FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .init();
}

pub fn run_cli(verbose: u8, command: CliCommands) -> Result<()> {
    setup_logging(verbose);

    match command {
        CliCommands::Index { action } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            match action.unwrap_or_default() {
                IndexAction::Status => index::show_status(&index_path)?,
                IndexAction::Rebuild => index::rebuild(&index_path)?,
                IndexAction::Vacuum => index::vacuum(&index_path)?,
            }
        }
        CliCommands::Completions { .. } => unreachable!("Completions handled in main"),
        CliCommands::Mcp => unreachable!("MCP handled in main"),
        CliCommands::Search {
            query,
            project,
            session,
            limit,
            context,
            ctx_before,
            ctx_after,
            exclude_project,
            exclude_pattern,
            sort,
            after,
            before,
            include,
            truncate,
        } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            shared::auto_index(&index_path)?;
            let cb = ctx_before.unwrap_or(context);
            let ca = ctx_after.unwrap_or(context);
            let opts = SearchOpts {
                query,
                project,
                session,
                limit,
                context_before: cb,
                context_after: ca,
                exclude_projects: exclude_project,
                exclude_patterns: exclude_pattern,
                sort: sort.into(),
                after: after
                    .as_deref()
                    .map(shared::parse_date)
                    .transpose()?,
                before: before
                    .as_deref()
                    .map(shared::parse_date)
                    .transpose()?,
                display: DisplayOptions {
                    include_thinking: include.contains(&IncludeArg::Thinking),
                    include_tools: include.contains(&IncludeArg::Tools),
                    truncate_length: truncate,
                },
            };
            search_conversations(&index_path, opts)?;
        }
        CliCommands::Topics { project, limit } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            shared::auto_index(&index_path)?;
            show_topics(&index_path, project, limit)?;
        }
        CliCommands::Stats { project } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            shared::auto_index(&index_path)?;
            show_stats(&index_path, project)?;
        }
        CliCommands::Session {
            session_id,
            full,
            center,
            context,
            before,
            after,
            truncate,
            offset,
            limit,
        } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            shared::auto_index(&index_path)?;
            let opts = SessionViewOpts {
                session_id,
                truncate_length: if full { 0 } else { truncate },
                window: Window {
                    center,
                    before: before.unwrap_or(context),
                    after: after.unwrap_or(context),
                    offset,
                    limit,
                },
            };
            view_session(&index_path, &opts)?;
        }
        CliCommands::Summary { session_id } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            shared::auto_index(&index_path)?;
            summarize_session(&index_path, session_id)?;
        }
        CliCommands::Cache { action } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            match action {
                CacheAction::Info => show_cache_info(&index_path)?,
                CacheAction::Clear => clear_cache(&index_path)?,
            }
        }
        CliCommands::Install { project } => install(project)?,
    }

    Ok(())
}

fn install(project_scope: bool) -> Result<()> {
    use std::process::Command;

    let exe = std::env::current_exe()?;
    let exe_path = exe
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid exe path"))?;
    let scope = if project_scope { "project" } else { "user" };

    let _ = Command::new("claude")
        .args(["mcp", "remove", "-s", scope, "claude-conversation-search"])
        .status();

    let status = Command::new("claude")
        .args([
            "mcp",
            "add",
            "-s",
            scope,
            "claude-conversation-search",
            exe_path,
        ])
        .status()?;

    if !status.success() {
        anyhow::bail!("claude mcp add failed");
    }

    println!("{}", exe_path);
    Ok(())
}

fn show_cache_info(index_path: &Path) -> Result<()> {
    let cache_manager = CacheManager::new(index_path)?;
    let stats = cache_manager.get_stats();

    println!("Cache Statistics:");
    println!("  Total files indexed: {}", stats.total_files);
    println!("  Total entries: {}", stats.total_entries);
    println!("  Cache size: {:.2} MB", stats.cache_size_mb);

    if let Some(last_updated) = stats.last_updated {
        println!(
            "  Last updated: {}",
            last_updated.format("%Y-%m-%d %H:%M:%S UTC")
        );
    }

    if !stats
        .projects
        .is_empty()
    {
        println!("\nProject breakdown:");
        for project in stats
            .projects
            .iter()
            .take(10)
        {
            println!(
                "  {} - {} files, {} entries (updated: {})",
                project.name,
                project.files,
                project.entries,
                project
                    .last_updated
                    .format("%Y-%m-%d")
            );
        }
        if stats
            .projects
            .len()
            > 10
        {
            println!(
                "  ... and {} more projects",
                stats
                    .projects
                    .len()
                    - 10
            );
        }
    }

    Ok(())
}

fn clear_cache(index_path: &Path) -> Result<()> {
    let mut cache_manager = CacheManager::new(index_path)?;
    cache_manager.clear_cache()?;
    println!("Cache cleared successfully. Run 'claude-conversation-search index' to rebuild.");
    Ok(())
}

struct SearchOpts {
    query: String,
    project: Option<String>,
    session: Option<String>,
    limit: usize,
    context_before: usize,
    context_after: usize,
    exclude_projects: Vec<String>,
    exclude_patterns: Vec<String>,
    sort: SortOrder,
    after: Option<chrono::DateTime<Utc>>,
    before: Option<chrono::DateTime<Utc>>,
    display: DisplayOptions,
}

/// True if the index exists; otherwise prints the standard "not found" message.
fn index_exists_or_notify(index_path: &Path) -> bool {
    if index_path.exists() {
        return true;
    }
    println!("Index not found. Please run 'claude-conversation-search index' first.");
    false
}

fn search_conversations(index_path: &Path, opts: SearchOpts) -> Result<()> {
    if !index_exists_or_notify(index_path) {
        return Ok(());
    }

    let config = shared::get_config();
    let mut all_exclude_patterns = config
        .search
        .exclude_patterns
        .clone();
    all_exclude_patterns.extend(opts.exclude_patterns);

    let exclude_regexes: Vec<Regex> = all_exclude_patterns
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

    let cache = CacheManager::new(index_path)?;
    let search_engine = SearchEngine::new(
        index_path,
        cache
            .get_session_counts()
            .clone(),
    )?;

    let query = SearchQuery {
        text: opts.query,
        project_filter: opts.project,
        session_filter: opts.session,
        limit: opts.limit * 3,
        sort_by: opts.sort,
        after: opts.after,
        before: opts.before,
    };

    let results =
        search_engine.search_with_context(query, opts.context_before, opts.context_after)?;

    let filtered = shared::apply_search_filters(
        results,
        &shared::ResultFilter {
            exclude_projects: opts.exclude_projects,
            exclude_regexes,
            active_session: None,
            limit: opts.limit,
        },
    );

    if filtered.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    let ctx_display = if opts.context_before == opts.context_after {
        format!("-C {}", opts.context_before)
    } else {
        format!("-B {} -A {}", opts.context_before, opts.context_after)
    };
    println!("Found {} results ({}):\n", filtered.len(), ctx_display);

    for (i, result) in filtered
        .iter()
        .enumerate()
    {
        print!("{}", result.format_compact_with_options(i, &opts.display));
        if i < filtered.len() - 1 {
            println!();
        }
    }

    Ok(())
}

/// Print a ranked topic section: header, then up to `limit` entries sorted by
/// count descending, formatted as "   item (count<count_suffix>)".
fn print_topic_section(
    header: &str,
    counts: &HashMap<String, i32>,
    limit: usize,
    count_suffix: &str,
    trailing_blank_line: bool,
) {
    if counts.is_empty() {
        return;
    }
    println!("{header}");
    let mut sorted: Vec<_> = counts
        .iter()
        .collect();
    sorted.sort_by(|a, b| {
        b.1.cmp(a.1)
    });

    for (item, count) in sorted
        .iter()
        .take(limit)
    {
        println!("   {item} ({count}{count_suffix})");
    }
    if trailing_blank_line {
        println!();
    }
}

fn show_topics(index_path: &Path, project_filter: Option<String>, limit: usize) -> Result<()> {
    if !index_exists_or_notify(index_path) {
        return Ok(());
    }

    let cache = CacheManager::new(index_path)?;
    let search_engine = SearchEngine::new(
        index_path,
        cache
            .get_session_counts()
            .clone(),
    )?;

    // Get all conversations to analyze topics
    let query = SearchQuery {
        text: "*".to_string(), // Match everything
        project_filter: project_filter.clone(),
        session_filter: None,
        limit: 100_000,
        sort_by: SortOrder::default(),
        after: None,
        before: None,
    };

    let results = search_engine.search(query)?;

    // Count technology mentions
    let mut tech_counts = HashMap::new();
    let mut lang_counts = HashMap::new();
    let mut tool_counts = HashMap::new();
    let mut project_counts = HashMap::new();

    for result in &results {
        project_counts
            .entry(
                result
                    .project
                    .clone(),
            )
            .and_modify(|count| *count += 1)
            .or_insert(1);

        for tech in &result.technologies {
            tech_counts
                .entry(tech.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }

        for lang in &result.code_languages {
            lang_counts
                .entry(lang.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }

        for tool in &result.tools_mentioned {
            tool_counts
                .entry(tool.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }
    }

    println!(
        "Topic Analysis - {} conversations analyzed\n",
        results.len()
    );

    if let Some(ref project) = project_filter {
        println!("Filtered by project: {project}\n");
    }

    print_topic_section("🔧 Top Technologies:", &tech_counts, limit, "", true);
    print_topic_section(
        "💻 Top Programming Languages:",
        &lang_counts,
        limit,
        "",
        true,
    );
    print_topic_section("🔨 Top Tools Mentioned:", &tool_counts, limit, "", true);

    // Project breakdown (if not filtering by project)
    if project_filter.is_none() {
        print_topic_section(
            "📂 Project Activity:",
            &project_counts,
            limit,
            " conversations",
            false,
        );
    }

    Ok(())
}

fn show_stats(index_path: &Path, project_filter: Option<String>) -> Result<()> {
    if !index_exists_or_notify(index_path) {
        return Ok(());
    }

    let cache_manager = CacheManager::new(index_path)?;
    let cache_stats = cache_manager.get_stats();
    let search_engine = SearchEngine::new(
        index_path,
        cache_manager
            .get_session_counts()
            .clone(),
    )?;

    // Get conversation stats
    let query = SearchQuery {
        text: "*".to_string(),
        project_filter: project_filter.clone(),
        session_filter: None,
        limit: 1_000_000,
        sort_by: SortOrder::default(),
        after: None,
        before: None,
    };

    let results = search_engine.search(query)?;

    let mut code_conversations = 0;
    let mut error_conversations = 0;
    let mut total_interactions = 0;
    let mut session_counts = HashMap::new();

    for result in &results {
        if result.has_code {
            code_conversations += 1;
        }
        if result.has_error {
            error_conversations += 1;
        }
        total_interactions += result.interaction_count;

        session_counts
            .entry(
                result
                    .session_id
                    .clone(),
            )
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }

    if let Some(ref project) = project_filter {
        println!("📊 Statistics for project: {project}\n");
    } else {
        println!("📊 Overall Statistics\n");
    }

    println!("Cache Information:");
    println!("  📁 Total files indexed: {}", cache_stats.total_files);
    println!("  💾 Cache size: {:.2} MB", cache_stats.cache_size_mb);

    if let Some(last_updated) = cache_stats.last_updated {
        println!(
            "  🕒 Last updated: {}",
            last_updated.format("%Y-%m-%d %H:%M UTC")
        );
    }

    println!();

    let total_indexed = cache_stats.total_entries as usize;
    let sampled = results.len();

    println!("Conversation Analysis:");
    println!("  💬 Total messages indexed: {}", total_indexed);
    println!("  🏗️ Unique sessions: {}", session_counts.len());
    if sampled < total_indexed {
        println!(
            "  📊 Sampled for stats: {} ({:.1}%)",
            sampled,
            (sampled as f64 / total_indexed as f64) * 100.0
        );
    }
    println!(
        "  📝 Messages with code: {} ({:.1}%)",
        code_conversations,
        (code_conversations as f64 / sampled as f64) * 100.0
    );
    println!(
        "  🚨 Messages with errors: {} ({:.1}%)",
        error_conversations,
        (error_conversations as f64 / sampled as f64) * 100.0
    );
    println!(
        "  💬 Total interactions: {} (avg: {} per conversation)",
        total_interactions,
        if !results.is_empty() {
            total_interactions / results.len()
        } else {
            0
        }
    );

    // Show most active sessions
    if !session_counts.is_empty() {
        println!();
        println!("Most Active Sessions:");
        let mut sorted_sessions: Vec<_> = session_counts
            .iter()
            .collect();
        sorted_sessions.sort_by(|a, b| {
            b.1.cmp(a.1)
        });

        for (session_id, count) in sorted_sessions
            .iter()
            .take(5)
        {
            let short_id = if session_id.len() > 12 {
                format!("{}…", &session_id[..12])
            } else {
                session_id.to_string()
            };
            println!("  {short_id} ({count} messages)");
        }
    }

    Ok(())
}

fn view_session(index_path: &Path, opts: &SessionViewOpts) -> Result<()> {
    let (entries, source) = session_view::load_session(index_path, &opts.session_id)?;
    if entries.is_empty() {
        anyhow::bail!("no messages found for session {}", opts.session_id);
    }

    let display = DisplayOptions {
        include_thinking: true,
        include_tools: true,
        truncate_length: opts.truncate_length,
    };
    print!(
        "{}",
        session_view::render(&entries, &source, opts, &display)
    );

    if opts.truncate_length > 0 {
        println!("\nUse --full or --truncate 0 for complete content");
    }

    Ok(())
}

fn summarize_session(index_path: &Path, session_id: String) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let (entries, _) = session_view::load_session(index_path, &session_id)?;
    if entries.is_empty() {
        anyhow::bail!("no messages found for session {session_id}");
    }

    let mut conversation = String::new();
    for entry in session_view::displayable_entries(&entries) {
        let content: String = entry
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        conversation.push_str(&format!(
            "{}: {}\n",
            entry
                .message_type
                .short_name(),
            content
        ));
    }

    // Create jail directory in temp dir (XDG_RUNTIME_DIR on Unix, %TEMP% on Windows)
    #[cfg(unix)]
    let temp_dir = std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    #[cfg(windows)]
    let temp_dir = std::env::temp_dir();
    let jail_dir = temp_dir.join("claude-summary-jail");
    std::fs::create_dir_all(&jail_dir)?;

    let prompt = format!(
        "Summarize this conversation concisely. Include: topic, key decisions, outcome.\n\n{}",
        conversation
    );

    // Run claude --print in jailed directory with no tools, using haiku for cost
    let mut child = Command::new("claude")
        .args([
            "--print",
            "--tools",
            "",
            "--no-session-persistence",
            "--model",
            "haiku",
        ])
        .current_dir(&jail_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    if let Some(mut stdin) = child
        .stdin
        .take()
    {
        stdin.write_all(prompt.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("claude exited with status: {}", status);
    }

    Ok(())
}
