//! Construction of ctxd events from GitHub JSON.
//!
//! GitHub responses are large (commonly 8–12 KB per issue, 20+ KB per PR).
//! We prune to the fields that downstream consumers actually need and
//! truncate body text to [`crate::MAX_BODY_BYTES`].

use serde_json::{json, Value};

use crate::MAX_BODY_BYTES;

/// Truncate a string to at most `max` UTF-8 bytes, preserving char boundaries.
///
/// If truncation occurs, a single `…` (3 UTF-8 bytes) is appended as a
/// sentinel.
pub fn truncate_body(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Walk back to a char boundary <= max - 3 (to leave room for the sentinel).
    let target = max.saturating_sub(3);
    let mut end = target;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 3);
    out.push_str(&s[..end]);
    out.push('…');
    out
}

/// Compute the subject path for an issue.
pub fn issue_subject(owner: &str, repo: &str, number: i64) -> String {
    format!("/work/github/{owner}/{repo}/issues/{number}")
}

/// Compute the subject path for a PR.
pub fn pr_subject(owner: &str, repo: &str, number: i64) -> String {
    format!("/work/github/{owner}/{repo}/pulls/{number}")
}

/// Compute the subject path for an issue comment.
pub fn issue_comment_subject(
    owner: &str,
    repo: &str,
    issue_number: i64,
    comment_id: i64,
) -> String {
    format!("/work/github/{owner}/{repo}/issues/{issue_number}/comments/{comment_id}")
}

/// Compute the subject path for a PR review comment.
pub fn pr_comment_subject(owner: &str, repo: &str, pr_number: i64, comment_id: i64) -> String {
    format!("/work/github/{owner}/{repo}/pulls/{pr_number}/comments/{comment_id}")
}

/// Compute the subject path for a notification.
pub fn notification_subject(id: &str) -> String {
    format!("/work/github/notifications/{id}")
}

/// Result of classifying a resource against any prior cursor state.
pub struct EventClass {
    /// The event type (e.g., `issue.opened`, `pr.merged`).
    pub event_type: &'static str,
}

