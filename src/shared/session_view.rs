//! One session rendered for the CLI `session` command and the MCP
//! `get_session_messages` tool.
//!
//! Entries arrive in display order and are not re-sorted: from JSONL, one
//! file in line order; from the index fallback, main transcripts first, then
//! subagent transcripts, each file in sequence order. sequence_num restarts
//! in every file, so sorting on it alone interleaves transcripts.
//!
//! Times are shown in the local timezone (TZ honoured). The header carries
//! the UTC offset; per-message times are in that same zone.

use super::format::{DisplayOptions, filter_content, local_stamp};
use super::models::{ConversationEntry, MessageType, SearchResult};
use super::parsers::JsonlParser;
use super::path_utils::{find_session_jsonl, home_to_tilde, session_of_transcript};
use super::search::SearchEngine;
use super::terminal::file_hyperlink;
use super::utils::truncate_content;
use anyhow::{Context, Result, bail};
use chrono::Local;
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
    /// Header and session statistics only, no messages.
    pub meta_only: bool,
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
    let Some(full_id) = resolve_in_index(&engine, session_id)? else {
        return Ok((Vec::new(), SessionSource::Index));
    };
    let results = engine
        .get_session_messages(&full_id)
        .with_context(|| format!("reading session {full_id} from the index"))?;

    Ok((
        results
            .into_iter()
            .map(ConversationEntry::from)
            .collect(),
        SessionSource::Index,
    ))
}

/// The one indexed session id starting with `prefix`, for a session whose
/// transcript is gone; more than one is an error naming them.
pub fn resolve_in_index(engine: &SearchEngine, prefix: &str) -> Result<Option<String>> {
    let mut ids = engine
        .session_ids_with_prefix(prefix)
        .with_context(|| format!("looking up session {prefix} in the index"))?;
    match ids.len() {
        0 | 1 => Ok(ids.pop()),
        n => bail!(
            "session id '{prefix}' matches {n} indexed sessions: {}",
            ids.join(", ")
        ),
    }
}

/// The full id of the session `prefix` names: its transcript's, else the
/// index's for a session whose transcript is gone.
pub fn resolve_session_id(engine: &SearchEngine, prefix: &str) -> Result<Option<String>> {
    match find_session_jsonl(prefix)? {
        Some(path) => Ok(session_of_transcript(&path)),
        None => resolve_in_index(engine, prefix),
    }
}

/// Displayable messages in the order `load_session` returned them, the only
/// ordering the window is defined against.
pub fn displayable_entries(entries: &[ConversationEntry]) -> Vec<ConversationEntry> {
    entries
        .iter()
        .filter(|e| e.is_displayable())
        .cloned()
        .collect()
}

