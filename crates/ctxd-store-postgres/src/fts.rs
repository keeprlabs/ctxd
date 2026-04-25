//! Postgres FTS (full-text search) helpers.
//!
//! The actual FTS implementation lives in `store.rs::PostgresStore::search`
//! because it needs to share the row-decoder with the rest of the
//! event-reading paths. This module exists for a single utility:
//! escaping caller-supplied query strings on the off chance they
//! contain `websearch_to_tsquery`-meta characters that we want to
//! pass through literally.
//!
//! In practice `websearch_to_tsquery` is permissive — it tolerates
//! unbalanced quotes and unknown operators by treating them as
//! literals — so we don't need to escape much. The function exists so
//! a future hardening pass has a place to land additional rules.

/// Pass-through query sanitizer.
///
/// `websearch_to_tsquery` is designed to accept user input, so the
/// only thing we strip is null bytes (which Postgres rejects anyway)
/// and control characters that can't appear in a sane query.
pub fn sanitize_query(input: &str) -> String {
    input
        .chars()
        .filter(|c| *c == '\t' || *c == '\n' || !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_passes_through_normal_text() {
        assert_eq!(sanitize_query("hello world"), "hello world");
        assert_eq!(sanitize_query("\"exact phrase\""), "\"exact phrase\"");
        assert_eq!(sanitize_query("foo or bar"), "foo or bar");
    }

    #[test]
    fn sanitize_strips_null_bytes_and_control_chars() {
        assert_eq!(sanitize_query("hel\0lo"), "hello");
        assert_eq!(sanitize_query("a\x07b"), "ab");
    }

    #[test]
    fn sanitize_keeps_whitespace() {
        assert_eq!(sanitize_query("a\tb\nc"), "a\tb\nc");
    }
}
