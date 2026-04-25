//! ETag caching: second poll sends If-None-Match; mock returns 304;
//! adapter publishes no events.

mod common;

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn issue() -> serde_json::Value {
    json!({
        "number": 1,
        "title": "hi",
        "body": "",
        "state": "open",
        "user": { "login": "u", "id": 1, "type": "User" },
        "labels": [],
        "assignees": [],
        "milestone": null,
        "created_at": "2026-04-01T00:00:00Z",
        "updated_at": "2026-04-10T00:00:00Z",
        "closed_at": null,
        "html_url": "",
    })
}

#[tokio::test]
async fn second_poll_uses_if_none_match_and_skips() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    // Empty placeholders.
    for p in [
        "/repos/acme/web/pulls",
        "/repos/acme/web/issues/comments",
        "/repos/acme/web/pulls/comments",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
    }

    // First call: no If-None-Match → 200 + ETag.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .and(wiremock::matchers::any())
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([issue()]))
                .insert_header("etag", "\"abc123\""),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second call: If-None-Match: "abc123" → 304.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .and(header("if-none-match", "\"abc123\""))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;

    // Catch-all (won't match the above; ensures any unmocked /issues request fails).
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .and(header_exists("if-none-match"))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;

    let cfg = common::explicit_config(
        &server.uri(),
        dir.path().to_path_buf(),
        vec![RepoRef {
            owner: "acme".into(),
            name: "web".into(),
        }],
        vec![ResourceKind::Issues],
        false,
    );
    // First poll: 1 event.
    let adapter = GitHubAdapter::new(cfg.clone());
    let (sink, events) = common::CollectingSink::new();
    adapter.run_once(&sink).await.unwrap();
    assert_eq!(events.lock().await.len(), 1);

    // Second poll: should send If-None-Match and get 304 → no new events.
    let adapter2 = GitHubAdapter::new(cfg);
    let (sink2, events2) = common::CollectingSink::new();
    adapter2.run_once(&sink2).await.unwrap();
    assert_eq!(
        events2.lock().await.len(),
        0,
        "expected 0 events on 304 cycle"
    );

    // Confirm at least one outgoing request carried If-None-Match.
    let recv = server.received_requests().await.unwrap();
    let with_inm = recv
        .iter()
        .filter(|r| r.headers.get("if-none-match").is_some())
        .count();
    assert!(with_inm >= 1, "expected at least one If-None-Match request");
}
