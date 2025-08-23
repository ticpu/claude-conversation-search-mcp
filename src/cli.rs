use crate::cache::CacheManager;
use crate::indexer::SearchIndexer;
use crate::models::SearchQuery;
use crate::search::SearchEngine;
use anyhow::Result;
use clap::{Parser, Subcommand};
use dirs::home_dir;
use glob::glob;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "claude-search")]
#[command(about = "Search Claude Code conversations")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

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

async fn auto_index(index_path: &Path) -> Result<()> {
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
    for entry in glob(&pattern_str)? {
        match entry {
            Ok(path) => all_files.push(path),
            Err(_) => {} // Silently skip errors during auto-indexing
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

fn get_claude_dir() -> Result<PathBuf> {
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

fn get_cache_dir() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let cache_dir = home.join(".cache").join("claude-search");
    Ok(cache_dir)
}
