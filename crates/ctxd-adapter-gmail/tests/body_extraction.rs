//! Body extraction:
//! - prefer text/plain over text/html
//! - fall back to text/html with a stripped-to-text body
//! - cap body at 128 KB

use base64::Engine;
use ctxd_adapter_gmail::parse::extract_body;
use ctxd_adapter_gmail::MAX_BODY_SIZE;
use serde_json::json;

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[test]
fn plain_preferred_over_html() {
    let plain = b64url(b"plain wins");
    let html = b64url(b"<p>html loses</p>");
    let payload = json!({
        "mimeType": "multipart/alternative",
        "parts": [
            { "mimeType": "text/html", "body": { "data": html } },
            { "mimeType": "text/plain", "body": { "data": plain } }
        ]
    });
    assert_eq!(extract_body(&payload), "plain wins");
}

#[test]
fn html_fallback_when_no_plain() {
    let html = b64url(b"<html><body>html <i>only</i></body></html>");
    let payload = json!({
        "mimeType": "text/html",
        "body": { "data": html }
    });
    let out = extract_body(&payload);
    assert!(out.contains("html"));
    assert!(out.contains("only"));
    assert!(!out.contains('<'));
}

#[test]
fn body_capped_at_128k() {
    // 200KB of 'a'.
    let big = "a".repeat(MAX_BODY_SIZE * 2);
    let encoded = b64url(big.as_bytes());
    let payload = json!({
        "mimeType": "text/plain",
        "body": { "data": encoded }
    });
    let out = extract_body(&payload);
    assert_eq!(out.len(), MAX_BODY_SIZE);
}

#[test]
fn empty_payload_returns_empty_string() {
    let payload = json!({ "mimeType": "text/plain" });
    assert_eq!(extract_body(&payload), "");
}

#[test]
fn html_with_script_strips_script_content() {
    let html = b64url(b"<p>visible</p><script>secret = 1;</script><p>also visible</p>");
    let payload = json!({
        "mimeType": "text/html",
        "body": { "data": html }
    });
    let out = extract_body(&payload);
    assert!(out.contains("visible"));
    assert!(out.contains("also visible"));
    assert!(!out.contains("secret"));
}

#[test]
fn nested_multipart_text_plain_found() {
    let plain = b64url(b"deep plain");
    let payload = json!({
        "mimeType": "multipart/mixed",
        "parts": [
            {
                "mimeType": "multipart/alternative",
                "parts": [
                    { "mimeType": "text/plain", "body": { "data": plain } },
                    { "mimeType": "text/html", "body": { "data": b64url(b"<p>html</p>") } }
                ]
            }
        ]
    });
    assert_eq!(extract_body(&payload), "deep plain");
}
