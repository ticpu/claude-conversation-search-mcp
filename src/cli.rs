use crate::cache::CacheManager;
use crate::indexer::SearchIndexer;
#[cfg(feature = "cli")]
use crate::models::SearchQuery;
#[cfg(feature = "cli")]
use crate::search::SearchEngine;
use anyhow::Result;
#[cfg(feature = "cli")]
use clap::Parser;
use clap::Subcommand;
use dirs::home_dir;
use glob::glob;
#[cfg(feature = "cli")]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::info;
#[cfg(feature = "cli")]
use tracing::warn;

#[cfg(feature = "cli")]
#[derive(Parser)]
#[command(name = "claude-search")]
#[command(about = "Search Claude Code conversations")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[cfg(feature = "cli")]
#[derive(Subcommand)]
pub enum Commands {
    /// Build/update search index
    Index {
        /// Force full rebuild
        #[arg(long)]
        rebuild: bool,
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
}

#[derive(Subcommand)]
pub enum CacheAction {
    /// Show cache statistics
    Info,
    /// Clear cache and rebuild
    Clear,
}

#[cfg(feature = "cli")]
pub async fn run_cli() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Index { rebuild } => {
            let index_path = get_cache_dir()?;
            index_conversations(&index_path, rebuild).await?;
        }
        Commands::Search {
            query,
            project,
            limit,
        } => {
            let index_path = get_cache_dir()?;
            // Auto-index before searching
            auto_index(&index_path).await?;
            search_conversations(&index_path, query, project, limit).await?;
        }
        Commands::Topics { project, limit } => {
            let index_path = get_cache_dir()?;
            auto_index(&index_path).await?;
            show_topics(&index_path, project, limit).await?;
        }
        Commands::Stats { project } => {
            let index_path = get_cache_dir()?;
            auto_index(&index_path).await?;
            show_stats(&index_path, project).await?;
        }
        Commands::Session { session_id, full } => {
            let index_path = get_cache_dir()?;
            auto_index(&index_path).await?;
            view_session(&index_path, session_id, full).await?;
        }
        Commands::Cache { action } => {
            let index_path = get_cache_dir()?;
            match action {
                CacheAction::Info => show_cache_info(&index_path).await?,
                CacheAction::Clear => clear_cache(&index_path).await?,
            }
        }
    }

    Ok(())
}

#[cfg(feature = "cli")]
async fn index_conversations(index_path: &Path, rebuild: bool) -> Result<()> {
    info!("Starting indexing process...");

    let mut cache_manager = CacheManager::new(index_path)?;

    if rebuild {
        cache_manager.clear_cache()?;
    }

    let mut indexer = if index_path.join("meta.json").exists() {
        SearchIndexer::open(index_path)?
    } else {
        SearchIndexer::new(index_path)?
    };

    let claude_dir = get_claude_dir()?;
    let pattern = claude_dir.join("projects/**/*.jsonl");
    let pattern_str = pattern.to_string_lossy();

    info!("Scanning for JSONL files in: {}", pattern_str);

    let mut all_files = Vec::new();
    for entry in glob(&pattern_str)? {
        match entry {
            Ok(path) => all_files.push(path),
            Err(e) => warn!("Failed to access file: {}", e),
        }
    }

    cache_manager.update_incremental(&mut indexer, all_files)?;
    Ok(())
}

pub async fn auto_index(index_path: &Path) -> Result<()> {
    let mut cache_manager = CacheManager::new(index_path)?;

    let mut indexer = if index_path.join("meta.json").exists() {
        SearchIndexer::open(index_path)?
    } else {
        info!("No index found, creating new one...");
        SearchIndexer::new(index_path)?
    };

    let claude_dir = get_claude_dir()?;
    let pattern = claude_dir.join("projects/**/*.jsonl");
    let pattern_str = pattern.to_string_lossy();

    let mut all_files = Vec::new();
    // Silently skip errors during auto-indexing
    for path in glob(&pattern_str)?.flatten() {
        all_files.push(path);
    }

    cache_manager.update_incremental(&mut indexer, all_files)?;
    Ok(())
}

#[cfg(feature = "cli")]
async fn show_cache_info(index_path: &Path) -> Result<()> {
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

#[cfg(feature = "cli")]
async fn clear_cache(index_path: &Path) -> Result<()> {
    let mut cache_manager = CacheManager::new(index_path)?;
    cache_manager.clear_cache()?;
    println!("Cache cleared successfully. Run 'claude-search index' to rebuild.");
    Ok(())
}

#[cfg(feature = "cli")]
async fn search_conversations(
    index_path: &Path,
    query_text: String,
    project_filter: Option<String>,
    limit: usize,
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
    };

    let results = search_engine.search(query)?;

    if results.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    println!("Found {} results:\n", results.len());

    for (i, result) in results.iter().enumerate() {
        println!(
            "{}. [{}] {} (score: {:.2})",
            i + 1,
            result.project,
            result.timestamp.format("%Y-%m-%d %H:%M"),
            result.score
        );

        // Show metadata tags
        let mut tags = Vec::new();

        if !result.technologies.is_empty() {
            tags.push(format!("🔧 {}", result.technologies.join(", ")));
        }

        if !result.code_languages.is_empty() {
            tags.push(format!("💻 {}", result.code_languages.join(", ")));
        }

        if result.has_code {
            tags.push("📝 code".to_string());
        }

        if result.has_error {
            tags.push("🚨 error".to_string());
        }

        if !result.tools_mentioned.is_empty() && result.tools_mentioned.len() <= 3 {
            tags.push(format!("🔨 {}", result.tools_mentioned.join(", ")));
        }

        tags.push(format!("📊 {} words", result.word_count));

        if !tags.is_empty() {
            println!("   {}", tags.join(" • "));
        }

        println!("   Session: {}", result.session_id);
        println!("   {}\n", result.snippet);
    }

    Ok(())
}

