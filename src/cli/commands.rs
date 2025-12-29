use crate::cli::index;
use crate::shared::{self, CacheManager, SearchEngine, SearchQuery, SortOrder};
use anyhow::Result;
use clap::Subcommand;
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
        /// Results limit
        #[arg(long, default_value = "10")]
        limit: usize,
        /// Context lines (messages before/after match, like grep -C)
        #[arg(short = 'C', long, default_value = "2")]
        context: usize,
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
    },
    /// Cache management
    Cache {
        #[command(subcommand)]
        action: CacheAction,
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
        CliCommands::Mcp => unreachable!("MCP handled in main"),
        CliCommands::Search {
            query,
            project,
            limit,
            context,
        } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            // Auto-index before searching
            shared::auto_index(&index_path)?;
            search_conversations(&index_path, query, project, limit, context)?;
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
        CliCommands::Session { session_id, full } => {
            let config = shared::get_config();
            let index_path = config.get_cache_dir()?;
            shared::auto_index(&index_path)?;
            view_session(&index_path, session_id, full)?;
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

    if !stats.projects.is_empty() {
        println!("\nProject breakdown:");
        for project in stats.projects.iter().take(10) {
            println!(
                "  {} - {} files, {} entries (updated: {})",
                project.name,
                project.files,
                project.entries,
                project.last_updated.format("%Y-%m-%d")
            );
        }
        if stats.projects.len() > 10 {
            println!("  ... and {} more projects", stats.projects.len() - 10);
        }
    }

    Ok(())
}

fn clear_cache(index_path: &Path) -> Result<()> {
    let mut cache_manager = CacheManager::new(index_path)?;
    cache_manager.clear_cache()?;
    println!("Cache cleared successfully. Run 'claude-search index' to rebuild.");
    Ok(())
}

fn search_conversations(
    index_path: &Path,
    query_text: String,
    project_filter: Option<String>,
    limit: usize,
    context: usize,
) -> Result<()> {
    if !index_path.exists() {
        println!("Index not found. Please run 'claude-search index' first.");
        return Ok(());
    }

    let search_engine = SearchEngine::new(index_path)?;

    let query = SearchQuery {
        text: query_text,
        project_filter,
        session_filter: None,
        limit,
        sort_by: SortOrder::default(),
        after: None,
        before: None,
    };

    let results = search_engine.search_with_context(query, context, context)?;

    if results.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    println!("Found {} results (-C {}):\n", results.len(), context);

    for (i, result) in results.iter().enumerate() {
        print!("{}", result.format_compact(i));
        if i < results.len() - 1 {
            println!();
        }
    }

    Ok(())
}

fn show_topics(index_path: &Path, project_filter: Option<String>, limit: usize) -> Result<()> {
    if !index_path.exists() {
        println!("Index not found. Please run 'claude-search index' first.");
        return Ok(());
    }

    let search_engine = SearchEngine::new(index_path)?;

    // Get all conversations to analyze topics
    let query = SearchQuery {
        text: "*".to_string(), // Match everything
        project_filter: project_filter.clone(),
        session_filter: None,
        limit: 1000, // Large limit to get comprehensive topic analysis
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
            .entry(result.project.clone())
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

    // Top technologies
    if !tech_counts.is_empty() {
        println!("🔧 Top Technologies:");
        let mut sorted_tech: Vec<_> = tech_counts.iter().collect();
        sorted_tech.sort_by(|a, b| b.1.cmp(a.1));

        for (tech, count) in sorted_tech.iter().take(limit) {
            println!("   {tech} ({count})");
        }
        println!();
    }

    // Top programming languages
    if !lang_counts.is_empty() {
        println!("💻 Top Programming Languages:");
        let mut sorted_lang: Vec<_> = lang_counts.iter().collect();
        sorted_lang.sort_by(|a, b| b.1.cmp(a.1));

        for (lang, count) in sorted_lang.iter().take(limit) {
            println!("   {lang} ({count})");
        }
        println!();
    }

    // Top tools mentioned
    if !tool_counts.is_empty() {
        println!("🔨 Top Tools Mentioned:");
        let mut sorted_tools: Vec<_> = tool_counts.iter().collect();
        sorted_tools.sort_by(|a, b| b.1.cmp(a.1));

        for (tool, count) in sorted_tools.iter().take(limit) {
            println!("   {tool} ({count})");
        }
        println!();
    }

    // Project breakdown (if not filtering by project)
    if project_filter.is_none() && !project_counts.is_empty() {
        println!("📂 Project Activity:");
        let mut sorted_projects: Vec<_> = project_counts.iter().collect();
        sorted_projects.sort_by(|a, b| b.1.cmp(a.1));

        for (project, count) in sorted_projects.iter().take(limit) {
            println!("   {project} ({count} conversations)");
        }
    }

    Ok(())
}

fn show_stats(index_path: &Path, project_filter: Option<String>) -> Result<()> {
    if !index_path.exists() {
        println!("Index not found. Please run 'claude-search index' first.");
        return Ok(());
    }

    let cache_manager = CacheManager::new(index_path)?;
    let cache_stats = cache_manager.get_stats();
    let search_engine = SearchEngine::new(index_path)?;

    // Get conversation stats
    let query = SearchQuery {
        text: "*".to_string(),
        project_filter: project_filter.clone(),
        session_filter: None,
        limit: 2000,
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
            .entry(result.session_id.clone())
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

    println!("Conversation Analysis:");
    println!("  💬 Total conversations: {}", results.len());
    println!("  🏗️ Unique sessions: {}", session_counts.len());
    println!(
        "  📝 Conversations with code: {} ({:.1}%)",
        code_conversations,
        (code_conversations as f64 / results.len() as f64) * 100.0
    );
    println!(
        "  🚨 Conversations with errors: {} ({:.1}%)",
        error_conversations,
        (error_conversations as f64 / results.len() as f64) * 100.0
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
        let mut sorted_sessions: Vec<_> = session_counts.iter().collect();
        sorted_sessions.sort_by(|a, b| b.1.cmp(a.1));

        for (session_id, count) in sorted_sessions.iter().take(5) {
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

fn view_session(index_path: &Path, session_id: String, show_full: bool) -> Result<()> {
    if !index_path.exists() {
        println!("Index not found. Please run 'claude-search index' first.");
        return Ok(());
    }

    let search_engine = SearchEngine::new(index_path)?;
    let mut results = search_engine.get_session_messages(&session_id)?;

    if results.is_empty() {
        println!("No messages found for session: {session_id}");
        println!("Tip: Use 'claude-search stats' to see available session IDs");
        return Ok(());
    }

    // Sort by timestamp for chronological display
    results.sort_by_key(|r| r.timestamp);

    let project_path = results[0].project_path_display();
    let short_session = shared::short_uuid(&session_id);
    let time_range = format!(
        "{} - {}",
        results[0].timestamp.format("%Y-%m-%d %H:%M"),
        results.last().unwrap().timestamp.format("%H:%M")
    );

    // Header line with all key info
    println!(
        "📁 {} 🗒️ {} ({} msgs) ⏱️ {}",
        project_path,
        short_session,
        results.len(),
        time_range
    );

    // Collect tags
    let mut techs = std::collections::HashSet::new();
    let mut langs = std::collections::HashSet::new();
    let mut has_code = false;
    let mut has_errors = false;
    for r in &results {
        techs.extend(r.technologies.iter().cloned());
        langs.extend(r.code_languages.iter().cloned());
        has_code |= r.has_code;
        has_errors |= r.has_error;
    }
    let mut tags = Vec::new();
    if !techs.is_empty() {
        let mut t: Vec<_> = techs.into_iter().collect();
        t.sort();
        tags.push(t.join(","));
    }
    if !langs.is_empty() {
        let mut l: Vec<_> = langs.into_iter().collect();
        l.sort();
        tags.push(l.join(","));
    }
    if has_code {
        tags.push("code".to_string());
    }
    if has_errors {
        tags.push("error".to_string());
    }
    if !tags.is_empty() {
        println!("tags: {}", tags.join(" "));
    }
    println!();

    // Messages in dense format, skip non-displayable messages
    let max_content = if show_full { 2000 } else { 200 };
    for result in results.iter().filter(|r| r.is_displayable()) {
        let time = result.timestamp.format("%H:%M:%S");
        let content: String = result.content.chars().take(max_content).collect();
        let content = content.split_whitespace().collect::<Vec<_>>().join(" ");
        let ellipsis = if result.content.chars().count() > max_content {
            "..."
        } else {
            ""
        };
        println!(
            "[{}] {}: {}{}",
            time,
            result.role_display(),
            content,
            ellipsis
        );
    }

    if !show_full && results.iter().any(|r| r.content.chars().count() > 200) {
        println!("\nUse --full for complete content");
    }

    Ok(())
}
