use crate::indexer::SearchIndexer;
use crate::models::SearchQuery;
use crate::parser::JsonlParser;
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
    /// Search conversations
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
            search_conversations(&index_path, query, project, limit).await?;
        }
    }

    Ok(())
}

async fn index_conversations(index_path: &Path, rebuild: bool) -> Result<()> {
    info!("Starting indexing process...");

    if rebuild && index_path.exists() {
        std::fs::remove_dir_all(index_path)?;
    }

    let mut indexer = if index_path.exists() {
        SearchIndexer::open(index_path)?
    } else {
        SearchIndexer::new(index_path)?
    };

    let parser = JsonlParser::new();
    let claude_dir = get_claude_dir()?;

    let pattern = claude_dir.join("projects/**/*.jsonl");
    let pattern_str = pattern.to_string_lossy();

    info!("Scanning for JSONL files in: {}", pattern_str);

    let mut total_files = 0;
    let mut total_entries = 0;

    for entry in glob(&pattern_str)? {
        match entry {
            Ok(path) => {
                total_files += 1;
                info!("Processing: {}", path.display());

                match parser.parse_file(&path) {
                    Ok(entries) => {
                        let entry_count = entries.len();
                        total_entries += entry_count;

                        if entry_count > 0 {
                            indexer.index_conversations(entries)?;
                            info!("  Indexed {} entries", entry_count);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse {}: {}", path.display(), e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to access file: {}", e);
            }
        }
    }

    info!(
        "Indexing complete: {} files, {} entries",
        total_files, total_entries
    );
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