pub fn get_claude_dir() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    let claude_dir = home.join(".claude");
    if claude_dir.exists() {
        return Ok(claude_dir);
    }

    let config_claude_dir = home.join(".config").join("claude");
    if config_claude_dir.exists() {
        return Ok(config_claude_dir);
    }

    Ok(claude_dir)
}

#[cfg(feature = "cli")]
async fn show_topics(
    index_path: &Path,
    project_filter: Option<String>,
    limit: usize,
) -> Result<()> {
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

#[cfg(feature = "cli")]
async fn show_stats(index_path: &Path, project_filter: Option<String>) -> Result<()> {
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
    };

    let results = search_engine.search(query)?;

    let mut code_conversations = 0;
    let mut error_conversations = 0;
    let mut total_words = 0;
    let mut session_counts = HashMap::new();

    for result in &results {
        if result.has_code {
            code_conversations += 1;
        }
        if result.has_error {
            error_conversations += 1;
        }
        total_words += result.word_count;

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
        "  📊 Total words: {} (avg: {} per conversation)",
        total_words,
        if !results.is_empty() {
            total_words / results.len()
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
                format!("{}...", &session_id[..12])
            } else {
                session_id.to_string()
            };
            println!("  {short_id} ({count} messages)");
        }
    }

    Ok(())
}

#[cfg(feature = "cli")]
async fn view_session(index_path: &Path, session_id: String, show_full: bool) -> Result<()> {
    if !index_path.exists() {
        println!("Index not found. Please run 'claude-search index' first.");
        return Ok(());
    }

    let search_engine = SearchEngine::new(index_path)?;

    // Search for the specific session using field query
    let query = SearchQuery {
        text: format!("session_id:{session_id}"),
        project_filter: None,
        session_filter: None,
        limit: 100, // Sessions can be long
    };

    let results = search_engine.search(query)?;

    if results.is_empty() {
        println!("No conversations found for session: {session_id}");
        println!("\nTip: Use 'claude-search stats' to see available session IDs");
        return Ok(());
    }

    // Sort results by timestamp
    let mut sorted_results = results;
    sorted_results.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    println!("📋 Session: {session_id}");
    println!("📂 Project: {}", sorted_results[0].project);
    println!(
        "🕒 Time range: {} to {}",
        sorted_results[0].timestamp.format("%Y-%m-%d %H:%M"),
        sorted_results
            .last()
            .unwrap()
            .timestamp
            .format("%Y-%m-%d %H:%M")
    );
    println!("💬 Total messages: {}\n", sorted_results.len());

    // Show session-level metadata
    let mut session_techs = std::collections::HashSet::new();
    let mut session_langs = std::collections::HashSet::new();
    let mut session_tools = std::collections::HashSet::new();
    let mut has_code = false;
    let mut has_errors = false;

    for result in &sorted_results {
        session_techs.extend(result.technologies.iter().cloned());
        session_langs.extend(result.code_languages.iter().cloned());
        session_tools.extend(result.tools_mentioned.iter().cloned());
        if result.has_code {
            has_code = true;
        }
        if result.has_error {
            has_errors = true;
        }
    }

    let mut session_tags = Vec::new();
    if !session_techs.is_empty() {
        let mut techs: Vec<_> = session_techs.into_iter().collect();
        techs.sort();
        session_tags.push(format!("🔧 {}", techs.join(", ")));
    }
    if !session_langs.is_empty() {
        let mut langs: Vec<_> = session_langs.into_iter().collect();
        langs.sort();
        session_tags.push(format!("💻 {}", langs.join(", ")));
    }
    if has_code {
        session_tags.push("📝 code".to_string());
    }
    if has_errors {
        session_tags.push("🚨 errors".to_string());
    }

    if !session_tags.is_empty() {
        println!("Session topics: {}\n", session_tags.join(" • "));
    }

    println!("Messages:");
    println!("{}", "─".repeat(80));

    for (i, result) in sorted_results.iter().enumerate() {
        println!(
            "{}. {} | Score: {:.2}",
            i + 1,
            result.timestamp.format("%H:%M:%S"),
            result.score
        );

        if show_full {
            println!("{}", result.content);
        } else {
            println!("{}", result.snippet);
        }

        if i < sorted_results.len() - 1 {
            println!("{}", "─".repeat(40));
        }
    }

    if !show_full && sorted_results.len() > 3 {
        println!("\nUse --full flag to see complete message content");
    }

    Ok(())
}

pub fn get_cache_dir() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let cache_dir = home.join(".cache").join("claude-search");
    Ok(cache_dir)
}
