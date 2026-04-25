//! When `history.list` returns 404, the adapter must fall back to a
//! full `messages.list` sync.

mod common;

use common::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn history_404_triggers_full_sync() {
    let server = MockServer::start().await;

    // OAuth refresh.
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "AT-1",
            "expires_in": 3600,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    // history.list returns 404 with a Gmail-style error body that
    // mentions historyId / "Start history".
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/history"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {
                "code": 404,
                "message": "Requested entity was not found. Start history record id is too old."
            }
        })))
        .mount(&server)
        .await;

    // Profile call (used during full-sync to record historyId).
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "emailAddress": "test@example.com",
            "historyId": "999"
        })))
        .mount(&server)
        .await;

    // messages.list returns one message.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [ { "id": "fallback-1", "threadId": "T1" } ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/fallback-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "fallback-1",
            "threadId": "T1",
            "labelIds": ["INBOX"],
            "snippet": "fallback",
            "payload": {
                "mimeType": "text/plain",
                "headers": [
                    { "name": "From", "value": "x@y.com" },
                    { "name": "To", "value": "me@example.com" },
                    { "name": "Subject", "value": "Fallback" },
                    { "name": "Date", "value": "Tue, 02 Apr 2024 10:00:00 +0000" }
                ],
                "body": { "data": b64url(b"recovered after fallback") }
            }
        })))
        .mount(&server)
        .await;

    // Pre-seed a state DB with a stale historyId so the adapter takes
    // the history.list code path on the first iteration.
    let (dir, state_path) = fresh_state_dir("RT-1").await;
    {
        let store = ctxd_adapter_gmail::state::StateStore::open(&state_path.join("gmail.state.db"))
            .await
            .unwrap();
        store.set_cursor("1", chrono::Utc::now()).await.unwrap();
    }

    let cfg = make_adapter_config(
        &state_path,
        &format!("{}/token", server.uri()),
        &server.uri(),
        vec!["INBOX".to_string()],
    );
    let (result, events) = run_once(cfg).await;
    result.expect("fallback sync");

    let received = events_of_type(&events, "email.received").await;
    assert_eq!(received.len(), 1, "expected 1 fallback event");
    assert!(received[0].0.contains("fallback-1"));

    // Cursor must have advanced to the new historyId from the profile.
    let store = ctxd_adapter_gmail::state::StateStore::open(&state_path.join("gmail.state.db"))
        .await
        .unwrap();
    let cursor = store.cursor().await.unwrap();
    assert_eq!(cursor.history_id.as_deref(), Some("999"));

    drop(dir);
}
