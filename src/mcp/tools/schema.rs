use crate::mcp::protocol::Tool;

fn search_conversations_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Search query. Field syntax: 'session_id:abc', 'project:name'"
            },
            "project": {
                "type": "string",
                "description": "Filter by project name",
                "optional": true
            },
            "session": {
                "type": "string",
                "description": "Filter by session ID (prefix match)",
                "optional": true
            },
            "-C": {
                "type": "integer",
                "description": "Messages before and after match (like grep -C)",
                "optional": true,
                "default": 2
            },
            "-B": {
                "type": "integer",
                "description": "Messages before match (like grep -B)",
                "optional": true
            },
            "-A": {
                "type": "integer",
                "description": "Messages after match (like grep -A)",
                "optional": true
            },
            "exclude_projects": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Project names to exclude",
                "optional": true
            },
            "exclude_patterns": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Regex patterns to exclude",
                "optional": true
            },
            "limit": {
                "type": "integer",
                "description": "Max results",
                "optional": true,
                "default": 10
            },
            "sort_by": {
                "type": "string",
                "enum": ["relevance", "date_desc", "date_asc"],
                "optional": true,
                "default": "relevance"
            },
            "after": {
                "type": "string",
                "description": "Results after date (YYYY-MM-DD or ISO 8601)",
                "optional": true
            },
            "before": {
                "type": "string",
                "description": "Results before date (YYYY-MM-DD or ISO 8601)",
                "optional": true
            },
            "include": {
                "type": "array",
                "items": { "type": "string", "enum": ["thinking", "tools", "current_session"] },
                "description": "Include: thinking, tools, current_session",
                "optional": true
            },
            "truncate_length": {
                "type": "integer",
                "description": "Chars shown per message around match. 0 = full content",
                "optional": true,
                "default": 300
            },
            "debug": {
                "type": "boolean",
                "optional": true
            }
        },
        "required": ["query"]
    })
}

fn reindex_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "full": { "type": "boolean", "description": "Force full rebuild (default: incremental)", "optional": true }
        }
    })
}

fn get_session_messages_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "session_id": {
                "type": "string",
                "description": "Session ID to retrieve messages for"
            },
            "offset": {
                "type": "integer",
                "description": "Starting message index",
                "optional": true,
                "default": 0
            },
            "limit": {
                "type": "integer",
                "description": "Messages per page",
                "optional": true,
                "default": 50
            },
            "center_on": {
                "type": "string",
                "description": "Message UUID to center around (from 💬 in search). Overrides offset/limit.",
                "optional": true
            },
            "-C": {
                "type": "integer",
                "description": "Messages before and after center_on (like grep -C)",
                "optional": true,
                "default": 10
            },
            "-B": {
                "type": "integer",
                "description": "Messages before center_on (like grep -B)",
                "optional": true
            },
            "-A": {
                "type": "integer",
                "description": "Messages after center_on (like grep -A)",
                "optional": true
            },
            "truncate_length": {
                "type": "integer",
                "description": "Chars shown per message. 0 = full content",
                "optional": true,
                "default": 0
            }
        },
        "required": ["session_id"]
    })
}

fn summarize_session_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "session_id": {
                "type": "string",
                "description": "Session ID to summarize"
            }
        },
        "required": ["session_id"]
    })
}

fn get_messages_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "ids": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Message UUIDs (from 💬 in search results)"
            }
        },
        "required": ["ids"]
    })
}

fn respawn_server_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {}
    })
}

pub(crate) fn tool_list() -> Vec<Tool> {
    vec![
        Tool {
            name: "search_conversations".to_string(),
            description: "Search conversation history (Tantivy/BM25). Exact terms for functions (`_fix_ssh_agent`), natural language for concepts. Workflow: search → get_messages(ids)/truncate_length:0 for full text → summarize_session for AI summary.".to_string(),
            input_schema: search_conversations_schema(),
        },
        Tool {
            name: "reindex".to_string(),
            description: "Update index for stale/new files. Use when search results seem incomplete or index warning shown.".to_string(),
            input_schema: reindex_schema(),
        },
        Tool {
            name: "get_session_messages".to_string(),
            description: "Paginate session messages. Use offset/limit for sequential reading, or center_on with -B/-A/-C to jump to a specific message.".to_string(),
            input_schema: get_session_messages_schema(),
        },
        Tool {
            name: "summarize_session".to_string(),
            description: "Get Task tool instructions to summarize a session with haiku. Use for long sessions when you need an AI-generated overview.".to_string(),
            input_schema: summarize_session_schema(),
        },
        Tool {
            name: "get_messages".to_string(),
            description: "Get full content of specific messages by UUID. Use after search to read complete message text.".to_string(),
            input_schema: get_messages_schema(),
        },
        Tool {
            name: "respawn_server".to_string(),
            description: "Respawn the MCP server to reload with latest changes".to_string(),
            input_schema: respawn_server_schema(),
        },
    ]
}
