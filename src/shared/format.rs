use super::models::SearchResult;
use super::path_utils::{home_to_tilde, session_jsonl_path, short_uuid};
use super::terminal::file_hyperlink;
use super::utils::truncate_content;

/// Search result with surrounding context messages
#[derive(Debug, Clone)]
pub struct SearchResultWithContext {
    pub matched_message: SearchResult,
    pub context_messages: Vec<SearchResult>,
    pub match_index: usize,
    pub total_session_messages: usize,
}

/// A result with itself as its own (sole) context: used when the session
/// lookup fails, the match isn't found in it, or the context window empties
/// out after filtering to displayable messages.
pub(crate) fn self_context_result(
    matched_message: SearchResult,
    total_session_messages: usize,
) -> SearchResultWithContext {
    SearchResultWithContext {
        matched_message: matched_message.clone(),
        context_messages: vec![matched_message],
        match_index: 0,
        total_session_messages,
    }
}

/// Options for what to include in search result display
#[derive(Debug, Clone)]
pub struct DisplayOptions {
    pub include_thinking: bool,
    pub include_tools: bool,
    /// Characters shown per message around match (0 = full content)
    pub truncate_length: usize,
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self {
            include_thinking: false,
            include_tools: false,
            truncate_length: 300,
        }
    }
}

/// Filter content based on display options
pub(crate) fn filter_content(s: &str, opts: &DisplayOptions) -> Option<String> {
    // Check if content should be hidden
    if !opts.include_thinking && s.starts_with("[thinking]") {
        return None;
    }
    if !opts.include_tools
        && (s.starts_with('[') && s.contains(']') && !s.starts_with("[result]"))
        && !s.starts_with("[thinking]")
    {
        // Looks like a tool call [ToolName] {...}
        if let Some(bracket_end) = s.find(']') {
            let prefix = &s[1..bracket_end];
            // Tool names are typically CamelCase or contain underscores/colons
            if prefix
                .chars()
                .any(|c| c.is_uppercase() || c == '_' || c == ':')
                && !prefix.contains(' ')
            {
                return None;
            }
        }
    }
    Some(s.to_string())
}

impl SearchResultWithContext {
    /// Format with display options
    pub fn format_compact_with_options(&self, index: usize, opts: &DisplayOptions) -> String {
        let mut output = String::new();

        let project_path_full = &self
            .matched_message
            .project_path;
        let project_path_display = home_to_tilde(project_path_full);
        let session_id = &self
            .matched_message
            .session_id;

        let jsonl_path = session_jsonl_path(project_path_full, session_id).unwrap_or_default();
        let jsonl_path_str = jsonl_path.to_string_lossy();

        let short_session = short_uuid(session_id);
        let short_msg = short_uuid(
            &self
                .matched_message
                .uuid,
        );

        let path_link = file_hyperlink(project_path_full, &project_path_display);
        let session_link = file_hyperlink(&jsonl_path_str, short_session);

        output.push_str(&format!(
            "{}. 📁 {} 🗒️ {} ({} msgs) 💬 {} 📅 {}\n",
            index + 1,
            path_link,
            session_link,
            self.total_session_messages,
            short_msg,
            self.matched_message
                .timestamp
                .format("%Y-%m-%d %H:%M"),
        ));

        let mut tags = Vec::new();
        tags.extend(
            self.matched_message
                .technologies
                .iter()
                .take(3)
                .cloned(),
        );
        tags.extend(
            self.matched_message
                .code_languages
                .iter()
                .take(2)
                .cloned(),
        );
        if self
            .matched_message
            .has_error
        {
            tags.push("error".to_string());
        }
        if !tags.is_empty() {
            output.push_str(&format!("🎟️{}\n", tags.join(",")));
        }

        self.format_context_messages(&mut output, opts);
        output
    }

    fn format_context_messages(&self, output: &mut String, opts: &DisplayOptions) {
        for (i, msg) in self
            .context_messages
            .iter()
            .enumerate()
        {
            // Filter content based on options
            if filter_content(&msg.content, opts).is_none() {
                continue;
            }

            let prefix = if i == self.match_index { "»  " } else { "   " };
            let content = if opts.truncate_length == 0 {
                msg.content
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                truncate_content(&msg.content, opts.truncate_length, true)
            };

            output.push_str(&format!(
                "{}{}: {}\n",
                prefix,
                msg.message_type
                    .short_name(),
                content
            ));
        }
    }
}