/// Extract a string field from a GitHub JSON object, returning `""` if absent.
fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Extract an i64 field, returning 0 if absent.
fn i64_field(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Pull a summarized author/user object from a GitHub user value.
fn user_summary(user: Option<&Value>) -> Value {
    let Some(u) = user else { return Value::Null };
    json!({
        "login": str_field(u, "login"),
        "id": i64_field(u, "id"),
        "type": str_field(u, "type"),
    })
}

/// Build the pruned issue payload.
pub fn issue_payload(owner: &str, repo: &str, raw: &Value) -> Value {
    let body_full = raw.get("body").and_then(Value::as_str).unwrap_or("");
    let body = truncate_body(body_full, MAX_BODY_BYTES);
    json!({
        "owner": owner,
        "repo": repo,
        "number": i64_field(raw, "number"),
        "title": str_field(raw, "title"),
        "body": body,
        "body_full_size": body_full.len(),
        "state": str_field(raw, "state"),
        "author": user_summary(raw.get("user")),
        "labels": raw.get("labels").cloned().unwrap_or(Value::Array(vec![])),
        "assignees": raw.get("assignees").cloned().unwrap_or(Value::Array(vec![])),
        "milestone": raw.get("milestone").cloned().unwrap_or(Value::Null),
        "created_at": str_field(raw, "created_at"),
        "updated_at": str_field(raw, "updated_at"),
        "closed_at": raw.get("closed_at").cloned().unwrap_or(Value::Null),
        "html_url": str_field(raw, "html_url"),
    })
}

/// Build the pruned PR payload.
pub fn pr_payload(owner: &str, repo: &str, raw: &Value) -> Value {
    let body_full = raw.get("body").and_then(Value::as_str).unwrap_or("");
    let body = truncate_body(body_full, MAX_BODY_BYTES);
    json!({
        "owner": owner,
        "repo": repo,
        "number": i64_field(raw, "number"),
        "title": str_field(raw, "title"),
        "body": body,
        "body_full_size": body_full.len(),
        "state": str_field(raw, "state"),
        "author": user_summary(raw.get("user")),
        "labels": raw.get("labels").cloned().unwrap_or(Value::Array(vec![])),
        "assignees": raw.get("assignees").cloned().unwrap_or(Value::Array(vec![])),
        "milestone": raw.get("milestone").cloned().unwrap_or(Value::Null),
        "created_at": str_field(raw, "created_at"),
        "updated_at": str_field(raw, "updated_at"),
        "closed_at": raw.get("closed_at").cloned().unwrap_or(Value::Null),
        "merged": raw.get("merged").and_then(Value::as_bool).unwrap_or(false),
        "merged_at": raw.get("merged_at").cloned().unwrap_or(Value::Null),
        "merge_commit_sha": raw.get("merge_commit_sha").cloned().unwrap_or(Value::Null),
        "head": raw.get("head").cloned().unwrap_or(Value::Null),
        "base": raw.get("base").cloned().unwrap_or(Value::Null),
        "html_url": str_field(raw, "html_url"),
    })
}

/// Build the pruned comment payload.
///
/// `parent_number` is the issue or PR number this comment belongs to; for
/// review comments, GitHub returns `pull_request_url` rather than a direct
/// number, so the caller is responsible for extracting it.
pub fn comment_payload(
    owner: &str,
    repo: &str,
    parent_number: i64,
    parent_kind: &str,
    raw: &Value,
) -> Value {
    let body_full = raw.get("body").and_then(Value::as_str).unwrap_or("");
    let body = truncate_body(body_full, MAX_BODY_BYTES);
    json!({
        "owner": owner,
        "repo": repo,
        "id": i64_field(raw, "id"),
        "parent_kind": parent_kind,
        "parent_number": parent_number,
        "body": body,
        "body_full_size": body_full.len(),
        "author": user_summary(raw.get("user")),
        "created_at": str_field(raw, "created_at"),
        "updated_at": str_field(raw, "updated_at"),
        "html_url": str_field(raw, "html_url"),
    })
}

/// Build the pruned notification payload.
pub fn notification_payload(raw: &Value) -> Value {
    let subject = raw.get("subject").cloned().unwrap_or(Value::Null);
    let repository = raw.get("repository");
    let repo_full = repository
        .and_then(|r| r.get("full_name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "id": str_field(raw, "id"),
        "reason": str_field(raw, "reason"),
        "subject": subject,
        "repository": { "full_name": repo_full },
        "updated_at": str_field(raw, "updated_at"),
        "unread": raw.get("unread").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Decide the event type for an issue given prior cursor history.
///
/// `seen_before` is true if we have a cursor entry for this resource in the
/// state DB. v0.3 does not diff against a prior state snapshot beyond seen /
/// not-seen.
pub fn classify_issue(seen_before: bool, current_state: &str) -> EventClass {
    if !seen_before {
        return EventClass {
            event_type: "issue.opened",
        };
    }
    if current_state == "closed" {
        return EventClass {
            event_type: "issue.closed",
        };
    }
    EventClass {
        event_type: "issue.updated",
    }
}

/// Decide the event type for a PR given prior cursor history.
pub fn classify_pr(seen_before: bool, current_state: &str, merged: bool) -> EventClass {
    if !seen_before {
        return EventClass {
            event_type: "pr.opened",
        };
    }
    if merged {
        return EventClass {
            event_type: "pr.merged",
        };
    }
    if current_state == "closed" {
        return EventClass {
            event_type: "pr.closed",
        };
    }
    EventClass {
        event_type: "pr.updated",
    }
}

/// Decide the event type for a comment.
pub fn classify_comment(seen_before: bool) -> EventClass {
    if seen_before {
        EventClass {
            event_type: "comment.updated",
        }
    } else {
        EventClass {
            event_type: "comment.created",
        }
    }
}

/// Decide the event type for a notification (always received in v0.3).
pub fn classify_notification() -> EventClass {
    EventClass {
        event_type: "notification.received",
    }
}

/// Extract the issue number from a GitHub `issue_url` (`.../issues/{n}`).
pub fn issue_number_from_url(url: &str) -> Option<i64> {
    let last = url.rsplit('/').next()?;
    last.parse().ok()
}

/// Extract the PR number from a `pull_request_url` (`.../pulls/{n}`).
pub fn pr_number_from_url(url: &str) -> Option<i64> {
    let last = url.rsplit('/').next()?;
    last.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_body_keeps_short_strings() {
        let s = "hello";
        assert_eq!(truncate_body(s, 100), "hello");
    }

    #[test]
    fn truncate_body_cuts_long_strings() {
        let s = "x".repeat(20_000);
        let truncated = truncate_body(&s, MAX_BODY_BYTES);
        assert!(truncated.len() <= MAX_BODY_BYTES);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_body_respects_char_boundary() {
        // "é" is 2 bytes; place a multi-byte char near the boundary.
        let s = format!("{}é{}", "a".repeat(10), "b".repeat(20_000));
        let truncated = truncate_body(&s, 16);
        assert!(truncated.is_char_boundary(truncated.len() - '…'.len_utf8()));
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn subjects_well_formed() {
        assert_eq!(
            issue_subject("acme", "web", 42),
            "/work/github/acme/web/issues/42"
        );
        assert_eq!(
            pr_subject("acme", "web", 7),
            "/work/github/acme/web/pulls/7"
        );
        assert_eq!(
            issue_comment_subject("acme", "web", 42, 100),
            "/work/github/acme/web/issues/42/comments/100"
        );
        assert_eq!(
            pr_comment_subject("acme", "web", 7, 9),
            "/work/github/acme/web/pulls/7/comments/9"
        );
        assert_eq!(
            notification_subject("12345"),
            "/work/github/notifications/12345"
        );
    }

    #[test]
    fn classify_issue_first_seen_is_opened() {
        let c = classify_issue(false, "open");
        assert_eq!(c.event_type, "issue.opened");
    }

    #[test]
    fn classify_issue_already_seen_open_is_updated() {
        let c = classify_issue(true, "open");
        assert_eq!(c.event_type, "issue.updated");
    }

    #[test]
    fn classify_issue_close_transition() {
        let c = classify_issue(true, "closed");
        assert_eq!(c.event_type, "issue.closed");
    }

    #[test]
    fn classify_pr_lifecycle() {
        assert_eq!(classify_pr(false, "open", false).event_type, "pr.opened");
        assert_eq!(classify_pr(true, "open", false).event_type, "pr.updated");
        assert_eq!(classify_pr(true, "closed", true).event_type, "pr.merged");
        assert_eq!(classify_pr(true, "closed", false).event_type, "pr.closed");
    }

    #[test]
    fn classify_comment_lifecycle() {
        assert_eq!(classify_comment(false).event_type, "comment.created");
        assert_eq!(classify_comment(true).event_type, "comment.updated");
    }

    #[test]
    fn issue_number_from_url_handles_simple_url() {
        assert_eq!(
            issue_number_from_url("https://api.github.com/repos/a/b/issues/42"),
            Some(42)
        );
    }

    #[test]
    fn pr_number_from_url_handles_simple_url() {
        assert_eq!(
            pr_number_from_url("https://api.github.com/repos/a/b/pulls/9"),
            Some(9)
        );
    }
}
