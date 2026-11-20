//! JSONL transcript parsing into ConversationEntry.
//!
//! sequence_num counts the entries kept from one file, from 0; it orders
//! messages within that file only.
//!
//! Summary lines carry only type, summary and leafUuid. Their session id
//! comes from the file path (path_utils), their uuid is the leafUuid, and
//! their timestamp and cwd are the leaf message's when the leaf is in the
//! same file, else those of the file's first timestamped message; with no
//! timestamped message the summary is skipped at debug level. A line whose timestamp
//! does not parse is skipped and counted, and one warning per file gives the
//! count and the first such line.

mod content;
pub mod source;

pub use source::SourceKind;

use super::config::get_config;
use super::metadata;
use super::models::{ConversationEntry, MessageType, RawJsonlMessage};
use super::path_utils::session_of_transcript;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::io::BufReader;
use std::path::Path;
use strip_ansi_escapes::strip_str;
use tracing::{debug, warn};

/// True when `line` is the last line of `content` and no newline ends it: a
/// writer may still be appending to it.
fn is_unterminated_tail(content: &str, line: &str) -> bool {
    let line_end = line.as_ptr() as usize + line.len();
    let content_end = content.as_ptr() as usize + content.len();
    line_end == content_end && !content.ends_with('\n')
}

/// Read text file, skipping UTF-8 BOM if present
fn read_text_file(path: &Path) -> Result<String> {
    use std::fs::File;
    use std::io::Read;

    let mut file = BufReader::new(File::open(path)?);
    let mut first3 = [0u8; 3];

    // Check for UTF-8 BOM (EF BB BF)
    match file.read_exact(&mut first3) {
        Ok(()) if first3 == [0xEF, 0xBB, 0xBF] => {
            // BOM found, skip it
        }
        Ok(()) => {
            // No BOM, include these bytes
            let mut content = String::from_utf8(first3.to_vec())?;
            file.read_to_string(&mut content)?;
            return Ok(content);
        }
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(String::from_utf8(first3[..].to_vec())?);
        }
        Err(e) => return Err(e.into()),
    }

    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

/// Where and when a message happened, borrowed by summary lines.
#[derive(Clone)]
pub struct Anchor {
    timestamp: String,
    cwd: Option<String>,
}

/// What a summary line takes from the rest of its file.
#[derive(Default)]
pub struct FileContext {
    session_id: Option<String>,
    by_uuid: HashMap<String, Anchor>,
    first: Option<Anchor>,
}

impl FileContext {
    fn new(path: &Path, raws: &[(usize, RawJsonlMessage)]) -> Self {
        let mut ctx = FileContext {
            session_id: session_of_transcript(path),
            ..Default::default()
        };
        for (_, raw) in raws {
            let Some(timestamp) = raw
                .timestamp
                .clone()
            else {
                continue;
            };
            let anchor = Anchor {
                timestamp,
                cwd: raw
                    .cwd
                    .clone(),
            };
            ctx.first
                .get_or_insert_with(|| anchor.clone());
            if let Some(uuid) = &raw.uuid {
                ctx.by_uuid
                    .insert(uuid.clone(), anchor);
            }
        }
        ctx
    }

    /// The leaf message when it is in this file, else the first timestamped one.
    fn anchor(&self, leaf_uuid: Option<&str>) -> Option<&Anchor> {
        leaf_uuid
            .and_then(|leaf| {
                self.by_uuid
                    .get(leaf)
            })
            .or(self
                .first
                .as_ref())
    }
}

/// Lines skipped for an unparsable timestamp, reported once per file.
#[derive(Default)]
struct BadTimestamps {
    count: usize,
    first: Option<(usize, String)>,
}

impl BadTimestamps {
    fn record(&mut self, line: usize, value: String) {
        self.count += 1;
        self.first
            .get_or_insert((line, value));
    }

    fn report(&self, path: &Path) {
        if let Some((line, value)) = &self.first {
            warn!(
                "Skipped {} line(s) of {} with an unparsable timestamp (first at line {}: {:?})",
                self.count,
                path.display(),
                line,
                value
            );
        }
    }
}

