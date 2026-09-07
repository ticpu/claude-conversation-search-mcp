use super::super::config::get_config;
use super::super::models::{ContentBlock, RawJsonlMessage};
use super::super::utils::truncate_content;

pub(super) struct SearchableContent {
    pub content: String,
    pub has_error: bool,
    pub tools_used: Vec<String>,
}

impl SearchableContent {
    fn empty() -> Self {
        SearchableContent {
            content: String::new(),
            has_error: false,
            tools_used: Vec::new(),
        }
    }
}

pub(super) fn extract_searchable_content(
    raw: &RawJsonlMessage,
    full_content: bool,
) -> SearchableContent {
    let message = match &raw.message {
        Some(m) => m,
        None => return SearchableContent::empty(),
    };

    let content_value = match &message.content {
        Some(c) => c,
        None => return SearchableContent::empty(),
    };

    // Handle string content (simple user messages)
    if let Some(text) = content_value.as_str() {
        return SearchableContent {
            content: text.to_string(),
            has_error: false,
            tools_used: Vec::new(),
        };
    }

    // Handle array content (assistant messages with blocks)
    let blocks = match content_value.as_array() {
        Some(arr) => arr,
        None => return SearchableContent::empty(),
    };

    let mut parts = Vec::new();
    let mut has_error = false;
    let mut tools_used = Vec::new();

    for block in blocks {
        if let Some(content_block) = parse_content_block(block, full_content) {
            match content_block {
                ContentBlock::Text(text) => {
                    parts.push(text);
                }
                ContentBlock::Thinking(thinking) => {
                    parts.push(format!("[thinking] {}", thinking));
                }
                ContentBlock::ToolUse {
                    name,
                    input_preview,
                } => {
                    tools_used.push(name.clone());
                    if !input_preview.is_empty() {
                        parts.push(format!("[{}] {}", name, input_preview));
                    }
                }
                ContentBlock::ToolResult {
                    content_preview,
                    is_error,
                } => {
                    if is_error {
                        has_error = true;
                        parts.push(format!("[error] {}", content_preview));
                    } else if !content_preview
                        .trim()
                        .is_empty()
                    {
                        parts.push(format!("[result] {}", content_preview));
                    }
                }
            }
        }
    }

    SearchableContent {
        content: parts.join("\n"),
        has_error,
        tools_used,
    }
}

pub(super) fn parse_content_block(
    block: &serde_json::Value,
    full_content: bool,
) -> Option<ContentBlock> {
    let block_type = block
        .get("type")?
        .as_str()?;

    match block_type {
        "text" => {
            let text = block
                .get("text")?
                .as_str()?;
            Some(ContentBlock::Text(text.to_string()))
        }
        "thinking" => {
            let thinking = block
                .get("thinking")?
                .as_str()?;
            Some(ContentBlock::Thinking(thinking.to_string()))
        }
        "tool_use" => {
            let name = block
                .get("name")?
                .as_str()?
                .to_string();
            let input = block.get("input");
            let input_preview = input
                .map(|v| {
                    let s = v.to_string();
                    if full_content {
                        s
                    } else {
                        truncate_content(
                            &s,
                            get_config()
                                .limits
                                .tool_input_max_chars,
                            false,
                        )
                    }
                })
                .unwrap_or_default();
            Some(ContentBlock::ToolUse {
                name,
                input_preview,
            })
        }
        "tool_result" => {
            let is_error = block
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content = block.get("content");
            let extracted = content.and_then(|v| {
                if let Some(s) = v.as_str() {
                    Some(s.to_string())
                } else if let Some(arr) = v.as_array() {
                    let texts: Vec<&str> = arr
                        .iter()
                        .filter_map(|item| {
                            item.get("text")
                                .and_then(|t| t.as_str())
                        })
                        .collect();
                    Some(texts.join(" "))
                } else {
                    None
                }
            });
            let content_preview = extracted
                .map(|s| {
                    if full_content {
                        s
                    } else {
                        truncate_content(
                            &s,
                            get_config()
                                .limits
                                .tool_result_max_chars,
                            false,
                        )
                    }
                })
                .unwrap_or_default();
            Some(ContentBlock::ToolResult {
                content_preview,
                is_error,
            })
        }
        _ => None,
    }
}
