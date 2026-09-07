use crate::cli::search::index_exists_or_notify;
use crate::shared::{self, SearchQuery, SortOrder};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

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

pub(crate) fn show_topics(
    index_path: &Path,
    project_filter: Option<String>,
    limit: usize,
) -> Result<()> {
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

pub(crate) fn show_stats(index_path: &Path, project_filter: Option<String>) -> Result<()> {
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
