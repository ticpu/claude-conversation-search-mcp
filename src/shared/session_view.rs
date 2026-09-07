use super::models::{ConversationEntry, SearchResult};
use super::parsers::JsonlParser;
use super::path_utils::{find_session_jsonl, home_to_tilde};
use super::search::{DisplayOptions, filter_content};
use super::terminal::file_hyperlink;
use super::utils::truncate_content;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub struct Window {
    pub center: Option<String>,
    pub before: usize,
    pub after: usize,
    pub offset: usize,
    /// 0 shows every message from `offset` on.
    pub limit: usize,
}

pub struct SessionViewOpts {
    pub session_id: String,
    pub truncate_length: usize,
    pub window: Window,
}

pub enum SessionSource {
    Jsonl(PathBuf),
    Index,
}

impl From<SearchResult> for ConversationEntry {
    fn from(r: SearchResult) -> Self {
        Self {
            uuid: r.uuid,
            parent_uuid: None,
            session_id: r.session_id,
            project_path: r.project,
            timestamp: r.timestamp,
            message_type: r.message_type,
            content: r.content,
            model: None,
            cwd: Some(r.project_path),
            sequence_num: r.sequence_num,
            is_sidechain: false,
            agent_id: None,
            technologies: r.technologies,
            has_code: r.has_code,
            code_languages: r.code_languages,
            has_error: r.has_error,
            tools_mentioned: r.tools_mentioned,
        }
    }
}

pub fn load_session(
    index_path: &Path,
    session_id: &str,
) -> Result<(Vec<ConversationEntry>, SessionSource)> {
    if let Some(jsonl_path) = find_session_jsonl(session_id)? {
        let entries = JsonlParser::with_full_content()
            .parse_file(&jsonl_path)
            .with_context(|| format!("reading session from {}", jsonl_path.display()))?;
        return Ok((entries, SessionSource::Jsonl(jsonl_path)));
    }

    if !index_path.exists() {
        bail!(
            "no JSONL file for session {session_id} and no index at {}",
            index_path.display()
        );
    }

    let (_cache, engine) = super::search::open_search_engine(index_path)?;
    let results = engine
        .get_session_messages(session_id)
        .with_context(|| format!("reading session {session_id} from the index"))?;

    Ok((
        results
            .into_iter()
            .map(ConversationEntry::from)
            .collect(),
        SessionSource::Index,
    ))
}

/// Displayable messages in sequence order, the only ordering the window is defined against.
pub fn displayable_entries(entries: &[ConversationEntry]) -> Vec<ConversationEntry> {
    let mut msgs: Vec<_> = entries
        .iter()
        .filter(|e| e.is_displayable())
        .cloned()
        .collect();
    msgs.sort_by_key(|e| e.sequence_num);
    msgs
}

pub fn select_window(entries: &[ConversationEntry], w: &Window) -> (Range<usize>, Option<usize>) {
    let total = entries.len();

    if let Some(ref uuid) = w.center
        && let Some(idx) = entries
            .iter()
            .position(|e| {
                e.uuid
                    .starts_with(uuid.as_str())
            })
    {
        let start = idx.saturating_sub(w.before);
        let end = (idx + w.after + 1).min(total);
        return (start..end, Some(idx));
    }

    let start = w
        .offset
        .min(total);
    let end = if w.limit == 0 {
        total
    } else {
        w.offset
            .saturating_add(w.limit)
            .min(total)
    };
    (start..end, None)
}

