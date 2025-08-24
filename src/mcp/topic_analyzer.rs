use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use super::server::{CallToolResponse, ToolResult};
use crate::shared::{SearchEngine, SearchQuery};

#[derive(Debug)]
struct ProjectData {
    message_count: usize,
    technologies: HashMap<String, usize>,
    languages: HashMap<String, usize>,
    has_code: bool,
    has_errors: bool,
}

pub async fn handle_analyze_topics(
    search_engine: Option<&SearchEngine>,
    args: Option<Value>,
) -> Result<Value> {
    let args = args.unwrap_or_default();

    // Get query parameter - if not provided, use a broad search
    let query_text = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("error OR rust OR code OR git OR api") // Common terms to catch most conversations
        .to_string();

    let project_filter = args
        .get("project")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let search_limit = args
        .get("search_limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(500) as usize;

    debug!(
        "Analyzing topics for query: '{}', project filter: {:?}, limit: {}",
        query_text, project_filter, limit
    );

    let query = SearchQuery {
        text: query_text.clone(),
        project_filter: project_filter.clone(),
        session_filter: None,
        limit: search_limit,
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

    // Analyze by project first
    let mut project_data: HashMap<String, ProjectData> = HashMap::new();
    let mut total_sessions = std::collections::HashSet::new();

    for result in &results {
        total_sessions.insert(&result.session_id);

        let project_entry = project_data
            .entry(result.project.clone())
            .or_insert_with(|| ProjectData {
                message_count: 0,
                technologies: HashMap::new(),
                languages: HashMap::new(),
                has_code: false,
                has_errors: false,
            });

        project_entry.message_count += 1;

        // Count technologies per project
        for tech in &result.technologies {
            *project_entry.technologies.entry(tech.clone()).or_insert(0) += 1;
        }

        // Count languages per project
        for lang in &result.code_languages {
            *project_entry.languages.entry(lang.clone()).or_insert(0) += 1;
        }

        if result.has_code {
            project_entry.has_code = true;
        }
        if result.has_error {
            project_entry.has_errors = true;
        }
    }

    // Sort projects by message count
    let mut project_sorted: Vec<_> = project_data.iter().collect();
    project_sorted.sort_by(|a, b| b.1.message_count.cmp(&a.1.message_count));

    let mut output = String::new();

    output.push_str(&format!(
        "{} messages, {} sessions across {} projects\n\n",
        results.len(),
        total_sessions.len(),
        project_sorted.len()
    ));

    for (project_name, project_data) in project_sorted.iter().take(limit) {
        output.push_str(&format!(
            "**{}** ({} messages",
            project_name, project_data.message_count
        ));

        // Add indicators
        let mut indicators = Vec::new();
        if project_data.has_code {
            indicators.push("code");
        }
        if project_data.has_errors {
            indicators.push("errors");
        }

        if !indicators.is_empty() {
            output.push_str(&format!(", {}", indicators.join("+")));
        }
        output.push_str(")\n");

        // Top technologies for this project
        let mut project_techs: Vec<_> = project_data.technologies.iter().collect();
        project_techs.sort_by(|a, b| b.1.cmp(a.1));

        if !project_techs.is_empty() {
            output.push_str("  Technologies: ");
            let tech_list: Vec<String> = project_techs
                .iter()
                .take(5)
                .map(|(tech, count)| format!("{} ({})", tech, count))
                .collect();
            output.push_str(&tech_list.join(", "));
            output.push('\n');
        }

        // Languages for this project
        let mut project_langs: Vec<_> = project_data.languages.iter().collect();
        project_langs.sort_by(|a, b| b.1.cmp(a.1));

        if !project_langs.is_empty() {
            output.push_str("  Languages: ");
            let lang_list: Vec<String> = project_langs
                .iter()
                .take(3)
                .map(|(lang, count)| format!("{} ({})", lang, count))
                .collect();
            output.push_str(&lang_list.join(", "));
            output.push('\n');
        }

        output.push('\n');
    }

    Ok(serde_json::to_value(CallToolResponse {
        content: vec![ToolResult {
            result_type: "text".to_string(),
            text: output,
        }],
        is_error: None,
    })?)
}
