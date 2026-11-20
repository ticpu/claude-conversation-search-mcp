use crate::cli::args::{CacheAction, CliCommands, IncludeArg, IndexAction, setup_logging};
use crate::cli::index;
use crate::cli::search::{SearchOpts, search_conversations};
use crate::cli::service;
use crate::cli::session::view_session;
use crate::cli::stats::{show_stats, show_topics};
use crate::cli::summary::summarize_session;
use crate::shared::session_view::{SessionViewOpts, Window};
use crate::shared::{self, CacheManager, DisplayOptions, ExclusiveIndexAccess};
use anyhow::Result;
use std::path::Path;

pub fn run_cli(verbose: u8, command: CliCommands) -> Result<()> {
    setup_logging(verbose);

    let index_path = shared::get_config().get_cache_dir()?;

    match command {
        CliCommands::Index { action } => match action.unwrap_or_default() {
            IndexAction::Status => index::show_status(&index_path)?,
            IndexAction::Rebuild => index::rebuild(&index_path)?,
            IndexAction::Vacuum => index::vacuum(&index_path)?,
            #[cfg(target_os = "linux")]
            IndexAction::Watch => {
                match crate::cli::watch::run(&shared::projects_dir()?, &index_path)? {}
            }
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
            shared::auto_index(&index_path, shared::Merges::Wait, &mut |_| Ok(()))?;
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
                after: shared::parse_date_opt(after.as_deref())?,
                before: shared::parse_date_opt(before.as_deref())?,
                display: DisplayOptions {
                    include_thinking: include.contains(&IncludeArg::Thinking),
                    include_tools: include.contains(&IncludeArg::Tools),
                    truncate_length: truncate,
                },
            };
            search_conversations(&index_path, opts)?;
        }
        CliCommands::Topics { project, limit } => {
            shared::auto_index(&index_path, shared::Merges::Wait, &mut |_| Ok(()))?;
            show_topics(&index_path, project, limit)?;
        }
        CliCommands::Stats { project } => {
            shared::auto_index(&index_path, shared::Merges::Wait, &mut |_| Ok(()))?;
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
            meta,
        } => {
            shared::auto_index(&index_path, shared::Merges::Wait, &mut |_| Ok(()))?;
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
                meta_only: meta,
            };
            view_session(&index_path, &opts)?;
        }
        CliCommands::Summary { session_id } => {
            shared::auto_index(&index_path, shared::Merges::Wait, &mut |_| Ok(()))?;
            summarize_session(&index_path, session_id)?;
        }
        CliCommands::Cache { action } => match action {
            CacheAction::Info => show_cache_info(&index_path)?,
            CacheAction::Clear => clear_cache(&index_path)?,
        },
        CliCommands::Install {
            project,
            standalone,
        } => service::install(project, standalone)?,
        CliCommands::Uninstall { project } => service::uninstall(project, &index_path)?,
    }

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
    let _lock = ExclusiveIndexAccess::acquire(index_path)?;
    let mut cache_manager = CacheManager::new(index_path)?;
    cache_manager.clear_cache()?;
    println!(
        "Cache cleared successfully. Run 'claude-conversation-search index rebuild' to rebuild."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn cache_clear_refuses_while_a_writer_holds_the_index() {
        let temp = TempDir::new().unwrap();
        let index = temp.path();
        let writer = ExclusiveIndexAccess::acquire(index).unwrap();
        std::fs::write(index.join("meta.json"), "{}").unwrap();

        let err = clear_cache(index).expect_err("cache clear must not wipe under a live writer");
        assert!(shared::is_index_busy(&err), "{err:#}");
        assert!(
            index
                .join("meta.json")
                .exists()
        );

        drop(writer);
        clear_cache(index).unwrap();
        assert!(
            !index
                .join("meta.json")
                .exists()
        );
        assert!(
            index
                .join("index.lock")
                .exists(),
            "the wipe keeps the lock file it ran under"
        );
    }
}
