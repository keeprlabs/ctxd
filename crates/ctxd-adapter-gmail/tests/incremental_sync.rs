//! Incremental sync via the Gmail History API publishes the right events.

mod common;

use common::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn history_list_publishes_two_events() {
    let server = MockServer::start().await;

    // OAuth refresh — needs a generic stub since the adapter calls it
    // on every run.
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "AT-1",
            "expires_in": 3600,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    // First run with no cursor: getProfile + messages.list. We pretend
    // the user has no messages so the initial sync is a no-op, and
    // record historyId=100.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "emailAddress": "test@example.com",
            "historyId": "100"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    // history.list returns 2 new messages.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "historyId": "150",
            "history": [
                {
                    "messagesAdded": [
                        { "message": { "id": "msg-A", "labelIds": ["INBOX"] } },
                        { "message": { "id": "msg-B", "labelIds": ["INBOX"] } }
                    ]
                }
            ]
        })))
        .mount(&server)
        .await;

    // messages.get returns full payloads for both messages.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/msg-A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg-A",
            "threadId": "thread-A",
            "labelIds": ["INBOX"],
            "snippet": "first message",
            "payload": {
                "mimeType": "text/plain",
                "headers": [
                    { "name": "From", "value": "alice@example.com" },
                    { "name": "To", "value": "me@example.com" },
                    { "name": "Subject", "value": "First!" },
                    { "name": "Date", "value": "Mon, 01 Apr 2024 12:00:00 +0000" },
                    { "name": "Message-ID", "value": "<a@example.com>" }
                ],
                "body": { "data": b64url(b"hello A") }
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/msg-B"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg-B",
            "threadId": "thread-B",
            "labelIds": ["INBOX"],
            "snippet": "second message",
            "payload": {
                "mimeType": "text/plain",
                "headers": [
                    { "name": "From", "value": "bob@example.com" },
                    { "name": "To", "value": "me@example.com" },
                    { "name": "Subject", "value": "Second" },
                    { "name": "Date", "value": "Mon, 01 Apr 2024 12:30:00 +0000" },
                    { "name": "Message-ID", "value": "<b@example.com>" }
                ],
                "body": { "data": b64url(b"hello B") }
            }
        })))
        .mount(&server)
        .await;

    // First run: bootstrap cursor.
    let (dir, state_path) = fresh_state_dir("RT-1").await;
    {
        let cfg = make_adapter_config(
            &state_path,
            &format!("{}/token", server.uri()),
            &server.uri(),
            vec!["INBOX".to_string()],
        );
        let (result, events) = run_once(cfg).await;
        result.expect("initial sync");
        // No messages were added on the first sync (the messages.list
        // mock returns empty), so events should be empty.
        let evs = events.lock().await;
        assert_eq!(evs.len(), 0);
    }

    // Second run: cursor already present, history.list returns the two
    // new messages.
    {
        let cfg = make_adapter_config(
            &state_path,
            &format!("{}/token", server.uri()),
            &server.uri(),
            vec!["INBOX".to_string()],
        );
        let (result, events) = run_once(cfg).await;
        result.expect("incremental sync");
        let received = events_of_type(&events, "email.received").await;
        assert_eq!(
            received.len(),
            2,
            "expected 2 email.received events, got {}",
            received.len()
        );
        // Subjects must include the message ids and the inbox label.
        let subjects: Vec<&String> = received.iter().map(|(s, _, _)| s).collect();
        assert!(subjects.iter().any(|s| s.contains("msg-a")));
        assert!(subjects.iter().any(|s| s.contains("msg-b")));
        for (subject, event_type, data) in &received {
            assert_eq!(event_type, "email.received");
            assert!(subject.starts_with("/work/email/gmail/inbox/"));
            let internal = data["gmail_internal_id"].as_str().unwrap_or("");
            let thread = data["thread_id"].as_str().unwrap_or("");
            // Each fixture ties msg-A → thread-A and msg-B → thread-B.
            let last_msg_char = internal.chars().last().unwrap_or('?');
            let last_thread_char = thread.chars().last().unwrap_or('?');
            assert_eq!(last_msg_char, last_thread_char);
            assert!(data["body"].as_str().unwrap_or("").contains("hello"));
            assert!(!data["from"].as_str().unwrap_or("").is_empty());
            assert!(data["labels"].is_array());
        }
    }

    drop(dir);
}
