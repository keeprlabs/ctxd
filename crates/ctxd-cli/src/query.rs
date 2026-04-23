//! Basic EventQL parser for a subset of the query language.
//!
//! Supported syntax:
//! ```text
//! FROM <var> IN events
//! WHERE <var>.subject LIKE "<pattern>"
//!   AND <var>.type = "<value>"
//!   AND <var>.time > "<timestamp>"
//!   AND <var>.time < "<timestamp>"
//! PROJECT INTO <var>
//! ```

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;

/// A parsed EventQL query.
#[derive(Debug)]
pub struct EventQuery {
    /// The variable name (e.g., "e").
    #[allow(dead_code)]
    pub var: String,
    /// WHERE clause conditions.
    pub conditions: Vec<Condition>,
}

/// A single WHERE condition.
#[derive(Debug)]
pub enum Condition {
    /// subject LIKE "pattern"
    SubjectLike(String),
    /// type = "value"
    TypeEquals(String),
    /// time > "timestamp"
    TimeAfter(String),
    /// time < "timestamp"
    TimeBefore(String),
}

/// Parse an EventQL query string.
pub fn parse_query(input: &str) -> Result<EventQuery, String> {
    let input = input.trim();

    // Parse FROM clause
    let upper = input.to_uppercase();
    if !upper.starts_with("FROM ") {
        return Err("query must start with FROM".to_string());
    }
    let rest = &input[5..].trim_start();

    // Extract variable name
    let var_end = rest
        .find(|c: char| c.is_whitespace())
        .ok_or("expected variable name after FROM")?;
    let var = rest[..var_end].to_string();
    let rest = rest[var_end..].trim_start();

    // Check "IN events"
    let rest_upper = rest.to_uppercase();
    if !rest_upper.starts_with("IN EVENTS") {
        return Err("expected 'IN events' after variable name".to_string());
    }
    let rest = rest[9..].trim_start();

    // Parse WHERE clause (optional)
    let mut conditions = Vec::new();
    let rest_upper = rest.to_uppercase();
    if rest_upper.starts_with("WHERE ") {
        let rest = &rest[6..];
        parse_where_conditions(rest, &var, &mut conditions)?;
    } else if !rest_upper.starts_with("PROJECT") && !rest.is_empty() {
        return Err(format!("unexpected token: {rest}"));
    }

    Ok(EventQuery { var, conditions })
}

fn parse_where_conditions(
    input: &str,
    var: &str,
    conditions: &mut Vec<Condition>,
) -> Result<(), String> {
    // Split by AND (case-insensitive), handling the first condition and subsequent AND-joined ones
    let mut remaining = input.trim();

    loop {
        // Remove any leading "PROJECT INTO ..." at the end
        let upper = remaining.to_uppercase();
        if upper.starts_with("PROJECT") || remaining.is_empty() {
            break;
        }

        // Parse one condition
        let prefix = format!("{var}.");
        if !remaining.starts_with(&prefix) {
            return Err(format!(
                "expected '{prefix}' in condition, got: {remaining}"
            ));
        }
        let after_var = &remaining[prefix.len()..];

        let cond_upper = after_var.to_uppercase();
        if cond_upper.starts_with("SUBJECT LIKE ") {
            let val_start = 13; // len("SUBJECT LIKE ")
            let value = extract_quoted_value(&after_var[val_start..])?;
            conditions.push(Condition::SubjectLike(value.0));
            remaining = value.1.trim_start();
        } else if cond_upper.starts_with("TYPE = ") {
            let val_start = 7;
            let value = extract_quoted_value(&after_var[val_start..])?;
            conditions.push(Condition::TypeEquals(value.0));
            remaining = value.1.trim_start();
        } else if cond_upper.starts_with("TIME > ") {
            let val_start = 7;
            let value = extract_quoted_value(&after_var[val_start..])?;
            conditions.push(Condition::TimeAfter(value.0));
            remaining = value.1.trim_start();
        } else if cond_upper.starts_with("TIME < ") {
            let val_start = 7;
            let value = extract_quoted_value(&after_var[val_start..])?;
            conditions.push(Condition::TimeBefore(value.0));
            remaining = value.1.trim_start();
        } else {
            return Err(format!("unknown condition: {after_var}"));
        }

        // Check for AND
        let upper = remaining.to_uppercase();
        if upper.starts_with("AND ") {
            remaining = remaining[4..].trim_start();
        }
    }

    Ok(())
}

/// Extract a quoted string value and return (value, remaining).
fn extract_quoted_value(input: &str) -> Result<(String, &str), String> {
    let input = input.trim_start();
    let quote_char = input.chars().next().ok_or("expected quoted value")?;
    if quote_char != '"' && quote_char != '\'' {
        return Err(format!("expected quote, got: {quote_char}"));
    }
    let after_quote = &input[1..];
    let end = after_quote
        .find(quote_char)
        .ok_or("unterminated quoted value")?;
    let value = after_quote[..end].to_string();
    let remaining = &after_quote[end + 1..];
    Ok((value, remaining))
}