pub fn render(
    entries: &[ConversationEntry],
    source: &SessionSource,
    opts: &SessionViewOpts,
    display: &DisplayOptions,
) -> String {
    let msgs = displayable_entries(entries);
    let total = msgs.len();
    if total == 0 {
        return format!("No displayable messages for session {}\n", opts.session_id);
    }

    let (range, center_idx) = select_window(&msgs, &opts.window);
    let window = &msgs[range.clone()];

    let mut output = String::new();
    if opts
        .window
        .center
        .is_some()
        && center_idx.is_none()
    {
        output.push_str(&format!(
            "warning: message {} not found, showing from the start\n",
            opts.window
                .center
                .as_deref()
                .unwrap_or_default()
        ));
    }

    let first = &msgs[0];
    let project = home_to_tilde(
        first
            .cwd
            .as_deref()
            .unwrap_or(&first.project_path),
    );
    let time_range = format!(
        "{} - {}",
        first
            .timestamp
            .format("%Y-%m-%d %H:%M"),
        msgs[total - 1]
            .timestamp
            .format("%H:%M")
    );
    let (session_cell, from_index) = match source {
        SessionSource::Index => (
            opts.session_id
                .clone(),
            " (from index, content may be truncated)",
        ),
        SessionSource::Jsonl(path) => (
            file_hyperlink(&path.to_string_lossy(), &opts.session_id),
            "",
        ),
    };
    output.push_str(&format!(
        "📁 {} 🗒️ {} ({}/{} msgs) ⏱️ {}{}\n",
        project,
        session_cell,
        window.len(),
        total,
        time_range,
        from_index
    ));

    if let Some(tags) = tag_line(entries) {
        output.push_str(&format!("tags: {tags}\n"));
    }
    output.push('\n');

    for (i, entry) in window
        .iter()
        .enumerate()
    {
        if filter_content(&entry.content, display).is_none() {
            continue;
        }
        let marker = if center_idx == Some(range.start + i) {
            "»"
        } else {
            " "
        };
        let content = if opts.truncate_length == 0 {
            entry
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            truncate_content(&entry.content, opts.truncate_length, true)
        };
        output.push_str(&format!(
            "{} [{}] {}: {}\n",
            marker,
            entry
                .timestamp
                .format("%H:%M:%S"),
            entry
                .message_type
                .short_name(),
            content
        ));
    }

    if range.end < total {
        output.push_str(&format!("\n+more: offset={}\n", range.end));
    }

    output
}

