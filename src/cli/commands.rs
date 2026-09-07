use crate::cli::args::{CacheAction, CliCommands, IncludeArg, IndexAction, setup_logging};
use crate::cli::index;
use crate::cli::search::{SearchOpts, search_conversations};
use crate::cli::session::view_session;
use crate::cli::stats::{show_stats, show_topics};
use crate::cli::summary::summarize_session;
use crate::shared::session_view::{SessionViewOpts, Window};
use crate::shared::{self, CacheManager, DisplayOptions};
use anyhow::{Context, Result};
use std::path::Path;
use tracing::{debug, error};

pub fn run_cli(verbose: u8, command: CliCommands) -> Result<()> {
    setup_logging(verbose);

    let index_path = shared::get_config().get_cache_dir()?;

    match command {
        CliCommands::Index { action } => match action.unwrap_or_default() {
            IndexAction::Status => index::show_status(&index_path)?,
            IndexAction::Rebuild => index::rebuild(&index_path)?,
            IndexAction::Vacuum => index::vacuum(&index_path)?,
        },
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
            shared::auto_index(&index_path)?;
            show_topics(&index_path, project, limit)?;
        }
        CliCommands::Stats { project } => {
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
            shared::auto_index(&index_path)?;
            summarize_session(&index_path, session_id)?;
        }
        CliCommands::Cache { action } => match action {
            CacheAction::Info => show_cache_info(&index_path)?,
            CacheAction::Clear => clear_cache(&index_path)?,
        },
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

    match Command::new("claude")
        .args(["mcp", "remove", "-s", scope, "claude-conversation-search"])
        .status()
    {
        Ok(status) if status.success() => {}
        // Nothing registered yet is the common case, so this is not an error.
        Ok(status) => debug!("claude mcp remove exited with {}", status),
        Err(e) => error!("Could not run claude mcp remove: {}", e),
    }

    let status = Command::new("claude")
        .args([
            "mcp",
            "add",
            "-s",
            scope,
            "claude-conversation-search",
            exe_path,
        ])
        .status()
        .context("running claude mcp add")?;

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
