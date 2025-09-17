use super::cache::CacheManager;
use super::config::get_config;
use super::indexer::SearchIndexer;
use super::lock::ExclusiveIndexAccess;
use anyhow::Result;
use glob::glob;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

pub fn get_claude_dir() -> Result<PathBuf> {
    get_config().get_claude_dir()
}

pub fn get_cache_dir() -> Result<PathBuf> {
    get_config().get_cache_dir()
}

pub async fn auto_index(index_path: &Path) -> Result<()> {
    let config = get_config();

    // Skip auto-indexing if disabled in config
    if !config.index.auto_index_on_startup {
        return Ok(());
    }

    // Try to acquire exclusive lock for indexing
    let _lock = match ExclusiveIndexAccess::acquire() {
        Ok(lock) => lock,
        Err(_) => {
            // Another process is already indexing, skip
            info!("Skipping auto-index: another process is currently indexing");
            return Ok(());
        }
    };

    let mut cache_manager = CacheManager::new(index_path)?;

    let mut indexer = if index_path.join("meta.json").exists() {
        // Check if existing index has correct schema
        match SearchIndexer::validate_schema(index_path) {
            Ok(true) => {
                // Schema is valid, open existing index
                SearchIndexer::open(index_path)?
            }
            Ok(false) => {
                // Schema mismatch, rebuild
                info!("Index schema mismatch detected. Rebuilding index...");

                // Remove the old index
                if let Err(rm_err) = std::fs::remove_dir_all(index_path) {
                    warn!("Failed to remove old index: {}", rm_err);
                }

                // Create new index
                SearchIndexer::new(index_path)?
            }
            Err(e) => {
                // Failed to validate (corrupted index), rebuild
                warn!("Failed to validate index: {}. Rebuilding...", e);

                // Remove the corrupted index
                if let Err(rm_err) = std::fs::remove_dir_all(index_path) {
                    warn!("Failed to remove corrupted index: {}", rm_err);
                }

                // Create new index
                SearchIndexer::new(index_path)?
            }
        }
    } else {
        info!("No index found, creating new one...");
        SearchIndexer::new(index_path)?
    };

    let claude_dir = get_claude_dir()?;
    let pattern = claude_dir.join("projects/**/*.jsonl");
    let pattern_str = pattern.to_string_lossy();

    let mut all_files = Vec::new();
    // Silently skip errors during auto-indexing
    for path in glob(&pattern_str)?.flatten() {
        all_files.push(path);
    }

    cache_manager.update_incremental(&mut indexer, all_files)?;
    Ok(())
}

/// Extract content from a Claude JSONL message entry
/// Handles both simple string content and complex array structures
pub fn extract_content_from_json(json: &Value) -> String {
    // Try message.content first (standard Claude format)
    if let Some(message) = json.get("message")
        && let Some(content) = message.get("content")
    {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if content.is_array() {
            let mut text_parts = Vec::new();
            for part in content.as_array().unwrap() {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    text_parts.push(text);
                }
            }
            return text_parts.join(" ");
        }
    }

    // Fallback to direct content field
    if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
        return content.to_string();
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_simple_string_content() {
        let json = json!({
            "message": {
                "content": "Hello world"
            }
        });

        assert_eq!(extract_content_from_json(&json), "Hello world");
    }

    #[test]
    fn test_extract_array_content() {
        let json = json!({
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Please analyze this codebase"
                    },
                    {
                        "type": "text",
                        "text": "and create documentation"
                    }
                ]
            }
        });

        assert_eq!(
            extract_content_from_json(&json),
            "Please analyze this codebase and create documentation"
        );
    }

    #[test]
    fn test_extract_complex_claude_format() {
        // Real Claude JSONL format from our problematic case
        let json = json!({
            "parentUuid": "4fae46dd-e514-4b7c-b173-7dd4ddee1cf3",
            "sessionId": "4af624da-58de-404f-92c9-bc582b288da6",
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "Please analyze this codebase and create a CLAUDE.md file"
                    }
                ]
            },
            "timestamp": "2025-08-28T20:08:33.947Z"
        });

        let result = extract_content_from_json(&json);
        assert_eq!(
            result,
            "Please analyze this codebase and create a CLAUDE.md file"
        );
        assert!(!result.is_empty());
    }

    #[test]
    fn test_extract_direct_content_fallback() {
        let json = json!({
            "content": "Direct content field"
        });

        assert_eq!(extract_content_from_json(&json), "Direct content field");
    }

    #[test]
    fn test_extract_empty_content() {
        let json = json!({
            "some_other_field": "value"
        });

        assert_eq!(extract_content_from_json(&json), "");
    }

    #[test]
    fn test_extract_array_with_mixed_types() {
        let json = json!({
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "First part"
                    },
                    {
                        "type": "image",
                        "url": "http://example.com/image.png"
                    },
                    {
                        "type": "text",
                        "text": "Second part"
                    }
                ]
            }
        });

        assert_eq!(extract_content_from_json(&json), "First part Second part");
    }
}
