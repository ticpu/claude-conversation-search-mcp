use crate::shared;
use crate::shared::{CacheManager, SearchEngine, SearchIndexer, SearchQuery};
use anyhow::Result;
use glob::glob;
use std::collections::HashMap;
use std::path::Path;
use tracing::Level;
use tracing::info;
use tracing::warn;
use tracing_subscriber::FmtSubscriber;

pub struct CliArgs {
    pub verbose: u8,
    pub command: CliCommands,
}

pub enum CliCommands {
    Index {
        rebuild: bool,
    },
    Search {
        query: String,
        project: Option<String>,
        limit: usize,
    },
    Topics {
        project: Option<String>,
        limit: usize,
    },
    Stats {
        project: Option<String>,
    },
    Session {
        session_id: String,
        full: bool,
    },
    Cache {
        action: CacheAction,
    },
}

pub enum CacheAction {
    Info,
    Clear,
}

fn setup_logging(verbose: u8) {
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

pub async fn run_cli(args: CliArgs) -> Result<()> {
    // Setup logging based on verbosity
    setup_logging(args.verbose);

    match args.command {
        CliCommands::Index { rebuild } => {
            let index_path = shared::get_cache_dir()?;
            index_conversations(&index_path, rebuild).await?;
        }
        CliCommands::Search {
            query,
            project,
            limit,
        } => {
            let index_path = shared::get_cache_dir()?;
            // Auto-index before searching
            shared::auto_index(&index_path).await?;
            search_conversations(&index_path, query, project, limit).await?;
        }
        CliCommands::Topics { project, limit } => {
            let index_path = shared::get_cache_dir()?;
            shared::auto_index(&index_path).await?;
            show_topics(&index_path, project, limit).await?;
        }
        CliCommands::Stats { project } => {
            let index_path = shared::get_cache_dir()?;
            shared::auto_index(&index_path).await?;
            show_stats(&index_path, project).await?;
        }
        CliCommands::Session { session_id, full } => {
            let index_path = shared::get_cache_dir()?;
            shared::auto_index(&index_path).await?;
            view_session(&index_path, session_id, full).await?;
        }
        CliCommands::Cache { action } => {
            let index_path = shared::get_cache_dir()?;
            match action {
                CacheAction::Info => show_cache_info(&index_path).await?,
                CacheAction::Clear => clear_cache(&index_path).await?,
            }
        }
    }

    Ok(())
}

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

    let claude_dir = shared::get_claude_dir()?;
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

async fn clear_cache(index_path: &Path) -> Result<()> {
    let mut cache_manager = CacheManager::new(index_path)?;
    cache_manager.clear_cache()?;
    println!("Cache cleared successfully. Run 'claude-search index' to rebuild.");
    Ok(())
}

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
