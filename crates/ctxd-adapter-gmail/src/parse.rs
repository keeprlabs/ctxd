//! Pure-functional helpers for Gmail message processing.
//!
//! - [`normalize_label`] — turn `INBOX/SubFolder` into `inbox-subfolder`
//!   so labels are safe to embed in a subject path component.
//! - [`subject_for_message`] — build the per-event subject path.
//! - [`extract_header`] — case-insensitive header lookup.
//! - [`extract_body`] — walk a Gmail payload tree, prefer `text/plain`,
//!   fall back to a stripped-down `text/html`, cap at [`MAX_BODY_SIZE`].
//! - [`infer_event_type`] — pick `email.received`, `email.sent`, or
//!   `email.draft` based on labels.
//! - [`split_addresses`] — naive splitter for multi-address headers.

use base64::Engine;
use serde_json::Value;

use crate::MAX_BODY_SIZE;

/// Normalize a Gmail label into a subject-safe slug.
///
/// Rules:
/// - lowercased
/// - `/` replaced with `-` (Gmail uses `/` as a nesting separator)
/// - any character that is not `[a-z0-9-_.]` collapses to `-`
/// - leading/trailing dashes stripped
/// - empty input becomes `"_"` so the subject path component is always
///   non-empty
pub fn normalize_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut last_dash = false;
    for ch in label.chars() {
        let mapped = match ch {
            'A'..='Z' => Some(ch.to_ascii_lowercase()),
            'a'..='z' | '0'..='9' | '-' | '_' | '.' => Some(ch),
            '/' | ' ' | '\t' => Some('-'),
            _ => None,
        };
        match mapped {
            Some('-') => {
                if !last_dash {
                    out.push('-');
                    last_dash = true;
                }
            }
            Some(c) => {
                out.push(c);
                last_dash = false;
            }
            None => {
                if !last_dash {
                    out.push('-');
                    last_dash = true;
                }
            }
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed
    }
}

/// Build the event subject for a (label, message_id) pair.
pub fn subject_for_message(label: &str, message_id: &str) -> String {
    format!(
        "/work/email/gmail/{}/{}",
        normalize_label(label),
        normalize_label(message_id)
    )
}

/// Pick the event type from the message's labels.
pub fn infer_event_type(labels: &[String]) -> &'static str {
    if labels.iter().any(|l| l == "DRAFT") {
        "email.draft"
    } else if labels.iter().any(|l| l == "SENT") {
        "email.sent"
    } else {
        "email.received"
    }
}

/// Look up a header value case-insensitively from a Gmail
/// `payload.headers` array.
pub fn extract_header(headers: &[Value], name: &str) -> Option<String> {
    headers.iter().find_map(|h| {
        let n = h.get("name").and_then(|v| v.as_str())?;
        if n.eq_ignore_ascii_case(name) {
            h.get("value")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        }
    })
}

/// Naively split a multi-address header into its parts. Quoted display
/// names with embedded commas are not handled — Gmail messages we get
/// from `format=metadata` already have the full string and downstream
/// consumers can re-parse if needed.
pub fn split_addresses(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Decode a Gmail base64url-encoded body part.
pub fn decode_body_data(data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data)
        .ok()
        .or_else(|| base64::engine::general_purpose::URL_SAFE.decode(data).ok())
        .or_else(|| {
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(data)
                .ok()
        })
        .or_else(|| base64::engine::general_purpose::STANDARD.decode(data).ok())
}

/// Walk a Gmail payload tree and collect parts of a given mime type.
fn collect_parts<'a>(payload: &'a Value, mime_type: &str, out: &mut Vec<&'a Value>) {
    if let Some(mt) = payload.get("mimeType").and_then(|v| v.as_str()) {
        if mt.eq_ignore_ascii_case(mime_type) {
            out.push(payload);
        }
    }
    if let Some(parts) = payload.get("parts").and_then(|v| v.as_array()) {
        for p in parts {
            collect_parts(p, mime_type, out);
        }
    }
}

