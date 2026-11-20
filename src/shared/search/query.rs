//! User query text to a tantivy query.
//!
//! User text is code more often than prose, so `word:` is a field clause only
//! when `word` is a schema field; every other colon outside a quoted phrase
//! is escaped before parsing (`std::fs::read`, `error: foo`, `http://x`).
//! Text that still fails strict parsing goes through `parse_query_lenient`,
//! and its parse errors are returned beside the query so the caller can say
//! the query was reinterpreted. A query is never refused for its syntax.

use tantivy::Index;
use tantivy::query::{Query, QueryParser};
use tantivy::schema::{Field, Schema};

/// The parsed query and, when strict parsing failed, why.
pub(super) struct TextQuery {
    pub(super) query: Box<dyn Query>,
    pub(super) lenient: Option<String>,
}

pub(super) fn parse_text_query(index: &Index, default_fields: Vec<Field>, text: &str) -> TextQuery {
    let schema = index.schema();
    let escaped = escape_non_field_colons(text, &schema);
    let parser = QueryParser::for_index(index, default_fields);
    match parser.parse_query(&escaped) {
        Ok(query) => TextQuery {
            query,
            lenient: None,
        },
        Err(strict) => {
            let (query, errors) = parser.parse_query_lenient(&escaped);
            let reasons: Vec<String> = errors
                .iter()
                .map(ToString::to_string)
                .collect();
            TextQuery {
                query,
                lenient: Some(if reasons.is_empty() {
                    strict.to_string()
                } else {
                    reasons.join("; ")
                }),
            }
        }
    }
}

/// Escapes every colon outside a quoted phrase that does not end a schema
/// field name.
fn escape_non_field_colons(text: &str, schema: &Schema) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word_start = 0;
    let mut in_quotes = false;
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => {
                out.push(c);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                out.push(c);
                word_start = out.len();
            }
            ':' if !in_quotes => {
                let word = out[word_start..].trim_start_matches(['+', '-', '(']);
                if !word.is_empty()
                    && schema
                        .get_field(word)
                        .is_ok()
                {
                    out.push(':');
                    word_start = out.len();
                } else {
                    out.push_str("\\:");
                }
            }
            c if c.is_whitespace() || c == '(' || c == ')' => {
                out.push(c);
                word_start = out.len();
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::indexer::SearchIndexer;

    #[test]
    fn only_schema_field_colons_stay_unescaped() {
        let (schema, _) = SearchIndexer::build_schema();
        let cases = [
            ("std::fs::read", r"std\:\:fs\:\:read"),
            ("error: foo", r"error\: foo"),
            ("http://x", r"http\://x"),
            ("content:foo", "content:foo"),
            ("+content:foo -project:bar", "+content:foo -project:bar"),
            ("(session_id:abc)", "(session_id:abc)"),
            ("content:a:b", r"content:a\:b"),
            (r#""a: b" c:d"#, r#""a: b" c\:d"#),
            (r"already\:escaped", r"already\:escaped"),
        ];
        for (input, expected) in cases {
            assert_eq!(escape_non_field_colons(input, &schema), expected, "{input}");
        }
    }
}