fn tag_line(entries: &[ConversationEntry]) -> Option<String> {
    let mut techs = BTreeSet::new();
    let mut langs = BTreeSet::new();
    let mut has_code = false;
    let mut has_error = false;
    for e in entries {
        techs.extend(
            e.technologies
                .iter()
                .cloned(),
        );
        langs.extend(
            e.code_languages
                .iter()
                .cloned(),
        );
        has_code |= e.has_code;
        has_error |= e.has_error;
    }

    let mut tags = Vec::new();
    if !techs.is_empty() {
        tags.push(
            techs
                .into_iter()
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    if !langs.is_empty() {
        tags.push(
            langs
                .into_iter()
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    if has_code {
        tags.push("code".to_string());
    }
    if has_error {
        tags.push("error".to_string());
    }

    if tags.is_empty() {
        None
    } else {
        Some(tags.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::models::{EntryBuilder, MessageType};
    use chrono::{TimeZone, Utc};

    fn entry(uuid: &str, seq: usize, kind: MessageType, content: &str) -> ConversationEntry {
        EntryBuilder::new(uuid, "session-1")
            .message_type(kind)
            .content(content)
            .sequence_num(seq)
            .timestamp(
                Utc.timestamp_opt(1_700_000_000 + seq as i64, 0)
                    .unwrap(),
            )
            .project("project-name", "/home/user/GIT/project-name")
            .build()
    }

    fn full_display() -> DisplayOptions {
        DisplayOptions {
            include_thinking: true,
            include_tools: true,
            truncate_length: 0,
        }
    }

    fn opts(window: Window) -> SessionViewOpts {
        SessionViewOpts {
            session_id: "session-1".to_string(),
            truncate_length: 0,
            window,
        }
    }

    fn window() -> Window {
        Window {
            center: None,
            before: 1,
            after: 1,
            offset: 0,
            limit: 0,
        }
    }

    #[test]
    fn search_result_conversion_keeps_project_and_cwd_apart() {
        let result = SearchResult {
            uuid: "u1".to_string(),
            content: "hello".to_string(),
            project: "project-name".to_string(),
            project_path: "/home/user/GIT/project-name".to_string(),
            session_id: "session-1".to_string(),
            timestamp: Utc::now(),
            technologies: vec!["rust".to_string()],
            code_languages: vec![],
            tools_mentioned: vec![],
            has_code: false,
            has_error: false,
            interaction_count: 0,
            sequence_num: 3,
            message_type: MessageType::User,
        };

        let entry = ConversationEntry::from(result);
        assert_eq!(entry.project_path, "project-name");
        assert_eq!(
            entry
                .cwd
                .as_deref(),
            Some("/home/user/GIT/project-name")
        );
        assert_eq!(entry.sequence_num, 3);
    }

    #[test]
    fn center_wins_over_offset() {
        let entries: Vec<_> = (0..10)
            .map(|i| entry(&format!("uuid-{i}"), i, MessageType::User, "text"))
            .collect();
        let (range, center) = select_window(
            &entries,
            &Window {
                center: Some("uuid-5".to_string()),
                before: 2,
                after: 1,
                offset: 7,
                limit: 2,
            },
        );
        assert_eq!(range, 3..7);
        assert_eq!(center, Some(5));
    }

    #[test]
    fn offset_and_limit_page_the_session() {
        let entries: Vec<_> = (0..10)
            .map(|i| entry(&format!("uuid-{i}"), i, MessageType::User, "text"))
            .collect();

        let (range, center) = select_window(
            &entries,
            &Window {
                center: None,
                before: 0,
                after: 0,
                offset: 8,
                limit: 5,
            },
        );
        assert_eq!(range, 8..10);
        assert_eq!(center, None);
    }

    #[test]
    fn missing_center_warns_and_starts_from_the_beginning() {
        let entries: Vec<_> = (0..3)
            .map(|i| entry(&format!("uuid-{i}"), i, MessageType::User, "text"))
            .collect();
        let mut w = window();
        w.center = Some("nope".to_string());

        let out = render(&entries, &SessionSource::Index, &opts(w), &full_display());
        assert!(out.starts_with("warning: message nope not found"), "{out}");
        assert!(out.contains("uuid-0") || out.contains("text"), "{out}");
    }

    #[test]
    fn header_prefers_cwd_and_flags_the_index_source() {
        let entries = vec![entry("uuid-0", 0, MessageType::User, "hello")];
        let out = render(
            &entries,
            &SessionSource::Index,
            &opts(window()),
            &full_display(),
        );
        assert!(out.contains("📁 /home/user/GIT/project-name"), "{out}");
        assert!(out.contains("(1/1 msgs)"), "{out}");
        assert!(out.contains("from index"), "{out}");
    }

    #[test]
    fn non_displayable_messages_are_dropped_before_windowing() {
        let entries = vec![
            entry("uuid-0", 0, MessageType::System, "system noise"),
            entry("uuid-1", 1, MessageType::User, "Warmup"),
            entry("uuid-2", 2, MessageType::User, "real message"),
        ];
        let out = render(
            &entries,
            &SessionSource::Jsonl(PathBuf::from("/dev/null")),
            &opts(window()),
            &full_display(),
        );
        assert!(out.contains("(1/1 msgs)"), "{out}");
        assert!(!out.contains("system noise"), "{out}");
    }

    #[test]
    fn zero_truncate_collapses_whitespace_without_cutting() {
        let entries = vec![entry(
            "uuid-0",
            0,
            MessageType::User,
            "line one\n\n   line two",
        )];
        let out = render(
            &entries,
            &SessionSource::Index,
            &opts(window()),
            &full_display(),
        );
        assert!(out.contains("User: line one line two"), "{out}");
    }

    #[test]
    fn paging_footer_reports_the_next_offset() {
        let entries: Vec<_> = (0..5)
            .map(|i| entry(&format!("uuid-{i}"), i, MessageType::User, "text"))
            .collect();
        let mut w = window();
        w.limit = 2;

        let out = render(&entries, &SessionSource::Index, &opts(w), &full_display());
        assert!(out.ends_with("+more: offset=2\n"), "{out}");
    }
}
