use anyhow::Result;
use serde_json::Value;
use tracing::debug;

use super::server::{CallToolResponse, ToolResult};
use crate::shared::{SearchEngine, SearchQuery};

pub async fn handle_get_conversation_context(
    search_engine: Option<&SearchEngine>,
    args: Option<Value>,
) -> Result<Value> {
    let args = args.unwrap_or_default();
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?
        .to_string();

    let include_content = args
        .get("include_content")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    debug!(
        "Getting conversation context for session: {}, include_content: {}",
        session_id, include_content
    );

    let query = SearchQuery {
        text: format!("session_id:{session_id}"),
        project_filter: None,
        session_filter: None,
        limit: 100,
    };

    let search_engine =
        search_engine.ok_or_else(|| anyhow::anyhow!("Search engine not initialized"))?;
    let results = search_engine.search(query)?;

    if results.is_empty() {
        return Ok(serde_json::to_value(CallToolResponse {
            content: vec![ToolResult {
                result_type: "text".to_string(),
                text: format!("No conversations found for session: {session_id}"),
            }],
            is_error: Some(true),
        })?);
    }

    // Sort by timestamp
    let mut sorted_results = results;
    sorted_results.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    let mut output = String::new();
    output.push_str(&format!("# Session: {session_id}\n"));
    output.push_str(&format!("**Project**: {}\n", sorted_results[0].project));
    output.push_str(&format!(
        "**Time range**: {} to {}\n",
        sorted_results[0].timestamp.format("%Y-%m-%d %H:%M"),
        sorted_results
            .last()
            .unwrap()
            .timestamp
            .format("%Y-%m-%d %H:%M")
    ));
    output.push_str(&format!("**Total messages**: {}\n\n", sorted_results.len()));

    // Session-level metadata
    let mut session_techs = std::collections::HashSet::new();
    let mut session_langs = std::collections::HashSet::new();
    let mut has_code = false;
    let mut has_errors = false;

    for result in &sorted_results {
        session_techs.extend(result.technologies.iter().cloned());
        session_langs.extend(result.code_languages.iter().cloned());
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
        session_tags.push(format!("tech: {}", techs.join(", ")));
    }
    if !session_langs.is_empty() {
        let mut langs: Vec<_> = session_langs.into_iter().collect();
        langs.sort();
        session_tags.push(format!("lang: {}", langs.join(", ")));
    }
    if has_code {
        session_tags.push("code".to_string());
    }
    if has_errors {
        session_tags.push("errors".to_string());
    }

    if !session_tags.is_empty() {
        output.push_str(&format!(
            "**Session topics**: {}\n\n",
            session_tags.join(" • ")
        ));
    }

    output.push_str("## Messages\n");
    output.push_str(&format!("{}\n", "─".repeat(80)));

    for (i, result) in sorted_results.iter().enumerate() {
        output.push_str(&format!(
            "{}. {} | Score: {:.2}\n",
            i + 1,
            result.timestamp.format("%H:%M:%S"),
            result.score
        ));

        if include_content {
            output.push_str(&format!("{}\n", result.content));
        } else {
            output.push_str(&format!("{}\n", result.snippet));
        }

        if i < sorted_results.len() - 1 {
            output.push_str(&format!("{}\n", "─".repeat(40)));
        }
    }

    if !include_content && sorted_results.len() > 3 {
        output.push_str("\n**Tip**: Use include_content: true to see full message content\n");
    }

    Ok(serde_json::to_value(CallToolResponse {
        content: vec![ToolResult {
            result_type: "text".to_string(),
            text: output,
        }],
        is_error: None,
    })?)
}
