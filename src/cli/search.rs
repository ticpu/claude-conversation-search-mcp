use crate::shared::{self, DisplayOptions, SearchQuery, SortOrder};
use anyhow::Result;
use chrono::Utc;
use std::path::Path;

pub(crate) struct SearchOpts {
    pub(crate) query: String,
    pub(crate) project: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) limit: usize,
    pub(crate) context_before: usize,
    pub(crate) context_after: usize,
    pub(crate) exclude_projects: Vec<String>,
    pub(crate) exclude_patterns: Vec<String>,
    pub(crate) sort: SortOrder,
    pub(crate) after: Option<chrono::DateTime<Utc>>,
    pub(crate) before: Option<chrono::DateTime<Utc>>,
    pub(crate) display: DisplayOptions,
}

/// True if the index exists; otherwise prints the standard "not found" message.
pub(crate) fn index_exists_or_notify(index_path: &Path) -> bool {
    if index_path.exists() {
        return true;
    }
    println!("Index not found. Please run 'claude-conversation-search index rebuild' first.");
    false
}

pub(crate) fn search_conversations(index_path: &Path, opts: SearchOpts) -> Result<()> {
    if !index_exists_or_notify(index_path) {
        return Ok(());
    }

    let config = shared::get_config();
    let mut all_exclude_patterns = config
        .search
        .exclude_patterns
        .clone();
    all_exclude_patterns.extend(opts.exclude_patterns);

    let exclude_regexes = shared::compile_exclude_patterns(&all_exclude_patterns)?;

    let (_cache, search_engine) = shared::open_search_engine(index_path)?;

    let query = SearchQuery {
        text: opts.query,
        project_filter: opts.project,
        session_filter: opts
            .session
            .as_deref()
            .map(shared::resolve_session_filter)
            .transpose()?,
        limit: opts.limit,
        sort_by: opts.sort,
        after: opts.after,
        before: opts.before,
    };

    let found = search_engine.search_filtered(
        &query,
        &shared::ResultFilter {
            exclude_projects: opts.exclude_projects,
            exclude_regexes,
            active_session: None,
        },
        &opts.display,
        opts.context_before,
        opts.context_after,
    )?;
    let filtered = found.results;

    if let Some(reason) = &found.lenient {
        println!("Note: {}", shared::lenient_note(reason));
    }
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
    if found.more {
        println!("\n+more: raise --limit above {}", opts.limit);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: crate::cli::CliCommands,
    }

    #[test]
    fn limit_zero_is_refused_by_the_parser() {
        let parse = |limit: &str| Cli::try_parse_from(["ccs", "search", "q", "--limit", limit]);
        assert!(parse("0").is_err());
        assert!(parse("1").is_ok());
    }
}
