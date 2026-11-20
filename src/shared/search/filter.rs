//! Filters applied to search hits after tantivy's top-K.
//!
//! Project filter. A hit matches when its cwd contains the filter's path
//! components contiguously, and an absolute filter must be a prefix of it, so
//! a subdirectory or `worktrees/<x>` of the project matches. A leading `~/`
//! is the home directory, as results display it. The tantivy clause built by
//! `project_query` on the cwd field is a superset prefilter;
//! `project_matches` decides.

use crate::shared::format::{DisplayOptions, filter_content};
use crate::shared::models::SearchResult;
use anyhow::{Context, Result};
use std::path::{Component, Path, PathBuf};
use tantivy::Term;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption};

/// The default tokenizer drops tokens this long or longer, so a clause on
/// one could never match.
const MAX_INDEXED_TOKEN_LEN: usize = 40;

fn expand_tilde(filter: &str) -> PathBuf {
    match (filter.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(filter),
    }
}

fn normal_components(path: &Path) -> Vec<Component<'_>> {
    path.components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect()
}

/// Every token of the filter path must appear in the cwd field.
pub(super) fn project_query(cwd_field: Field, filter: &str) -> BooleanQuery {
    let path = expand_tilde(filter);
    let clauses = path
        .to_string_lossy()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty() && token.len() < MAX_INDEXED_TOKEN_LEN)
        .map(|token| {
            let term = Term::from_field_text(cwd_field, &token.to_lowercase());
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
            )
        })
        .collect();
    BooleanQuery::new(clauses)
}

pub(super) fn project_matches(cwd: &str, filter: &str) -> bool {
    let filter = expand_tilde(filter);
    let cwd = Path::new(cwd);
    if filter.is_absolute() {
        return cwd.starts_with(&filter);
    }
    let wanted = normal_components(&filter);
    !wanted.is_empty()
        && normal_components(cwd)
            .windows(wanted.len())
            .any(|w| w == wanted.as_slice())
}

/// Compile exclusion patterns, refusing an invalid one rather than searching
/// unfiltered.
pub fn compile_exclude_patterns(patterns: &[String]) -> Result<Vec<regex::Regex>> {
    patterns
        .iter()
        .map(|p| regex::Regex::new(p).with_context(|| format!("invalid exclude pattern '{p}'")))
        .collect()
}

/// Hits to leave out: excluded projects, excluded path/project regexes, and
/// an optional session (the caller's own).
#[derive(Debug, Default)]
pub struct ResultFilter {
    pub exclude_projects: Vec<String>,
    pub exclude_regexes: Vec<regex::Regex>,
    pub active_session: Option<String>,
}

impl ResultFilter {
    pub(super) fn keeps(&self, hit: &SearchResult, display: &DisplayOptions) -> bool {
        if !hit.is_displayable() || filter_content(&hit.content, display).is_none() {
            return false;
        }
        if self
            .active_session
            .as_deref()
            == Some(
                hit.session_id
                    .as_str(),
            )
        {
            return false;
        }
        if self
            .exclude_projects
            .contains(&hit.project)
        {
            return false;
        }
        !self
            .exclude_regexes
            .iter()
            .any(|re| re.is_match(&hit.project) || re.is_match(&hit.project_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_matches_subdirectories_and_worktrees() {
        let cases = [
            ("/home/u/GIT/foo", "/home/u/GIT/foo", true),
            ("/home/u/GIT/foo/src/bin", "/home/u/GIT/foo", true),
            ("/home/u/GIT/foo/worktrees/x", "/home/u/GIT/foo", true),
            ("/home/u/GIT/foo/worktrees/x", "foo", true),
            ("/home/u/GIT/foo/worktrees/x", "GIT/foo", true),
            ("/home/u/GIT/foobar", "/home/u/GIT/foo", false),
            ("/home/u/GIT/foobar", "foo", false),
            ("/elsewhere/foo", "/home/u/GIT/foo", false),
            ("/home/u/GIT/foo", "other", false),
            ("/home/u/GIT/foo", "", false),
        ];
        for (cwd, filter, expected) in cases {
            assert_eq!(project_matches(cwd, filter), expected, "{cwd} vs {filter}");
        }
    }

    #[test]
    fn tilde_filter_means_the_home_directory() {
        let home = dirs::home_dir().unwrap();
        let cwd = home.join("GIT/foo/worktrees/x");
        assert!(project_matches(&cwd.to_string_lossy(), "~/GIT/foo"));
    }
}
