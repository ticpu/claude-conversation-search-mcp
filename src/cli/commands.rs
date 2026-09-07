use crate::cli::args::{CacheAction, CliCommands, IncludeArg, IndexAction, setup_logging};
use crate::cli::index;
use crate::cli::search::{SearchOpts, index_exists_or_notify, search_conversations};
use crate::cli::session::view_session;
use crate::shared::session_view::{self, SessionViewOpts, Window};
use crate::shared::{self, CacheManager, DisplayOptions, SearchQuery, SortOrder};
use anyhow::{Context, Result};
use std::collections::HashMap;
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

    let (_cache, search_engine) = shared::open_search_engine(index_path)?;

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

    let (cache_manager, search_engine) = shared::open_search_engine(index_path)?;
    let cache_stats = cache_manager.get_stats();

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

fn summarize_session(index_path: &Path, session_id: String) -> Result<()> {
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

    let prompt = format!(
        "Summarize this conversation concisely. Include: topic, key decisions, outcome.\n\n{}",
        conversation
    );

    run_summary_subprocess(&prompt)
}

/// Run `claude --print` in a jailed, empty directory with no tools, feeding
/// `prompt` on stdin and using haiku for cost.
fn run_summary_subprocess(prompt: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[cfg(unix)]
    let temp_dir = std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    #[cfg(windows)]
    let temp_dir = std::env::temp_dir();
    let jail = tempfile::Builder::new()
        .prefix("claude-summary-jail-")
        .tempdir_in(&temp_dir)
        .with_context(|| format!("creating a jail directory in {}", temp_dir.display()))?;
    let jail_dir = jail.path();

    let mut child = Command::new("claude")
        .args([
            "--print",
            "--tools",
            "",
            "--no-session-persistence",
            "--model",
            "haiku",
        ])
        .current_dir(jail_dir)
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