pub fn select_window(entries: &[ConversationEntry], w: &Window) -> (Range<usize>, Option<usize>) {
    let total = entries.len();

    if let Some(ref uuid) = w.center
        && let Some(idx) = entries
            .iter()
            .position(|e| {
                e.uuid
                    .to_lowercase()
                    .starts_with(&uuid.to_lowercase())
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
    let first = &msgs[0];

    let mut output = resume_line(first);
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

    let project = home_to_tilde(
        first
            .cwd
            .as_deref()
            .unwrap_or(&first.project_path),
    );
    let time_range = format!(
        "{} - {}",
        local_stamp(&first.timestamp),
        local_stamp(&msgs[total - 1].timestamp)
    );
    let (session_cell, from_index) = match source {
        SessionSource::Index => (
            first
                .session_id
                .clone(),
            " (from index, content may be truncated)",
        ),
        SessionSource::Jsonl(path) => (
            file_hyperlink(&path.to_string_lossy(), &first.session_id),
            "",
        ),
    };
    let shown = if opts.meta_only {
        total.to_string()
    } else {
        format!("{}/{}", window.len(), total)
    };
    output.push_str(&format!(
        "📁 {} 🗒️ {} ({} msgs) ⏱️ {}{}\n",
        project, session_cell, shown, time_range, from_index
    ));

    if let Some(tags) = tag_line(entries) {
        output.push_str(&format!("tags: {tags}\n"));
    }
    if opts.meta_only {
        output.push_str(&stats_lines(entries, &msgs));
        return output;
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
                .with_timezone(&Local)
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

/// Sessions are stored per project directory and `claude -r` resolves the ID in the
/// current one, so the command changes into the session's cwd first.
fn resume_line(first: &ConversationEntry) -> String {
    let id = &first.session_id;
    match first
        .cwd
        .as_deref()
        .map(shlex::try_quote)
    {
        Some(Ok(cwd)) => format!("resume: cd {cwd} && claude -r {id}\n"),
        Some(Err(e)) => format!("resume: claude -r {id} (cwd not quotable: {e})\n"),
        None => format!("resume: claude -r {id}\n"),
    }
}

fn stats_lines(entries: &[ConversationEntry], displayable: &[ConversationEntry]) -> String {
    let counts: Vec<String> = [
        MessageType::User,
        MessageType::Assistant,
        MessageType::Summary,
    ]
    .iter()
    .filter_map(|kind| {
        let n = displayable
            .iter()
            .filter(|e| &e.message_type == kind)
            .count();
        (n > 0).then(|| format!("{}={n}", kind.short_name()))
    })
    .collect();
    let mut out = format!("messages: {}\n", counts.join(" "));

    let models: BTreeSet<&str> = entries
        .iter()
        .filter_map(|e| {
            e.model
                .as_deref()
        })
        .collect();
    if !models.is_empty() {
        let models: Vec<&str> = models
            .into_iter()
            .collect();
        out.push_str(&format!("models: {}\n", models.join(",")));
    }
    out
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
            meta_only: false,
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
            source_file: "/p/session-1.jsonl".to_string(),
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
        let warning = out
            .lines()
            .nth(1)
            .unwrap_or_default();
        assert!(
            warning.starts_with("warning: message nope not found"),
            "{out}"
        );
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
    fn prefix_lookup_prints_the_full_session_id() {
        let entries = vec![entry("uuid-0", 0, MessageType::User, "hello")];
        let mut o = opts(window());
        o.session_id = "sess".to_string();
        let out = render(&entries, &SessionSource::Index, &o, &full_display());
        assert!(
            out.starts_with("resume: cd /home/user/GIT/project-name && claude -r session-1\n"),
            "{out}"
        );
        assert!(out.contains("🗒️ session-1 "), "{out}");
    }

    #[test]
    fn resume_line_quotes_the_cwd() {
        let entries = vec![
            EntryBuilder::new("uuid-0", "session-1")
                .content("hello")
                .project("p", "/home/user/My Project")
                .build(),
        ];
        let out = render(
            &entries,
            &SessionSource::Index,
            &opts(window()),
            &full_display(),
        );
        assert!(
            out.starts_with("resume: cd '/home/user/My Project' && claude -r session-1\n"),
            "{out}"
        );
    }

    #[test]
    fn meta_only_prints_statistics_without_messages() {
        let mut ai = entry("uuid-1", 1, MessageType::Assistant, "answer text");
        ai.model = Some("claude-test".to_string());
        let entries = vec![
            entry("uuid-0", 0, MessageType::User, "question text"),
            ai,
            entry("uuid-2", 2, MessageType::User, "follow-up text"),
        ];
        let mut w = window();
        w.limit = 1;
        let mut o = opts(w);
        o.meta_only = true;
        let out = render(&entries, &SessionSource::Index, &o, &full_display());
        assert!(out.contains("(3 msgs)"), "{out}");
        assert!(out.contains("messages: User=2 AI=1\n"), "{out}");
        assert!(out.contains("models: claude-test\n"), "{out}");
        assert!(!out.contains("question text"), "{out}");
        assert!(!out.contains("+more"), "{out}");
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