#[derive(Default)]
pub struct JsonlParser {
    full_content: bool,
}

impl JsonlParser {
    pub fn with_full_content() -> Self {
        Self { full_content: true }
    }

    pub fn parse_file(&self, path: &Path) -> Result<Vec<ConversationEntry>> {
        let content = read_text_file(path)?;
        let mut entries = Vec::new();
        let project_name = self.extract_project_name(path);

        let file_agent_id = match SourceKind::classify(path) {
            SourceKind::Agent { agent_id } => Some(agent_id),
            SourceKind::MainSession => None,
        };

        let mut raws = Vec::new();
        for (line_num, line) in content
            .lines()
            .enumerate()
        {
            if line
                .trim()
                .is_empty()
            {
                continue;
            }

            match serde_json::from_str::<RawJsonlMessage>(line) {
                Ok(raw) => raws.push((line_num + 1, raw)),
                Err(e) if is_unterminated_tail(&content, line) => {
                    debug!(
                        "Skipping the unterminated last line of {}, likely mid-write: {}",
                        path.display(),
                        e
                    );
                }
                Err(e) => {
                    warn!("Invalid JSON at {}:{}: {}", path.display(), line_num + 1, e);
                }
            }
        }

        let file = FileContext::new(path, &raws);
        let mut bad_timestamps = BadTimestamps::default();
        let mut sequence_counter = 0;
        for (line_num, raw) in raws {
            match self.parse_raw_message(
                raw,
                &project_name,
                sequence_counter,
                &file_agent_id,
                &file,
            ) {
                Ok(Some(entry)) => {
                    entries.push(entry);
                    sequence_counter += 1;
                }
                Ok(None) => {}
                Err(value) => bad_timestamps.record(line_num, value),
            }
        }
        bad_timestamps.report(path);

        Ok(entries)
    }

    /// One line to an entry. None for a line that carries no message;
    /// `Err` holds a timestamp that does not parse.
    pub fn parse_raw_message(
        &self,
        raw: RawJsonlMessage,
        fallback_project: &str,
        sequence_num: usize,
        file_agent_id: &Option<String>,
        file: &FileContext,
    ) -> Result<Option<ConversationEntry>, String> {
        let Some(msg_type) = raw
            .message_type
            .as_deref()
        else {
            return Ok(None);
        };

        match msg_type {
            "user" | "assistant" | "summary" => {}
            _ => return Ok(None),
        }
        let is_summary = msg_type == "summary";

        let uuid = match (&raw.uuid, &raw.leaf_uuid) {
            (Some(uuid), _) => uuid.clone(),
            (None, Some(leaf)) if is_summary => leaf.clone(),
            _ => return Ok(None),
        };
        let session_id = match (&raw.session_id, &file.session_id) {
            (Some(id), _) => id.clone(),
            (None, Some(id)) if is_summary => id.clone(),
            _ => return Ok(None),
        };
        let timestamp_str = match raw
            .timestamp
            .as_deref()
        {
            Some(ts) => ts,
            None if is_summary => {
                let Some(anchor) = file.anchor(
                    raw.leaf_uuid
                        .as_deref(),
                ) else {
                    debug!("Summary {uuid} has no timestamped message to date it by");
                    return Ok(None);
                };
                anchor
                    .timestamp
                    .as_str()
            }
            None => return Ok(None),
        };
        let timestamp: DateTime<Utc> = timestamp_str
            .parse()
            .map_err(|_| timestamp_str.to_string())?;

        let message_type = match msg_type {
            "user" => MessageType::User,
            "assistant" => MessageType::Assistant,
            "summary" => MessageType::Summary,
            _ => MessageType::System,
        };

        let extracted = if msg_type == "summary" {
            content::SearchableContent {
                content: raw
                    .summary
                    .unwrap_or_default(),
                has_error: false,
                tools_used: Vec::new(),
            }
        } else {
            content::extract_searchable_content(&raw, self.full_content)
        };
        let mut has_error = extracted.has_error;
        let tools_used = extracted.tools_used;
        let content = strip_str(&extracted.content);

        if content
            .trim()
            .is_empty()
        {
            return Ok(None);
        }

        let cwd = match raw.cwd {
            None if is_summary => file
                .anchor(
                    raw.leaf_uuid
                        .as_deref(),
                )
                .and_then(|anchor| {
                    anchor
                        .cwd
                        .clone()
                }),
            cwd => cwd,
        };
        let project_path = cwd
            .as_ref()
            .map(|cwd| self.extract_project_name_from_path(cwd))
            .unwrap_or_else(|| fallback_project.to_string());

        let model = raw
            .message
            .as_ref()
            .and_then(|m| {
                m.model
                    .clone()
            });

        let agent_id = raw
            .agent_id
            .or_else(|| file_agent_id.clone());

        let (technologies, code_languages, has_code, tools_mentioned) = if get_config()
            .index
            .enable_tagging
        {
            let meta = metadata::extract_all_metadata(&content);
            has_error |= meta.has_error;
            let mut all_tools = meta.tools_mentioned;
            for tool in tools_used {
                if !all_tools.contains(&tool) {
                    all_tools.push(tool);
                }
            }
            (
                meta.technologies,
                meta.code_languages,
                meta.has_code,
                all_tools,
            )
        } else {
            (vec![], vec![], false, tools_used)
        };

        Ok(Some(ConversationEntry {
            uuid,
            parent_uuid: raw.parent_uuid,
            session_id,
            project_path,
            timestamp,
            message_type,
            content,
            model,
            cwd,
            sequence_num,
            is_sidechain: raw
                .is_sidechain
                .unwrap_or(false),
            agent_id,
            technologies,
            has_code,
            code_languages,
            has_error,
            tools_mentioned,
        }))
    }