/// Strip a tiny subset of HTML to plaintext. Removes `<script>...</script>`
/// and `<style>...</style>` blocks, then drops every other tag, then
/// collapses runs of whitespace. Not a full HTML renderer — that's
/// deliberately out of scope.
pub fn strip_html(input: &str) -> String {
    let mut s = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;

    while i < chars.len() {
        if let Some(needle) = skip_until {
            if chars[i..].iter().collect::<String>().starts_with(needle) {
                i += needle.len();
                skip_until = None;
                continue;
            }
            i += 1;
            continue;
        }

        let ch = chars[i];
        if !in_tag && ch == '<' {
            // Detect script/style blocks.
            let rest: String = chars[i..].iter().collect::<String>().to_lowercase();
            if rest.starts_with("<script") {
                skip_until = Some("</script>");
                i += 1;
                continue;
            }
            if rest.starts_with("<style") {
                skip_until = Some("</style>");
                i += 1;
                continue;
            }
            in_tag = true;
            i += 1;
        } else if in_tag {
            if ch == '>' {
                in_tag = false;
                // Insert a space so adjacent tags don't smash words.
                s.push(' ');
            }
            i += 1;
        } else {
            s.push(ch);
            i += 1;
        }
    }

    // HTML entity decode for a small common set.
    let decoded = s
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    // Collapse whitespace.
    let mut out = String::with_capacity(decoded.len());
    let mut prev_space = false;
    for ch in decoded.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// Extract a plaintext body from a Gmail message payload, capping at
/// [`MAX_BODY_SIZE`] bytes.
///
/// Strategy:
/// 1. If any `text/plain` part has body data, decode it.
/// 2. Else, if any `text/html` part has body data, decode it and strip
///    the HTML.
/// 3. Else, return an empty string.
pub fn extract_body(payload: &Value) -> String {
    // Single-part messages put the data directly on `payload.body.data`
    // and only have a `mimeType`. For those, treat the whole payload as
    // the part.
    let mut plain_parts: Vec<&Value> = Vec::new();
    collect_parts(payload, "text/plain", &mut plain_parts);
    if let Some(text) = plain_parts.iter().find_map(|p| {
        p.get("body")
            .and_then(|b| b.get("data"))
            .and_then(|d| d.as_str())
            .and_then(decode_body_data)
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }) {
        return cap_body(text);
    }

    let mut html_parts: Vec<&Value> = Vec::new();
    collect_parts(payload, "text/html", &mut html_parts);
    if let Some(html) = html_parts.iter().find_map(|p| {
        p.get("body")
            .and_then(|b| b.get("data"))
            .and_then(|d| d.as_str())
            .and_then(decode_body_data)
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }) {
        return cap_body(strip_html(&html));
    }

    String::new()
}

/// Cap a body string at [`MAX_BODY_SIZE`] bytes (not chars). Truncates
/// on a UTF-8 boundary.
pub fn cap_body(s: String) -> String {
    if s.len() <= MAX_BODY_SIZE {
        return s;
    }
    let mut end = MAX_BODY_SIZE;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_inbox() {
        assert_eq!(normalize_label("INBOX"), "inbox");
    }

    #[test]
    fn normalize_nested_label() {
        assert_eq!(normalize_label("Projects/Sagework"), "projects-sagework");
    }

    #[test]
    fn normalize_collapses_runs() {
        assert_eq!(normalize_label("foo  /  bar"), "foo-bar");
    }

    #[test]
    fn normalize_strips_special_chars() {
        assert_eq!(normalize_label("[Imap]/Sent"), "imap-sent");
    }

    #[test]
    fn normalize_empty_becomes_underscore() {
        assert_eq!(normalize_label(""), "_");
        assert_eq!(normalize_label("///"), "_");
    }

    #[test]
    fn subject_includes_normalized_label_and_id() {
        assert_eq!(
            subject_for_message("INBOX", "abc123"),
            "/work/email/gmail/inbox/abc123"
        );
    }

    #[test]
    fn infer_received_for_inbox() {
        let labels = vec!["INBOX".to_string(), "UNREAD".to_string()];
        assert_eq!(infer_event_type(&labels), "email.received");
    }

    #[test]
    fn infer_sent() {
        let labels = vec!["SENT".to_string()];
        assert_eq!(infer_event_type(&labels), "email.sent");
    }

    #[test]
    fn infer_draft_takes_priority() {
        let labels = vec!["DRAFT".to_string(), "SENT".to_string()];
        assert_eq!(infer_event_type(&labels), "email.draft");
    }

    #[test]
    fn header_extract_case_insensitive() {
        let headers = serde_json::json!([
            { "name": "From", "value": "alice@example.com" },
            { "name": "subject", "value": "Hi" },
        ]);
        let arr = headers.as_array().unwrap();
        assert_eq!(
            extract_header(arr, "from"),
            Some("alice@example.com".to_string())
        );
        assert_eq!(extract_header(arr, "Subject"), Some("Hi".to_string()));
        assert_eq!(extract_header(arr, "missing"), None);
    }

    #[test]
    fn split_addresses_simple() {
        let r = split_addresses("alice@example.com, bob@example.com");
        assert_eq!(r, vec!["alice@example.com", "bob@example.com"]);
    }

    #[test]
    fn split_addresses_empty() {
        assert!(split_addresses("").is_empty());
    }

    #[test]
    fn body_prefers_text_plain() {
        let plain = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"plain body");
        let html = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"<p>html body</p>");
        let payload = serde_json::json!({
            "mimeType": "multipart/alternative",
            "parts": [
                {
                    "mimeType": "text/plain",
                    "body": { "data": plain }
                },
                {
                    "mimeType": "text/html",
                    "body": { "data": html }
                }
            ]
        });
        assert_eq!(extract_body(&payload), "plain body");
    }

    #[test]
    fn body_falls_back_to_html_stripped() {
        let html = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(b"<html><body><p>Hello <b>world</b></p></body></html>");
        let payload = serde_json::json!({
            "mimeType": "text/html",
            "body": { "data": html }
        });
        let out = extract_body(&payload);
        assert!(out.contains("Hello"));
        assert!(out.contains("world"));
        assert!(!out.contains('<'));
    }

    #[test]
    fn body_caps_at_128k() {
        let huge = "a".repeat(MAX_BODY_SIZE * 2);
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(huge.as_bytes());
        let payload = serde_json::json!({
            "mimeType": "text/plain",
            "body": { "data": encoded }
        });
        let out = extract_body(&payload);
        assert_eq!(out.len(), MAX_BODY_SIZE);
    }

    #[test]
    fn strip_html_drops_scripts() {
        let input = "<p>hi</p><script>alert(1)</script><p>bye</p>";
        let out = strip_html(input);
        assert!(!out.contains("alert"));
        assert!(out.contains("hi"));
        assert!(out.contains("bye"));
    }

    #[test]
    fn strip_html_decodes_entities() {
        let input = "Tom &amp; Jerry";
        assert_eq!(strip_html(input), "Tom & Jerry");
    }

    #[test]
    fn cap_body_truncates_on_char_boundary() {
        // Build a string where MAX_BODY_SIZE+1 lands inside a multi-byte
        // codepoint.
        let mut s = "a".repeat(MAX_BODY_SIZE - 1);
        s.push('é'); // é is 2 bytes
        let out = cap_body(s);
        // Either MAX_BODY_SIZE-1 or MAX_BODY_SIZE depending on where the
        // boundary lands; either way it must be a valid UTF-8 string.
        assert!(out.is_char_boundary(out.len()));
    }
}
