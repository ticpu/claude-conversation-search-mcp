use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum SourceKind {
    MainSession,
    Agent { agent_id: String },
}

impl SourceKind {
    /// Classify a JSONL path as either a main session file or a subagent transcript.
    /// Subagent files contain a `subagents` component and are named `agent-<id>.jsonl`.
    /// This covers both `<uuid>/subagents/agent-*.jsonl` and nested workflow layouts
    /// like `<uuid>/subagents/workflows/wf_*/agent-*.jsonl`.
    pub fn classify(path: &Path) -> Self {
        let has_subagents = path
            .components()
            .any(|c| {
                c.as_os_str()
                    .to_str()
                    == Some("subagents")
            });

        if has_subagents
            && let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
            && let Some(id) = name
                .strip_prefix("agent-")
                .and_then(|s| s.strip_suffix(".jsonl"))
        {
            return SourceKind::Agent {
                agent_id: id.to_string(),
            };
        }

        SourceKind::MainSession
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_main_session() {
        let path = PathBuf::from(
            "/home/user/.claude/projects/-home-user-GIT-myproject/abc12345-1234-5678-abcd-ef0123456789.jsonl",
        );
        assert_eq!(SourceKind::classify(&path), SourceKind::MainSession);
    }

    #[test]
    fn test_agent_direct_subagent() {
        let path = PathBuf::from(
            "/home/user/.claude/projects/-home-user-GIT-myproject/abc12345-1234-5678-abcd-ef0123456789/subagents/agent-xyz789.jsonl",
        );
        assert_eq!(
            SourceKind::classify(&path),
            SourceKind::Agent {
                agent_id: "xyz789".to_string()
            }
        );
    }

    #[test]
    fn test_agent_workflow_subagent() {
        let path = PathBuf::from(
            "/home/user/.claude/projects/-home-user-GIT-myproject/abc12345-1234-5678-abcd-ef0123456789/subagents/workflows/wf_01abc/agent-def456.jsonl",
        );
        assert_eq!(
            SourceKind::classify(&path),
            SourceKind::Agent {
                agent_id: "def456".to_string()
            }
        );
    }

    #[test]
    fn test_non_agent_in_subagents_dir() {
        let path = PathBuf::from(
            "/home/user/.claude/projects/-home-user-GIT-myproject/abc12345/subagents/summary.jsonl",
        );
        assert_eq!(SourceKind::classify(&path), SourceKind::MainSession);
    }
}