/// Execute an EventQL query against the store.
pub async fn execute_query(store: &EventStore, query: &EventQuery) -> Result<Vec<Event>, String> {
    // Get all subjects
    let all_subjects = store
        .subjects(None, false)
        .await
        .map_err(|e| format!("failed to list subjects: {e}"))?;

    // Filter subjects by LIKE pattern if present
    let subject_pattern = query.conditions.iter().find_map(|c| match c {
        Condition::SubjectLike(p) => Some(p.as_str()),
        _ => None,
    });

    let matching_subjects: Vec<&String> = if let Some(pattern) = subject_pattern {
        all_subjects
            .iter()
            .filter(|s| sql_like_match(s, pattern))
            .collect()
    } else {
        all_subjects.iter().collect()
    };

    // Collect events from matching subjects
    let mut all_events = Vec::new();
    for subj_str in matching_subjects {
        let subj = Subject::new(subj_str).map_err(|e| format!("invalid subject: {e}"))?;
        let events = store
            .read(&subj, false)
            .await
            .map_err(|e| format!("read failed: {e}"))?;
        all_events.extend(events);
    }

    // Apply remaining filters
    let type_filter = query.conditions.iter().find_map(|c| match c {
        Condition::TypeEquals(t) => Some(t.as_str()),
        _ => None,
    });
    let time_after = query.conditions.iter().find_map(|c| match c {
        Condition::TimeAfter(t) => Some(t.as_str()),
        _ => None,
    });
    let time_before = query.conditions.iter().find_map(|c| match c {
        Condition::TimeBefore(t) => Some(t.as_str()),
        _ => None,
    });

    let filtered: Vec<Event> = all_events
        .into_iter()
        .filter(|e| {
            if let Some(t) = type_filter {
                if e.event_type != t {
                    return false;
                }
            }
            if let Some(after) = time_after {
                if let Ok(threshold) = chrono::DateTime::parse_from_rfc3339(after) {
                    if e.time <= threshold {
                        return false;
                    }
                }
            }
            if let Some(before) = time_before {
                if let Ok(threshold) = chrono::DateTime::parse_from_rfc3339(before) {
                    if e.time >= threshold {
                        return false;
                    }
                }
            }
            true
        })
        .collect();

    Ok(filtered)
}

/// Basic SQL LIKE matching (supports % wildcard).
fn sql_like_match(value: &str, pattern: &str) -> bool {
    if pattern == "%" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('%') {
        return value.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('%') {
        return value.ends_with(suffix);
    }
    if pattern.contains('%') {
        let parts: Vec<&str> = pattern.split('%').collect();
        if parts.len() == 2 {
            return value.starts_with(parts[0]) && value.ends_with(parts[1]);
        }
    }
    value == pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_query() {
        let q = parse_query(
            r#"FROM e IN events WHERE e.subject LIKE "/test/%" AND e.type = "ctx.note" PROJECT INTO e"#,
        )
        .unwrap();
        assert_eq!(q.var, "e");
        assert_eq!(q.conditions.len(), 2);
        assert!(matches!(&q.conditions[0], Condition::SubjectLike(p) if p == "/test/%"));
        assert!(matches!(&q.conditions[1], Condition::TypeEquals(t) if t == "ctx.note"));
    }

    #[test]
    fn parse_time_conditions() {
        let q = parse_query(
            r#"FROM e IN events WHERE e.time > "2025-01-01T00:00:00Z" AND e.time < "2025-12-31T23:59:59Z" PROJECT INTO e"#,
        )
        .unwrap();
        assert_eq!(q.conditions.len(), 2);
        assert!(matches!(&q.conditions[0], Condition::TimeAfter(t) if t == "2025-01-01T00:00:00Z"));
        assert!(
            matches!(&q.conditions[1], Condition::TimeBefore(t) if t == "2025-12-31T23:59:59Z")
        );
    }

    #[test]
    fn parse_full_query() {
        let q = parse_query(
            r#"FROM e IN events WHERE e.subject LIKE "/pattern/%" AND e.type = "ctx.note" AND e.time > "2025-01-01T00:00:00Z" PROJECT INTO e"#,
        )
        .unwrap();
        assert_eq!(q.var, "e");
        assert_eq!(q.conditions.len(), 3);
    }

    #[test]
    fn parse_no_where() {
        let q = parse_query("FROM e IN events PROJECT INTO e").unwrap();
        assert_eq!(q.var, "e");
        assert_eq!(q.conditions.len(), 0);
    }

    #[test]
    fn parse_error_no_from() {
        assert!(parse_query("SELECT * FROM events").is_err());
    }

    #[test]
    fn parse_error_wrong_collection() {
        assert!(parse_query("FROM e IN tables").is_err());
    }

    #[test]
    fn sql_like_matching() {
        assert!(sql_like_match("/test/hello", "/test/%"));
        assert!(!sql_like_match("/other/hello", "/test/%"));
        assert!(sql_like_match("/anything", "%"));
        assert!(sql_like_match("/test/hello", "%hello"));
        assert!(sql_like_match("/test/hello", "/test/hello"));
    }
}
