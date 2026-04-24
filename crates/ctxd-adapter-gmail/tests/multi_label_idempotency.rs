//! A message that appears in two consecutive polling cycles must not
//! be re-published.

mod common;

use common::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn idempotent_publish_across_polls() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "AT-1",
            "expires_in": 3600,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    // Profile is used both during initial sync and as a no-op on later
    // calls.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "emailAddress": "test@example.com",
            "historyId": "100"
        })))
        .mount(&server)
        .await;

    // Both polls return the same single new message via history.list.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "historyId": "200",
            "history": [
                {
                    "messagesAdded": [
                        { "message": { "id": "dup-1", "labelIds": ["INBOX"] } }
                    ]
                }
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/dup-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "dup-1",
            "threadId": "T1",
            "labelIds": ["INBOX"],
            "snippet": "duplicate",
            "payload": {
                "mimeType": "text/plain",
                "headers": [
                    { "name": "From", "value": "z@example.com" },
                    { "name": "To", "value": "me@example.com" },
                    { "name": "Subject", "value": "Dup" },
                    { "name": "Date", "value": "Wed, 03 Apr 2024 10:00:00 +0000" }
                ],
                "body": { "data": b64url(b"dup body") }
            }
        })))
        .mount(&server)
        .await;

    let (dir, state_path) = fresh_state_dir("RT-1").await;

    // Pre-seed a cursor so we go through history.list, not full sync.
    {
        let store = ctxd_adapter_gmail::state::StateStore::open(&state_path.join("gmail.state.db"))
            .await
            .unwrap();
        store.set_cursor("1", chrono::Utc::now()).await.unwrap();
    }

    // First poll publishes the event.
    let cfg1 = make_adapter_config(
        &state_path,
        &format!("{}/token", server.uri()),
        &server.uri(),
        vec!["INBOX".to_string()],
    );
    let (r1, e1) = run_once(cfg1).await;
    r1.unwrap();
    let p1 = events_of_type(&e1, "email.received").await;
    assert_eq!(p1.len(), 1, "first poll should publish 1 event");

    // Second poll (a fresh sink) sees the same message; must not republish.
    let cfg2 = make_adapter_config(
        &state_path,
        &format!("{}/token", server.uri()),
        &server.uri(),
        vec!["INBOX".to_string()],
    );
    let (r2, e2) = run_once(cfg2).await;
    r2.unwrap();
    let p2 = events_of_type(&e2, "email.received").await;
    assert_eq!(
        p2.len(),
        0,
        "second poll must not republish the same (msg, label) pair"
    );

    // The persisted state should have exactly one (msg, label) row.
    let store = ctxd_adapter_gmail::state::StateStore::open(&state_path.join("gmail.state.db"))
        .await
        .unwrap();
    assert_eq!(store.published_count().await.unwrap(), 1);

    drop(dir);
}
