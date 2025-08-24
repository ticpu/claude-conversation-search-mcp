use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use super::server::{CallToolResponse, ToolResult};
use crate::shared::{SearchEngine, SearchQuery};

pub async fn handle_analyze_topics(
    search_engine: Option<&SearchEngine>,
    args: Option<Value>,
) -> Result<Value> {
    let args = args.unwrap_or_default();
    let project_filter = args
        .get("project")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

    debug!(
        "Analyzing topics for project filter: {:?}, limit: {}",
        project_filter, limit
    );

    let query = SearchQuery {
        text: "*".to_string(),
        project_filter: project_filter.clone(),
        session_filter: None,
        limit: 1000,
    };

    let search_engine =
        search_engine.ok_or_else(|| anyhow::anyhow!("Search engine not initialized"))?;
    let results = search_engine.search(query)?;

    if results.is_empty() {
        let msg = if let Some(project) = project_filter {
            format!("No conversations found for project: {project}")
        } else {
            "No conversations found in index".to_string()
        };

        return Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: msg,
            }],
            is_error: Some(true),
        })?);
    }

    // Analyze technology usage
    let mut tech_counts = HashMap::new();
    let mut lang_counts = HashMap::new();
    let mut project_counts = HashMap::new();
    let mut monthly_activity = HashMap::new();

    // Count sessions and messages with different characteristics
    let mut sessions_with_code = std::collections::HashSet::new();
    let mut sessions_with_errors = std::collections::HashSet::new();
    let mut total_sessions = std::collections::HashSet::new();

    for result in &results {
        total_sessions.insert(&result.session_id);

        // Count technologies
        for tech in &result.technologies {
            *tech_counts.entry(tech.clone()).or_insert(0) += 1;
        }

        // Count programming languages
        for lang in &result.code_languages {
            *lang_counts.entry(lang.clone()).or_insert(0) += 1;
        }

        // Count project activity
        *project_counts.entry(result.project.clone()).or_insert(0) += 1;

        // Count monthly activity
        let month_key = result.timestamp.format("%Y-%m").to_string();
        *monthly_activity.entry(month_key).or_insert(0) += 1;

        // Track sessions with special characteristics
        if result.has_code {
            sessions_with_code.insert(&result.session_id);
        }
        if result.has_error {
            sessions_with_errors.insert(&result.session_id);
        }
    }

    // Sort by frequency
    let mut tech_sorted: Vec<_> = tech_counts.into_iter().collect();
    tech_sorted.sort_by(|a, b| b.1.cmp(&a.1));
    tech_sorted.truncate(limit);

    let mut lang_sorted: Vec<_> = lang_counts.into_iter().collect();
    lang_sorted.sort_by(|a, b| b.1.cmp(&a.1));
    lang_sorted.truncate(limit.min(10)); // Cap languages at 10

    let mut project_sorted: Vec<_> = project_counts.into_iter().collect();
    project_sorted.sort_by(|a, b| b.1.cmp(&a.1));

    let mut monthly_sorted: Vec<_> = monthly_activity.into_iter().collect();
    monthly_sorted.sort_by(|a, b| a.0.cmp(&b.0)); // Chronological order

    let mut output = String::new();

    // Header
    let title = if let Some(ref proj) = project_filter {
        format!("Technology Analysis - Project: {}", proj)
    } else {
        "Technology Analysis - All Projects".to_string()
    };
    output.push_str(&format!("# {}\n\n", title));

    // Overview
    output.push_str("## Overview\n");
    output.push_str(&format!("**Total Messages**: {}\n", results.len()));
    output.push_str(&format!("**Unique Sessions**: {}\n", total_sessions.len()));
    output.push_str(&format!(
        "**Sessions with Code**: {} ({:.1}%)\n",
        sessions_with_code.len(),
        (sessions_with_code.len() as f32 / total_sessions.len() as f32) * 100.0
    ));
    output.push_str(&format!(
        "**Sessions with Errors**: {} ({:.1}%)\n\n",
        sessions_with_errors.len(),
        (sessions_with_errors.len() as f32 / total_sessions.len() as f32) * 100.0
    ));

    // Top Technologies
    if !tech_sorted.is_empty() {
        output.push_str("## Top Technologies\n");
        for (i, (tech, count)) in tech_sorted.iter().enumerate() {
            let percentage = (*count as f32 / results.len() as f32) * 100.0;
            output.push_str(&format!(
                "{}. **{}**: {} mentions ({:.1}%)\n",
                i + 1,
                tech,
                count,
                percentage
            ));
        }
        output.push('\n');
    }

    // Programming Languages
    if !lang_sorted.is_empty() {
        output.push_str("## Programming Languages\n");
        for (i, (lang, count)) in lang_sorted.iter().enumerate() {
            let percentage = (*count as f32 / results.len() as f32) * 100.0;
            output.push_str(&format!(
                "{}. **{}**: {} mentions ({:.1}%)\n",
                i + 1,
                lang,
                count,
                percentage
            ));
        }
        output.push('\n');
    }

    // Project Activity (if showing all projects)
    if project_filter.is_none() && project_sorted.len() > 1 {
        output.push_str("## Project Activity\n");
        for (project, count) in project_sorted.iter().take(10) {
            let percentage = (*count as f32 / results.len() as f32) * 100.0;
            output.push_str(&format!(
                "**{}**: {} messages ({:.1}%)\n",
                project, count, percentage
            ));
        }
        if project_sorted.len() > 10 {
            output.push_str(&format!(
                "... and {} more projects\n",
                project_sorted.len() - 10
            ));
        }
        output.push('\n');
    }

    // Monthly Activity Timeline
    if monthly_sorted.len() > 1 {
        output.push_str("## Activity Timeline\n");
        for (month, count) in monthly_sorted.iter().take(12) {
            // Last 12 months
            output.push_str(&format!("**{}**: {} messages\n", month, count));
        }
        if monthly_sorted.len() > 12 {
            output.push_str("... (showing last 12 months)\n");
        }
        output.push('\n');
    }

    // Insights
    output.push_str("## Key Insights\n");

    if let Some((top_tech, top_count)) = tech_sorted.first() {
        output.push_str(&format!(
            "- **Most discussed technology**: {} ({} mentions)\n",
            top_tech, top_count
        ));
    }

    if let Some((top_lang, lang_count)) = lang_sorted.first() {
        output.push_str(&format!(
            "- **Primary programming language**: {} ({} mentions)\n",
            top_lang, lang_count
        ));
    }

    let code_percentage = (sessions_with_code.len() as f32 / total_sessions.len() as f32) * 100.0;
    output.push_str(&format!(
        "- **Sessions involving code**: {:.1}%\n",
        code_percentage
    ));

    let error_percentage =
        (sessions_with_errors.len() as f32 / total_sessions.len() as f32) * 100.0;
    output.push_str(&format!(
        "- **Sessions with troubleshooting**: {:.1}%\n",
        error_percentage
    ));

    // Usage tips
    output.push_str("\n## Next Steps\n");
    output.push_str("- Use `search_conversations` to find specific technology discussions\n");
    output.push_str("- Try `get_conversation_context` to explore high-activity sessions\n");
    output.push_str("- Filter by project to get focused technology insights\n");

    Ok(serde_json::to_value(CallToolResponse {
        content: vec![ToolResult {
            result_type: "text".to_string(),
            text: output,
        }],
        is_error: None,
    })?)
}