    fn extract_project_name(&self, path: &Path) -> String {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    fn extract_project_name_from_path(&self, cwd_path: &str) -> String {
        let path = Path::new(cwd_path);
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| {
                c.as_os_str()
                    .to_str()
            })
            .collect();

        for i in (0..components.len()).rev() {
            let component = components[i];
            if matches!(
                component,
                "src" | "lib" | "bin" | "target" | "node_modules" | ".git"
            ) {
                continue;
            }
            if !component.starts_with('.') && component.len() > 1 {
                return component.to_string();
            }
        }

        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_user_message() {
        let json = r#"{"uuid":"abc123","sessionId":"sess1","type":"user","timestamp":"2025-12-28T10:00:00Z","message":{"role":"user","content":"Hello world"}}"#;
        let raw: RawJsonlMessage = serde_json::from_str(json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser
            .parse_raw_message(raw, "test", 0, &None, &FileContext::default())
            .unwrap()
            .unwrap();

        assert_eq!(entry.uuid, "abc123");
        assert_eq!(entry.content, "Hello world");
        assert_eq!(entry.message_type, MessageType::User);
    }

    #[test]
    fn test_skip_file_history_snapshot() {
        let json = r#"{"type":"file-history-snapshot","messageId":"xyz"}"#;
        let raw: RawJsonlMessage = serde_json::from_str(json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser.parse_raw_message(raw, "test", 0, &None, &FileContext::default());

        assert!(
            entry
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_parse_assistant_with_text_block() {
        let json = r#"{"uuid":"abc123","sessionId":"sess1","type":"assistant","timestamp":"2025-12-28T10:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Here is my response"}]}}"#;
        let raw: RawJsonlMessage = serde_json::from_str(json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser
            .parse_raw_message(raw, "test", 0, &None, &FileContext::default())
            .unwrap()
            .unwrap();

        assert_eq!(entry.content, "Here is my response");
        assert_eq!(entry.message_type, MessageType::Assistant);
    }

    #[test]
    fn test_parse_thinking_block() {
        let json = r#"{"uuid":"abc123","sessionId":"sess1","type":"assistant","timestamp":"2025-12-28T10:00:00Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think about this..."}]}}"#;
        let raw: RawJsonlMessage = serde_json::from_str(json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser
            .parse_raw_message(raw, "test", 0, &None, &FileContext::default())
            .unwrap()
            .unwrap();

        assert!(
            entry
                .content
                .contains("[thinking]")
        );
        assert!(
            entry
                .content
                .contains("Let me think about this")
        );
    }

    #[test]
    fn test_tool_result_truncation() {
        let long_content = "x".repeat(5000);
        let json = format!(
            r#"{{"uuid":"abc123","sessionId":"sess1","type":"assistant","timestamp":"2025-12-28T10:00:00Z","message":{{"role":"assistant","content":[{{"type":"tool_result","content":"{}"}}]}}}}"#,
            long_content
        );
        let raw: RawJsonlMessage = serde_json::from_str(&json).unwrap();
        let parser = JsonlParser::default();
        let entry = parser
            .parse_raw_message(raw, "test", 0, &None, &FileContext::default())
            .unwrap()
            .unwrap();

        assert!(
            entry
                .content
                .len()
                < get_config()
                    .limits
                    .tool_result_max_chars
                    + 100
        );
        assert!(
            entry
                .content
                .ends_with('…')
        );
    }

    const SESSION: &str = "5e5e5e5e-1111-2222-3333-444444444444";

    fn message(uuid: &str, ts: &str) -> String {
        format!(
            r#"{{"uuid":"{uuid}","sessionId":"{SESSION}","type":"user","timestamp":"{ts}","cwd":"/home/u/GIT/proj","message":{{"role":"user","content":"text of {uuid}"}}}}"#
        )
    }

    fn summary(leaf: &str, text: &str) -> String {
        format!(r#"{{"type":"summary","summary":"{text}","leafUuid":"{leaf}"}}"#)
    }

    fn parse(lines: &[String]) -> Vec<ConversationEntry> {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp
            .path()
            .join("-home-u-GIT-proj")
            .join(format!("{SESSION}.jsonl"));
        std::fs::create_dir_all(
            path.parent()
                .unwrap(),
        )
        .unwrap();
        std::fs::write(&path, lines.join("\n")).unwrap();
        JsonlParser::default()
            .parse_file(&path)
            .unwrap()
    }

    #[test]
    fn summary_lines_take_session_from_file_and_time_from_leaf() {
        let entries = parse(&[
            summary("m2", "Fixing the frobnicator"),
            summary("in-another-file", "Earlier work"),
            message("m1", "2026-03-01T10:00:00Z"),
            message("m2", "2026-03-01T11:00:00Z"),
        ]);
        let summaries: Vec<_> = entries
            .iter()
            .filter(|e| e.message_type == MessageType::Summary)
            .collect();
        assert_eq!(summaries.len(), 2);

        let leaf = summaries[0];
        assert_eq!(leaf.session_id, SESSION);
        assert_eq!(leaf.uuid, "m2");
        assert_eq!(leaf.content, "Fixing the frobnicator");
        assert_eq!(
            leaf.timestamp
                .to_rfc3339(),
            "2026-03-01T11:00:00+00:00"
        );
        assert_eq!(
            leaf.cwd
                .as_deref(),
            Some("/home/u/GIT/proj")
        );

        let orphan = summaries[1];
        assert_eq!(orphan.uuid, "in-another-file");
        assert_eq!(
            orphan
                .timestamp
                .to_rfc3339(),
            "2026-03-01T10:00:00+00:00",
            "a leaf outside the file dates the summary by the file's first message"
        );
    }

    /// Collects what the tracing subscriber writes.
    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn unparsable_timestamps_are_reported_once_per_file() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let entries = tracing::subscriber::with_default(subscriber, || {
            parse(&[
                message("m1", "2026-03-01T10:00:00Z"),
                message("bad1", "yesterday"),
                message("bad2", "soon"),
            ])
        });
        assert_eq!(entries.len(), 1);
        let log = String::from_utf8(
            captured
                .0
                .lock()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(
            log.lines()
                .count(),
            1,
            "{log}"
        );
        assert!(
            log.contains("Skipped 2 line(s)")
                && log.contains("line 2")
                && log.contains("yesterday"),
            "{log}"
        );
    }
}
