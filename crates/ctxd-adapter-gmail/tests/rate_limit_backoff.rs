//! When the Gmail API returns 429 with `Retry-After: 1`, the adapter
//! sleeps and retries.

mod common;

use std::time::Instant;

use common::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn retry_after_429_observable_delay() {
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

    // Profile (used by full sync).
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "emailAddress": "test@example.com",
            "historyId": "10"
        })))
        .mount(&server)
        .await;

    // First call to messages.list: 429 with Retry-After: 1.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "1")
                .set_body_json(json!({"error": "rate limited"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second call succeeds.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let (dir, state_path) = fresh_state_dir("RT-1").await;
    let cfg = make_adapter_config(
        &state_path,
        &format!("{}/token", server.uri()),
        &server.uri(),
        vec!["INBOX".to_string()],
    );

    let started = Instant::now();
    let (result, _events) = run_once(cfg).await;
    let elapsed = started.elapsed();
    result.expect("rate-limited sync should succeed after retry");

    // Allow some slack: must sleep at least ~1s, but tight enough that
    // we know we actually honored the Retry-After header instead of
    // falling back to default backoff.
    assert!(
        elapsed.as_millis() >= 900,
        "expected at least 900ms elapsed (Retry-After: 1s), got {}ms",
        elapsed.as_millis()
    );
    assert!(
        elapsed.as_secs() < 10,
        "elapsed should be under 10s, got {:?}",
        elapsed
    );

    drop(dir);
}
